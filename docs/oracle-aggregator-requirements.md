# On-Chain Decentralized Oracle Aggregator Requirements

## Status

Design documentation only. The requirements described here are not implemented by this change.

## Goal

Aggregate spot-price submissions from independent, registered oracle nodes and expose a weighted median price vector for protocol consumers.

## Required Behavior

### Provider quorum

- Only submissions from registered oracle nodes are eligible.
- At least three distinct registered providers must contribute eligible prices before an aggregate can be produced.
- Multiple submissions from the same provider must not count more than once toward quorum for the same aggregation window.

### Outlier filtering

1. Calculate the arithmetic mean of eligible provider prices for an asset.
2. Calculate each price's absolute deviation from that mean.
3. Exclude prices whose deviation exceeds 10% of the mean.
4. Verify that at least three eligible providers remain after filtering.

Implementations must use checked integer arithmetic and define deterministic rounding behavior suitable for Soroban execution.

### Weighted median

- Each retained provider price must be paired with the provider's registered weight.
- Entries must be ordered deterministically by price, with a deterministic tie-breaking rule.
- The weighted median is the first ordered price whose cumulative weight reaches the median threshold of total retained weight.
- Zero or invalid provider weights must not influence the result.

### Protocol output

The aggregator must return a price vector suitable for protocol consumption. Each result should retain enough metadata for consumers to validate the value, including:

- asset identifier;
- aggregated price;
- decimal precision;
- aggregation timestamp;
- number of retained providers; and
- total retained weight.

## Security Invariants

- Unregistered providers cannot contribute to quorum, mean calculation, outlier filtering, or weighted median calculation.
- A provider cannot contribute more than one effective observation per asset and aggregation window.
- Fewer than three retained providers must never produce an aggregate.
- Outliers exceeding the configured 10% boundary must not influence the weighted median.
- Aggregation must be deterministic for identical contract state and input submissions.
- Arithmetic overflow, division by zero, and invalid weights must fail safely.

## Acceptance Criteria

Implementation work is complete only when automated tests demonstrate all of the following:

1. Three distinct registered providers can produce an aggregate.
2. Fewer than three registered providers cannot produce an aggregate.
3. Unregistered providers are rejected or ignored without affecting quorum.
4. Duplicate submissions do not increase provider count or voting weight.
5. Prices deviating by more than 10% from the mean are excluded.
6. The exact 10% boundary is handled according to the requirement that only values exceeding 10% are excluded.
7. Quorum is checked again after outlier filtering.
8. Unequal provider weights produce the expected weighted median.
9. Multiple assets produce a correctly ordered price vector.
10. Overflow and zero-weight edge cases fail safely and deterministically.
