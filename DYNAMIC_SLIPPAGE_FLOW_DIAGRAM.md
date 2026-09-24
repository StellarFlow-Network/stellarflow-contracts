# Dynamic Slippage Protection - Flow Diagrams

## Overview Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Price Oracle Contract                         │
├─────────────────────────────────────────────────────────────────┤
│                                                                   │
│  ┌───────────────┐      ┌──────────────┐     ┌──────────────┐ │
│  │   Existing    │      │   NEW        │     │   Existing   │ │
│  │   Price       │◄────►│   Slippage   │────►│   Math       │ │
│  │   Functions   │      │   Module     │     │   Module     │ │
│  └───────────────┘      └──────────────┘     └──────────────┘ │
│                                │                                 │
│                                ▼                                 │
│                    ┌──────────────────┐                         │
│                    │   Storage Keys   │                         │
│                    │ - Config         │                         │
│                    │ - Volatility     │                         │
│                    └──────────────────┘                         │
└─────────────────────────────────────────────────────────────────┘
```

## Dynamic Swap Execution Flow

```
┌─────────────────────────────────────────────────────────────────┐
│  User calls execute_swap_with_dynamic_slippage()                │
│  Parameters: from_asset, to_asset, amount, manual_min, liquidity│
└────────────────────────┬────────────────────────────────────────┘
                         ▼
         ┌───────────────────────────────┐
         │  1. Get Current Prices        │
         │     - from_price              │
         │     - to_price                │
         └───────────┬───────────────────┘
                     ▼
         ┌───────────────────────────────┐
         │  2. Calculate Expected Output │
         │     expected = amount * rate  │
         └───────────┬───────────────────┘
                     ▼
         ┌───────────────────────────────┐
         │  3. Calculate Dynamic         │
         │     Slippage Tolerance        │
         └───────────┬───────────────────┘
                     ▼
    ┌────────────────────────────────────────┐
    │  calculate_dynamic_slippage()          │
    ├────────────────────────────────────────┤
    │  A. Get volatility for both assets     │
    │     from_vol = get_volatility(from)    │
    │     to_vol = get_volatility(to)        │
    │     max_vol = max(from_vol, to_vol)    │
    │                                         │
    │  B. Volatility adjustment               │
    │     factor = max_vol * multiplier      │
    │     adj = base * (10000 + factor)      │
    │           / 10000                       │
    │                                         │
    │  C. Liquidity penalty (if needed)      │
    │     if liquidity < threshold:          │
    │       deficit = threshold - liquidity  │
    │       penalty = (deficit/threshold)    │
    │                 * 20bps per 10%        │
    │       total = adj + penalty            │
    │                                         │
    │  D. Clamp to bounds                    │
    │     result = clamp(total, min, max)    │
    └─────────────────┬──────────────────────┘
                      ▼
         ┌───────────────────────────────┐
         │  4. Calculate Dynamic Minimum │
         │     dynamic_min = expected *  │
         │     (10000 - slippage) / 10000│
         └───────────┬───────────────────┘
                     ▼
         ┌───────────────────────────────┐
         │  5. Determine Effective Min   │
         │     if manual_min > 0:        │
         │       eff_min = max(           │
         │         dynamic_min,          │
         │         manual_min            │
         │       )                        │
         │     else:                      │
         │       eff_min = dynamic_min   │
         └───────────┬───────────────────┘
                     ▼
         ┌───────────────────────────────┐
         │  6. Execute Conversion        │
         │     actual_output = convert() │
         └───────────┬───────────────────┘
                     ▼
              ┌─────────────┐
              │  Check      │
              │  Output >=  │
              │  eff_min?   │
              └──┬──────┬───┘
          Yes    │      │    No
                 ▼      ▼
    ┌────────────────┐  ┌────────────────┐
    │  7a. SUCCESS   │  │  7b. REJECT    │
    ├────────────────┤  ├────────────────┤
    │ Update         │  │ Emit           │
    │ volatility     │  │ rejection      │
    │                │  │ event          │
    │ Emit           │  │                │
    │ execution      │  │ Return error   │
    │ event          │  └────────────────┘
    │                │
    │ Return output  │
    └────────────────┘
```

## Volatility Tracking Flow

```
┌─────────────────────────────────────────────────────────────────┐
│  Price Update Occurs (automatic or manual trigger)              │
└────────────────────────┬────────────────────────────────────────┘
                         ▼
         ┌───────────────────────────────┐
         │  update_volatility_metrics()  │
         │  Parameters: asset, new_price │
         └───────────┬───────────────────┘
                     ▼
         ┌───────────────────────────────┐
         │  Load Existing Metrics        │
         │  (or create new)              │
         └───────────┬───────────────────┘
                     ▼
              ┌──────────────┐
              │  First       │
              │  Update?     │
              └──┬───────┬───┘
          Yes    │       │    No
                 ▼       ▼
    ┌────────────────┐  ┌────────────────────────────┐
    │  Initialize    │  │  Calculate Price Change    │
    │  Metrics       │  │  change_bps = |new - old|  │
    │  - vol = 0     │  │               * 10000      │
    │  - price = new │  │               / old        │
    │  - count = 0   │  └──────────┬─────────────────┘
    └────────┬───────┘             ▼
             │          ┌────────────────────────────┐
             │          │  Update EMA                │
             │          │  new_ema =                 │
             │          │    (alpha * change_bps +   │
             │          │     (10000 - alpha) *      │
             │          │     old_ema) / 10000       │
             │          └──────────┬─────────────────┘
             └─────────────────────┘
                         ▼
         ┌───────────────────────────────┐
         │  Update Metrics Record        │
         │  - ema_volatility_bps         │
         │  - last_price = new_price     │
         │  - last_updated = now         │
         │  - price_update_count++       │
         └───────────┬───────────────────┘
                     ▼
         ┌───────────────────────────────┐
         │  Store Updated Metrics        │
         └───────────┬───────────────────┘
                     ▼
         ┌───────────────────────────────┐
         │  Emit Volatility Event        │
         └───────────────────────────────┘
```

## Configuration Flow

```
┌─────────────────────────────────────────────────────────────────┐
│  Admin calls set_slippage_config()                              │
└────────────────────────┬────────────────────────────────────────┘
                         ▼
         ┌───────────────────────────────┐
         │  Validate Configuration       │
         ├───────────────────────────────┤
         │  ✓ All tolerance values       │
         │    are ≤ 10,000 bps          │
         │  ✓ min ≤ base ≤ max          │
         │  ✓ volatility_multiplier     │
         │    ≤ 1000 (10x)              │
         │  ✓ ema_alpha in [100, 5000]  │
         │  ✓ liquidity_threshold ≥ 0   │
         └───────────┬───────────────────┘
                     ▼
              ┌──────────────┐
              │  Valid?      │
              └──┬───────┬───┘
          Yes    │       │    No
                 ▼       ▼
    ┌────────────────┐  ┌────────────────┐
    │  Store Config  │  │  Return Error  │
    │  in Persistent │  │  Invalid       │
    │  Storage       │  │  Configuration │
    └────────┬───────┘  └────────────────┘
             ▼
    ┌────────────────────────┐
    │  Config Active         │
    │  All future swaps use  │
    │  new parameters        │
    └────────────────────────┘
```

## Decision Tree: Which Execution Mode?

```
                    START: Need to execute swap
                              │
                              ▼
              ┌───────────────────────────────┐
              │  Do you trust the oracle      │
              │  to calculate optimal         │
              │  slippage?                     │
              └───────────┬───────────────────┘
                  YES     │      NO
                          ▼
            ┌─────────────────────────────────┐
            │  Do you have your own           │
            │  risk model/preferences?        │
            └──────────┬────────────┬─────────┘
                YES    │            │   NO
                       ▼            ▼
         ┌──────────────────┐  ┌──────────────────┐
         │  Do you want     │  │  execute_swap_   │
         │  additional      │  │  with_manual_    │
         │  protection      │  │  slippage()      │
         │  from dynamic?   │  │                  │
         └──────┬───────┬───┘  │  • You control   │
            YES │   NO  │      │    exact         │
                │       │      │    tolerance     │
                ▼       ▼      │  • Predictable   │
    ┌───────────────┐  │      │  • No dynamic    │
    │ execute_swap_ │  │      │    adjustment    │
    │ with_dynamic_ │  │      └──────────────────┘
    │ slippage()    │  │
    │               │  │
    │ manual_min =  │  │
    │ your_minimum  │  │
    │               │  │
    │ • Uses        │  │
    │   stricter of │  │
    │   dynamic vs  │  │
    │   manual      │  │
    └───────────────┘  │
                       │
                       ▼
           ┌──────────────────┐
           │ execute_swap_    │
           │ with_dynamic_    │
           │ slippage()       │
           │                  │
           │ manual_min = 0   │
           │                  │
           │ • Oracle         │
           │   calculates     │
           │   everything     │
           │ • Fully          │
           │   adaptive       │
           └──────────────────┘
```

## Slippage Calculation Example

```
Configuration:
┌──────────────────────────────────────┐
│ base_tolerance_bps: 50  (0.5%)       │
│ min_tolerance_bps: 10   (0.1%)       │
│ max_tolerance_bps: 500  (5%)         │
│ volatility_multiplier: 500 (5x)      │
│ liquidity_threshold: 5,000,000,000   │
│ ema_alpha_bps: 2000  (20%)           │
└──────────────────────────────────────┘

Market Conditions:
┌──────────────────────────────────────┐
│ NGN volatility: 300 bps (3%)         │
│ KES volatility: 200 bps (2%)         │
│ Available liquidity: 4,000,000,000   │
│                      (80% threshold) │
└──────────────────────────────────────┘

Step-by-Step Calculation:
┌──────────────────────────────────────────────────────┐
│ 1. Select Maximum Volatility                         │
│    max_volatility = max(300, 200) = 300 bps         │
└──────────────────┬───────────────────────────────────┘
                   ▼
┌──────────────────────────────────────────────────────┐
│ 2. Calculate Volatility Adjustment                   │
│    factor = 300 * 500 = 150,000                     │
│    adj = 50 * (10,000 + 150,000) / 10,000          │
│        = 50 * 160,000 / 10,000                      │
│        = 50 * 16                                     │
│        = 800 bps                                     │
│                                                      │
│    Wait! This exceeds max_tolerance (500)           │
│    Before liquidity penalty, we'd clamp to 500      │
└──────────────────┬───────────────────────────────────┘
                   ▼
┌──────────────────────────────────────────────────────┐
│ 3. Check Liquidity and Calculate Penalty            │
│    liquidity < threshold?                           │
│    4,000,000,000 < 5,000,000,000 → YES             │
│                                                      │
│    liquidity_ratio = 4,000,000,000 * 10,000        │
│                      / 5,000,000,000                │
│                    = 8,000 (80%)                    │
│                                                      │
│    deficit_pct = 10,000 - 8,000 = 2,000 (20%)     │
│                                                      │
│    penalty = 2,000 * 20 / 1,000                    │
│            = 40 bps                                 │
│                                                      │
│    total = 800 + 40 = 840 bps                      │
└──────────────────┬───────────────────────────────────┘
                   ▼
┌──────────────────────────────────────────────────────┐
│ 4. Clamp to Configured Bounds                       │
│    min_tolerance = 10 bps                           │
│    max_tolerance = 500 bps                          │
│    total = 840 bps                                  │
│                                                      │
│    clamped = clamp(840, 10, 500) = 500 bps         │
└──────────────────┬───────────────────────────────────┘
                   ▼
┌──────────────────────────────────────────────────────┐
│ RESULT: Dynamic Slippage = 500 bps (5%)             │
│                                                      │
│ Interpretation:                                      │
│ - High volatility (3%) pushed us to max            │
│ - Low liquidity added 40bps penalty                │
│ - Total would be 840bps but clamped to 500bps      │
│ - This is maximum allowed tolerance                 │
│ - Consider this a "high risk" swap                  │
└──────────────────────────────────────────────────────┘
```

## Event Flow Diagram

```
┌─────────────────────────────────────────────────────────────────┐
│                        Swap Execution                            │
└────────────────────────┬────────────────────────────────────────┘
                         ▼
         ┌───────────────────────────────┐
         │  During Execution             │
         │  - Update volatility metrics  │
         │    for both assets            │
         └───────────┬───────────────────┘
                     ▼
         ┌───────────────────────────────┐
         │  Emit VolatilityMetrics       │
         │  Topic: (volatility, updated) │
         │  Data: {                      │
         │    asset,                     │
         │    ema_volatility_bps,        │
         │    last_price,                │
         │    last_updated,              │
         │    price_update_count         │
         │  }                            │
         └───────────┬───────────────────┘
                     ▼
              ┌──────────────┐
              │  Swap        │
              │  Success?    │
              └──┬───────┬───┘
          YES    │       │    NO
                 ▼       ▼
    ┌────────────────────┐  ┌────────────────────┐
    │ Emit               │  │ Emit               │
    │ SwapExecutionEvent │  │ SlippageRejection  │
    │                    │  │ Event              │
    │ Topic:             │  │                    │
    │ (swap, executed)   │  │ Topic:             │
    │                    │  │ (swap, rejected)   │
    │ Data: {            │  │                    │
    │   from_asset,      │  │ Data: {            │
    │   to_asset,        │  │   from_asset,      │
    │   amount_in,       │  │   to_asset,        │
    │   amount_out,      │  │   amount_in,       │
    │   expected_rate,   │  │   amount_out,      │
    │   actual_rate,     │  │   min_acceptable,  │
    │   dynamic_slippage,│  │   deviation_bps,   │
    │   applied_slippage │  │   allowed_slippage │
    │ }                  │  │ }                  │
    └────────────────────┘  └────────────────────┘
                 │                    │
                 └────────┬───────────┘
                          ▼
              ┌────────────────────┐
              │  Events Indexed    │
              │  by Frontend       │
              │  for Monitoring    │
              └────────────────────┘
```

## State Diagram: Volatility Metrics

```
         ┌────────────────────┐
         │    No Metrics      │
         │    Exist Yet       │
         └──────┬─────────────┘
                │ First price update
                ▼
         ┌────────────────────┐
         │  Initialized       │
         │  - vol = 0         │
         │  - price = P₀      │
         │  - count = 0       │
         └──────┬─────────────┘
                │ Second price update
                ▼
         ┌────────────────────┐
         │  Tracking Started  │
         │  - vol = change₁   │
         │  - price = P₁      │
         │  - count = 1       │
         └──────┬─────────────┘
                │ Subsequent updates
                ▼
         ┌────────────────────┐
    ┌───►│  Active Tracking   │◄───┐
    │    │  - EMA smoothing   │    │
    │    │  - Continuous      │    │ More price
    │    │    updates         │    │ updates
    │    └────────────────────┘    │
    │                               │
    └───────────────────────────────┘
```

## Data Structure Relationships

```
┌─────────────────────────────────────────────────────────────────┐
│                     SlippageConfig                               │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │ base_tolerance_bps: u32                                     │ │
│  │ min_tolerance_bps: u32                                      │ │
│  │ max_tolerance_bps: u32                                      │ │
│  │ volatility_multiplier: u32                                  │ │
│  │ liquidity_threshold: i128                                   │ │
│  │ ema_alpha_bps: u32                                          │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  Stored in: SlippageDataKey::Config                             │
│  Lifecycle: Set by admin, persists until changed                │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                  VolatilityMetrics (per asset)                   │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │ asset: Symbol                                               │ │
│  │ ema_volatility_bps: u32                                     │ │
│  │ last_price: i128                                            │ │
│  │ last_updated: u64                                           │ │
│  │ price_update_count: u32                                     │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  Stored in: SlippageDataKey::Volatility(Symbol)                 │
│  Lifecycle: Created on first price, updated on each price change│
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                     SwapExecutionEvent                           │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │ from_asset, to_asset: Symbol                                │ │
│  │ amount_in, amount_out: i128                                 │ │
│  │ expected_rate, actual_rate: i128                            │ │
│  │ dynamic_slippage_bps: u32                                   │ │
│  │ applied_slippage_bps: u32                                   │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  Emitted: On successful swap completion                         │
│  Topic: (symbol_short!("swap"), symbol_short!("executed"))      │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                   SlippageRejectionEvent                         │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │ from_asset, to_asset: Symbol                                │ │
│  │ amount_in, amount_out: i128                                 │ │
│  │ min_acceptable: i128                                        │ │
│  │ deviation_bps: u32                                          │ │
│  │ allowed_slippage_bps: u32                                   │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                   │
│  Emitted: When swap rejected due to slippage                    │
│  Topic: (symbol_short!("swap"), symbol_short!("rejected"))      │
└─────────────────────────────────────────────────────────────────┘
```

---

**Note**: These diagrams are conceptual representations. For actual implementation details, refer to `src/slippage.rs` and the complete documentation.
