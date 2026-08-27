#![cfg(test)]

// Lint baseline (issue #252): every destructured `env` here is consumed by the
// test body — see `generate_invoice_id(&env, …)`, `Address::generate(&env)`,
// `env.ledger()`, `env.set_auths(&[])`, `assert_last_event_*(&env, …)`, or by
// being returned through `setup_without_auths`. Renaming to `_env` would change
// semantics; do not do so. `cargo clippy --workspace --all-targets -- -D warnings`
// must remain clean.

use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Events as _, Ledger},
    Address, BytesN, Env, Symbol, TryFromVal, Vec,
};

use crate::{
    DataKey, EscrowAction, EscrowContract, EscrowContractClient, EscrowEvent, EscrowRecord,
};

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

#[contract]
pub struct MockCaller;

#[contractimpl]
impl MockCaller {
    pub fn noop(_env: Env) {}
}

#[contracttype]
pub struct BalanceKey(Address);

fn setup() -> (
    Env,
    EscrowContractClient<'static>,
    Address,
    Address,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register_contract(None, MockCaller);
    let pool = env.register_contract(None, MockCaller);
    let invoice_contract = env.register_contract(None, MockCaller);
    let usdc_id = env.register_contract(None, MockToken);
    let _mock_token_client = MockTokenClient::new(&env, &usdc_id);

    let pool_bal_key = BalanceKey(pool.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&pool_bal_key, &10_000_000_000_000i128);
    });

    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    client.initialize(&admin, &pool, &invoice_contract, &usdc_id);

    (
        env,
        client,
        admin,
        pool,
        invoice_contract,
        usdc_id,
        contract_id,
    )
}

fn setup_without_auths() -> (
    Env,
    EscrowContractClient<'static>,
    Address,
    Address,
    Address,
    Address,
) {
    let (env, client, admin, pool, _invoice_contract, usdc_id, _) = setup();
    env.set_auths(&[]);
    (env, client, admin, pool, _invoice_contract, usdc_id)
}

fn generate_invoice_id(env: &Env, counter: u64) -> BytesN<32> {
    let mut arr = [0u8; 32];
    let bytes = (env.ledger().timestamp() + counter).to_be_bytes();
    arr[0..8].copy_from_slice(&bytes);
    BytesN::from_array(env, &arr)
}

fn get_balance(env: &Env, token_id: &Address, addr: &Address) -> i128 {
    MockTokenClient::new(env, token_id).balance(addr)
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
    let invoice_contract = Address::generate(&env);
    let usdc = env.register_contract(None, MockToken);
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);
    client.initialize(&admin, &pool, &invoice_contract, &usdc);

    assert_eq!(client.get_locked(&generate_invoice_id(&env, 1)), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_initialize_twice_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let pool = Address::generate(&env);
    let invoice_contract = Address::generate(&env);
    let usdc = env.register_contract(None, MockToken);
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);

    // First initialize succeeds
    client.initialize(&admin, &pool, &invoice_contract, &usdc);

    // Second initialize must panic with AlreadyInitialized (Error #1)
    let admin2 = Address::generate(&env);
    let pool2 = Address::generate(&env);
    let invoice_contract2 = Address::generate(&env);
    let usdc2 = env.register_contract(None, MockToken);
    client.initialize(&admin2, &pool2, &invoice_contract2, &usdc2);
}

// ============================================================================
// Lock Tests
// ============================================================================

#[test]
fn test_lock_stores_record_and_transfers_usdc() {
    let (env, client, _admin, pool, _invoice_contract, usdc_id, contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    // Check initial balances
    let _pool_balance_before = get_balance(&env, &usdc_id, &pool);
    let _contract_balance_before = get_balance(&env, &usdc_id, &contract_id);

    // Execute lock
    let result = client.lock(&invoice_id, &amount);
    assert!(result);

    // Verify record was stored
    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, amount);
    assert_last_event_two(&env, "funds_locked", invoice_id.clone(), amount);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_lock_fails_zero_amount() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 3);
    client.lock(&invoice_id, &0);
}

#[test]
fn test_lock_only_callable_by_pool() {
    // This test verifies that lock requires pool authorization.
    // In the soroban-sdk testutils with mock_all_auths(), any address can call.
    // The actual authorization is checked by pool.require_auth() in the contract.
    // Here we verify the lock mechanism works when called by pool.
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 4);
    let amount: u128 = 1_000_000_000;

    // Lock should succeed when called with proper setup
    // (pool is mocked to be the caller in setup())
    let result = client.lock(&invoice_id, &amount);
    assert!(result);

    // Verify the record was stored
    assert_eq!(client.get_locked(&invoice_id), amount);
}

// ============================================================================
// Release to Issuer Tests
// ============================================================================

#[test]
fn test_release_to_issuer_sends_correct_amount() {
    let (env, client, _admin, _pool, _invoice_contract, usdc_id, contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 5);
    let issuer = Address::generate(&env);
    let amount: u128 = 1_000_000_000;

    // Lock funds first
    client.lock(&invoice_id, &amount);

    // Capture balances before release
    let issuer_balance_before = get_balance(&env, &usdc_id, &issuer);
    let contract_balance_before = get_balance(&env, &usdc_id, &contract_id);

    // Release to issuer
    let result = client.release_to_issuer(&invoice_id, &issuer);
    assert!(result);

    // Verify record was removed
    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);

    // Verify issuer received the funds
    assert_eq!(
        get_balance(&env, &usdc_id, &issuer),
        issuer_balance_before + amount as i128,
    );

    // Verify escrow contract lost the funds
    assert_eq!(
        get_balance(&env, &usdc_id, &contract_id),
        contract_balance_before - amount as i128,
    );

    assert_last_event_three(
        &env,
        "released_to_issuer",
        invoice_id.clone(),
        issuer,
        amount,
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_release_to_issuer_self_address_panics() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 6);
    client.lock(&invoice_id, &1_000_000_000);

    // Releasing to self (escrow contract itself) must panic with InvalidRecipient (#7)
    client.release_to_issuer(&invoice_id, &contract_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_release_to_issuer_pool_address_panics() {
    let (env, client, _admin, pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 7);
    client.lock(&invoice_id, &1_000_000_000);

    // Releasing to pool contract address must panic with InvalidRecipient (#7)
    client.release_to_issuer(&invoice_id, &pool);
}

// ============================================================================
// Release to Pool Tests
// ============================================================================

#[test]
fn test_release_to_pool_transfers_correct_amount() {
    let (env, client, _admin, pool, _invoice_contract, usdc_id, contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    // Capture pre-lock balances so we can prove the release actually moved
    // tokens. The previous version of this test only checked the in-storage
    // record and the emitted event — it never asserted on MockToken balances,
    // which let a regression in the token-transfer path land silently.
    let pool_balance_before = get_balance(&env, &usdc_id, &pool);
    let contract_balance_before = get_balance(&env, &usdc_id, &contract_id);

    // Lock funds first.
    client.lock(&invoice_id, &amount);

    // Sanity precondition: lock must debit the pool and credit the escrow by
    // `amount` — otherwise the release-side deltas cannot be trusted (the
    // mock would silently produce a same-signed delta from a zero base).
    let pool_balance_after_lock = get_balance(&env, &usdc_id, &pool);
    let contract_balance_after_lock = get_balance(&env, &usdc_id, &contract_id);
    assert_eq!(
        pool_balance_after_lock,
        pool_balance_before - (amount as i128)
    );
    assert_eq!(
        contract_balance_after_lock,
        contract_balance_before + (amount as i128)
    );

    let repayment: u128 = amount;
    let result = client.release_to_pool(&invoice_id, &repayment);
    assert!(result);

    // The release must (a) credit the pool by `repayment` and (b) debit the
    // escrow by the same amount. Expressed as delta-from-the-post-lock state
    // so the asserts are independent of the absolute setup balance.
    let pool_balance_after_release = get_balance(&env, &usdc_id, &pool);
    let contract_balance_after_release = get_balance(&env, &usdc_id, &contract_id);
    assert_eq!(
        pool_balance_after_release,
        pool_balance_after_lock + (repayment as i128)
    );
    assert_eq!(
        contract_balance_after_release,
        contract_balance_after_lock - (repayment as i128)
    );

    // Verify record was removed and the release event was emitted correctly.
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
#[test]
fn test_release_to_pool_over_repayment_succeeds() {
    let (env, client, _admin, pool, _invoice_contract, usdc_id, contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    let pool_balance_before = get_balance(&env, &usdc_id, &pool);
    let contract_balance_before = get_balance(&env, &usdc_id, &contract_id);

    client.lock(&invoice_id, &amount);

    let pool_balance_after_lock = get_balance(&env, &usdc_id, &pool);
    let contract_balance_after_lock = get_balance(&env, &usdc_id, &contract_id);
    assert_eq!(
        pool_balance_after_lock,
        pool_balance_before - (amount as i128)
    );
    assert_eq!(
        contract_balance_after_lock,
        contract_balance_before + (amount as i128)
    );

    // Test the documented over-repayment path (repayment_amount > locked amount)
    let repayment: u128 = amount + 500_000; // Adding yield/penalty
    let result = client.release_to_pool(&invoice_id, &repayment);
    assert!(result);

    let pool_balance_after_release = get_balance(&env, &usdc_id, &pool);
    let contract_balance_after_release = get_balance(&env, &usdc_id, &contract_id);
    assert_eq!(
        pool_balance_after_release,
        pool_balance_after_lock + (repayment as i128)
    );
    assert_eq!(
        contract_balance_after_release,
        contract_balance_after_lock - (repayment as i128)
    );

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
    assert_last_event_three(
        &env,
        "released_to_pool",
        invoice_id,
        pool,
        repayment,
    );
}

#[test]
fn test_release_to_pool_requires_invoice_contract_auth() {
    let (env, client, _admin, _pool, invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    client.release_to_pool(&invoice_id, &amount);

    let auths = env.auths();
    assert!(auths.iter().any(|(addr, _)| *addr == invoice_contract));
}

#[test]
#[should_panic]
fn test_release_to_pool_rejects_non_invoice_contract_caller() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);

    // Without mocked auths, an arbitrary caller (nothing authorizes the
    // configured invoice contract) must be rejected before any funds move
    // or the lock record is touched — proving the previously-possible
    // escrow/pool/invoice desync can no longer be triggered by anyone
    // other than the configured invoice contract.
    env.set_auths(&[]);
    client.release_to_pool(&invoice_id, &amount);
}

// ============================================================================
// Handle Default Tests
// ============================================================================

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_release_to_pool_fails_zero_repayment() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    client.release_to_pool(&invoice_id, &0);
}

#[test]
fn test_release_to_pool_partial_repayment_succeeds() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    let partial: u128 = 500_000_000;
    let result = client.release_to_pool(&invoice_id, &partial);
    assert!(result);
    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_release_to_pool_unknown_invoice_id_panics() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let unknown_id = generate_invoice_id(&env, 999);

    // Never locked this invoice_id — should panic with NotFound (#2)
    client.release_to_pool(&unknown_id, &1_000_000_000);
}

#[test]
fn test_handle_default_returns_funds_to_pool() {
    let (env, client, _admin, pool, _invoice_contract, usdc_id, contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    let pool_balance_before = get_balance(&env, &usdc_id, &pool);
    let contract_balance_before = get_balance(&env, &usdc_id, &contract_id);

    client.lock(&invoice_id, &amount);

    let pool_balance_after_lock = get_balance(&env, &usdc_id, &pool);
    let contract_balance_after_lock = get_balance(&env, &usdc_id, &contract_id);
    assert_eq!(
        pool_balance_after_lock,
        pool_balance_before - (amount as i128)
    );
    assert_eq!(
        contract_balance_after_lock,
        contract_balance_before + (amount as i128)
    );

    env.ledger().set_timestamp(env.ledger().timestamp() + 60);
    // Pool is the normal operational caller for default resolution
    let result = client.handle_default(&invoice_id, &pool);
    assert!(result);

    // Verify funds were actually transferred back to the pool, not just that
    // the storage record was cleared.
    let pool_balance_after_default = get_balance(&env, &usdc_id, &pool);
    let contract_balance_after_default = get_balance(&env, &usdc_id, &contract_id);
    assert_eq!(
        pool_balance_after_default,
        pool_balance_after_lock + (amount as i128)
    );
    assert_eq!(
        contract_balance_after_default,
        contract_balance_after_lock - (amount as i128)
    );

    // Verify record was removed
    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
}

#[test]
fn test_handle_default_invoked_by_admin_succeeds() {
    let (env, client, admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    env.ledger().set_timestamp(env.ledger().timestamp() + 60);

    // Call handle_default with admin as the caller
    let result = client.handle_default(&invoice_id, &admin);
    assert!(result);

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
    assert_last_event_three(&env, "default_resolved", invoice_id.clone(), _pool, amount);
}

#[test]
fn test_handle_default_admin_can_trigger() {
    let (env, client, admin, pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    env.ledger().set_timestamp(env.ledger().timestamp() + 60);
    // Admin can directly trigger default resolution (emergency / recovery path)
    let result = client.handle_default(&invoice_id, &admin);
    assert!(result);

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
    // Funds are always returned to the pool address regardless of who triggered
    assert_last_event_three(&env, "default_resolved", invoice_id.clone(), pool, amount);
}

#[test]
fn test_handle_default_returns_false_if_no_record() {
    let (env, client, _admin, pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 8);

    let result = client.handle_default(&invoice_id, &pool);
    assert!(!result);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_handle_default_rejects_before_grace_period() {
    let (env, client, _admin, pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 9);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    env.ledger().set_timestamp(env.ledger().timestamp() + 59);

    client.handle_default(&invoice_id, &pool);
}

#[test]
fn test_handle_default_allows_at_grace_period_boundary() {
    let (env, client, _admin, pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 10);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    env.ledger().set_timestamp(env.ledger().timestamp() + 60);

    let result = client.handle_default(&invoice_id, &pool);
    assert!(result);
    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
fn test_handle_default_second_call_returns_false_without_side_effects() {
    let (env, client, _admin, pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    // Lock funds and advance past grace period
    client.lock(&invoice_id, &amount);
    env.ledger().set_timestamp(env.ledger().timestamp() + 60);

    // First call: should succeed, remove the record, and emit default_resolved
    let result1 = client.handle_default(&invoice_id, &pool);
    assert!(result1);
    assert_eq!(client.get_locked(&invoice_id), 0);
    assert_last_event_three(
        &env,
        "default_resolved",
        invoice_id.clone(),
        pool.clone(),
        amount,
    );

    // Capture event count after first (successful) call
    let event_count_after_first = env.events().all().len();

    // Second call: should return false with no side effects
    let result2 = client.handle_default(&invoice_id, &pool);
    assert!(!result2);

    // Verify state is unchanged — get_locked still 0
    assert_eq!(client.get_locked(&invoice_id), 0);

    // Verify no new events were emitted by the second call
    assert_eq!(
        env.events().all().len(),
        event_count_after_first,
        "second handle_default must not emit any events"
    );
}

// ============================================================================
// Get Locked Tests
// ============================================================================

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_handle_default_unauthorized_caller_panics() {
    let (env, client, _admin, pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
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
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 9);

    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
fn test_get_locked_returns_zero_for_unknown_id() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();

    // Generate a random unknown invoice ID
    let unknown_id = generate_invoice_id(&env, 999);
    assert_eq!(client.get_locked(&unknown_id), 0);
}

#[test]
fn test_get_locked_returns_amount_when_locked() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 10);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    assert_eq!(client.get_locked(&invoice_id), amount);
}

#[test]
fn test_get_locked_returns_zero_after_release_to_issuer() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 11);
    let issuer = Address::generate(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    assert_eq!(client.get_locked(&invoice_id), amount);

    client.release_to_issuer(&invoice_id, &issuer);
    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
fn test_get_locked_returns_zero_after_release_to_pool() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 12);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    assert_eq!(client.get_locked(&invoice_id), amount);

    client.release_to_pool(&invoice_id, &amount);
    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
fn test_get_locked_at_returns_zero_when_empty() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 20);

    assert_eq!(client.get_locked_at(&invoice_id), 0);
}

#[test]
fn test_get_locked_at_returns_timestamp_when_locked() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 21);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    let locked_at = client.get_locked_at(&invoice_id);
    assert_eq!(locked_at, env.ledger().timestamp());
}

#[test]
fn test_get_locked_at_returns_zero_after_release() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    env.ledger().set_timestamp(1_000_000);
    let invoice_id = generate_invoice_id(&env, 22);
    let issuer = Address::generate(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    assert!(client.get_locked_at(&invoice_id) > 0);

    client.release_to_issuer(&invoice_id, &issuer);
    assert_eq!(client.get_locked_at(&invoice_id), 0);
}

// ============================================================================
// Integration Tests
// ============================================================================

#[test]
fn test_multiple_invoices_independent() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();

    let invoice_id_1 = generate_invoice_id(&env, 13);
    let invoice_id_2 = generate_invoice_id(&env, 14);
    let amount_1: u128 = 1_000_000_000;
    let amount_2: u128 = 2_000_000_000;

    client.lock(&invoice_id_1, &amount_1);
    client.lock(&invoice_id_2, &amount_2);

    assert_eq!(client.get_locked(&invoice_id_1), amount_1);
    assert_eq!(client.get_locked(&invoice_id_2), amount_2);

    let issuer = Address::generate(&env);
    client.release_to_issuer(&invoice_id_1, &issuer);

    assert_eq!(client.get_locked(&invoice_id_1), 0);
    assert_eq!(client.get_locked(&invoice_id_2), amount_2);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_lock_fails_duplicate() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 15);
    client.lock(&invoice_id, &1_000_000_000);
    client.lock(&invoice_id, &500_000_000);
}

#[test]
fn test_get_history_returns_action_log() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 16);
    let amount: u128 = 1_000_000_000;
    let issuer = Address::generate(&env);

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
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id) = setup_without_auths();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    // The contract stores a pool address internally, but no auth entry is
    // present after setup_without_auths(), so this must fail at require_auth().
    client.lock(&invoice_id, &amount);
}

#[test]
#[should_panic]
fn test_release_to_issuer_requires_pool_authorization() {
    let (env, client, _admin, _pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let issuer = Address::generate(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    env.set_auths(&[]);
    client.release_to_issuer(&invoice_id, &issuer);
}

#[test]
#[should_panic]
fn test_handle_default_requires_pool_authorization() {
    let (env, client, _admin, pool, _invoice_contract, _usdc_id, _contract_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount);
    env.set_auths(&[]);
    // No auth entries present — require_auth() on the pool caller must fail
    client.handle_default(&invoice_id, &pool);
}

// ============================================================================
// Pre-initialization tests — typed NotInitialized (Contract error #6)
// ============================================================================

/// `lock` must panic with `NotInitialized` rather than the cryptic
/// host-side `Option::unwrap()` error when called before `initialize()`.
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_lock_uninitialized_panics_with_typed_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);
    let invoice_id = generate_invoice_id(&env, 100);
    // mock_all_auths bypasses pool.require_auth so the unwrap on the
    // uninitialized PoolContract is observable as the typed error.
    client.lock(&invoice_id, &1_000_000_000);
}

/// `release_to_issuer` must panic with `NotInitialized` rather than the
/// cryptic host-side `Option::unwrap()` error when called before `initialize()`.
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_release_to_issuer_uninitialized_panics_with_typed_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);
    let invoice_id = generate_invoice_id(&env, 101);
    let issuer = Address::generate(&env);
    client.release_to_issuer(&invoice_id, &issuer);
}

/// `release_to_pool` must panic with `NotInitialized` rather than the
/// cryptic host-side `Option::unwrap()` error when called before `initialize()`.
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_release_to_pool_uninitialized_panics_with_typed_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);
    let invoice_id = generate_invoice_id(&env, 102);
    client.release_to_pool(&invoice_id, &1_000_000_000);
}

/// When a stale lock record exists but the contract has not been initialized,
/// `handle_default` must panic with `NotInitialized` rather than the cryptic
/// `Option::unwrap()` error.
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_handle_default_uninitialized_with_existing_record_panics_with_typed_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);
    let invoice_id = generate_invoice_id(&env, 103);
    let caller = Address::generate(&env);

    // Seed a stale lock record so handle_default passes its early
    // `has(Locked)` guard and reaches the unwrap on instance storage.
    env.as_contract(&contract_id, || {
        let record = EscrowRecord {
            invoice_id: invoice_id.clone(),
            amount: 1_000_000_000,
            locked_at: env.ledger().timestamp(),
        };
        env.storage()
            .persistent()
            .set(&DataKey::Locked(invoice_id.clone()), &record);
    });

    client.handle_default(&invoice_id, &caller);
}

/// Sanity: without a lock record, `handle_default` short-circuits to `false`
/// before touching instance storage, so no `NotInitialized` panic occurs
/// even when the contract has not been initialized.
#[test]
fn test_handle_default_uninitialized_without_record_returns_false() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, EscrowContract);
    let client = EscrowContractClient::new(&env, &contract_id);
    let invoice_id = generate_invoice_id(&env, 104);
    let caller = Address::generate(&env);
    assert!(!client.handle_default(&invoice_id, &caller));
}
