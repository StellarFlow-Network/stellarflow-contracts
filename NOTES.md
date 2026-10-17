# Notes

This repository does not contain the Solidity source files referenced by the issue reports in the issue list. To keep the work aligned with the requested issue fixes, the repository includes standalone Solidity examples under `examples/standalone-security-fixes/` that implement the described remediations.

The implemented standalone examples cover:

- YieldStrategyVault.sol: maximum fee cap guard
- FeeCalculator.sol: stack-cached state reads
- CDPEngine.sol: minimum health threshold validation for new positions
- RewardDistributor.sol: mapping value caching in loops

These examples were created as minimal, issue-matching reference implementations and are not intended to replace the actual production contracts in the current workspace.
