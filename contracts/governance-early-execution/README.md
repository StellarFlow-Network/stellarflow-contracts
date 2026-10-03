# Governance Proposal Early Execution Threshold Verification Module

Implements issue **#983** — *Build Governance Proposal Early Execution Threshold
Verification Module*.

A governance proposal whose affirmative weight has already crossed the
supermajority line cannot be overturned by the votes still outstanding, so
making the network wait out the rest of the voting window buys nothing. This
module lets such a proposal execute early — but only once it *strictly exceeds*
75 % of the eligible supply, and never before the 48-hour administrative
timelock has elapsed.

## Requirements → implementation

| Issue requirement | Where it lives |
| --- | --- |
| Trigger the early path when affirmative votes exceed 75 % of `V_total` | `early_execution_required_weight` computes `floor(V_total * 7500 / 10_000)` and `exceeds_early_execution_threshold` requires `affirmative > required`, so exactly 75 % does **not** qualify. `trigger_early_execution` re-evaluates this against the live `V_total` on every call. |
| Bypass the remaining voting window | `open_proposal` records `voting_ends_at = opened_at + voting_window`; `trigger_early_execution` only fires while `now < voting_ends_at` and reports the skipped remainder as `bypassed_voting_seconds`. Once the window has closed there is nothing left to bypass and the call is rejected with `VotingWindowElapsed`. |
| Keep the 48-hour administrative timelock intact | `open_proposal` records `timelock_ends_at = opened_at + ADMINISTRATIVE_TIMELOCK_SECONDS` (48 h) and `trigger_early_execution` refuses to run before it (`TimelockNotElapsed`). `initialize` also rejects a voting window shorter than the timelock, because the early path could then never be reached. |
| Emit `ProposalEarlyExecutionTriggered` | `trigger_early_execution` publishes the event with the affirmative weight, the eligible supply, the required weight and the trigger timestamp, then marks the proposal executed/early. |

## Lifecycle

```text
open_proposal ──► voting window open ──┬─► trigger_early_execution (threshold + timelock)
                                       └─► … normal window elapses
```

## API

* `initialize(admin, eligible_supply, voting_window_seconds)` — deploy-time setup.
* `set_eligible_supply(caller, eligible_supply)` — admin-only `V_total` update.
* `open_proposal(proposer)` — opens the single live proposal slot.
* `cast_affirmative_vote(voter, proposal_id, weight)` — one vote per address, capped voter set.
* `early_execution_status(proposal_id)` — read-only verification view (threshold, countdowns).
* `verify_early_execution(proposal_id)` — verifies eligibility without mutating state.
* `trigger_early_execution(proposal_id)` — takes the early path and emits the event.
* View helpers: `get_admin`, `get_eligible_supply`, `get_voting_window`, `get_proposal`, `get_vote_weight`.

## Build and test

```bash
cd contracts/governance-early-execution
cargo test
```

The crate is deliberately standalone (its own `[workspace]` table) so it can be
built and tested in place without touching the root workspace manifest. Its
`Cargo.lock` is committed because a loose transitive `derive_arbitrary`
resolution breaks `stellar-xdr` 20; the committed lock keeps
`arbitrary`/`derive_arbitrary` at the 1.3.2 pair the workspace already pins.
