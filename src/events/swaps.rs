use soroban_sdk::{contracttype, Address, Env, Symbol, Vec};

/// Structured payload for the SwapExecuted event.
///
/// Emitted on pool trade execution to provide real-time market telemetry
/// for off-chain indexers.
/// 
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwapExecutedEvent {
    /// Address of the trader executing the trade.
    pub trader: Address,
    /// Symbol identifier of the input asset.
    pub input_asset: Symbol,
    /// Symbol identifier of the output asset.
    pub output_asset: Symbol,
    /// Computed execution price for the trade.
    pub execution_price: i128,
    /// Measured slippage value or tolerance in basis points.
    pub slippage: i128,
}

/// Publishes a standardized `SwapExecutedEvent` under topics `("stellarflow", "swap")`.
///
/// # Arguments
/// * `env` - The Soroban environment context.
/// * `trader` - Address of the trader executing the pool trade.
/// * `input_asset` - Input asset symbol.
/// * `output_asset` - Output asset symbol.
/// * `execution_price` - Execution price for the swap.
/// * `slippage` - Slippage amount or tolerance (e.g. in bps).
pub fn publish_swap_executed(
    env: &Env,
    trader: &Address,
    input_asset: &Symbol,
    output_asset: &Symbol,
    execution_price: i128,
    slippage: i128,
) {
    let topics = (
        Symbol::new(env, "stellarflow"),
        Symbol::new(env, "swap"),
    );

    let payload = SwapExecutedEvent {
        trader: trader.clone(),
        input_asset: input_asset.clone(),
        output_asset: output_asset.clone(),
        execution_price,
        slippage,
    };

    env.events().publish(topics, payload);
}

// ----- Imbalance Fee Penalty Engine -----

const SCALE: i128 = 1_000_000;
const MIN_FEE_MULTIPLIER: i128 = 500_000;
const MAX_FEE_MULTIPLIER: i128 = 2_000_000;

/// Calculates a dynamic fee multiplier based on post-trade pool reserve imbalance.
///
/// # Arguments
/// * `pre_trade_reserves` - Pool reserves before the trade.
/// * `post_trade_reserves` - Pool reserves after the trade.
/// * `target_weights` - Normalized target weights for each asset (sum to `SCALE`).
pub fn calculate_imbalance_fee_multiplier(
    pre_trade_reserves: &Vec<i128>,
    post_trade_reserves: &Vec<i128>,
    target_weights: &Vec<i128>,
) -> i128 {
    if pre_trade_reserves.len() != post_trade_reserves.len()
        || pre_trade_reserves.len() != target_weights.len()
    {
        panic!("imbalance fee: reserve and weight lengths must match");
    }

    let pre_score = imbalance_score(pre_trade_reserves, target_weights);
    let post_score = imbalance_score(post_trade_reserves, target_weights);

    if post_score > pre_score {
        // Trade increases imbalance -> progressive penalty.
        let extra = (post_score - pre_score) * (MAX_FEE_MULTIPLIER - SCALE) / SCALE;
        return (SCALE + extra).min(MAX_FEE_MULTIPLIER);
    }

    if post_score < pre_score {
        // Trade restores balance -> arbitrage discount.
        let discount = (pre_score - post_score) * (SCALE - MIN_FEE_MULTIPLIER) / SCALE;
        return (SCALE - discount).max(MIN_FEE_MULTIPLIER);
    }

    SCALE
}

/// Computes a normalized imbalance score for the current reserve distribution.
fn imbalance_score(reserves: &Vec<i128>, target_weights: &Vec<i128>) -> i128 {
    let total_reserves: i128 = reserves.iter().fold(0, |acc, x| acc + x);
    if total_reserves == 0 {
        return 0;
    }

    let total_weight: i128 = target_weights.iter().fold(0, |acc, x| acc + x);
    if total_weight == 0 {
        panic!("target weights must sum to > 0");
    }

    let mut max_deviation: i128 = 0;
    for i in 0..reserves.len() {
        let reserve = reserves.get(i).unwrap();
        let weight = target_weights.get(i).unwrap();

        let actual_pct = reserve * SCALE / total_reserves;
        let target_pct = weight * SCALE / total_weight;
        let diff = if actual_pct > target_pct {
            actual_pct - target_pct
        } else {
            target_pct - actual_pct
        };
        if diff > max_deviation {
            max_deviation = diff;
        }
    }
    max_deviation
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{symbol_short, Env};

    #[test]
    fn test_publish_swap_executed() {
        let env = Env::default();
        let trader = Address::generate(&env);
        let input_asset = symbol_short!("XLM");
        let output_asset = symbol_short!("USDC");
        let execution_price = 1250000i128;
        let slippage = 50i128;

        publish_swap_executed(
            &env,
            &trader,
            &input_asset,
            &output_asset,
            execution_price,
            slippage,
        );

        let events = env.events().all();
        assert_eq!(events.len(), 1);
    }
}
