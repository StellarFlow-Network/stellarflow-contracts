# Dynamic Variable Interest Rate Model for Borrow Vaults

Implements issue **#942** — *Dynamic Variable Interest Rate Model for Borrow Vaults*.

A two-slope ("kink") variable borrow rate curve driven by pool utilization.

## Requirements → implementation

| Issue requirement | Where it lives |
| --- | --- |
| Calculate pool utilization rate `U = D_total / C_total` | `utilization_rate` (and the `utilization` entry point): `U = D * 10_000 / C` in bps; empty pool → 0, debt without collateral → full |
| Variable rate `r = r_base + (U / U_optimal) * slope1` when `U <= U_optimal` | `borrow_rate` gentle branch (exact integer formula, floored) |
| Steep scaling `r = r_base + slope1 + ((U - U_optimal) / (1 - U_optimal)) * slope2` when `U > U_optimal` | `borrow_rate` steep branch; continuous with the gentle branch at the kink |

## Notes

- `U_optimal` is constrained to `(0, 10_000)` so both divisors are non-zero.
- The curve is a pure function of `(D, C)` — ledger time is not an input.
- Parameters are admin-governed (`set_model`, evented); rates quoted through
  `rate_for_utilization` / `borrow_rate_for_pool`.
