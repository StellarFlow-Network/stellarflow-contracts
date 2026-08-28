use soroban_sdk::Env;

/// The maximum allowable ledger threshold for 1 year, assuming ~5 seconds per ledger.
/// 60 * 60 * 24 * 365 / 5 = 6,307,200 (rounded to 6,312,000 for standard 365.25 days)
const MAX_LEDGER_TTL: u32 = 6_312_000;

/// The threshold before which we bump the TTL again. 
/// Using 30 days as a safe buffer: 60 * 60 * 24 * 30 / 5 = 518,400
const TTL_THRESHOLD: u32 = 518_400;

/// Automatically extends the instance-level storage TTL to the maximum allowable threshold.
/// This prevents contract instance metadata from expiring during long periods of administrative inactivity.
pub fn bump_instance_ttl(env: &Env) {
    env.storage().instance().extend_ttl(TTL_THRESHOLD, MAX_LEDGER_TTL);
}
