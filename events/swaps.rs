use soroban_sdk::{contracttype, Address, Env, Symbol};

/// Structured payload for the SwapExecuted event.
///
/// Emitted on pool trade execution to provide real-time market telemetry
/// for off-chain indexers.
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
