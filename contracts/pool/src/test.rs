#![cfg(test)]

use super::*;
use soroban_sdk::{Env, Address};

#[test]
fn test_max_loss_threshold_and_reconciliation()
{
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let donor = Address::generate(&env);

    // Test basic initialization stubbing or direct storage for tests
    env.storage().instance().set(&DataKey::Admin, &admin);
    env.storage().instance().set(&DataKey::TotalDeposits, &1000u128);

    // Test max loss threshold setter and checks
    assert!(PoolContract::set_max_loss_threshold(env.clone(), Some(500)).is_ok());
    let config_opt: Option<u32> = env.storage().instance().get(&DataKey::MaxLossThresholdBps);
    assert_eq!(config_opt, Some(500));

    // Test invalid threshold > 10000
    assert_eq!(PoolContract::set_max_loss_threshold(env.clone(), Some(10001)), Err(PoolError::InvalidConfiguration));

    // Test checkpoint emitter logic
    let lp = Address::generate(&env);
    env.storage().persistent().set(&DataKey::LPShares(lp.clone()), &250u128);
    env.storage().persistent().set(&DataKey::LPInitialDeposit(lp.clone()), &200u128);
    PoolContract::emit_lp_checkpoint(&env, &lp);
}
