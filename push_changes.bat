@echo off
cd /d "c:\Users\hp\Desktop\wave7\stellarflow-contracts-1"

:: Create and checkout new branch
git checkout -b feature/dynamic-fees

:: Add all modified files
git add src/fees.rs src/lib.rs src/amm/invariant.rs

:: Commit changes
git commit -m "Implement dynamic trading fee adjustment (0.05%%-0.30%%) based on pool volume shifts

- Add VolumeHistory and DynamicFeeState structs to track volume and fees
- Implement automatic fee adjustment based on volume delta (>50%% increase, >30%% decrease)
- Integrate dynamic fee deduction with swap output calculation
- Add admin configuration for fee parameters"

:: Push branch (you may need to specify your remote name, usually 'origin')
echo "Branch created and committed. To push, run: git push -u origin feature/dynamic-fees"