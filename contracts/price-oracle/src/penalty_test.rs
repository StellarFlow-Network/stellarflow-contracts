// Simple test to verify penalty system implementation
use soroban_sdk::{Address, Env};
use crate::types::RelayerState;
use crate::auth::{
    _get_relayer_state, _set_relayer_state, _calculate_penalty_multiplier,
    _record_relayer_success, _record_relayer_error, _get_consecutive_errors,
    _get_relayer_penalty_multiplier, _is_relayer_suspended
};

#[test]
fn test_penalty_system_basic() {
    let env = Env::default();
    let contract_id = env.register_contract("test", crate::PriceOracle);
    let provider = Address::generate(&env);

    env.as_contract(&contract_id, || {
        // Test initial state
        let state = _get_relayer_state(&env, &provider);
        assert_eq!(state.consecutive_errors, 0);
        assert_eq!(state.penalty_multiplier, 100);
        assert!(!_is_relayer_suspended(&env, &provider));

        // Test penalty calculation
        assert_eq!(_calculate_penalty_multiplier(0), 100);
        assert_eq!(_calculate_penalty_multiplier(1), 110);
        assert_eq!(_calculate_penalty_multiplier(3), 125);
        assert_eq!(_calculate_penalty_multiplier(5), 150);
        assert_eq!(_calculate_penalty_multiplier(8), 200);

        // Test error recording
        _record_relayer_error(&env, &provider);
        assert_eq!(_get_consecutive_errors(&env, &provider), 1);
        assert_eq!(_get_relayer_penalty_multiplier(&env, &provider), 110);

        // Test success resets errors
        _record_relayer_success(&env, &provider);
        assert_eq!(_get_consecutive_errors(&env, &provider), 0);
        assert_eq!(_get_relayer_penalty_multiplier(&env, &provider), 100);

        // Test suspension after 8 errors
        for i in 0..8 {
            _record_relayer_error(&env, &provider);
        }
        assert_eq!(_get_consecutive_errors(&env, &provider), 8);
        assert!(_is_relayer_suspended(&env, &provider));
    });
}
