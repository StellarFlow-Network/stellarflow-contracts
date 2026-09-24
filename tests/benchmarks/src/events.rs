//! Soroban event payload layouts used by the emission benchmarks.

use soroban_sdk::{contracttype, Address, Env, Symbol};

/// The stable topic prefix shared by AMM, vault, and governance events.
pub const EVENT_NAMESPACE: &str = "stellarflow";

/// A raw AMM payload with named fields.
#[contracttype]
#[derive(Clone)]
pub struct AmmRawEvent {
    pub trader: Address,
    pub input_asset: Symbol,
    pub output_asset: Symbol,
    pub amount_in: i128,
    pub amount_out: i128,
}

/// A raw vault payload with named fields.
#[contracttype]
#[derive(Clone)]
pub struct VaultRawEvent {
    pub keeper: Address,
    pub yield_amount: i128,
    pub fee: i128,
    pub compounded: i128,
    pub total_assets: i128,
}

/// A raw governance payload with named fields.
#[contracttype]
#[derive(Clone)]
pub struct GovernanceRawEvent {
    pub proposer: Address,
    pub proposal_id: u32,
    pub action: Symbol,
    pub approvals: u32,
    pub quorum: u32,
}

/// Compact vault payload used for the layout comparison.
pub type VaultCompactEvent = (Address, i128, i128, i128, i128);

/// Compact governance payload used for the layout comparison.
pub type GovernanceCompactEvent = (Address, u32, Symbol, u32, u32);

/// Canonical topics: namespace, event name, then the indexed entity.
pub fn publish_topics(env: &Env, event_name: Symbol, entity: Symbol) -> (Symbol, Symbol, Symbol) {
    (
        Symbol::new(env, EVENT_NAMESPACE),
        event_name,
        entity,
    )
}

/// Emit a payload in the raw struct layout.
pub fn emit_raw(env: &Env, event_name: Symbol, entity: Symbol, payload: AmmRawEvent) {
    env.events().publish(publish_topics(env, event_name, entity), payload);
}

/// Emit a payload in the compact tuple layout.
pub fn emit_compact(
    env: &Env,
    event_name: Symbol,
    entity: Symbol,
    payload: (Address, Symbol, Symbol, i128, i128),
) {
    env.events().publish(publish_topics(env, event_name, entity), payload);
}

/// Emit a vault event using the canonical topic schema.
pub fn emit_vault_compact(
    env: &Env,
    event_name: Symbol,
    entity: Symbol,
    payload: VaultCompactEvent,
) {
    env.events().publish(publish_topics(env, event_name, entity), payload);
}

/// Emit a governance event using the canonical topic schema.
pub fn emit_governance_compact(
    env: &Env,
    event_name: Symbol,
    entity: Symbol,
    payload: GovernanceCompactEvent,
) {
    env.events().publish(publish_topics(env, event_name, entity), payload);
}