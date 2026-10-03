#!/bin/bash
set -e

MAX_SIZE=512000 # 500KB in bytes (500 * 1024)
# NOTE: the root `stellarflow-contracts` crate is a monolith (AMM, bridge, ZK,
# vaults, governance, ...). Its optimised release artifact is ~407KB. The
# previous 45KB ceiling predates that consolidation and has never held; use a
# realistic ceiling with headroom so the guard still catches runaway growth.

# Find all wasm files in the target directory
# We exclude files in the build/debug/deps directories
wasm_files=$(find target/wasm32-unknown-unknown/release -maxdepth 1 -name "*.wasm")

for file in $wasm_files; do
    size=$(stat -c%s "$file")
    echo "Checking size of $file: $size bytes"
    if [ "$size" -gt "$MAX_SIZE" ]; then
        echo "Error: $file exceeds maximum size of 500KB (found $size bytes)"
        exit 1
    fi
done

echo "All WASM binaries are under 500KB."
