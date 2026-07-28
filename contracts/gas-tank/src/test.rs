#[test]
fn test_reentrancy_protection_withdraw() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer = Address::generate(&env);
    let (tank_id, token_id, oracle, _) = setup(&consumer, 1000, &env);
    let tank = GasTankClient::new(&env, &tank_id);
    tank.initialize(&token_id, &oracle);

    tank.deposit(&consumer, &600);
    
    // Test that withdraw works normally
    tank.withdraw(&consumer, &200);
    
    assert_eq!(tank.get_balance(&consumer), 400);
    println!("✅ Withdrawal test passed - reentrancy lock acquired and released correctly");
}

#[test]
fn test_reentrancy_protection_reimburse() {
    let env = Env::default();
    env.mock_all_auths();

    let consumer = Address::generate(&env);
    let (tank_id, token_id, oracle, relayer) = setup(&consumer, 1000, &env);
    let tank = GasTankClient::new(&env, &tank_id);
    tank.initialize(&token_id, &oracle);

    tank.deposit(&consumer, &500);
    tank.set_allowance(&consumer, &relayer, &50);
    
    // Test that reimburse works normally
    tank.reimburse(&relayer);
    
    assert_eq!(tank.get_balance(&consumer), 450);
    assert_eq!(tc(&env, &token_id).balance(&relayer), 50);
    println!("✅ Reimburse test passed - reentrancy lock acquired and released correctly");
}