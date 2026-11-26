#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, Env, BytesN};

#[test]
fn test_min_funding_amount_policy() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let escrow = Address::generate(&env);
    let usdc = Address::generate(&env);

    PoolContract::initialize(env.clone(), admin.clone(), escrow, usdc).unwrap();

    // Default min is 0
    assert_eq!(PoolContract::get_min_fund_principal(env.clone()), 0);

    // Set min to 1000
    PoolContract::set_min_fund_principal(env.clone(), 1000).unwrap();
    assert_eq!(PoolContract::get_min_fund_principal(env.clone()), 1000);

    let invoice_id = BytesN::from_array(&env, &[1; 32]);
    let issuer = Address::generate(&env);

    // Under minimum should fail
    let res = PoolContract::fund_invoice(env.clone(), invoice_id.clone(), 999, issuer.clone());
    assert_eq!(res, Err(PoolError::UnderMinimumFunding));

    // Exact boundary should succeed
    let res2 = PoolContract::fund_invoice(env.clone(), invoice_id.clone(), 1000, issuer.clone());
    assert!(res2.is_ok());
}

#[test]
fn test_utilization_change_event() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let escrow = Address::generate(&env);
    let usdc = Address::generate(&env);

    PoolContract::initialize(env.clone(), admin.clone(), escrow, usdc).unwrap();

    PoolContract::deposit(env.clone(), 10000).unwrap();

    let invoice_id = BytesN::from_array(&env, &[1; 32]);
    let issuer = Address::generate(&env);
    PoolContract::fund_invoice(env.clone(), invoice_id, 5000, issuer).unwrap();

    assert_eq!(PoolContract::get_utilization_bps(env.clone()), 5000);
}
