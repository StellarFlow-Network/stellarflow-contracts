#!/bin/bash
set -e

# Maximum compiled Wasm binary size budget limit of 500 KB (Issue #949)
MAX_SIZE=512000 # 500 KB (500 * 1024 bytes)

echo "Profiling compiled Wasm binary sizes (budget limit: 500 KB / $MAX_SIZE bytes)..."

if [ ! -d "target/wasm32-unknown-unknown/release" ]; then
    echo "Warning: No target/wasm32-unknown-unknown/release directory found. Skipping check."
    exit 0
fi

wasm_files=$(find target/wasm32-unknown-unknown/release -maxdepth 1 -name "*.wasm")

if [ -z "$wasm_files" ]; then
    echo "No .wasm files found in target/wasm32-unknown-unknown/release."
    exit 0
fi

failed=0
for file in $wasm_files; do
    size=$(stat -c%s "$file")
    echo "Profiled $file: $size bytes (budget: $MAX_SIZE bytes)"
    if [ "$size" -gt "$MAX_SIZE" ]; then
        echo "Error: $file exceeds maximum compiled binary budget limit of 500 KB (found $size bytes)"
        failed=1
    fi
done

if [ "$failed" -ne 0 ]; then
    echo "Binary size budget check failed!"
    exit 1
fi

echo "All compiled Wasm binaries satisfy the 500 KB budget limit."
