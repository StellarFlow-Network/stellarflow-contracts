# Administrative Key Recovery Delay and Dispute Module

Implements issue **#962** — *Build Administrative Key Recovery Delay and Dispute Module*.

When an admin key is lost, the key itself cannot authorize its own replacement.
This contract replaces it optimistically: anyone may file a recovery proposal,
but the swap executes only after a mandatory dispute window — and any active
guardian can veto in the meantime.

## Requirements → implementation

| Issue requirement | Where it lives |
| --- | --- |
| Initiate key recovery proposal with a mandatory 7-day waiting period | `request_recovery` records the proposal with `executable_at = requested_at + RECOVERY_DELAY_SECONDS` (7 days) |
| Allow active multi-sig guardians to cancel recovery requests during the dispute window | `cancel_recovery` lets any current guardian kill the pending proposal (`RecoveryError::NotGuardian` otherwise); removed guardians lose veto power |
| Finalize key replacement only post-expiration without guardian vetoes | `finalize_recovery` requires `now >= executable_at` (`RecoveryError::DelayNotElapsed`) and atomically swaps the admin key and clears the proposal |

## Lifecycle

```text
request_recovery ──► pending (7-day dispute window) ──┬─► finalize_recovery (admin replaced)
                                                     └──► cancel_recovery (guardian veto)
```

Initiation is permissionless (a lost key has no credential left to present);
only one proposal may be active at a time. Guardian membership stays under
admin control (`add_guardian` / `remove_guardian`, set can never be emptied).
