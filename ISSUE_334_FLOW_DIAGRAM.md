# Issue #334 - Buffer Truncation Flow

## Before Implementation (Problem)
```
High Volatility Event
        ↓
15+ Providers Submit Prices
        ↓
All 15+ Entries Added to Buffer
        ↓
Median Calculation (Insertion Sort on 15+ items)
        ↓
❌ CPU BUDGET EXCEEDED
        ↓
Transaction Fails
```

## After Implementation (Solution)
```
High Volatility Event
        ↓
15+ Providers Submit Prices
        ↓
All Entries Added to Buffer
        ↓
🔧 TRUNCATE_BUFFER_BY_WEIGHT()
   ├─ Check if entries > 11
   ├─ Get provider weights
   ├─ Sort by weight (descending)
   └─ Keep top 11 highest-weight
        ↓
Buffer with ≤ 11 Entries
        ↓
Median Calculation (Insertion Sort on ≤11 items)
        ↓
✅ CPU Budget Safe
        ↓
Transaction Succeeds
```

## Truncation Algorithm Detail
```
Input: Buffer with 15 entries
       Provider Weights: [100, 95, 90, 85, 80, 75, 70, 65, 60, 55, 50, 45, 40, 35, 30]

Step 1: Build (index, weight) pairs
        [(0,100), (1,95), (2,90), ..., (14,30)]

Step 2: Sort by weight descending
        [(0,100), (1,95), (2,90), (3,85), (4,80), (5,75), 
         (6,70), (7,65), (8,60), (9,55), (10,50), (11,45), ...]

Step 3: Keep top 11 indices
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]

Step 4: Rebuild buffer with selected entries
        Buffer now has 11 entries from highest-weight providers

Output: Buffer with 11 entries (weights ≥ 50)
        Discarded: 4 entries (weights < 50)
```

## Weight-Based Prioritization
```
Provider Weight    Priority    Kept in Buffer?
─────────────────────────────────────────────
100               Highest     ✅ Always
95                High        ✅ Always
90                High        ✅ Always
...
50                Medium      ✅ If space available
45                Low         ❌ Truncated
40                Low         ❌ Truncated
35                Lower       ❌ Truncated
30                Lowest      ❌ Truncated
```

## Performance Impact
```
Scenario                    Before      After       Improvement
─────────────────────────────────────────────────────────────────
15 providers submit         ❌ Fails    ✅ Success  100%
20 providers submit         ❌ Fails    ✅ Success  100%
CPU budget usage            High        Capped      ~45% reduction
Median accuracy             N/A         High        Uses best sources
```

## Configuration
```rust
const MAX_MEDIAN_ENTRIES: u32 = 11;  // Maximum entries for median calculation
```

This threshold was chosen to:
- Stay well below CPU budget limits
- Provide sufficient data points for accurate median
- Allow for future growth in provider count
- Balance between accuracy and performance
