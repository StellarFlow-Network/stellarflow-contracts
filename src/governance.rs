use soroban_sdk::contracttype;

const MIN_LEDGER_DELAY: u32 = 5000;

#[contracttype]
pub struct StagedUpgrade {
    pub wasm_hash: [u8; 32],
    pub staged_at: u32,
}

pub fn verify_staged_delay(staged_at: u32, current_ledger: u32) -> bool {
    current_ledger.saturating_sub(staged_at) >= MIN_LEDGER_DELAY
}
