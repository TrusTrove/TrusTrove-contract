#![cfg(test)]

use super::*;
use soroban_sdk::{testutils::Address as _, Env};

#[test]
fn test_get_locked_batch() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let pool = Address::generate(&env);
    let usdc = Address::generate(&env);

    // Setup contract manually or via initialize
    EscrowContract::initialize(env.clone(), admin, pool.clone(), usdc, 100).unwrap();

    let id1 = BytesN::from_array(&env, &[1; 32]);
    let id2 = BytesN::from_array(&env, &[2; 32]);
    let id3 = BytesN::from_array(&env, &[3; 32]);

    let issuer = Address::generate(&env);
    // Since token transfer happens during lock, mock token if needed or test via standard flow
    // For unit testing get_locked_batch directly without actual token transfer or with mocked token:
    let record1 = EscrowRecord {
        invoice_id: id1.clone(),
        amount: 500,
        locked_at: 10,
        issuer: issuer.clone(),
    };
    let record3 = EscrowRecord {
        invoice_id: id3.clone(),
        amount: 1000,
        locked_at: 20,
        issuer: issuer.clone(),
    };

    env.storage().persistent().set(&DataKey::Locked(id1.clone()), &record1);
    env.storage().persistent().set(&DataKey::Locked(id3.clone()), &record3);

    let mut ids = Vec::new(&env);
    ids.push_back(id1.clone());
    ids.push_back(id2.clone());
    ids.push_back(id3.clone());

    let res = EscrowContract::get_locked_batch(env.clone(), ids);
    assert_eq!(res.len(), 9);
    // id1: amount (500), locked_at (10), true
    assert_eq!(u128::try_from(res.get(0).unwrap()).unwrap(), 500);
    assert_eq!(u64::try_from(res.get(1).unwrap()).unwrap(), 10);
    assert_eq!(bool::try_from(res.get(2).unwrap()).unwrap(), true);

    // id2: 0, 0, false
    assert_eq!(u128::try_from(res.get(3).unwrap()).unwrap(), 0);
    assert_eq!(u64::try_from(res.get(4).unwrap()).unwrap(), 0);
    assert_eq!(bool::try_from(res.get(5).unwrap()).unwrap(), false);

    // id3: 1000, 20, true
    assert_eq!(u128::try_from(res.get(6).unwrap()).unwrap(), 1000);
    assert_eq!(u64::try_from(res.get(7).unwrap()).unwrap(), 20);
    assert_eq!(bool::try_from(res.get(8).unwrap()).unwrap(), true);
}
