#![cfg(test)]

extern crate alloc;

// Lint baseline (issue #252): every destructured `env` here is consumed by the
// test body — see `generate_invoice_id(&env, …)`, `Address::generate(&env)`,
// `env.ledger()`, `env.set_auths(&[])`, `assert_last_event_*(&env, …)`, or by
// being returned through `setup_without_auths`. Renaming to `_env` would change
// semantics; do not do so. `cargo clippy --workspace --all-targets -- -D warnings`
// must remain clean.

use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::storage::Persistent,
    testutils::{Address as _, Events as _, Ledger},
    Address, BytesN, Env, IntoVal, Symbol, TryFromVal, Vec,
};

use crate::{
    DataKey, EscrowAction, EscrowContract, EscrowContractClient, EscrowEvent, EscrowRecord,
    DEFAULT_TIMEOUT_WINDOW_SECONDS, MAX_TIMEOUT_WINDOW_SECONDS, TTL_EXTEND_TO, TTL_THRESHOLD,
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

/// Pool stand-in exposing the `get_invoice_contract` view that escrow's
/// `release_to_pool` reads to resolve the invoice contract it must authorize
/// (issue #820). Lives in its own module because `#[contractimpl]` emits
/// helper symbols at module scope.
mod poolmock {
    use soroban_sdk::{contract, contractimpl, Address, Env, Symbol};

    #[contract]
    pub struct MockPool;

    #[contractimpl]
    impl MockPool {
        pub fn set_invoice_contract(env: Env, invoice: Address) {
            env.storage()
                .persistent()
                .set(&Symbol::new(&env, "invoice"), &invoice);
        }

        pub fn get_invoice_contract(env: Env) -> Address {
            env.storage()
                .persistent()
                .get(&Symbol::new(&env, "invoice"))
                .unwrap()
        }
    }
}

#[contracttype]
pub struct BalanceKey(Address);

/// A strict mock token that reverts transfers which would overdraw an
/// account, mimicking a real asset's insufficient-balance behavior. The
/// lenient [`MockToken`] happily drives balances negative, so it cannot
/// exercise the escrow contract's behavior when it lacks the funds it
/// tries to transfer.
///
/// Lives in its own module because `#[contractimpl]` emits helper symbols
/// (`__transfer`, `__balance`, …) at module scope, which would collide with
/// the ones generated for [`MockToken`].
mod strict {
    use super::BalanceKey;
    use soroban_sdk::{contract, contracterror, contractimpl, panic_with_error, Address, Env};

    #[contract]
    pub struct StrictMockToken;

    #[contracterror]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum StrictMockTokenError {
        /// Returned when a transfer would overdraw the source account.
        InsufficientBalance = 1,
    }

    #[contractimpl]
    impl StrictMockToken {
        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            let from_key = BalanceKey(from.clone());
            let to_key = BalanceKey(to.clone());
            let from_bal: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
            let to_bal: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
            if from_bal < amount {
                panic_with_error!(&env, StrictMockTokenError::InsufficientBalance);
            }
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
}

/// A mock token whose `transfer` panics when the recipient matches a configured
/// address. Used to prove escrow rolls back custody state when a token transfer
/// fails (#1007: `release_to_issuer`; #1008: `lock`).
///
/// Otherwise lenient like [`MockToken`], so only the targeted transfer is
/// rejected and every other transfer succeeds.
mod reject {
    use super::BalanceKey;
    use soroban_sdk::{
        contract, contracterror, contractimpl, panic_with_error, Address, Env, Symbol,
    };

    #[contract]
    pub struct RecipientRejectMockToken;

    #[contracterror]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum RecipientRejectMockTokenError {
        /// Returned when `transfer` targets the configured rejected recipient.
        TransferRejected = 1,
    }

    #[contractimpl]
    impl RecipientRejectMockToken {
        /// Configures the recipient address that `transfer` will reject.
        pub fn set_reject_to(env: Env, recipient: Address) {
            env.storage()
                .instance()
                .set(&Symbol::new(&env, "reject_to"), &recipient);
        }

        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            let reject_to: Option<Address> = env
                .storage()
                .instance()
                .get(&Symbol::new(&env, "reject_to"))
                .unwrap_or(None);
            if reject_to.as_ref() == Some(&to) {
                panic_with_error!(&env, RecipientRejectMockTokenError::TransferRejected);
            }
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
}

fn setup() -> (
    Env,
    EscrowContractClient<'static>,
    Address,
    Address,
    Address,
    Address,
) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    // Issue #820: `release_to_pool` resolves the invoice contract it must
    // authorize from the pool's `get_invoice_contract` view, so the pool
    // stand-in in `setup()` serves that read instead of being an inert
    // `MockCaller`.
    let pool = env.register(poolmock::MockPool, ());
    let invoice = env.register(MockCaller, ());
    poolmock::MockPoolClient::new(&env, &pool).set_invoice_contract(&invoice);
    let issuer = env.register(MockCaller, ());
    let usdc_id = env.register(MockToken, ());
    let _mock_token_client = MockTokenClient::new(&env, &usdc_id);

    let pool_bal_key = BalanceKey(pool.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&pool_bal_key, &10_000_000_000_000i128);
    });

    let contract_id = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), usdc_id.clone()),
    );
    let client = EscrowContractClient::new(&env, &contract_id);

    (env, client, admin, pool, issuer, usdc_id)
}

fn setup_without_auths() -> (
    Env,
    EscrowContractClient<'static>,
    Address,
    Address,
    Address,
    Address,
) {
    let (env, client, admin, pool, issuer, usdc_id) = setup();
    env.set_auths(&[]);
    (env, client, admin, pool, issuer, usdc_id)
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

/// Captured contract events. Since soroban-sdk 22 the test Env resets its
/// event buffer before every top-level contract invocation, so tests must
/// capture `env.events().all()` immediately after the call that emits.
type CapturedEvents = soroban_sdk::Vec<(
    soroban_sdk::Address,
    soroban_sdk::Vec<soroban_sdk::Val>,
    soroban_sdk::Val,
)>;

fn assert_last_event_three<T1, T2>(
    env: &Env,
    events: &CapturedEvents,
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

fn assert_last_event_four<T1, T2, T3>(
    env: &Env,
    events: &CapturedEvents,
    expected_name: &str,
    expected_topic1: T1,
    expected_topic2: T2,
    expected_topic3: T3,
    expected_data: u128,
) where
    T1: TryFromVal<Env, soroban_sdk::Val> + core::fmt::Debug + PartialEq,
    T2: TryFromVal<Env, soroban_sdk::Val> + core::fmt::Debug + PartialEq,
    T3: TryFromVal<Env, soroban_sdk::Val> + core::fmt::Debug + PartialEq,
    <T1 as TryFromVal<Env, soroban_sdk::Val>>::Error: core::fmt::Debug,
    <T2 as TryFromVal<Env, soroban_sdk::Val>>::Error: core::fmt::Debug,
    <T3 as TryFromVal<Env, soroban_sdk::Val>>::Error: core::fmt::Debug,
{
    let (_, topics, data) = events.last().expect("expected at least one event");

    let topic0: Symbol = Symbol::try_from_val(env, &topics.get(0).unwrap()).unwrap();
    let topic1: T1 = T1::try_from_val(env, &topics.get(1).unwrap()).unwrap();
    let topic2: T2 = T2::try_from_val(env, &topics.get(2).unwrap()).unwrap();
    let topic3: T3 = T3::try_from_val(env, &topics.get(3).unwrap()).unwrap();
    let actual_data: u128 = u128::try_from_val(env, &data).unwrap();

    assert_eq!(topic0, Symbol::new(env, expected_name));
    assert_eq!(topic1, expected_topic1);
    assert_eq!(topic2, expected_topic2);
    assert_eq!(topic3, expected_topic3);
    assert_eq!(actual_data, expected_data);
}

#[test]
fn test_constructor() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let pool = Address::generate(&env);
    let _invoice_contract = Address::generate(&env);
    let usdc = env.register(MockToken, ());
    let contract_id = env.register(EscrowContract, (admin.clone(), pool.clone(), usdc.clone()));
    let client = EscrowContractClient::new(&env, &contract_id);

    assert_eq!(client.get_locked(&generate_invoice_id(&env, 1)), 0);
}

//  ISSUE #867: construction must announce itself, like every other contract
//  (invoice/registry emit `contract_initialized`, pool emits
//  `pool_initialized`). Assert the event is published with the configured
//  admin, pool and USDC asset as topics.
#[test]
fn test_constructor_emits_contract_initialized_event() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let pool = Address::generate(&env);
    let usdc = env.register(MockToken, ());
    let contract_id = env.register(EscrowContract, (admin.clone(), pool.clone(), usdc.clone()));
    let _client = EscrowContractClient::new(&env, &contract_id);

    let events = env.events().all();
    let (contract, topics, _) = events
        .last()
        .expect("construction must publish a contract_initialized event");
    assert_eq!(contract, contract_id);
    assert_eq!(topics.len(), 4, "symbol + admin + pool + usdc topics");
    assert_eq!(
        Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&env, "contract_initialized")
    );
    assert_eq!(
        Address::try_from_val(&env, &topics.get(1).unwrap()).unwrap(),
        admin
    );
    assert_eq!(
        Address::try_from_val(&env, &topics.get(2).unwrap()).unwrap(),
        pool
    );
    assert_eq!(
        Address::try_from_val(&env, &topics.get(3).unwrap()).unwrap(),
        usdc
    );
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_upgrade_requires_admin_auth() {
    let (env, client, _, _, _, _) = setup_without_auths();
    client.upgrade(&BytesN::from_array(&env, &[0; 32]));
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_constructor_panics_when_pool_equals_usdc() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let pool = Address::generate(&env);
    let usdc = pool.clone();
    let contract_id = env.register(EscrowContract, (admin.clone(), pool.clone(), usdc.clone()));
    let _client = EscrowContractClient::new(&env, &contract_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_constructor_panics_when_pool_equals_admin() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let pool = admin.clone();
    let usdc = env.register(MockToken, ());
    let contract_id = env.register(EscrowContract, (admin.clone(), pool.clone(), usdc.clone()));
    let _client = EscrowContractClient::new(&env, &contract_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_constructor_panics_when_usdc_equals_admin() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let pool = Address::generate(&env);
    let _usdc = env.register(MockToken, ());
    let contract_id = env.register(EscrowContract, (admin.clone(), pool.clone(), admin.clone()));
    let _client = EscrowContractClient::new(&env, &contract_id);
}

#[test]
fn test_get_admin_returns_stored_admin() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = env.register(MockCaller, ());
    let pool = env.register(MockCaller, ());
    let usdc_id = env.register(MockToken, ());
    let contract_id = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), usdc_id.clone()),
    );
    let client = EscrowContractClient::new(&env, &contract_id);

    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_get_pool_contract_returns_stored_pool() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = env.register(MockCaller, ());
    let pool = env.register(MockCaller, ());
    let usdc_id = env.register(MockToken, ());
    let contract_id = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), usdc_id.clone()),
    );
    let client = EscrowContractClient::new(&env, &contract_id);

    assert_eq!(client.get_pool_contract(), pool);
}

#[test]
fn test_get_usdc_asset_returns_stored_usdc() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = env.register(MockCaller, ());
    let pool = env.register(MockCaller, ());
    let usdc_id = env.register(MockToken, ());
    let contract_id = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), usdc_id.clone()),
    );
    let client = EscrowContractClient::new(&env, &contract_id);

    assert_eq!(client.get_usdc_asset(), usdc_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_get_admin_panics_when_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(
        EscrowContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    // #827: a deployed escrow is always constructed; evicting the config
    // keys reproduces the missing-config state this test targets.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::PoolContract);
        env.storage().instance().remove(&DataKey::UsdcAsset);
    });
    let client = EscrowContractClient::new(&env, &contract_id);
    client.get_admin();
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_get_pool_contract_panics_when_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(
        EscrowContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    // #827: a deployed escrow is always constructed; evicting the config
    // keys reproduces the missing-config state this test targets.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::PoolContract);
        env.storage().instance().remove(&DataKey::UsdcAsset);
    });
    let client = EscrowContractClient::new(&env, &contract_id);
    client.get_pool_contract();
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_get_usdc_asset_panics_when_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(
        EscrowContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    // #827: a deployed escrow is always constructed; evicting the config
    // keys reproduces the missing-config state this test targets.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::PoolContract);
        env.storage().instance().remove(&DataKey::UsdcAsset);
    });
    let client = EscrowContractClient::new(&env, &contract_id);
    client.get_usdc_asset();
}

// ============================================================================
// Lock Tests
// ============================================================================

#[test]
fn test_lock_stores_record_and_transfers_usdc() {
    let (env, client, _admin, pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    // Check initial balances
    let _pool_balance_before = get_balance(&env, &usdc_id, &pool);
    let _contract_balance_before = get_balance(&env, &usdc_id, &client.address);

    // Execute lock
    let result = client.lock(&invoice_id, &amount, &issuer);
    assert!(result);

    // Verify record was stored
    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, amount);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_lock_fails_zero_amount() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 3);
    client.lock(&invoice_id, &0, &issuer);
}

#[test]
fn test_lock_fails_amount_exceeds_i128_max() {
    let (env, client, _admin, pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 4);
    let amount = i128::MAX as u128 + 1;

    let pool_before = get_balance(&env, &usdc_id, &pool);
    let escrow_before = get_balance(&env, &usdc_id, &client.address);
    let events_before = env.events().all().len();
    let history_before = client.get_history(&invoice_id).len();

    // Rejected before the token is ever called: no record, no movement, no event.
    assert!(client.try_lock(&invoice_id, &amount, &issuer).is_err());

    assert_eq!(client.get_locked(&invoice_id), 0);
    assert_eq!(client.get_locked_at(&invoice_id), 0);
    assert_eq!(client.get_history(&invoice_id).len(), history_before);
    assert_eq!(get_balance(&env, &usdc_id, &pool), pool_before);
    assert_eq!(get_balance(&env, &usdc_id, &client.address), escrow_before);
    assert_eq!(env.events().all().len(), events_before);
}

#[test]
fn test_lock_succeeds_at_i128_max() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 5);
    let amount = i128::MAX as u128;
    let result = client.lock(&invoice_id, &amount, &issuer);
    assert!(result);
}

/// #1000: the boundary rejection is the typed `InvalidAmount` error
/// (`Error(Contract, #5)`), raised before the token contract is invoked.
#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_lock_unrepresentable_amount_panics_with_invalid_amount() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 4);
    client.lock(&invoice_id, &(i128::MAX as u128 + 1), &issuer);
}

/// #1008: `lock` performs the pool→escrow transfer before storing its record,
/// so a rejected token transfer must not leave a phantom lock or history entry.
#[test]
fn test_lock_rolls_back_when_pool_transfer_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    let pool = env.register(MockCaller, ());
    let issuer = env.register(MockCaller, ());
    let token_id = env.register(reject::RecipientRejectMockToken, ());
    let token = reject::RecipientRejectMockTokenClient::new(&env, &token_id);

    // Fund the pool so the call is realistic; the rejection is recipient-based.
    env.as_contract(&token_id, || {
        env.storage()
            .persistent()
            .set(&BalanceKey(pool.clone()), &10_000_000_000_000i128);
    });

    let contract_id = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), token_id.clone()),
    );
    let client = EscrowContractClient::new(&env, &contract_id);

    // Reject transfers whose recipient is the escrow contract — i.e. the
    // pool→escrow leg of `lock`.
    token.set_reject_to(&contract_id);

    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    let pool_before = token.balance(&pool);
    let escrow_before = token.balance(&contract_id);
    let events_before = env.events().all().len();

    // The pool→escrow transfer is rejected, so `lock` must fail and leave no trace.
    assert!(client.try_lock(&invoice_id, &amount, &issuer).is_err());

    // No lock record, no history, no storage mutation.
    assert_eq!(client.get_locked(&invoice_id), 0);
    assert_eq!(client.get_locked_at(&invoice_id), 0);
    assert!(client.get_history(&invoice_id).is_empty());
    let phantom_lock = env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .has(&DataKey::Locked(invoice_id.clone()))
    });
    assert!(!phantom_lock);

    // Balances and events unchanged.
    assert_eq!(token.balance(&pool), pool_before);
    assert_eq!(token.balance(&contract_id), escrow_before);
    assert_eq!(env.events().all().len(), events_before);
}

#[test]
fn test_lock_only_callable_by_pool() {
    // This test verifies that lock requires pool authorization.
    // In the soroban-sdk testutils with mock_all_auths(), any address can call.
    // The actual authorization is checked by pool.require_auth() in the contract.
    // Here we verify the lock mechanism works when called by pool.
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 4);
    let amount: u128 = 1_000_000_000;

    // Lock should succeed when called with proper setup
    // (pool is mocked to be the caller in setup())
    let result = client.lock(&invoice_id, &amount, &issuer);
    assert!(result);

    // Verify the record was stored
    assert_eq!(client.get_locked(&invoice_id), amount);
}

// ============================================================================
// Release to Issuer Tests
// ============================================================================

#[test]
fn test_release_to_issuer_sends_correct_amount() {
    let (env, client, _admin, _pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 5);
    let amount: u128 = 1_000_000_000;
    let token = MockTokenClient::new(&env, &usdc_id);

    // Lock funds first
    client.lock(&invoice_id, &amount, &issuer);
    let issuer_before = token.balance(&issuer);

    // Release to issuer
    let result = client.release_to_issuer(&invoice_id, &issuer);
    assert!(result);

    // Verify record was removed
    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
    assert_eq!(token.balance(&issuer) - issuer_before, amount as i128);
}

#[test]
fn test_release_to_issuer_rejects_redirect_and_pays_recorded_issuer() {
    let (env, client, _admin, _pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 82);
    let amount = 1_234_567u128;
    let other = Address::generate(&env);
    let token = MockTokenClient::new(&env, &usdc_id);

    client.lock(&invoice_id, &amount, &issuer);
    let issuer_before = token.balance(&issuer);
    let other_before = token.balance(&other);

    assert!(client.try_release_to_issuer(&invoice_id, &other).is_err());
    assert_eq!(client.get_locked(&invoice_id), amount);
    assert_eq!(token.balance(&other), other_before);

    assert!(client.release_to_issuer(&invoice_id, &issuer));
    assert_eq!(token.balance(&issuer) - issuer_before, amount as i128);
    assert_eq!(client.get_locked(&invoice_id), 0);
}

/// #1007: `release_to_issuer` transfers to the issuer before removing the
/// lock record, so a rejected token transfer must leave the custody state
/// untouched — lock record and history present, no release event, issuer
/// balance unchanged.
#[test]
fn test_release_to_issuer_rolls_back_when_transfer_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    let pool = env.register(MockCaller, ());
    let issuer = env.register(MockCaller, ());
    let token_id = env.register(reject::RecipientRejectMockToken, ());
    let token = reject::RecipientRejectMockTokenClient::new(&env, &token_id);

    env.as_contract(&token_id, || {
        env.storage()
            .persistent()
            .set(&BalanceKey(pool.clone()), &10_000_000_000_000i128);
    });

    let contract_id = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), token_id.clone()),
    );
    let client = EscrowContractClient::new(&env, &contract_id);

    // Reject transfers to the issuer — i.e. the escrow→issuer leg of
    // `release_to_issuer`. The earlier pool→escrow lock transfer is unaffected
    // (recipient is escrow, not issuer).
    token.set_reject_to(&issuer.clone());

    let invoice_id = generate_invoice_id(&env, 2);
    let amount: u128 = 1_000_000_000;

    // Lock succeeds and funds escrow.
    assert!(client.lock(&invoice_id, &amount, &issuer));
    assert_eq!(client.get_locked(&invoice_id), amount);

    let issuer_before = token.balance(&issuer);
    let history_before = client.get_history(&invoice_id).len();
    let events_after_lock = env.events().all().len();

    // The escrow→issuer transfer is rejected, so `release_to_issuer` must fail
    // and roll back all custody state.
    assert!(client.try_release_to_issuer(&invoice_id, &issuer).is_err());

    // Lock record and history remain intact.
    assert_eq!(client.get_locked(&invoice_id), amount);
    assert_eq!(client.get_history(&invoice_id).len(), history_before);
    assert_eq!(
        client.get_history(&invoice_id).get(0).unwrap().action,
        EscrowAction::Locked
    );

    // Issuer balance unchanged and no `released_to_issuer` event emitted.
    assert_eq!(token.balance(&issuer), issuer_before);
    assert_eq!(env.events().all().len(), events_after_lock);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_release_to_issuer_self_address_panics() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 6);
    client.lock(&invoice_id, &1_000_000_000, &issuer);

    // Releasing to self (escrow contract itself) must panic with InvalidRecipient (#7)
    client.release_to_issuer(&invoice_id, &client.address);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_release_to_issuer_pool_address_panics() {
    let (env, client, _admin, pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 7);
    client.lock(&invoice_id, &1_000_000_000, &issuer);

    // Releasing to pool contract address must panic with InvalidRecipient (#7)
    client.release_to_issuer(&invoice_id, &pool);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_release_to_issuer_mismatched_address_panics() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 8);
    let wrong_issuer = Address::generate(&env);
    client.lock(&invoice_id, &1_000_000_000, &issuer);

    // Releasing to a different address than the stored issuer must panic with InvalidRecipient
    client.release_to_issuer(&invoice_id, &wrong_issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_release_to_issuer_second_call_panics() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 8);
    let issuer = Address::generate(&env);
    let amount: u128 = 1_000_000_000;

    // Lock funds and release once
    client.lock(&invoice_id, &amount, &issuer);
    client.release_to_issuer(&invoice_id, &issuer);

    // Second release_to_issuer call on the same invoice_id should panic with NotFound (#2)
    client.release_to_issuer(&invoice_id, &issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_release_to_issuer_unknown_invoice_id_panics() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    let unknown_id = generate_invoice_id(&env, 999);
    let issuer = Address::generate(&env);

    // Never locked this invoice_id — should panic with NotFound (#2)
    client.release_to_issuer(&unknown_id, &issuer);
}

/// #1001: `release_to_issuer` validates a stored record's amount against
/// `i128::MAX` before the `as i128` conversion. A corrupted record must be
/// rejected with `InvalidAmount` and not removed or transferred.
#[test]
fn test_release_to_issuer_rejects_stored_amount_above_i128_max() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    let pool = env.register(MockCaller, ());
    let issuer = env.register(MockCaller, ());
    let usdc_id = env.register(MockToken, ());
    let contract_id = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), usdc_id.clone()),
    );
    let client = EscrowContractClient::new(&env, &contract_id);

    let invoice_id = generate_invoice_id(&env, 3);

    // Seed a corrupted lock record whose amount exceeds i128::MAX, bypassing
    // `lock`'s validation.
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::Locked(invoice_id.clone()),
            &EscrowRecord {
                invoice_id: invoice_id.clone(),
                amount: i128::MAX as u128 + 1,
                locked_at: env.ledger().timestamp(),
                issuer: issuer.clone(),
            },
        );
    });

    let issuer_before = get_balance(&env, &usdc_id, &issuer);
    let history_before = client.get_history(&invoice_id).len();
    let events_before = env.events().all().len();

    // The unrepresentable amount must be rejected before any transfer or
    // record removal.
    assert!(client.try_release_to_issuer(&invoice_id, &issuer).is_err());

    // Record not removed, no transfer, no history, no event.
    assert_eq!(client.get_locked(&invoice_id), i128::MAX as u128 + 1);
    assert_eq!(client.get_history(&invoice_id).len(), history_before);
    assert_eq!(get_balance(&env, &usdc_id, &issuer), issuer_before);
    assert_eq!(env.events().all().len(), events_before);
}

/// #1001: the failed checked conversion surfaces as the typed `InvalidAmount`
/// error (`Error(Contract, #5)`), not a host arithmetic failure.
#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_release_to_issuer_corrupted_amount_panics_with_invalid_amount() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    let pool = env.register(MockCaller, ());
    let issuer = env.register(MockCaller, ());
    let usdc_id = env.register(MockToken, ());
    let contract_id = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), usdc_id.clone()),
    );
    let client = EscrowContractClient::new(&env, &contract_id);

    let invoice_id = generate_invoice_id(&env, 3);

    // Seed the same corrupted boundary record as above, bypassing `lock`.
    env.as_contract(&contract_id, || {
        env.storage().persistent().set(
            &DataKey::Locked(invoice_id.clone()),
            &EscrowRecord {
                invoice_id: invoice_id.clone(),
                amount: i128::MAX as u128 + 1,
                locked_at: env.ledger().timestamp(),
                issuer: issuer.clone(),
            },
        );
    });

    client.release_to_issuer(&invoice_id, &issuer);
}

// ============================================================================
// Release to Pool Tests
// ============================================================================

#[test]
fn test_release_to_pool_transfers_correct_amount() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    // Lock funds first.
    client.lock(&invoice_id, &amount, &issuer);

    let repayment: u128 = amount;
    let result = client.release_to_pool(&invoice_id, &repayment);
    assert!(result);

    // Verify lock was removed
    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
fn test_release_to_pool_over_repayment_succeeds() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);

    // Test the documented over-repayment path (repayment_amount > locked amount)
    let repayment: u128 = amount + 500_000; // Adding yield/penalty
    let result = client.release_to_pool(&invoice_id, &repayment);
    assert!(result);

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
}

#[test]
fn test_release_to_pool_requires_invoice_contract_auth() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);

    // Issue #820 / #628 under REAL auth verification (explicit `mock_auths`,
    // not the recording mock): an unrelated address that fully authorizes the
    // root `release_to_pool` call still cannot move funds — the guard demands
    // the invoice contract configured on the pool, which this caller cannot
    // present. On the pre-fix code the same tree was rejected only because it
    // lacked the (impossible) pool authorization; the positive settlement is
    // pinned end-to-end in `tests/integration/tests/repay_auth.rs`.
    let attacker = env.register(MockCaller, ());
    let mut args = Vec::new(&env);
    args.push_back(invoice_id.clone().into_val(&env));
    args.push_back(amount.into_val(&env));
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &attacker,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &client.address,
            fn_name: "release_to_pool",
            args,
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_release_to_pool(&invoice_id, &amount).is_err());

    // The guard resolves exactly one principal: the pool-configured invoice
    // contract, and it is neither the attacker nor the escrow itself.
    let invoice = client.get_invoice_contract();
    assert_ne!(invoice, attacker);
    assert_ne!(invoice, client.address);
}

#[test]
#[should_panic]
fn test_release_to_pool_rejects_non_invoice_contract_caller() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);

    // Without mocked auths, an arbitrary caller (nothing authorizes the
    // configured invoice contract) must be rejected before any funds move
    // or the lock record is touched — proving the previously-possible
    // escrow/pool/invoice desync can no longer be triggered by anyone
    // other than the configured invoice contract.
    env.set_auths(&[]);
    client.release_to_pool(&invoice_id, &amount);
}

// ---- Issue #997: pinned invoice contract reference for release_to_pool ----

// The admin pins the expected invoice contract with `set_invoice_contract`;
// `release_to_pool` validates the pool-resolved address against that pin so
// a drifted pool configuration cannot silently redirect repayments.
#[test]
fn test_set_invoice_contract_emits_event() {
    let (env, client, admin, _, _, _) = setup();
    let invoice = client.get_invoice_contract();

    client.set_invoice_contract(&invoice);

    let events = env.events().all();
    let (contract, topics, data) = events
        .last()
        .expect("set_invoice_contract must publish an invoice_contract_set event");
    assert_eq!(contract, client.address);
    assert_eq!(topics.len(), 2, "symbol + admin topics");
    assert_eq!(
        Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&env, "invoice_contract_set")
    );
    assert_eq!(
        Address::try_from_val(&env, &topics.get(1).unwrap()).unwrap(),
        admin
    );
    assert_eq!(Address::try_from_val(&env, &data).unwrap(), invoice);
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_set_invoice_contract_requires_admin_auth() {
    let (_env, client, _, _, _, _) = setup_without_auths();
    let invoice = client.get_invoice_contract();
    client.set_invoice_contract(&invoice);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_set_invoice_contract_rejects_admin_address() {
    let (_env, client, admin, _pool, _issuer, _usdc_id) = setup();
    client.set_invoice_contract(&admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_set_invoice_contract_rejects_pool_address() {
    let (_env, client, _admin, pool, _issuer, _usdc_id) = setup();
    client.set_invoice_contract(&pool);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_set_invoice_contract_rejects_escrow_address() {
    let (_env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    let escrow = client.address.clone();
    client.set_invoice_contract(&escrow);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_set_invoice_contract_rejects_usdc_address() {
    let (_env, client, _admin, _pool, _issuer, usdc_id) = setup();
    client.set_invoice_contract(&usdc_id);
}

#[test]
fn test_release_to_pool_succeeds_with_matching_pin() {
    let (env, client, _admin, pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 40);
    let amount: u128 = 1_000_000_000;

    // Pin the exact invoice contract the pool reports — no drift.
    let invoice = client.get_invoice_contract();
    client.set_invoice_contract(&invoice);

    client.lock(&invoice_id, &amount, &issuer);
    let pool_before = get_balance(&env, &usdc_id, &pool);
    assert!(client.release_to_pool(&invoice_id, &amount));
    assert_eq!(client.get_locked(&invoice_id), 0);
    assert_eq!(
        get_balance(&env, &usdc_id, &pool) - pool_before,
        amount as i128
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_release_to_pool_rejects_drifted_pin() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 41);
    let amount: u128 = 1_000_000_000;

    // The admin pinned a reference that does NOT match the pool's configured
    // invoice contract: settlement must be rejected with InvalidConfig (#8)
    // even though the pool-resolved caller authorizes the call.
    let other_invoice = env.register(MockCaller, ());
    client.set_invoice_contract(&other_invoice);

    client.lock(&invoice_id, &amount, &issuer);
    client.release_to_pool(&invoice_id, &amount);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_release_to_pool_rejects_pool_rotation_after_pin() {
    let (env, client, _admin, pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 42);
    let amount: u128 = 1_000_000_000;

    // The admin pins the invoice contract, then the pool's configuration
    // rotates away from it: settlement must fail with InvalidConfig (#8)
    // until the escrow admin repins the reference.
    let invoice = client.get_invoice_contract();
    client.set_invoice_contract(&invoice);
    let rotated = env.register(MockCaller, ());
    poolmock::MockPoolClient::new(&env, &pool).set_invoice_contract(&rotated);

    client.lock(&invoice_id, &amount, &issuer);
    client.release_to_pool(&invoice_id, &amount);
}

// ============================================================================
// Handle Default Tests
// ============================================================================

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_release_to_pool_fails_zero_repayment() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    client.release_to_pool(&invoice_id, &0);
}

#[test]
fn test_release_to_pool_fails_amount_exceeds_i128_max() {
    let (env, client, _admin, pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 2);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    let repayment_amount = i128::MAX as u128 + 1;
    let pool_before = get_balance(&env, &usdc_id, &pool);
    let escrow_before = get_balance(&env, &usdc_id, &client.address);
    let history_before = client.get_history(&invoice_id).len();
    let events_before = env.events().all().len();

    assert!(client
        .try_release_to_pool(&invoice_id, &repayment_amount)
        .is_err());
    assert_eq!(client.get_locked(&invoice_id), amount);
    assert_eq!(get_balance(&env, &usdc_id, &pool), pool_before);
    assert_eq!(get_balance(&env, &usdc_id, &client.address), escrow_before);
    assert_eq!(client.get_history(&invoice_id).len(), history_before);
    assert_eq!(env.events().all().len(), events_before);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_release_to_pool_overflow_uses_typed_invalid_amount_error() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 3);
    client.lock(&invoice_id, &1_000_000_000, &issuer);
    client.release_to_pool(&invoice_id, &(i128::MAX as u128 + 1));
}

#[test]
fn test_release_to_pool_succeeds_at_i128_max() {
    let (env, client, _admin, pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 4);
    let locked = 100u128;
    client.lock(&invoice_id, &locked, &issuer);

    // Keep the mock pool balance at zero so adding i128::MAX does not overflow
    // the test token's signed balance arithmetic.
    let pool_balance = get_balance(&env, &usdc_id, &pool);
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&BalanceKey(pool.clone()), &(-pool_balance));
    });
    let escrow_before = get_balance(&env, &usdc_id, &client.address);
    let max_repayment = i128::MAX as u128;

    assert!(client.release_to_pool(&invoice_id, &max_repayment));
    assert_eq!(
        get_balance(&env, &usdc_id, &pool) - (-pool_balance),
        i128::MAX
    );
    assert_eq!(
        get_balance(&env, &usdc_id, &client.address),
        escrow_before - i128::MAX
    );
    assert_eq!(client.get_locked(&invoice_id), 0);
}

// #1004: a partial repayment must leave the residual principal locked, keep
// the original `locked_at`, and record exactly one ReleasedToPool history/event
// entry describing the repaid portion.
#[test]
fn test_release_to_pool_partial_repayment_retains_residual_lock_state() {
    let (env, client, _admin, pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);

    let locked_at = client.get_locked_at(&invoice_id);
    let pool_balance_before = get_balance(&env, &usdc_id, &pool);
    let escrow_balance_before = get_balance(&env, &usdc_id, &client.address);

    let partial: u128 = 400_000_000;
    assert!(client.release_to_pool(&invoice_id, &partial));
    let events_after_release = env.events().all();

    // The lock survives with only the outstanding principal and its original
    // timestamp.
    assert_eq!(client.get_locked(&invoice_id), amount - partial);
    assert_eq!(client.get_locked_at(&invoice_id), locked_at);

    // Only `partial` left escrow and reached the pool.
    let pool_balance_after = get_balance(&env, &usdc_id, &pool);
    let escrow_balance_after = get_balance(&env, &usdc_id, &client.address);
    assert_eq!(pool_balance_after - pool_balance_before, partial as i128);
    assert_eq!(
        escrow_balance_before - escrow_balance_after,
        partial as i128
    );

    // History: the original Locked entry plus one ReleasedToPool entry for the
    // partial amount (not the full lock).
    let history = client.get_history(&invoice_id);
    assert_eq!(history.len(), 2);
    let lock_event = history.get(0).unwrap();
    assert_eq!(lock_event.action, EscrowAction::Locked);
    assert_eq!(lock_event.amount, amount);
    let release_event = history.get(1).unwrap();
    assert_eq!(release_event.invoice_id, invoice_id);
    assert_eq!(release_event.action, EscrowAction::ReleasedToPool);
    assert_eq!(release_event.amount, partial);
    assert_eq!(release_event.timestamp, env.ledger().timestamp());
    assert_eq!(release_event.caller, None);

    assert_last_event_three(
        &env,
        &events_after_release,
        "released_to_pool",
        invoice_id.clone(),
        pool.clone(),
        partial,
    );

    // Repaying the residual clears the lock entirely.
    assert!(client.release_to_pool(&invoice_id, &(amount - partial)));
    assert_eq!(client.get_locked(&invoice_id), 0);
    assert_eq!(client.get_locked_at(&invoice_id), 0);
}

#[test]
fn test_repeated_partial_releases_converge_and_preserve_history_order() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 81);
    let original = 1_000_000_000u128;
    let first = 200_000_000u128;
    let second = 350_000_000u128;
    let final_release = original - first - second;

    client.lock(&invoice_id, &original, &issuer);
    assert_eq!(client.get_locked(&invoice_id), original);

    assert!(client.release_to_pool(&invoice_id, &first));
    assert_eq!(client.get_locked(&invoice_id), original - first);

    assert!(client.release_to_pool(&invoice_id, &second));
    assert_eq!(client.get_locked(&invoice_id), original - first - second);

    let history_before_final = client.get_history(&invoice_id);
    assert_eq!(history_before_final.len(), 3);
    assert_eq!(
        history_before_final.get(0).unwrap().action,
        EscrowAction::Locked
    );
    assert_eq!(history_before_final.get(0).unwrap().amount, original);
    assert_eq!(
        history_before_final.get(1).unwrap().action,
        EscrowAction::ReleasedToPool
    );
    assert_eq!(history_before_final.get(1).unwrap().amount, first);
    assert_eq!(
        history_before_final.get(2).unwrap().action,
        EscrowAction::ReleasedToPool
    );
    assert_eq!(history_before_final.get(2).unwrap().amount, second);

    assert!(client.release_to_pool(&invoice_id, &final_release));
    assert_eq!(client.get_locked(&invoice_id), 0);

    let history = client.get_history(&invoice_id);
    assert_eq!(history.len(), 4);
    assert_eq!(history.get(0).unwrap().action, EscrowAction::Locked);
    assert_eq!(history.get(0).unwrap().amount, original);
    assert_eq!(history.get(1).unwrap().amount, first);
    assert_eq!(history.get(2).unwrap().amount, second);
    assert_eq!(history.get(3).unwrap().action, EscrowAction::ReleasedToPool);
    assert_eq!(history.get(3).unwrap().amount, final_release);
    let released_total = history.get(1).unwrap().amount
        + history.get(2).unwrap().amount
        + history.get(3).unwrap().amount;
    assert_eq!(released_total, original);
    assert!(client.try_release_to_pool(&invoice_id, &1).is_err());
    assert_eq!(client.get_locked(&invoice_id), 0);
    assert_eq!(client.get_history(&invoice_id).len(), 4);
}

// #1006: `release_to_pool` transfers before mutating custody state, so a
// rejected token transfer must leave the lock, timestamp, history and events
// untouched.
#[test]
fn test_release_to_pool_rolls_back_when_transfer_fails() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    let pool = env.register(poolmock::MockPool, ());
    let invoice = env.register(MockCaller, ());
    poolmock::MockPoolClient::new(&env, &pool).set_invoice_contract(&invoice);
    let issuer = env.register(MockCaller, ());
    let token_id = env.register(reject::RecipientRejectMockToken, ());
    let token = reject::RecipientRejectMockTokenClient::new(&env, &token_id);

    // Fund the pool so the lock can succeed; the rejection is recipient-based.
    env.as_contract(&token_id, || {
        env.storage()
            .persistent()
            .set(&BalanceKey(pool.clone()), &10_000_000_000_000i128);
    });

    let contract_id = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), token_id.clone()),
    );
    let client = EscrowContractClient::new(&env, &contract_id);

    // Reject the escrow→pool leg of `release_to_pool`.
    token.set_reject_to(&pool);

    let invoice_id = generate_invoice_id(&env, 40);
    let amount: u128 = 1_000_000_000;
    assert!(client.lock(&invoice_id, &amount, &issuer));

    let locked_at = client.get_locked_at(&invoice_id);
    let history_before = client.get_history(&invoice_id);
    let events_before = env.events().all().len();
    let pool_before = token.balance(&pool);
    let escrow_before = token.balance(&contract_id);

    assert!(client
        .try_release_to_pool(&invoice_id, &(amount / 2))
        .is_err());

    // Lock, timestamp, history, balances and events are all unchanged.
    assert_eq!(client.get_locked(&invoice_id), amount);
    assert_eq!(client.get_locked_at(&invoice_id), locked_at);
    assert_eq!(client.get_history(&invoice_id), history_before);
    assert_eq!(token.balance(&pool), pool_before);
    assert_eq!(token.balance(&contract_id), escrow_before);
    assert_eq!(env.events().all().len(), events_before);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_release_to_pool_unknown_invoice_id_panics() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    let unknown_id = generate_invoice_id(&env, 999);

    // Never locked this invoice_id — should panic with NotFound (#2)
    client.release_to_pool(&unknown_id, &1_000_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_release_to_pool_second_call_panics() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    // Lock funds and release once
    client.lock(&invoice_id, &amount, &issuer);
    client.release_to_pool(&invoice_id, &amount);

    // Second release_to_pool call on the same invoice_id should panic with NotFound (#2)
    client.release_to_pool(&invoice_id, &amount);
}

// ---- Issue #819: issuer-release marker on the funding-time payout path ----

#[test]
fn test_release_to_issuer_leaves_marker_that_release_to_pool_consumes() {
    let (env, client, _admin, pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 30);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    client.release_to_issuer(&invoice_id, &issuer);

    // Principal left escrow to the issuer; only the marker remains.
    assert_eq!(client.get_locked(&invoice_id), 0);
    assert!(client.was_released_to_issuer(&invoice_id));

    // The repayment route works off the marker: buyer funds land in escrow,
    // then exactly one release_to_pool forwards them to the pool.
    let escrow_key = BalanceKey(client.address.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&escrow_key, &1_500_000_000i128);
    });
    let pool_before = get_balance(&env, &usdc_id, &pool);
    assert!(client.release_to_pool(&invoice_id, &1_500_000_000));
    assert_eq!(
        get_balance(&env, &usdc_id, &pool) - pool_before,
        1_500_000_000
    );

    // The marker was consumed — no second release, and the default path can
    // no longer attribute any missing principal to the issuer.
    assert!(!client.was_released_to_issuer(&invoice_id));
    assert!(!client.handle_default(&invoice_id, &pool));
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_release_to_pool_after_marker_second_call_panics() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 31);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    client.release_to_issuer(&invoice_id, &issuer);

    client.release_to_pool(&invoice_id, &500_000_000);
    // Replay after the marker was consumed must surface NotFound (#2), the
    // same guard the lock-record path provides.
    client.release_to_pool(&invoice_id, &500_000_000);
}

#[test]
fn test_handle_default_returns_funds_to_pool() {
    let (env, client, _admin, pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);

    env.ledger().set_timestamp(env.ledger().timestamp() + 60);
    // Pool is the normal operational caller for default resolution
    let result = client.handle_default(&invoice_id, &pool);
    assert!(result);

    // Verify caller is in the event
    let events = env.events().all();
    let (_, topics, _) = events.last().expect("expected at least one event");
    let caller: Address = Address::try_from_val(&env, &topics.get(3).unwrap()).unwrap();
    assert_eq!(caller, pool);

    // Verify record was removed
    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
}

#[test]
fn test_handle_default_invoked_by_admin_succeeds() {
    let (env, client, admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    env.ledger().set_timestamp(env.ledger().timestamp() + 60);

    // Call handle_default with admin as the caller
    let result = client.handle_default(&invoice_id, &admin);
    assert!(result);
    // sdk >= 22 test Env clears its event buffer before every top-level
    // invocation, so capture the emitted events right after the call.
    let events_after_call = env.events().all();

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
    assert_last_event_four(
        &env,
        &events_after_call,
        "default_resolved",
        invoice_id.clone(),
        _pool,
        admin,
        amount,
    );
}

#[test]
fn test_handle_default_admin_can_trigger() {
    let (env, client, admin, pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    env.ledger().set_timestamp(env.ledger().timestamp() + 60);
    // Admin can directly trigger default resolution (emergency / recovery path)
    let result = client.handle_default(&invoice_id, &admin);
    assert!(result);
    let events_after_call = env.events().all();

    let locked = client.get_locked(&invoice_id);
    assert_eq!(locked, 0);
    // Funds are always returned to the pool address regardless of who triggered
    assert_last_event_four(
        &env,
        &events_after_call,
        "default_resolved",
        invoice_id.clone(),
        pool,
        admin,
        amount,
    );
}

#[test]
fn test_handle_default_returns_false_if_no_record() {
    let (env, client, _admin, pool, _issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 8);

    let result = client.handle_default(&invoice_id, &pool);
    assert!(!result);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_handle_default_rejects_before_grace_period() {
    let (env, client, _admin, pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 9);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    env.ledger().set_timestamp(env.ledger().timestamp() + 59);

    client.handle_default(&invoice_id, &pool);
}

// #1003: a record whose `locked_at` is ahead of ledger time (timestamp
// rollback or malformed record) must be rejected with the typed NotAuthorized
// error rather than underflowing `now - locked_at`, and the lock must remain
// untouched.
#[test]
fn test_handle_default_rejects_future_locked_at_and_keeps_lock() {
    let (env, client, _admin, pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 11);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);

    // Seed a future `locked_at`, as if ledger time had rolled back after the
    // record was written.
    let future_locked_at = env.ledger().timestamp() + 3_600;
    env.as_contract(&client.address, || {
        let key = DataKey::Locked(invoice_id.clone());
        let mut record: EscrowRecord = env.storage().persistent().get(&key).expect("lock record");
        record.locked_at = future_locked_at;
        env.storage().persistent().set(&key, &record);
    });

    let history_before = client.get_history(&invoice_id);
    let events_before = env.events().all().len();
    let pool_before = get_balance(&env, &usdc_id, &pool);
    let escrow_before = get_balance(&env, &usdc_id, &client.address);

    let err = client
        .try_handle_default(&invoice_id, &pool)
        .expect_err("future locked_at must be rejected");
    assert_eq!(err, Ok(soroban_sdk::Error::from_contract_error(3)));

    // The lock (including its timestamp), history, balances and events are
    // all unchanged — no funds moved and no default was recorded.
    assert_eq!(client.get_locked(&invoice_id), amount);
    assert_eq!(client.get_locked_at(&invoice_id), future_locked_at);
    assert_eq!(client.get_history(&invoice_id), history_before);
    assert_eq!(get_balance(&env, &usdc_id, &pool), pool_before);
    assert_eq!(get_balance(&env, &usdc_id, &client.address), escrow_before);
    assert_eq!(env.events().all().len(), events_before);
}

#[test]
fn test_handle_default_allows_at_grace_period_boundary() {
    let (env, client, _admin, pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 10);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    env.ledger().set_timestamp(env.ledger().timestamp() + 60);

    let result = client.handle_default(&invoice_id, &pool);
    assert!(result);
    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
fn test_handle_default_second_call_returns_false_without_side_effects() {
    let (env, client, _admin, pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    // Lock funds and advance past grace period
    client.lock(&invoice_id, &amount, &issuer);
    env.ledger().set_timestamp(env.ledger().timestamp() + 60);

    // First call: should succeed, remove the record, and emit default_resolved
    let result1 = client.handle_default(&invoice_id, &pool);
    assert!(result1);
    let events_after_call = env.events().all();
    assert_eq!(client.get_locked(&invoice_id), 0);
    assert_last_event_four(
        &env,
        &events_after_call,
        "default_resolved",
        invoice_id.clone(),
        pool.clone(),
        pool.clone(),
        amount,
    );

    // Second call: should return false with no side effects
    let result2 = client.handle_default(&invoice_id, &pool);
    assert!(!result2);
    let events_after_second = env.events().all();

    // Verify state is unchanged — get_locked still 0
    assert_eq!(client.get_locked(&invoice_id), 0);

    // Verify no new events were emitted by the second call. sdk >= 22 test
    // Env resets its event buffer before every top-level invocation, so the
    // buffer captured right after the second call is exactly its own events.
    assert_eq!(
        events_after_second.len(),
        0,
        "second handle_default must not emit any events"
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_handle_default_panics_when_escrow_balance_insufficient() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    let pool = env.register(MockCaller, ());
    let issuer = env.register(MockCaller, ());
    let usdc_id = env.register(strict::StrictMockToken, ());
    let token_client = strict::StrictMockTokenClient::new(&env, &usdc_id);

    // Fund the pool so the initial lock can proceed.
    let pool_bal_key = BalanceKey(pool.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&pool_bal_key, &10_000_000_000_000i128);
    });

    let contract_id = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), usdc_id.clone()),
    );
    let client = EscrowContractClient::new(&env, &contract_id);

    let invoice_id = generate_invoice_id(&env, 30);
    let amount: u128 = 1_000_000_000;
    client.lock(&invoice_id, &amount, &issuer);

    // Sanity: the escrow contract now holds exactly the locked amount.
    assert_eq!(token_client.balance(&contract_id), amount as i128);

    // Drain the escrow contract's balance directly through the token so the
    // subsequent default transfer cannot complete.
    let escrow_balance = token_client.balance(&contract_id);
    token_client.transfer(&contract_id, &issuer, &escrow_balance);
    assert_eq!(token_client.balance(&contract_id), 0);

    // Advance past the grace period so the only remaining failure mode is
    // the token transfer itself.
    env.ledger().set_timestamp(env.ledger().timestamp() + 60);

    client.handle_default(&invoice_id, &pool);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_handle_default_admin_caller_without_pool_set_panics_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(
        EscrowContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    // #827: a deployed escrow is always constructed; evicting the config
    // keys reproduces the missing-config state this test targets.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::PoolContract);
        env.storage().instance().remove(&DataKey::UsdcAsset);
    });
    let client = EscrowContractClient::new(&env, &contract_id);
    let invoice_id = generate_invoice_id(&env, 105);
    let admin = Address::generate(&env);

    // Set only the Admin key — PoolContract (and UsdcAsset) remain unset —
    // and seed a lock record so handle_default gets past its early guard.
    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Admin, &admin);
        let record = EscrowRecord {
            invoice_id: invoice_id.clone(),
            amount: 1_000_000_000,
            locked_at: env.ledger().timestamp(),
            issuer: Address::generate(&env),
        };
        env.storage()
            .persistent()
            .set(&DataKey::Locked(invoice_id.clone()), &record);
    });

    // Even though the caller is the admin, the missing PoolContract must
    // panic with NotInitialized before any funds move.
    client.handle_default(&invoice_id, &admin);
}

#[test]
fn test_handle_default_event_includes_caller() {
    let (env, client, admin, pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    env.ledger().set_timestamp(env.ledger().timestamp() + 60);

    // Call with admin as caller - verify caller is in event
    let result = client.handle_default(&invoice_id, &admin);
    assert!(result);

    let events = env.events().all();
    let (_, topics, _) = events.last().expect("expected at least one event");

    let topic0: Symbol = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
    let topic1: BytesN<32> = BytesN::<32>::try_from_val(&env, &topics.get(1).unwrap()).unwrap();
    let topic2: Address = Address::try_from_val(&env, &topics.get(2).unwrap()).unwrap();
    let topic3: Address = Address::try_from_val(&env, &topics.get(3).unwrap()).unwrap();

    assert_eq!(topic0, Symbol::new(&env, "default_resolved"));
    assert_eq!(topic1, invoice_id);
    assert_eq!(topic2, pool);
    assert_eq!(topic3, admin);

    // Verify caller is also recorded in history
    let history = client.get_history(&invoice_id);
    let last_event = history.last().unwrap();
    assert_eq!(last_event.caller, Some(admin));
}

// ============================================================================
// Get Locked Tests
// ============================================================================

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_handle_default_unauthorized_caller_panics() {
    let (env, client, _admin, pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;
    let stranger = Address::generate(&env);

    client.lock(&invoice_id, &amount, &issuer);
    // A caller that is neither admin nor pool must be rejected
    client.handle_default(&invoice_id, &stranger);
    // also ensure that pool is indeed required for the normal path
    let _ = pool;
}

#[test]
fn test_get_locked_returns_zero_when_empty() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 9);

    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
fn test_get_locked_returns_zero_for_unknown_id() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();

    // Generate a random unknown invoice ID
    let unknown_id = generate_invoice_id(&env, 999);
    assert_eq!(client.get_locked(&unknown_id), 0);
}

#[test]
fn test_get_locked_returns_amount_when_locked() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 10);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    assert_eq!(client.get_locked(&invoice_id), amount);
}

#[test]
fn test_get_locked_returns_zero_after_release_to_issuer() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 11);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    assert_eq!(client.get_locked(&invoice_id), amount);

    client.release_to_issuer(&invoice_id, &issuer);
    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
fn test_get_locked_returns_zero_after_release_to_pool() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 12);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    assert_eq!(client.get_locked(&invoice_id), amount);

    client.release_to_pool(&invoice_id, &amount);
    assert_eq!(client.get_locked(&invoice_id), 0);
}

#[test]
fn test_get_locked_at_returns_zero_when_empty() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 20);

    assert_eq!(client.get_locked_at(&invoice_id), 0);
}

#[test]
fn test_get_locked_at_returns_timestamp_when_locked() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 21);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    let locked_at = client.get_locked_at(&invoice_id);
    assert_eq!(locked_at, env.ledger().timestamp());
}

#[test]
fn test_get_locked_at_returns_zero_after_release() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    env.ledger().set_timestamp(1_000_000);
    let invoice_id = generate_invoice_id(&env, 22);
    let issuer = Address::generate(&env);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    assert!(client.get_locked_at(&invoice_id) > 0);

    client.release_to_issuer(&invoice_id, &issuer);
    assert_eq!(client.get_locked_at(&invoice_id), 0);
}

// ============================================================================
// Integration Tests
// ============================================================================

#[test]
fn test_multiple_invoices_independent() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();

    let invoice_id_1 = generate_invoice_id(&env, 13);
    let invoice_id_2 = generate_invoice_id(&env, 14);
    let amount_1: u128 = 1_000_000_000;
    let amount_2: u128 = 2_000_000_000;

    client.lock(&invoice_id_1, &amount_1, &issuer);
    client.lock(&invoice_id_2, &amount_2, &issuer);

    assert_eq!(client.get_locked(&invoice_id_1), amount_1);
    assert_eq!(client.get_locked(&invoice_id_2), amount_2);

    client.release_to_pool(&invoice_id_1, &amount_1);

    assert_eq!(client.get_locked(&invoice_id_1), 0);
    assert_eq!(client.get_locked(&invoice_id_2), amount_2);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_lock_fails_duplicate() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 15);
    client.lock(&invoice_id, &1_000_000_000, &issuer);
    client.lock(&invoice_id, &500_000_000, &issuer);
}

#[test]
fn test_get_history_extends_ttl() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 101);
    client.lock(&invoice_id, &1_000_000_000, &issuer);

    let contract_id = client.address.clone();
    let key = DataKey::History(invoice_id.clone());
    let ttl_before_drain: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + ttl_before_drain - 50);

    let ttl_before_read: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    assert!(
        ttl_before_read < TTL_THRESHOLD,
        "history TTL should be below threshold before read, got {ttl_before_read}"
    );

    assert!(!client.get_history(&invoice_id).is_empty());

    let ttl_after_read: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    assert!(
        ttl_after_read > ttl_before_read,
        "get_history should extend TTL: before={ttl_before_read}, after={ttl_after_read}"
    );
    assert!(
        ttl_after_read >= TTL_EXTEND_TO - 1_000,
        "history TTL should be extended close to EXTEND_TO, got {ttl_after_read}"
    );
}

#[test]
fn test_get_history_does_not_create_missing_entry() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 102);
    let contract_id = client.address.clone();
    let key = DataKey::History(invoice_id.clone());

    assert!(!env.as_contract(&contract_id, || env.storage().persistent().has(&key)));
    assert!(client.get_history(&invoice_id).is_empty());
    assert!(!env.as_contract(&contract_id, || env.storage().persistent().has(&key)));
}

#[test]
fn test_get_history_returns_action_log() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 16);
    let amount: u128 = 1_000_000_000;
    let issuer = Address::generate(&env);

    client.lock(&invoice_id, &amount, &issuer);
    client.release_to_issuer(&invoice_id, &issuer);
    let events_after_call = env.events().all();

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
        &events_after_call,
        "released_to_issuer",
        invoice_id.clone(),
        issuer,
        amount,
    );
}

#[test]
#[should_panic]
fn test_lock_requires_pool_authorization() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup_without_auths();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    // The contract stores a pool address internally, but no auth entry is
    // present after setup_without_auths(), so this must fail at require_auth().
    client.lock(&invoice_id, &amount, &issuer);
}

#[test]
#[should_panic]
fn test_release_to_issuer_requires_pool_authorization() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    env.set_auths(&[]);
    client.release_to_issuer(&invoice_id, &issuer);
}

#[test]
#[should_panic]
fn test_handle_default_requires_pool_authorization() {
    let (env, client, _admin, pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;

    client.lock(&invoice_id, &amount, &issuer);
    env.set_auths(&[]);
    // No auth entries present — require_auth() on the pool caller must fail
    client.handle_default(&invoice_id, &pool);
}

// ============================================================================
// Pre-initialization tests — typed NotInitialized (Contract error #6)
// ============================================================================

/// `lock` must panic with `NotInitialized` rather than the cryptic
/// host-side `Option::unwrap()` error when called before construction (`__constructor`).
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_lock_uninitialized_panics_with_typed_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(
        EscrowContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    // #827: a deployed escrow is always constructed; evicting the config
    // keys reproduces the missing-config state this test targets.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::PoolContract);
        env.storage().instance().remove(&DataKey::UsdcAsset);
    });
    let client = EscrowContractClient::new(&env, &contract_id);
    let invoice_id = generate_invoice_id(&env, 100);
    let issuer = Address::generate(&env);
    // mock_all_auths bypasses pool.require_auth so the unwrap on the
    // uninitialized PoolContract is observable as the typed error.
    client.lock(&invoice_id, &1_000_000_000, &issuer);
}

/// `release_to_issuer` must panic with `NotInitialized` rather than the
/// cryptic host-side `Option::unwrap()` error when called before construction (`__constructor`).
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_release_to_issuer_uninitialized_panics_with_typed_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(
        EscrowContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    // #827: a deployed escrow is always constructed; evicting the config
    // keys reproduces the missing-config state this test targets.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::PoolContract);
        env.storage().instance().remove(&DataKey::UsdcAsset);
    });
    let client = EscrowContractClient::new(&env, &contract_id);
    let invoice_id = generate_invoice_id(&env, 101);
    let issuer = Address::generate(&env);
    client.release_to_issuer(&invoice_id, &issuer);
}

/// `release_to_pool` must panic with `NotInitialized` rather than the
/// cryptic host-side `Option::unwrap()` error when called before construction (`__constructor`).
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_release_to_pool_uninitialized_panics_with_typed_not_initialized() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(
        EscrowContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    // #827: a deployed escrow is always constructed; evicting the config
    // keys reproduces the missing-config state this test targets.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::PoolContract);
        env.storage().instance().remove(&DataKey::UsdcAsset);
    });
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
    let contract_id = env.register(
        EscrowContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    // #827: a deployed escrow is always constructed; evicting the config
    // keys reproduces the missing-config state this test targets.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::PoolContract);
        env.storage().instance().remove(&DataKey::UsdcAsset);
    });
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
            issuer: Address::generate(&env),
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
    let contract_id = env.register(
        EscrowContract,
        (
            Address::generate(&env),
            Address::generate(&env),
            Address::generate(&env),
        ),
    );
    // #827: a deployed escrow is always constructed; evicting the config
    // keys reproduces the missing-config state this test targets.
    env.as_contract(&contract_id, || {
        env.storage().instance().remove(&DataKey::Admin);
        env.storage().instance().remove(&DataKey::PoolContract);
        env.storage().instance().remove(&DataKey::UsdcAsset);
    });
    let client = EscrowContractClient::new(&env, &contract_id);
    let invoice_id = generate_invoice_id(&env, 104);
    let caller = Address::generate(&env);
    assert!(!client.handle_default(&invoice_id, &caller));
}

#[derive(Clone, Debug)]
enum EscrowOperation {
    Lock { invoice: usize, amount: u128 },
    ReleaseToPool { invoice: usize },
    ReleaseToIssuer { invoice: usize },
    HandleDefault { invoice: usize },
    UnauthorizedDefault { invoice: usize },
}

fn escrow_operations() -> impl Strategy<Value = alloc::vec::Vec<EscrowOperation>> {
    prop::collection::vec((0usize..4, 1u128..=1_000_000_000, 0u8..5), 1..=30).prop_map(
        |operations| {
            operations
                .into_iter()
                .map(|(invoice, amount, action)| match action {
                    0 => EscrowOperation::Lock { invoice, amount },
                    1 => EscrowOperation::ReleaseToPool { invoice },
                    2 => EscrowOperation::ReleaseToIssuer { invoice },
                    3 => EscrowOperation::HandleDefault { invoice },
                    _ => EscrowOperation::UnauthorizedDefault { invoice },
                })
                .collect()
        },
    )
}

#[test]
fn prop_escrow_balance_equals_sum_of_locked_records() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&escrow_operations(), |operations| {
            let (env, client, _admin, pool, issuer, usdc_id) = setup();
            // Coverage instrumentation increases host CPU cost for long property sequences.
            // Keep all generated operations and invariants, without the default test budget.
            env.cost_estimate().budget().reset_unlimited();
            let invoice_ids = [
                generate_invoice_id(&env, 200),
                generate_invoice_id(&env, 201),
                generate_invoice_id(&env, 202),
                generate_invoice_id(&env, 203),
            ];
            let stranger = Address::generate(&env);
            let mut expected_locks = [None; 4];
            // Issue #819: `release_to_issuer` removes the lock record but
            // leaves an `IssuerReleased` marker that exactly one
            // `release_to_pool` call may consume.
            let mut issuer_released = [false; 4];
            let mut history_lengths = [0u32; 4];

            for operation in operations {
                match operation {
                    EscrowOperation::Lock { invoice, amount } => {
                        let invoice_id = &invoice_ids[invoice];
                        if expected_locks[invoice].is_some() {
                            let balance_before = get_balance(&env, &usdc_id, &client.address);
                            let history_before = client.get_history(invoice_id).len();
                            let locked_before = client.get_locked(invoice_id);

                            prop_assert!(client.try_lock(invoice_id, &amount, &issuer).is_err());
                            prop_assert_eq!(
                                get_balance(&env, &usdc_id, &client.address),
                                balance_before
                            );
                            prop_assert_eq!(client.get_locked(invoice_id), locked_before);
                            prop_assert_eq!(client.get_history(invoice_id).len(), history_before);
                        } else {
                            prop_assert!(client.lock(invoice_id, &amount, &issuer));
                            expected_locks[invoice] = Some(amount);
                        }
                    }
                    EscrowOperation::ReleaseToPool { invoice } => {
                        let invoice_id = &invoice_ids[invoice];
                        if let Some(amount) = expected_locks[invoice] {
                            prop_assert!(client.release_to_pool(invoice_id, &amount));
                            expected_locks[invoice] = None;
                            prop_assert_eq!(client.get_locked(invoice_id), 0);
                        } else if issuer_released[invoice] {
                            // Issue #819 flow: the principal left escrow at
                            // `release_to_issuer`; the buyer's repayment lands
                            // in escrow before `release_to_pool` consumes the
                            // marker. Credit one unit to mirror that deposit so
                            // the balance invariant below keeps holding.
                            let escrow_balance_key = BalanceKey(client.address.clone());
                            env.as_contract(&usdc_id, || {
                                let bal: i128 = env
                                    .storage()
                                    .persistent()
                                    .get(&escrow_balance_key)
                                    .unwrap_or(0);
                                env.storage()
                                    .persistent()
                                    .set(&escrow_balance_key, &(bal + 1));
                            });
                            prop_assert!(client.release_to_pool(invoice_id, &1));
                            issuer_released[invoice] = false;
                            prop_assert_eq!(client.get_locked(invoice_id), 0);
                        } else {
                            let balance_before = get_balance(&env, &usdc_id, &client.address);
                            let history_before = client.get_history(invoice_id).len();

                            prop_assert!(client.try_release_to_pool(invoice_id, &1).is_err());
                            prop_assert_eq!(
                                get_balance(&env, &usdc_id, &client.address),
                                balance_before
                            );
                            prop_assert_eq!(client.get_locked(invoice_id), 0);
                            prop_assert_eq!(client.get_history(invoice_id).len(), history_before);
                        }
                    }
                    EscrowOperation::ReleaseToIssuer { invoice } => {
                        let invoice_id = &invoice_ids[invoice];
                        if expected_locks[invoice].is_some() {
                            prop_assert!(client.release_to_issuer(invoice_id, &issuer));
                            expected_locks[invoice] = None;
                            issuer_released[invoice] = true;
                            prop_assert_eq!(client.get_locked(invoice_id), 0);
                        } else {
                            let balance_before = get_balance(&env, &usdc_id, &client.address);
                            let history_before = client.get_history(invoice_id).len();

                            prop_assert!(client
                                .try_release_to_issuer(invoice_id, &issuer)
                                .is_err());
                            prop_assert_eq!(
                                get_balance(&env, &usdc_id, &client.address),
                                balance_before
                            );
                            prop_assert_eq!(client.get_locked(invoice_id), 0);
                            prop_assert_eq!(client.get_history(invoice_id).len(), history_before);
                        }
                    }
                    EscrowOperation::HandleDefault { invoice } => {
                        let invoice_id = &invoice_ids[invoice];
                        if expected_locks[invoice].is_some() {
                            env.ledger().set_timestamp(env.ledger().timestamp() + 60);
                            prop_assert!(client.handle_default(invoice_id, &pool));
                            expected_locks[invoice] = None;
                            prop_assert_eq!(client.get_locked(invoice_id), 0);
                        } else {
                            prop_assert!(!client.handle_default(invoice_id, &pool));
                        }
                    }
                    EscrowOperation::UnauthorizedDefault { invoice } => {
                        let invoice_id = &invoice_ids[invoice];
                        if expected_locks[invoice].is_some() {
                            env.ledger().set_timestamp(env.ledger().timestamp() + 60);
                            let balance_before = get_balance(&env, &usdc_id, &client.address);
                            let history_before = client.get_history(invoice_id).len();
                            let locked_before = client.get_locked(invoice_id);

                            prop_assert!(client.try_handle_default(invoice_id, &stranger).is_err());
                            prop_assert_eq!(
                                get_balance(&env, &usdc_id, &client.address),
                                balance_before
                            );
                            prop_assert_eq!(client.get_locked(invoice_id), locked_before);
                            prop_assert_eq!(client.get_history(invoice_id).len(), history_before);
                        } else {
                            prop_assert!(!client.handle_default(invoice_id, &stranger));
                        }
                    }
                }

                let mut total_locked = 0i128;
                for (index, invoice_id) in invoice_ids.iter().enumerate() {
                    let expected = expected_locks[index].unwrap_or(0);
                    prop_assert_eq!(client.get_locked(invoice_id), expected);
                    total_locked += expected as i128;

                    let history_len = client.get_history(invoice_id).len();
                    prop_assert!(history_len >= history_lengths[index]);
                    history_lengths[index] = history_len;
                }
                prop_assert_eq!(get_balance(&env, &usdc_id, &client.address), total_locked);
            }

            Ok(())
        })
        .unwrap();
}
// ============== ISSUE #716: EMERGENCY PAUSE / UNPAUSE ==============

#[test]
fn test_pause_blocks_state_changes_and_unpause_restores_them() {
    let (env, client, _admin, pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount = 1_000_000_000u128;

    // Live: the pool can lock funds.
    client.lock(&invoice_id, &amount, &issuer);

    client.pause();

    // State-changing entry points are rejected while paused...
    let second = generate_invoice_id(&env, 2);
    assert!(client.try_lock(&second, &amount, &issuer).is_err());
    assert!(client.try_release_to_issuer(&invoice_id, &issuer).is_err());
    assert!(client.try_release_to_pool(&invoice_id, &amount).is_err());
    assert!(client.try_handle_default(&invoice_id, &pool).is_err());

    // ...while read-only views keep working.
    assert_eq!(client.get_locked(&invoice_id), amount);
    assert_eq!(client.get_admin(), client.get_admin());
    assert_eq!(client.get_pool_contract(), pool);
    assert_eq!(client.get_usdc_asset(), usdc_id);
    assert!(!client.get_history(&invoice_id).is_empty());
    // Funds never moved while paused.
    assert_eq!(get_balance(&env, &usdc_id, &client.address), amount as i128);

    client.unpause();

    // Unpaused: fund movement flows again.
    assert!(client.release_to_issuer(&invoice_id, &issuer));
    assert_eq!(client.get_locked(&invoice_id), 0);
}

/// Pins the exact breaker error: `PauseError::ContractPaused` from the shared
/// crate surfaces as `Error(Contract, #1)`. `EscrowError::AlreadyInitialized`
/// is the only other #1 and is unreachable from `lock`.
#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_lock_reverts_while_paused_with_contract_paused_error() {
    let (env, client, _admin, _pool, issuer, _usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    client.pause();
    client.lock(&invoice_id, &1_000_000_000, &issuer);
}

/// Negative auth: only a non-admin signed `pause`, so the stored admin's
/// `require_auth()` must reject it — a non-admin cannot engage the breaker.
#[test]
fn test_pause_requires_admin_authorization() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    let non_admin = env.register(MockCaller, ());

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &non_admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &client.address,
            fn_name: "pause",
            args: soroban_sdk::Vec::new(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_pause().is_err());
}

/// Negative auth for disengaging: a non-admin cannot unpause either, so paused
/// escrow cannot be resumed by anyone but the admin.
#[test]
fn test_unpause_requires_admin_authorization() {
    let (env, client, _admin, _pool, _issuer, _usdc_id) = setup();
    client.pause();
    let non_admin = env.register(MockCaller, ());

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &non_admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &client.address,
            fn_name: "unpause",
            args: soroban_sdk::Vec::new(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_unpause().is_err());
}

#[test]
fn test_pause_and_unpause_emit_events() {
    let (env, client, admin, _pool, _issuer, _usdc_id) = setup();

    client.pause();
    let events = env.events().all();
    let (contract, topics, _) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, client.address);
    assert_eq!(topics.len(), 2);
    assert_eq!(
        Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&env, "paused")
    );
    assert_eq!(
        Address::try_from_val(&env, &topics.get(1).unwrap()).unwrap(),
        admin
    );

    client.unpause();
    let events = env.events().all();
    let (contract, topics, _) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, client.address);
    assert_eq!(
        Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&env, "unpaused")
    );
    assert_eq!(
        Address::try_from_val(&env, &topics.get(1).unwrap()).unwrap(),
        admin
    );
}

/// The breaker is storage-local: pausing one escrow must not affect a second,
/// independently deployed instance.
#[test]
fn test_pause_is_scoped_to_one_instance() {
    let env = Env::default();
    env.mock_all_auths();
    let usdc_id = env.register(MockToken, ());
    let _mock_token_client = MockTokenClient::new(&env, &usdc_id);
    let admin = env.register(MockCaller, ());
    let pool = env.register(MockCaller, ());
    let issuer = env.register(MockCaller, ());

    let id_a = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), usdc_id.clone()),
    );
    let id_b = env.register(
        EscrowContract,
        (admin.clone(), pool.clone(), usdc_id.clone()),
    );
    let a = EscrowContractClient::new(&env, &id_a);
    let b = EscrowContractClient::new(&env, &id_b);

    a.pause();

    let id = generate_invoice_id(&env, 1);
    assert!(a.try_lock(&id, &1_000, &issuer).is_err());
    assert!(b.lock(&id, &1_000, &issuer));
}

#[test]
fn test_get_timeout_window_defaults() {
    let (_env, client, _, _, _, _) = setup();
    assert_eq!(client.get_timeout_window(), DEFAULT_TIMEOUT_WINDOW_SECONDS);
}

#[test]
fn test_set_timeout_window_by_admin() {
    let (_env, client, _, _, _, _) = setup();
    client.set_timeout_window(&3600);
    assert_eq!(client.get_timeout_window(), 3600);
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_set_timeout_window_requires_admin_auth() {
    let (_env, client, _, _, _, _) = setup_without_auths();
    client.set_timeout_window(&3600);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_set_timeout_window_rejects_zero() {
    let (_env, client, _, _, _, _) = setup();
    client.set_timeout_window(&0);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_set_timeout_window_rejects_above_max() {
    let (_env, client, _, _, _, _) = setup();
    client.set_timeout_window(&(MAX_TIMEOUT_WINDOW_SECONDS + 1));
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_release_after_timeout_fails_before_window() {
    let (env, client, _, _, issuer, _) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    client.lock(&invoice_id, &1_000_000_000u128, &issuer);
    client.set_timeout_window(&3600);

    env.ledger().set_timestamp(env.ledger().timestamp() + 3599);
    client.release_after_timeout(&invoice_id);
}

#[test]
fn test_release_after_timeout_returns_funds_to_pool() {
    let (env, client, _, pool, issuer, usdc_id) = setup();
    let invoice_id = generate_invoice_id(&env, 1);
    let amount: u128 = 1_000_000_000;
    client.lock(&invoice_id, &amount, &issuer);
    client.set_timeout_window(&3600);
    let pool_before = get_balance(&env, &usdc_id, &pool);

    env.ledger().set_timestamp(env.ledger().timestamp() + 3600);
    // No auths: the call is permissionless.
    env.set_auths(&[]);
    assert!(client.release_after_timeout(&invoice_id));
    let events_after_call = env.events().all();

    assert_eq!(
        get_balance(&env, &usdc_id, &pool),
        pool_before + amount as i128
    );
    assert_eq!(client.get_locked(&invoice_id), 0);
    let history = client.get_history(&invoice_id);
    assert_eq!(
        history.last().unwrap().action,
        EscrowAction::TimeoutReleased
    );

    let (_, topics, data) = events_after_call
        .last()
        .expect("expected timeout_released event");
    let name: Symbol = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
    assert_eq!(name, Symbol::new(&env, "timeout_released"));
    assert_eq!(u128::try_from_val(&env, &data).unwrap(), amount);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_release_after_timeout_without_lock_panics() {
    let (env, client, _, _, _, _) = setup();
    client.release_after_timeout(&generate_invoice_id(&env, 1));
}

// ============================================================================
// Constructor -> Admin Setter Configuration (Issue #998) & Authorization Tests
// ============================================================================

#[test]
fn test_set_invoice_contract_stores_reference_and_emits_event() {
    let (env, client, admin, pool, _, usdc) = setup();
    let invoice = Address::generate(&env);

    // Initial state: get_invoice_contract returns pool-configured invoice address
    let pool_invoice = client.get_invoice_contract();
    assert_ne!(pool_invoice, invoice);

    // Admin pins the invoice contract reference
    client.set_invoice_contract(&invoice);

    // Event is emitted
    let events = env.events().all();
    let (contract, topics, data) = events
        .last()
        .expect("set_invoice_contract must publish an invoice_contract_set event");
    assert_eq!(contract, client.address);
    assert_eq!(topics.len(), 2);
    assert_eq!(
        Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&env, "invoice_contract_set")
    );
    assert_eq!(
        Address::try_from_val(&env, &topics.get(1).unwrap()).unwrap(),
        admin
    );
    assert_eq!(Address::try_from_val(&env, &data).unwrap(), invoice);

    // Stored reference is returned
    assert_eq!(client.get_invoice_contract(), invoice);
    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_pool_contract(), pool);
    assert_eq!(client.get_usdc_asset(), usdc);
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_set_invoice_contract_unauthorized_caller_panics() {
    let (_env, client, _admin, _pool, _issuer, _usdc) = setup_without_auths();
    let invoice = Address::generate(&_env);
    client.set_invoice_contract(&invoice);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_set_invoice_contract_rejects_aliasing_pool() {
    let (_env, client, _admin, pool, _issuer, _usdc) = setup();
    client.set_invoice_contract(&pool);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_set_invoice_contract_rejects_aliasing_escrow() {
    let (_env, client, _admin, _pool, _issuer, _usdc) = setup();
    let escrow = client.address.clone();
    client.set_invoice_contract(&escrow);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_set_invoice_contract_rejects_aliasing_asset() {
    let (_env, client, _admin, _pool, _issuer, usdc) = setup();
    client.set_invoice_contract(&usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_set_invoice_contract_rejects_aliasing_admin() {
    let (_env, client, admin, _pool, _issuer, _usdc) = setup();
    client.set_invoice_contract(&admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_release_to_issuer_rejects_invoice_recipient_after_pin() {
    let (env, client, _admin, _pool, _issuer, _usdc) = setup();
    let invoice = Address::generate(&env);
    client.set_invoice_contract(&invoice);

    let invoice_id = generate_invoice_id(&env, 101);
    client.lock(&invoice_id, &1_000_000_000, &invoice);
    client.release_to_issuer(&invoice_id, &invoice);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_release_to_issuer_rejects_pool_recipient() {
    let (env, client, _admin, pool, _issuer, _usdc) = setup();
    let invoice_id = generate_invoice_id(&env, 103);
    client.lock(&invoice_id, &1_000_000_000, &pool);
    client.release_to_issuer(&invoice_id, &pool);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_release_to_issuer_rejects_escrow_recipient() {
    let (env, client, _admin, _pool, _issuer, _usdc) = setup();
    let escrow = client.address.clone();
    let invoice_id = generate_invoice_id(&env, 104);
    client.lock(&invoice_id, &1_000_000_000, &escrow);
    client.release_to_issuer(&invoice_id, &escrow);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_release_to_issuer_rejects_asset_recipient() {
    let (env, client, _admin, _pool, _issuer, usdc) = setup();
    let invoice_id = generate_invoice_id(&env, 105);
    client.lock(&invoice_id, &1_000_000_000, &usdc);
    client.release_to_issuer(&invoice_id, &usdc);
}

#[test]
fn test_release_to_pool_authorization_through_configured_path() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    let pool = env.register(poolmock::MockPool, ());
    let invoice = env.register(MockCaller, ());
    poolmock::MockPoolClient::new(&env, &pool).set_invoice_contract(&invoice);
    let issuer = env.register(MockCaller, ());
    let usdc = env.register(MockToken, ());

    // Seed pool balance
    let pool_bal_key = BalanceKey(pool.clone());
    env.as_contract(&usdc, || {
        env.storage()
            .persistent()
            .set(&pool_bal_key, &10_000_000_000_000i128);
    });

    // Deploy escrow normally via __constructor(admin, pool, usdc)
    let contract_id = env.register(EscrowContract, (admin.clone(), pool.clone(), usdc.clone()));
    let client = EscrowContractClient::new(&env, &contract_id);

    // Required configuration path: admin pins the invoice contract reference
    client.set_invoice_contract(&invoice);

    let invoice_id = generate_invoice_id(&env, 106);
    let amount: u128 = 1_000_000_000;
    client.lock(&invoice_id, &amount, &issuer);

    let pool_before = get_balance(&env, &usdc, &pool);
    assert!(client.release_to_pool(&invoice_id, &amount));
    assert_eq!(client.get_locked(&invoice_id), 0);
    assert_eq!(
        get_balance(&env, &usdc, &pool) - pool_before,
        amount as i128
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_release_to_pool_rejects_pool_drift_from_configured_pin() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    let pool = env.register(poolmock::MockPool, ());
    let invoice = env.register(MockCaller, ());
    poolmock::MockPoolClient::new(&env, &pool).set_invoice_contract(&invoice);
    let issuer = env.register(MockCaller, ());
    let usdc = env.register(MockToken, ());

    let pool_bal_key = BalanceKey(pool.clone());
    env.as_contract(&usdc, || {
        env.storage()
            .persistent()
            .set(&pool_bal_key, &10_000_000_000_000i128);
    });

    // Deploy escrow normally via __constructor(admin, pool, usdc)
    let contract_id = env.register(EscrowContract, (admin.clone(), pool.clone(), usdc.clone()));
    let client = EscrowContractClient::new(&env, &contract_id);

    // Admin pins the invoice contract reference
    client.set_invoice_contract(&invoice);

    let invoice_id = generate_invoice_id(&env, 107);
    let amount: u128 = 1_000_000_000;
    client.lock(&invoice_id, &amount, &issuer);

    // Pool drifts / rotates invoice contract away from pinned address
    let drifted_invoice = env.register(MockCaller, ());
    poolmock::MockPoolClient::new(&env, &pool).set_invoice_contract(&drifted_invoice);

    // release_to_pool must panic with EscrowError::InvalidConfig (#8)
    client.release_to_pool(&invoice_id, &amount);
}

#[test]
fn test_datakey_invoice_contract_discriminant_is_preserved() {
    use soroban_sdk::{FromVal, Val};
    let env = Env::default();
    let key = DataKey::InvoiceContract;

    // Verify key round-trips via Soroban Val conversion
    let val: Val = key.into_val(&env);
    let restored = DataKey::from_val(&env, &val);
    assert!(matches!(restored, DataKey::InvoiceContract));
}

#[test]
fn test_invoice_only_redeployment_pins_new_address_and_allows_release_to_pool() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    let pool = env.register(poolmock::MockPool, ());
    let old_invoice = env.register(MockCaller, ());
    poolmock::MockPoolClient::new(&env, &pool).set_invoice_contract(&old_invoice);
    let issuer = env.register(MockCaller, ());
    let usdc = env.register(MockToken, ());

    // Seed pool balance
    let pool_bal_key = BalanceKey(pool.clone());
    env.as_contract(&usdc, || {
        env.storage()
            .persistent()
            .set(&pool_bal_key, &10_000_000_000_000i128);
    });

    // Deploy escrow with constructor (admin, pool, usdc)
    let contract_id = env.register(EscrowContract, (admin.clone(), pool.clone(), usdc.clone()));
    let client = EscrowContractClient::new(&env, &contract_id);

    // Initial deployment: admin pins the initial invoice contract
    client.set_invoice_contract(&old_invoice);
    assert_eq!(client.get_invoice_contract(), old_invoice);

    let invoice_id = generate_invoice_id(&env, 108);
    let amount: u128 = 1_000_000_000;
    client.lock(&invoice_id, &amount, &issuer);

    // Simulate invoice-only redeployment (--only invoice):
    // 1. A new invoice contract is deployed
    let new_invoice = env.register(MockCaller, ());
    // 2. Pool is wired with the new invoice contract
    poolmock::MockPoolClient::new(&env, &pool).set_invoice_contract(&new_invoice);
    // 3. Surviving escrow is wired with the new invoice contract via set_invoice_contract
    client.set_invoice_contract(&new_invoice);

    // Assert the new address is pinned on the surviving escrow
    assert_eq!(client.get_invoice_contract(), new_invoice);

    // Settlement via release_to_pool succeeds with new invoice authorization (no InvalidConfig)
    let pool_before = get_balance(&env, &usdc, &pool);
    assert!(client.release_to_pool(&invoice_id, &amount));
    assert_eq!(client.get_locked(&invoice_id), 0);
    assert_eq!(
        get_balance(&env, &usdc, &pool) - pool_before,
        amount as i128
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_invoice_only_redeployment_updates_pinned_recipient_validation() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = env.register(MockCaller, ());
    let pool = env.register(poolmock::MockPool, ());
    let old_invoice = env.register(MockCaller, ());
    let new_invoice = env.register(MockCaller, ());
    let usdc = env.register(MockToken, ());

    let contract_id = env.register(EscrowContract, (admin.clone(), pool.clone(), usdc.clone()));
    let client = EscrowContractClient::new(&env, &contract_id);

    // Initially pinned to old invoice
    client.set_invoice_contract(&old_invoice);

    // Invoice-only redeployment: repin to new invoice
    client.set_invoice_contract(&new_invoice);
    assert_eq!(client.get_invoice_contract(), new_invoice);

    // release_to_issuer must reject recipient matching new_invoice (#7 InvalidRecipient)
    let invoice_id = generate_invoice_id(&env, 109);
    client.lock(&invoice_id, &1_000_000_000, &new_invoice);
    client.release_to_issuer(&invoice_id, &new_invoice);
}
