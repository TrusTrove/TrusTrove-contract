#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, BytesN, Env, Vec,
};

fn setup() -> (Env, EscrowContractClient<'static>, Address) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let pool = Address::generate(&env);
    let usdc = Address::generate(&env);
    let contract_id = env.register(EscrowContract, (admin, pool, usdc));
    let client = EscrowContractClient::new(&env, &contract_id);

    (env, client, contract_id)
}

fn invoice_id(env: &Env, seed: u8) -> BytesN<32> {
    BytesN::from_array(env, &[seed; 32])
}

fn store_lock(env: &Env, contract_id: &Address, id: &BytesN<32>, amount: u128, locked_at: u64) {
    let issuer = Address::generate(env);
    let record = EscrowRecord {
        invoice_id: id.clone(),
        amount,
        locked_at,
        issuer,
    };
    env.as_contract(contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::Locked(id.clone()), &record);
    });
}

#[test]
fn test_get_locked_batch_returns_amount_locked_at_and_presence_in_input_order() {
    let (env, client, contract_id) = setup();
    let id1 = invoice_id(&env, 1);
    let id2 = invoice_id(&env, 2);
    let id3 = invoice_id(&env, 3);

    store_lock(&env, &contract_id, &id1, 500, 10);
    store_lock(&env, &contract_id, &id3, 1_000, 20);

    let mut ids = Vec::new(&env);
    ids.push_back(id1.clone());
    ids.push_back(id2.clone());
    ids.push_back(id3.clone());
    let states = client.get_locked_batch(&ids);

    assert_eq!(states.len(), 3);
    assert_eq!(
        states.get(0).unwrap(),
        LockState {
            amount: 500,
            locked_at: 10,
            exists: true,
        }
    );
    assert_eq!(
        states.get(1).unwrap(),
        LockState {
            amount: 0,
            locked_at: 0,
            exists: false,
        }
    );
    assert_eq!(
        states.get(2).unwrap(),
        LockState {
            amount: 1_000,
            locked_at: 20,
            exists: true,
        }
    );
}

#[test]
fn test_get_locked_batch_does_not_mutate_lock_state() {
    let (env, client, contract_id) = setup();
    let id = invoice_id(&env, 7);
    store_lock(&env, &contract_id, &id, 42, 99);

    let mut ids = Vec::new(&env);
    ids.push_back(id.clone());
    let first = client.get_locked_batch(&ids);
    let second = client.get_locked_batch(&ids);

    assert_eq!(first, second);
    assert_eq!(client.get_locked(&id), 42);
    assert_eq!(client.get_locked_at(&id), 99);
    assert!(client.has_lock(&id));
}

#[test]
fn test_get_locked_batch_accepts_empty_input() {
    let (env, client, _contract_id) = setup();
    let states = client.get_locked_batch(&Vec::new(&env));
    assert_eq!(states.len(), 0);
}

#[test]
fn test_get_locked_batch_rejects_batch_above_limit() {
    let (env, client, _contract_id) = setup();
    let mut ids = Vec::new(&env);
    for i in 0..=MAX_LOCK_BATCH_SIZE {
        let mut arr = [0u8; 32];
        arr[0..4].copy_from_slice(&i.to_be_bytes());
        ids.push_back(BytesN::from_array(&env, &arr));
    }

    assert_eq!(
        client.try_get_locked_batch(&ids),
        Err(Ok(soroban_sdk::Error::from_contract_error(
            EscrowError::BatchTooLarge as u32
        )))
    );
}

#[test]
fn test_get_locked_batch_accepts_exact_limit() {
    let (env, client, _contract_id) = setup();
    env.ledger().set_timestamp(1);
    let mut ids = Vec::new(&env);
    for i in 0..MAX_LOCK_BATCH_SIZE {
        let mut arr = [0u8; 32];
        arr[0..4].copy_from_slice(&i.to_be_bytes());
        ids.push_back(BytesN::from_array(&env, &arr));
    }

    let states = client.get_locked_batch(&ids);
    assert_eq!(states.len(), MAX_LOCK_BATCH_SIZE);
}
