pub struct ImbalanceFeeEngine { target_weights: Vec<f64> }
impl ImbalanceFeeEngine { pub fn new(target_weights: Vec<f64>) -> Self { Self { target_weights } }
pub fn imbalance_ratio(&self, reserves: &[f64]) -> f64 { let total: f64 = reserves.iter().sum(); if total == 0.0 { return 0.0; } let mut score = 0.0; for (r, w) in reserves.iter().zip(&self.target_weights) { score += (r / total - w).abs(); } score / 2.0 }
pub fn fee_multiplier(&self, current: &[f64], post_trade: &[f64]) -> f64 { let before = self.imbalance_ratio(current); let after = self.imbalance_ratio(post_trade); if after > before { 1.0 + (after - before) * 10.0 } else { (1.0 - (before - after) * 10.0).max(0.0) } }
