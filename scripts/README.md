# scripts/

Standalone tooling for cross-repo PR workflow around
`feat/issue-625-fuzz-harness` (PR #705 → `StellarFlow-Network/stellarflow-contracts`).
All scripts are stdlib-only (Python 3.8+) and gracefully degrade when
admin scope on the destination repo is unavailable — they ship read-only
fallbacks in that case.

## scripts/preflight-merge.py

Diagnose which GitHub branch-protection rule is blocking a PR from
being merged. Two paths, in order of authority:

1. **Admin-scope path** — calls
   `gh api /repos/{owner}/{repo}/branches/{branch}/protection` and returns
   the full rule set verbatim. Available only when the active `gh`
   credential has admin (or `pull_requests:write`) scope on the
   destination repo.

2. **Read-only-fallback path** — when the admin-scope call 403s (the
   common case with a contributor's `pull: true` token), infers the
   most-likely rule from PR-side signals:

   | Rule                              | Trigger signals                                                            |
   |----------------------------------|-----------------------------------------------------------------------------|
   | `required_pull_request_reviews`  | `reviewDecision null / REVIEW_REQUIRED` + empty `latestReviews`              |
   | `required_status_checks`         | `statusCheckRollup.state` is `FAILURE` or `PENDING`                          |
   | `restrictions` (i.e. who can merge)  | cross-repo + all other signals clean + `mergeable != MERGEABLE`         |
   | `required_signatures`             | cross-repo + signing tooling not configured on the fork                    |
   | `required_conversation_resolution`| unresolved review-thread comments (extra API call, currently heuristic)    |
   | `required_linear_history`         | head branch has merge commits                                                |
   | `enforce_admins`                  | direct `/protection` 403s but other rules appear to pass                    |

   Each match returns a `confidence: high / medium / low` score and a
   `signals_matched` array. Alternatives are listed in priority order.

### Usage

```bash
./scripts/preflight-merge.py                              # default repo + PR 705
./scripts/preflight-merge.py owner/other-repo             # override destination
./scripts/preflight-merge.py --pr 1234                   # override PR number
./scripts/preflight-merge.py --json-only                 # suppress human summary
```

### Exit codes

| Code | Meaning                                                                  |
|------|--------------------------------------------------------------------------|
| 0    | Rule unambiguously identified (admin scope succeeded, **or** read-only-fallback returned `confidence: high`). |
| 2    | Read-only-fallback returned `confidence: medium` or `low` (heuristic only — actual rule should be verified by an admin-scoped probe). |

### Output shape

```json
{
  "repo": "StellarFlow-Network/stellarflow-contracts",
  "branch": "main",
  "pr_number": 705,
  "admin_scope_available": false,
  "pr_signals": { "pr_state": "OPEN", "merged_at": null, ... },
  "read_only_fallback": {
    "rule": "required_pull_request_reviews",
    "confidence": "high",
    "signals_matched": ["reviewDecision null/REVIEW_REQUIRED with empty latestReviews"],
    "alternative_rules": [],
    "summary": "Most likely rule: required_pull_request_reviews (confidence: high)."
  },
  "human_summary": "Most likely rule: required_pull_request_reviews (confidence: high)."
}
```

If admin scope is available, `protection` is the full REST response body
verbatim (including the `required_pull_request_reviews.required_approving_review_count`,
`required_status_checks.contexts[]`, `restrictions.users[]`, etc.) and
`read_only_fallback` is null.

## Adding new scripts

- Place under `scripts/` with a `#!/usr/bin/env python3` shebang.
- Stdlib only — no `requests`, `urllib3`, or `httpx`. Call out to `gh`
  via `subprocess.run` for any GitHub-API work; this keeps the scripts
  compatible with the codespace's `pull: true` token scope.
- Exit codes: 0 = success, 2 = heuristic/no verdict (never fatal). Document
  both in this README under a new section.
