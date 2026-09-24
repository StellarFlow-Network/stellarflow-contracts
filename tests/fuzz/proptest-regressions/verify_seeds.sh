#!/usr/bin/env bash
#
# verify_seeds.sh -- re-compute SHA-256 of
#   tests/fuzz/proptest-regressions/lib.txt            (file-level)
#   each cc-line's 32-byte raw seed bytes              (per-seed)
# and compare against tests/fuzz/proptest-regressions/SHA256SUMS.
#
# Usage:  ./verify_seeds.sh
# Exits:  0  on full match
#         1  on any mismatch (prints diagnostic to stderr)
#         2  on infrastructure failure (missing tools / wrong layout)
#
# Requires: xxd (BSD/Linux/macOS), sha256sum (GNU coreutils or BSD), awk.
#
# Notes on shell strictness:
#   -u            catch unset variables.
#   -o pipefail   propagate command pipeline failures.
#   -e OFF (intentional): the per-seed loop's exit-on-mismatch path
#   needs to accumulate diagnostics across all rows, not abort on
#   the first failure. We track failures in `fail_count` and exit
#   with status 1 only after the loop completes.

set -uo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SUMS="$DIR/SHA256SUMS"
LIB_ABS="$DIR/lib.txt"
LIB_REL="lib.txt"   # matches the file-level label used in SHA256SUMS

# ----- pre-conditions -----
for tool in xxd sha256sum awk; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "FATAL: required tool '$tool' not on PATH" >&2
        exit 2
    fi
done
for f in "$LIB_ABS" "$SUMS"; do
    if [ ! -r "$f" ]; then
        echo "FATAL: required file '$f' missing or unreadable" >&2
        exit 2
    fi
done

# ----- 1. file-level entry -----
expected_file=$(awk -v p="$LIB_REL" \
                       '$1 ~ /^[0-9a-f]{64}$/ && $2 == p {print $1; exit}' \
                       "$SUMS")
if [ -z "$expected_file" ]; then
    echo "FATAL: no file-level entry in $SUMS for $LIB_REL" >&2
    exit 2
fi
actual_file=$(sha256sum "$LIB_ABS" | awk '{print $1}')
if [ "$expected_file" != "$actual_file" ]; then
    echo "MISMATCH file-level: $LIB_REL" >&2
    echo "  expected: $expected_file" >&2
    echo "  actual:   $actual_file" >&2
    exit 1
fi
echo "OK file: $LIB_REL = $actual_file"

# ----- 2. per-seed raw-bytes (label branch) -----
# awk pre-filter isolates per-seed rows ("<64-hex>  <64-hex>"); the
# while-loop in bash verifies each. fail_count + ok_count track results
# so we can print all diagnostics before deciding the exit code.
fail_count=0
ok_count=0
while read -r expected label; do
    # Defensive branch: even though awk filtered to 64-hex labels,
    # double-check so a future awk change can't silently bypass the
    # SHA-256(raw-bytes) path with a non-hex label.
    if [ "${#label}" -ne 64 ] || ! [[ "$label" =~ ^[0-9a-f]{64}$ ]]; then
        echo "WARN: non-seed row skipped: $expected  $label" >&2
        continue
    fi
    actual=$(printf '%s' "$label" | xxd -r -p | sha256sum | awk '{print $1}')
    if [ "$expected" = "$actual" ]; then
        echo "OK seed: $label = $actual"
        ok_count=$((ok_count + 1))
    else
        echo "MISMATCH seed: $label" >&2
        echo "  expected: $expected" >&2
        echo "  actual:   $actual" >&2
        fail_count=$((fail_count + 1))
    fi
done < <(awk '$1 ~ /^[0-9a-f]{64}$/ && $2 ~ /^[0-9a-f]{64}$/ {print $1, $2}' "$SUMS")

# All per-seed rows processed; decide overall pass/fail.
if [ "$fail_count" -gt 0 ]; then
    echo "FAIL: $fail_count per-seed hash(es) did not verify" >&2
    exit 1
fi
total=$((ok_count + fail_count))
if [ "$total" -eq 0 ]; then
    echo "FATAL: no per-seed entries parsed from $SUMS" >&2
    echo "  (regex: \$1 ~ /^[0-9a-f]{64}\$/ && \$2 ~ /^[0-9a-f]{64}\$/)" >&2
    exit 2
fi
echo "OK: all $ok_count per-seed entries verified"
exit 0
