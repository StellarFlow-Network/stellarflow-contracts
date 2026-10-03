//! Shared re-exports for the integration test binaries.
//!
//! The library owns the scenarios and the invariant set; the test binaries here
//! are thin so a property is defined once rather than restated per file.

pub use volatility_stress::scenario::{assert_record_holds, scenario_selloff_then_rebalance};
