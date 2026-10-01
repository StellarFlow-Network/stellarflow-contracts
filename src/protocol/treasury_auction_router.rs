pub const TREASURY_SURPLUS_MAX_THRESHOLD: u128 = 100_000_000;
pub const NULL_STORAGE_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

pub struct TreasuryAuctionRouter {
    pub surplus_balance: u128,
    pub threshold: u128,
    pub burned_tokens: u128,
}

impl TreasuryAuctionRouter {
    pub fn new(threshold: u128) -> Self {
        Self {
            surplus_balance: 0,
            threshold,
            burned_tokens: 0,
        }
    }

    pub fn deposit_fees(&mut self, amount: u128) {
        self.surplus_balance += amount;
    }

    pub fn check_and_trigger_auction(&mut self) -> bool {
        if self.surplus_balance > self.threshold {
            self.execute_auction();
            true
        } else {
            false
        }
    }

    fn execute_auction(&mut self) {
        let excess = self.surplus_balance - self.threshold;
        self.surplus_balance = self.threshold;
        // Swap excess for governance tokens and burn by locking in null storage address
        self.burned_tokens += excess;
    }
}
