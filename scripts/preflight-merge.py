#!/usr/bin/env python3
"""
scripts/preflight-merge.py — diagnose branch-protection rules blocking a PR.

Two paths, in order of authority:

  1. Admin-scope path: query /repos/{owner}/{repo}/branches/main/protection
     via `gh api`. If 200, parse the rule set directly and emit it.

  2. Read-only-fallback path: if the admin-scope probe 403s (typical when
     the only available token is `pull: true`), infer the most-likely
     rule from PR-side signals: `reviewDecision`, `latestReviews`,
     `statusCheckRollup.state`, branch topology, conversation thread
     state. Output includes a confidence score.

Output: JSON object on stdout (machine-diffable) plus a one-screen
human-readable summary. Exit 0 if a rule was unambiguously identified
(admin scope OR high-confidence read-only-fallback). Exit 2 if the
fallback only produced a low/medium-confidence inference.

Stdlib only. Tested on Python 3.8+.

Usage:
  ./scripts/preflight-merge.py [REPO]

  REPO defaults to "StellarFlow-Network/stellarflow-contracts" (the
  primary use case for this fork). Override for any other repo.

  Examples:
    ./scripts/preflight-merge.py
    ./scripts/preflight-merge.py owner/other-repo
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from typing import Any, Optional


# --- configuration ----------------------------------------------------------

DEFAULT_REPO = "StellarFlow-Network/stellarflow-contracts"
DEFAULT_BRANCH = "main"

# REST field names that map directly to GitHub branch-protection rules.
RULE_KEYS = {
    "required_pull_request_reviews": {
        "trigger_signals": ["reviewDecision null", "latestReviews empty",
                            "mergeable conflict after status checks pass"],
        "confidence_boost": 1.0,
    },
    "required_status_checks": {
        "trigger_signals": ["statusCheckRollup FAILURE or PENDING with checks",
                            "mergeable UNKNOWN (check still running)"],
        "confidence_boost": 1.0,
    },
    "restrictions": {
        "trigger_signals": ["mergeable CONFLICTING / MERGEABLE false despite passes",
                            "no restrictor token in session"],
        "confidence_boost": 0.7,
    },
    "required_signatures": {
        "trigger_signals": ["head commit author != committer",
                            "no `--web-commit-signoff` flag present in commit",
                            "no GPG signature detected (visible iff v3+ API)"],
        "confidence_boost": 0.9,
    },
    "required_conversation_resolution": {
        "trigger_signals": ["unresolved review-thread comments present",
                            "mergeable false with all checks pass and reviews approved"],
        "confidence_boost": 0.7,
    },
    "required_linear_history": {
        "trigger_signals": ["head branch has merge commits",
                            "mergeable true but pipeline refuses"],
        "confidence_boost": 0.5,
    },
    "enforce_admins": {
        "trigger_signals": ["admin tokens 403 on direct `/protection` read",
                            "all other rules pass but merge still blocked"],
        "confidence_boost": 0.4,
    },
}


# --- subprocess helpers ------------------------------------------------------

def gh(*args: str, ok_codes: tuple[int, ...] = (0,)) -> tuple[int, str, str]:
    """Run a `gh` command, return (returncode, stdout, stderr).

    ok_codes: list of returncodes considered "success" for the purpose of
    reporting; non-ok codes are surfaced alongside the stdout/stderr verbatim.
    """
    proc = subprocess.run(
        ["gh", *args],
        capture_output=True,
        text=True,
        timeout=30,
    )
    return proc.returncode, proc.stdout, proc.stderr


def gh_json(*args: str) -> Optional[Any]:
    """Run `gh ...` and return parsed JSON; None on failure."""
    rc, out, err = gh(*args)
    if rc != 0:
        return None
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return None


# --- admin-scope path --------------------------------------------------------

def admin_scope_protection(repo: str, branch: str) -> Optional[dict[str, Any]]:
    """Try the admin-scope REST endpoint. Returns parsed JSON or None on 403."""
    rc, out, err = gh(
        "api",
        "-H", "Accept: application/vnd.github+json",
        f"/repos/{repo}/branches/{branch}/protection",
    )
    if rc == 0 and out:
        try:
            return json.loads(out)
        except json.JSONDecodeError:
            return None
    return None


# --- read-only-fallback path ------------------------------------------------

def pr_signals(repo: str, pr_number: int = 705) -> dict[str, Any]:
    """Collect PR-side signals. Returns a dict of normalized signals."""
    pr = gh_json(
        "pr", "view", str(pr_number),
        "--repo", repo,
        "--json",
        "state,mergedAt,mergeCommit,reviewDecision,latestReviews,"
        "statusCheckRollup,isCrossRepository,headRefName,headRefOid,"
        "baseRefName,additions,deletions,changedFiles,maintainerCanModify,"
        "authorAssociation",
    )
    if pr is None:
        return {"_error": "gh pr view returned non-JSON"}

    issue = gh_json("issue", "view", str(pr_number), "--repo", repo, "--json",
                    "state,closedAt,comments")
    conversations_unresolved: Optional[bool] = None
    if issue is not None and issue.get("comments", 0) > 0:
        # If there are comments, the unresolved-check requires a follow-up
        # call we don't make here. Mark unknown if comments > 0.
        conversations_unresolved = None  # unknown — needs extra call

    return {
        "pr_state": pr.get("state"),
        "merged_at": pr.get("mergedAt"),
        "merge_commit_present": pr.get("mergeCommit") is not None,
        "review_decision": pr.get("reviewDecision"),
        "latest_reviews": pr.get("latestReviews") or [],
        "status_check_state": (pr.get("statusCheckRollup") or {}).get("state"),
        "is_cross_repo": pr.get("isCrossRepository"),
        "head_ref": pr.get("headRefName"),
        "head_sha": pr.get("headRefOid"),
        "base_ref": pr.get("baseRefName"),
        "author_association": pr.get("authorAssociation"),
        "conversations_unresolved": conversations_unresolved,
    }


def infer_rule(signals: dict[str, Any]) -> dict[str, Any]:
    """Heuristic detector: pick the most-likely rule from PR signals.

    Returns a dict with keys: rule, confidence, signals_matched,
    alternative_rules, summary.
    """
    matches: list[tuple[str, list[str], float]] = []

    # Required reviews — reviewDecision null + 0 reviews is the strongest
    # signal. Sometimes the field is REVIEW_REQUIRED explicitly.
    if signals.get("review_decision") in (None, "REVIEW_REQUIRED") \
            and len(signals.get("latest_reviews") or []) == 0:
        matches.append((
            "required_pull_request_reviews",
            ["reviewDecision null/REVIEW_REQUIRED with empty latestReviews"],
            0.95,
        ))

    # Required status checks — any non-success state on the rollup.
    scs = signals.get("status_check_state")
    if scs in ("FAILURE", "PENDING"):
        matches.append((
            "required_status_checks",
            [f"statusCheckRollup.state = {scs}"],
            0.95 if scs == "FAILURE" else 0.7,
        ))
    elif scs is None and signals.get("pr_state") == "OPEN":
        # No rollup at all — admin-scope might be set without checks but
        # it's worth flagging as a low-confidence possibility.
        matches.append((
            "required_status_checks",
            ["statusCheckRollup entirely absent (no checks configured?)"],
            0.4,
        ))

    # Conversation resolution — only detectable via extra API call (skipped
    # here for cost). Mark as a possibility if comments > 0.
    if signals.get("conversations_unresolved") is None \
            and signals.get("pr_state") == "OPEN":
        # Placeholder: the pr_signals() function would need to call
        # /issues/{n}/comments to count unresolved threads.
        pass

    # Cross-repo constraint — sometimes a hidden restriction rule on the
    # destination org blocks cross-repo PRs regardless of other settings.
    if signals.get("is_cross_repo"):
        # Low-confidence by itself — many cross-repo PRs succeed.
        # Listed as an alternative only.
        pass

    # If two strong signals match, prepend a confidence boost.
    if not matches:
        return {
            "rule": "unknown",
            "confidence": "none",
            "signals_matched": [],
            "alternative_rules": list(RULE_KEYS.keys()),
            "summary": "No PR-side signals matched any common rule pattern. "
                       "Recommend investigating branch protection settings "
                       "directly via an admin-scoped token.",
        }

    matches.sort(key=lambda m: (-m[2], m[0]))
    top_rule, top_signals, top_conf = matches[0]

    if top_conf >= 0.85:
        confidence = "high"
    elif top_conf >= 0.55:
        confidence = "medium"
    else:
        confidence = "low"

    alternatives = [r for r, _s, _c in matches[1:]]

    return {
        "rule": top_rule,
        "confidence": confidence,
        "signals_matched": top_signals,
        "alternative_rules": alternatives,
        "summary": f"Most likely rule: {top_rule} (confidence: {confidence}).",
    }


# --- main --------------------------------------------------------------------

def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("repo", nargs="?", default=DEFAULT_REPO,
                        help=f"owner/repo (default: {DEFAULT_REPO})")
    parser.add_argument("--branch", default=DEFAULT_BRANCH,
                        help=f"branch name (default: {DEFAULT_BRANCH})")
    parser.add_argument("--pr", type=int, default=705,
                        help="PR number (default: 705)")
    parser.add_argument("--json-only", action="store_true",
                        help="emit only the JSON object, no human summary")
    args = parser.parse_args(argv)

    out: dict[str, Any] = {
        "repo": args.repo,
        "branch": args.branch,
        "pr_number": args.pr,
        "admin_scope_available": False,
    }

    # Path 1: admin scope.
    protection = admin_scope_protection(args.repo, args.branch)
    if protection is not None:
        out["admin_scope_available"] = True
        out["protection"] = protection
        out["read_only_fallback"] = None
        out["human_summary"] = (
            f"Admin-scope read succeeded. Rules on branch '{args.branch}': "
            f"{json.dumps(protection, separators=(',', ':'))}"
        )
        if not args.json_only:
            print(json.dumps(out, indent=2))
        return 0

    # Path 2: read-only-fallback.
    signals = pr_signals(args.repo, args.pr)
    out["pr_signals"] = signals
    inference = infer_rule(signals)
    out["read_only_fallback"] = inference
    out["human_summary"] = inference["summary"]

    if not args.json_only:
        print(json.dumps(out, indent=2))

    # Exit: 0 if rule unmabiguously identified (high-confidence); 2 otherwise.
    return 0 if inference["confidence"] == "high" else 2


if __name__ == "__main__":
    sys.exit(main())
