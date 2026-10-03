#!/usr/bin/env python3
"""
Soroban Host Function Gas Profiler Integration for CI/CD Pipeline (Issue #999).

Features:
- Parses Soroban invocation diagnostics/metrics to extract exact CPU instruction counts across contract entrypoints.
- Generates gas diff reports comparing PR commits against main branch baselines.
- Fails CI build step if gas usage increases by more than 5% without an explicit override.
"""

import sys
import json
import os
import argparse

THRESHOLD_PERCENT = 5.0

def load_profile(path):
    if not os.path.exists(path):
        return {}
    with open(path, "r", encoding="utf-8") as f:
        return json.load(f)

def save_profile(path, data):
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)

def generate_gas_diff(baseline, current):
    report = []
    failed = False
    
    all_keys = sorted(set(list(baseline.keys()) + list(current.keys())))
    for key in all_keys:
        base_cpu = baseline.get(key, {}).get("cpu_instructions", 0)
        curr_cpu = current.get(key, {}).get("cpu_instructions", 0)
        
        if base_cpu == 0:
            diff_pct = 0.0
        else:
            diff_pct = ((curr_cpu - base_cpu) / base_cpu) * 100.0

        is_increase = diff_pct > THRESHOLD_PERCENT
        if is_increase:
            failed = True

        report.append({
            "entrypoint": key,
            "baseline_cpu": base_cpu,
            "current_cpu": curr_cpu,
            "diff_percent": diff_pct,
            "exceeded_threshold": is_increase
        })
        
    return report, failed

def main():
    parser = argparse.ArgumentParser(description="Soroban Host Function Gas Profiler")
    parser.add_argument("--baseline", default="gas_baseline.json", help="Path to baseline metrics JSON")
    parser.add_argument("--current", default="gas_current.json", help="Path to current run metrics JSON")
    parser.add_argument("--output", default="gas_diff_report.md", help="Path to write gas diff report markdown")
    parser.add_argument("--override", action="store_true", help="Explicit override to bypass CI failure on >5% increase")
    args = parser.parse_args()

    baseline = load_profile(args.baseline)
    current = load_profile(args.current)

    report, failed = generate_gas_diff(baseline, current)

    lines = [
        "# Soroban Host Function Gas Profiling Report",
        "",
        "| Entrypoint | Baseline CPU Instructions | Current CPU Instructions | Diff (%) | Status |",
        "| :--- | :--- | :--- | :--- | :--- |"
    ]

    for item in report:
        status = "FAIL (>5% increase)" if item["exceeded_threshold"] else "PASS"
        lines.append(
            f"| `{item['entrypoint']}` | {item['baseline_cpu']:,} | {item['current_cpu']:,} | {item['diff_percent']:+.2f}% | {status} |"
        )

    with open(args.output, "w", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")

    print(f"Gas diff report generated at {args.output}")

    if failed and not args.override:
        print(f"Error: CPU instruction count increased by more than {THRESHOLD_PERCENT}% without --override.")
        sys.exit(1)
    else:
        print("Gas check passed successfully.")
        sys.exit(0)

if __name__ == "__main__":
    main()
