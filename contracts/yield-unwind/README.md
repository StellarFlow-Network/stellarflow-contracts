# Yield Strategy Unwind & Emergency Liquidation Module

Implements issue **#923** — *Yield Strategy Unwind & Emergency Liquidation Module*.

Protocol assets deployed to external yield strategies earn yield only while the
strategy stays solvent. Standard exits are deliberately slow (each strategy
carries a withdrawal-delay window), so the protocol needs a separate fast path
for genuine market distress. This contract provides it.

## Requirements → implementation

| Issue requirement | Where it lives |
| --- | --- |
| Emergency entrypoint unwinding active yield farming positions into base tokens | `emergency_unwind` pulls the full position out of the external strategy's `withdraw` entry point and settles in base tokens |
| Bypasses standard withdrawal delay timers during verified protocol emergency states | The standard path (`request_unwind` → `execute_unwind`) enforces each strategy's `withdraw_delay_seconds`; `emergency_unwind` runs only while the admin-declared `emergency_active` flag is set and skips the delay entirely (`UnwindError::NotEmergency` otherwise) |
| Returns recovered underlying liquidity directly to vault reserves | Both paths credit recovered base tokens to the per-token vault reserves (`reserve_balance`); impaired strategies settle shortfalls and still close the position, with amounts in the `PositionUnwound` event |

## Lifecycle

```text
                    ┌─ request_unwind ──► execute_unwind (after delay)
open_position ──────┤
                    └─ emergency_unwind (emergency only, no delay)
```

`declare_emergency` / `resolve_emergency` (admin-only, authenticated, evented)
bracket the verified emergency state. One declaration covers every active
strategy at once.
