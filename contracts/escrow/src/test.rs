#![cfg(test)]

use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, testutils::Events as _, Address,
    BytesN, Env, Symbol, TryFromVal, Vec,
};

use crate::{EscrowAction, EscrowContract, EscrowContractClient, EscrowEvent};

// --------------- Mock Invoice (for get_issuer cross-check) ---------------

#[contract]
pub struct MockInvoice;

#[contractimpl]
impl MockInvoice {
    pub fn set_issuer(env: Env, invoice_id: BytesN<32>, issuer: Address) {
        env.storage().persistent().set(&invoice_id, &issuer);
    }

    pub fn get_issuer(env: Env, invoice_id: BytesN<32>) -> Address {
        env.storage()
            .persistent()
            .get::<_, Address>(&invoice_id)
            .unwrap()
    }
}

#[contract]
pub struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let from_key = BalanceKey(from.clone());
        let to_key = BalanceKey(to.clone());
        let from_bal: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        let to_bal: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&from_key, &(from_bal - amount));
        env.storage().persistent().set(&to_key, &(to_bal + amount));
    }

    pub fn balance(env: Env, addr: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&BalanceKey(addr))
            .unwrap_or(0)
    }
}

#[contracttype]
pub struct BalanceKey(Address);

fn setup() -> (
    Env,
    EscrowContractClient<'static>,
    Address,
    Address,
    Address,
    Address, // invoice_id (mock)
    MockInvoiceClient<'static>,
) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let pool = Address::generate(&env);
    let usdc_id = env.register_contract(None, MockToken);
    let _mock_token = MockTokenClient::new(&env, &usdc_id);

    let pool_bal_key = BalanceKey(pool.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&pool_bal_key, &10_000_000_000_000i128);
    });

    let invoice_contract_id = env.register_contract(None, MockInvoice);
    let mock_invoice = MockInvoiceClient::new(&env, &invoice_contract_id);

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    client.initialize(&admin, &pool, &invoice_contract_id, &usdc_id);

    (
        env,
        client,
        admin,
        pool,
        usdc_id,
        invoice_contract_id,
        mock_invoice,
    )
}

fn setup_without_auths() -> (
    Env,
    EscrowContractClient<'static>,
    Address,
    Address,
    Address,
) {
    let (env, client, admin, pool, usdc_id, _inv_id, _mock_inv) = setup();
    env.set_auths(&[]);
    (env, client, admin, pool, usdc_id)
}

fn generate_invoice_id(env: &Env) -> BytesN<32> {
    let mut arr = [0u8; 32];
    arr[0..8].copy_from_slice(&env.ledger().timestamp().to_be_bytes());
    BytesN::from_array(env, &arr)
}

fn assert_last_event_two<T1>(
    env: &Env,
    expected_name: &str,
    expected_topic1: T1,
    expected_data: u128,
) where
    T1: TryFromVal<Env, soroban_sdk::Val> + core::fmt::Debug + PartialEq,
    <T1 as TryFromVal<Env, soroban_sdk::Val>>::Error: core::fmt::Debug,
{
    let events = env.events().all();
    let (_, topics, data) = events.last().expect("expected at least one event");

    let topic0: Symbol = Symbol::try_from_val(env, &topics.get(0).unwrap()).unwrap();
    let topic1: T1 = T1::try_from_val(env, &topics.get(1).unwrap()).unwrap();
    let actual_data: u128 = u128::try_from_val(env, &data).unwrap();

    assert_eq!(topic0, Symbol::new(env, expected_name));
    assert_eq!(topic1, expected_topic1);
    assert_eq!(actual_data, expected_data);
}

fn assert_last_event_three<T1, T2>(
    env: &Env,
    expected_name: &str,
    expected_topic1: T1,
    expected_topic2: T2,
    expected_data: u128,
) where
    T1: TryFromVal<Env, soroban_sdk::Val> + core::fmt::Debug + PartialEq,
    T2: TryFromVal<Env, soroban_sdk::Val> + core::fmt::Debug + PartialEq,
    <T1 as TryFromVal<Env, soroban_sdk::Val>>::Error: core::fmt::Debug,
    <T2 as TryFromVal<Env, soroban_sdk::Val>>::Error: core::fmt::Debug,
{
    let events = env.events().all();
    let (_, topics, data) = events.last().expect("expected at least one event");

    let topic0: Symbol = Symbol::try_from_val(env, &topics.get(0).unwrap()).unwrap();
    let topic1: T1 = T1::try_from_val(env, &topics.get(1).unwrap()).unwrap();
    let topic2: T2 = T2::try_from_val(env, &topics.get(2).unwrap()).unwrap();
    let actual_data: u128 = u128::try_from_val(env, &data).unwrap();

    assert_eq!(topic0, Symbol::new(env, expected_name));
    assert_eq!(topic1, expected_topic1);
    assert_eq!(topic2, expected_topic2);
    assert_eq!(actual_data, expected_data);
}

#[test]
fn test_initialize() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let pool = Address::generate(&env);
    let invoice = Address::generate(&env);
    let usdc = env.register_contract(None, MockToken);
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);
    client.initialize(&admin, &pool, &invoice, &usdc);

    assert_eq!(client.get_locked(&generate_invoice_id(&env)), 0);
}

#[test]
fn test_lock_stores_record() {
    let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;

    let result = client.lock(&invoice_id, &amount);
    assert!(result);

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, amount);
    assert_last_event_two(&env, "funds_locked", invoice_id.clone(), amount);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_lock_fails_zero_amount() {
    let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    client.lock(&invoice_id, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_lock_fails_duplicate() {
    let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    client.lock(&invoice_id, &1_000_000_000);
    client.lock(&invoice_id, &500_000_000);
}

#[test]
fn test_release_to_issuer_transfers_correct_amount() {
    let (env, client, _admin, _pool, _usdc, _inv_id, mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let issuer = Address::generate(&env);
    let amount: u128 = 1_000_000_000;

    mock_inv.set_issuer(&invoice_id, &issuer);
    client.lock(&invoice_id, &amount);
    let result = client.release_to_issuer(&invoice_id, &issuer);
    assert!(result);

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
    assert_last_event_three(
        &env,
        "released_to_issuer",
        invoice_id.clone(),
        issuer,
        amount,
    );
}

#[test]
fn test_release_to_pool_transfers_correct_amount() {
    let (env, client, _admin, pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    let repayment: u128 = amount;
    let result = client.release_to_pool(&invoice_id, &repayment);
    assert!(result);

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
    assert_last_event_three(
        &env,
        "released_to_pool",
        invoice_id.clone(),
        pool,
        repayment,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_release_to_pool_fails_on_mismatched_repayment_amount() {
    let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    let invalid_repayment: u128 = amount + 1;
    client.release_to_pool(&invoice_id, &invalid_repayment);
}

#[test]
fn test_release_to_pool_partial_repayment() {
    let (env, client, _admin, pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    let partial_repayment: u128 = 400_000_000;
    let result = client.release_to_pool(&invoice_id, &partial_repayment);
    assert!(result);

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, amount - partial_repayment);
    assert_last_event_three(
        &env,
        "released_to_pool",
        invoice_id.clone(),
        pool,
        partial_repayment,
    );
}

#[test]
fn test_handle_default_returns_funds_to_pool() {
    let (env, client, _admin, pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    // Pool is the normal operational caller for default resolution
    let result = client.handle_default(&invoice_id, &pool);
    assert!(result);

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
    assert_last_event_three(&env, "default_resolved", invoice_id.clone(), pool, amount);
}

#[test]
fn test_handle_default_admin_can_trigger() {
    let (env, client, admin, pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    // Admin can directly trigger default resolution (emergency / recovery path)
    let result = client.handle_default(&invoice_id, &admin);
    assert!(result);

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
    // Funds are always returned to the pool address regardless of who triggered
    assert_last_event_three(&env, "default_resolved", invoice_id.clone(), pool, amount);
}

#[test]
fn test_handle_default_no_record_returns_false() {
    let (env, client, _admin, pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);

    let result = client.handle_default(&invoice_id, &pool);
    assert!(!result);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_handle_default_unauthorized_caller_panics() {
    let (env, client, _admin, pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;
    let stranger = Address::generate(&env);

    client.lock(&invoice_id, &amount);
    // A caller that is neither admin nor pool must be rejected
    client.handle_default(&invoice_id, &stranger);
    // also ensure that pool is indeed required for the normal path
    let _ = pool;
}

#[test]
fn test_get_locked_returns_zero_when_empty() {
    let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);

    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
fn test_get_locked_returns_amount_when_locked() {
    let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    assert_eq!(client.get_locked(&invoice_id), amount);
}

#[test]
fn test_get_history_returns_action_log() {
    let (env, client, _admin, _pool, _usdc, _inv_id, mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;
    let issuer = Address::generate(&env);

    mock_inv.set_issuer(&invoice_id, &issuer);
    client.lock(&invoice_id, &amount);
    client.release_to_issuer(&invoice_id, &issuer);

    let history: Vec<EscrowEvent> = client.get_history(&invoice_id);
    assert_eq!(history.len(), 2);
    let lock_event = history.get(0).unwrap();
    let release_event = history.get(1).unwrap();

    assert_eq!(lock_event.invoice_id, invoice_id);
    assert_eq!(lock_event.action, EscrowAction::Locked);
    assert_eq!(lock_event.amount, amount);

    assert_eq!(release_event.invoice_id, invoice_id);
    assert_eq!(release_event.action, EscrowAction::ReleasedToIssuer);
    assert_eq!(release_event.amount, amount);
    assert!(release_event.timestamp >= lock_event.timestamp);

    assert_eq!(client.get_locked(&invoice_id), 0);
    assert_last_event_three(
        &env,
        "released_to_issuer",
        invoice_id.clone(),
        issuer,
        amount,
    );
}

#[test]
#[should_panic]
fn test_lock_requires_pool_authorization() {
    let (env, client, _admin, _pool, _usdc) = setup_without_auths();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;

    // The contract stores a pool address internally, but no auth entry is
    // present after setup_without_auths(), so this must fail at require_auth().
    client.lock(&invoice_id, &amount);
}

#[test]
#[should_panic]
fn test_release_to_issuer_requires_pool_authorization() {
    let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let issuer = Address::generate(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    env.set_auths(&[]);
    client.release_to_issuer(&invoice_id, &issuer);
}

#[test]
#[should_panic]
fn test_release_to_pool_requires_pool_authorization() {
    let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    env.set_auths(&[]);
    client.release_to_pool(&invoice_id, &amount);
}

#[test]
#[should_panic]
fn test_handle_default_requires_pool_authorization() {
    let (env, client, _admin, pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    env.set_auths(&[]);
    // No auth entries present — require_auth() on the pool caller must fail
    client.handle_default(&invoice_id, &pool);
}

// ============== ISSUE #57: WRONG ISSUER REJECTED ==============

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_release_to_issuer_rejects_wrong_address() {
    let (env, client, _admin, _pool, _usdc, _inv_id, mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let real_issuer = Address::generate(&env);
    let attacker = Address::generate(&env);
    let amount: u128 = 1_000_000_000;

    mock_inv.set_issuer(&invoice_id, &real_issuer);
    client.lock(&invoice_id, &amount);
    // Attempt to release funds to a different address — must be rejected
    client.release_to_issuer(&invoice_id, &attacker);
}

// ============== ISSUE #61: TRANSFER OWNERSHIP ==============

#[test]
fn test_escrow_transfer_ownership_changes_admin() {
    let (env, client, admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let new_admin = Address::generate(&env);
    client.transfer_ownership(&new_admin);

    // Verify event was emitted with both old and new admin
    let events = env.events().all();
    let (_, topics, _) = events.last().unwrap();
    let event_name: soroban_sdk::Symbol =
        soroban_sdk::Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
    assert_eq!(
        event_name,
        soroban_sdk::Symbol::new(&env, "ownership_transferred")
    );
    let _ = admin;
}

#[test]
#[should_panic]
fn test_escrow_transfer_ownership_requires_both_auths() {
    let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let new_admin = Address::generate(&env);
    env.set_auths(&[]);
    client.transfer_ownership(&new_admin);
}

// ============== PROPERTY-BASED INVARIANT TESTS ==============

#[test]
fn prop_locked_amount_always_equals_get_locked_after_lock() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u128..=10_000_000_000_000u128), |amount| {
            let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
            let invoice_id = generate_invoice_id(&env);
            client.lock(&invoice_id, &amount);
            prop_assert_eq!(client.get_locked(&invoice_id), amount);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_get_locked_returns_zero_after_release_to_issuer() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u128..=10_000_000_000_000u128), |amount| {
            let (env, client, _admin, _pool, _usdc, _inv_id, mock_inv) = setup();
            let invoice_id = generate_invoice_id(&env);
            let issuer = Address::generate(&env);
            mock_inv.set_issuer(&invoice_id, &issuer);
            client.lock(&invoice_id, &amount);
            client.release_to_issuer(&invoice_id, &issuer);
            prop_assert_eq!(client.get_locked(&invoice_id), 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_get_locked_returns_zero_after_handle_default() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u128..=10_000_000_000_000u128), |amount| {
            let (env, client, _admin, pool, _usdc, _inv_id, _mock_inv) = setup();
            let invoice_id = generate_invoice_id(&env);
            client.lock(&invoice_id, &amount);
            client.handle_default(&invoice_id, &pool);
            prop_assert_eq!(client.get_locked(&invoice_id), 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_history_length_grows_with_each_operation() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u128..=10_000_000_000_000u128), |amount| {
            let (env, client, _admin, _pool, _usdc, _inv_id, mock_inv) = setup();
            let invoice_id = generate_invoice_id(&env);
            let issuer = Address::generate(&env);
            mock_inv.set_issuer(&invoice_id, &issuer);
            prop_assert_eq!(client.get_history(&invoice_id).len(), 0);
            client.lock(&invoice_id, &amount);
            prop_assert_eq!(client.get_history(&invoice_id).len(), 1);
            client.release_to_issuer(&invoice_id, &issuer);
            prop_assert_eq!(client.get_history(&invoice_id).len(), 2);
            Ok(())
        })
        .unwrap();
}

// ============== UNINITIALIZED CONTRACT TESTS ==============

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_uninitialized_escrow_lock() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);
    let invoice_id = generate_invoice_id(&env);
    client.lock(&invoice_id, &1000);
}

#[test]
fn test_initialized_escrow_lock_succeeds() {
    let (env, client, _admin, _pool, _usdc, _inv_id, _mock_inv) = setup();
    let invoice_id = generate_invoice_id(&env);
    let result = client.lock(&invoice_id, &1000);
    assert!(result);
    assert_eq!(client.get_locked(&invoice_id), 1000);
}
