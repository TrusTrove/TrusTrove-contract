#![cfg(test)]

extern crate std;

use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    testutils::{
        storage::Instance as _, storage::Persistent as _, Address as _, Events as _, Ledger,
        MockAuth, MockAuthInvoke,
    },
    vec,
    xdr::ToXdr,
    Address, BytesN, Env, IntoVal, InvokeError, String, Symbol, TryFromVal, Vec,
};

use crate::{
    constants::{
        MAX_BATCH_SIZE, MAX_PROTOCOL_FEE_BPS, SUGGESTED_DISCOUNT_CEILING_BPS,
        SUGGESTED_DISCOUNT_FLOOR_BPS,
    },
    DataKey, PoolContract, PoolContractClient, PoolError, DEFAULT_MIN_INITIAL_DEPOSIT,
    DEFAULT_SHARE_DECIMALS, TTL_EXTEND_TO, TTL_THRESHOLD,
};

use trusttrove_escrow::{EscrowContract as RealEscrow, EscrowContractClient as RealEscrowClient};
use trusttrove_invoice::{
    InvoiceContract as RealInvoice, InvoiceContractClient as RealInvoiceClient,
};
use trusttrove_test_utils::{
    setup_full_protocol, test_agent_id, test_agent_pubkey, test_agent_signing_key,
};

// Default invoice parameters matching create_and_list() defaults
// These computed constants eliminate magic numbers in test assertions
// and make the tests self-correcting when parameters change.
const DEFAULT_FACE_VALUE: u128 = 10_000_000_000;
const DEFAULT_DISCOUNT_BPS: u32 = 200;
const DEFAULT_FUNDED_AMOUNT: u128 =
    DEFAULT_FACE_VALUE * (10000 - DEFAULT_DISCOUNT_BPS as u128) / 10000;

// SEP-41 share metadata every test initializer passes to the constructor (issue
// #757). Kept as constants so the assertions in the metadata tests and the
// constructor call sites cannot drift apart.
const TEST_SHARE_NAME: &str = "TrusTrove USDC Pool Shares";
const TEST_SHARE_SYMBOL: &str = "TT-USDC";
const DEFAULT_YIELD_AMOUNT: u128 = DEFAULT_FACE_VALUE * DEFAULT_DISCOUNT_BPS as u128 / 10000;

// --------------- Mock Registry ---------------

#[contract]
pub struct MockRegistry;

#[contractimpl]
impl MockRegistry {
    pub fn is_verified(env: Env, address: Address) -> bool {
        env.storage()
            .persistent()
            .get::<_, bool>(&RegKey(address))
            .unwrap_or(false)
    }

    pub fn register(env: Env, address: Address) {
        env.storage()
            .persistent()
            .set(&RegKey(address.clone()), &true);
        env.storage()
            .persistent()
            .extend_ttl(&RegKey(address), TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    pub fn revoke(env: Env, address: Address) {
        env.storage()
            .persistent()
            .set(&RegKey(address.clone()), &false);
        env.storage()
            .persistent()
            .extend_ttl(&RegKey(address), TTL_THRESHOLD, TTL_EXTEND_TO);
    }
}

#[contracttype]
pub struct RegKey(Address);

// --------------- Mock Agent Registry (Underwrite) ---------------
//
// Stands in for the agent-registry contract from the separate
// `underwrite-contract` repo, so `create_and_list_with_params` can satisfy
// invoice's `submit_attestation` gate with a real secp256k1 signature.

#[contract]
pub struct MockAgentRegistry;

#[contractimpl]
impl MockAgentRegistry {
    pub fn get_agent(env: Env, agent_id: Symbol) -> Option<trusttrove_invoice::Agent> {
        env.storage().persistent().get(&AgentKey(agent_id))
    }

    pub fn register_agent(env: Env, agent_id: Symbol, agent: trusttrove_invoice::Agent) {
        env.storage().persistent().set(&AgentKey(agent_id), &agent);
    }
}

#[contracttype]
pub struct AgentKey(Symbol);

// Shared test_agent helpers imported from trusttrove_test_utils (Issue #813)

/// Submits a validly signed attestation for `invoice_id` against the
/// agent-registry wired up in `setup()`, unlocking it for
/// `list_for_financing`.
fn attest_invoice(te: &TestEnv, invoice_id: &BytesN<32>) {
    let payload = trusttrove_invoice::AttestationPayload {
        domain_separator: BytesN::from_array(
            &te.env,
            &trusttrove_invoice::ATTESTATION_DOMAIN_SEPARATOR,
        ),
        invoice_id: invoice_id.clone(),
        contract_address: te.invoice.address.clone(),
        network_id: te.env.ledger().network_id(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(&te.env, &[9u8; 32]),
        agent_id: test_agent_id(&te.env),
        nonce: 1,
        valid_until: u64::MAX,
    };
    let payload_bytes = payload.to_xdr(&te.env);
    let digest = te.env.crypto().keccak256(&payload_bytes).to_array();
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(&te.env, &sig_bytes);

    te.invoice
        .submit_attestation(invoice_id, &payload_bytes, &signature);
}

// --------------- Mock Token ---------------

#[contract]
pub struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let from_key = TKey(from.clone());
        let to_key = TKey(to.clone());
        let from_bal: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        let to_bal: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&from_key, &(from_bal - amount));
        env.storage().persistent().set(&to_key, &(to_bal + amount));
    }

    pub fn balance(env: Env, addr: Address) -> i128 {
        env.storage().persistent().get(&TKey(addr)).unwrap_or(0)
    }
}

#[contracttype]
pub struct TKey(Address);

// --------------- Mock Escrow (lock failure) ---------------

mod failing_escrow {
    use soroban_sdk::{contract, contractimpl, Address, BytesN, Env};

    #[contract]
    pub struct MockFailingEscrow;

    #[contractimpl]
    impl MockFailingEscrow {
        pub fn lock(_env: Env, _invoice_id: BytesN<32>, _amount: u128, _issuer: Address) -> bool {
            panic!("escrow lock failed");
        }
    }
}

// --------------- Mock Escrow (handle_default failure) ---------------
// Stands in for escrow to prove that handle_default rolls back pool state
// when escrow.handle_default fails (issue #970).

mod failing_escrow_default {
    use soroban_sdk::{contract, contractimpl, Address, BytesN, Env};

    #[contract]
    pub struct MockFailingEscrowDefault;

    #[contractimpl]
    impl MockFailingEscrowDefault {
        pub fn handle_default(_env: Env, _invoice_id: BytesN<32>, _pool: Address) -> bool {
            panic!("escrow handle_default failed");
        }
    }
}

// --------------- Mock Invoice (mark_defaulted failure) ---------------
// Stands in for the pool's configured invoice contract to prove that
// handle_default does not book the loss before invoice.mark_defaulted
// persists the Defaulted status (issue #971). Escrow stays real in these
// tests, so escrow.handle_default succeeds and mark_defaulted is the first
// failing step. Two failure modes are covered: a panicking invoice (the
// normal contract-error path) and an invoice that simply reports `false`.

mod failing_mark_defaulted {
    use soroban_sdk::{contract, contractimpl, BytesN, Env};

    #[contract]
    pub struct MockFailingMarkDefaultedInvoice;

    #[contractimpl]
    impl MockFailingMarkDefaultedInvoice {
        pub fn mark_defaulted(_env: Env, _invoice_id: BytesN<32>) -> bool {
            panic!("invoice mark_defaulted failed");
        }
    }
}

mod refusing_mark_defaulted {
    use soroban_sdk::{contract, contractimpl, BytesN, Env};

    #[contract]
    pub struct MockRefusingMarkDefaultedInvoice;

    #[contractimpl]
    impl MockRefusingMarkDefaultedInvoice {
        pub fn mark_defaulted(_env: Env, _invoice_id: BytesN<32>) -> bool {
            false
        }
    }
}

// --------------- Mock Token (refund transfer failure) ---------------
//
// Stands in for the pool's funding asset to prove that a failing refund
// transfer cannot leave the pool partially settled.
//
// `receive_repayment_with_refund` runs the pool's settlement accounting
// (`settle_repayment`: LP yield credited, `TotalFunded` and
// `ActiveInvoiceCount` decremented, `FundedInvoice` entry removed) *before*
// it transfers the buyer's refund. This stub's `transfer` applies the
// balance moves first and only then fails, so the test can prove that the
// failed transfer rolls back both the stub's own writes and the pool's
// settlement accounting in one atomic revert.

mod failing_token {
    use core::sync::atomic::{AtomicU32, Ordering};
    use soroban_sdk::{
        contract, contracterror, contractimpl, contracttype, panic_with_error, Address, Env,
    };

    /// Number of `transfer` calls the stub has seen.
    ///
    /// The stub's storage writes are rolled back together with the failed
    /// call, so the attempt counter cannot live in the contract's storage:
    /// it has to be kept outside the rollback boundary to prove that the
    /// refund transfer was actually reached.
    pub static TRANSFER_ATTEMPTS: AtomicU32 = AtomicU32::new(0);

    #[contracterror]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum MockTokenError {
        TransferFailed = 1,
    }

    #[contracttype]
    pub struct TKey(pub Address);

    #[contract]
    pub struct MockFailingToken;

    #[contractimpl]
    impl MockFailingToken {
        pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
            TRANSFER_ATTEMPTS.fetch_add(1, Ordering::SeqCst);

            // Move the balances *before* failing: a token that fails after
            // applying its effects must still leave no trace, and neither
            // must the pool's accounting.
            let from_key = TKey(from);
            let to_key = TKey(to);
            let from_bal: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
            let to_bal: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
            env.storage()
                .persistent()
                .set(&from_key, &(from_bal - amount));
            env.storage().persistent().set(&to_key, &(to_bal + amount));

            panic_with_error!(&env, MockTokenError::TransferFailed);
        }

        pub fn balance(env: Env, addr: Address) -> i128 {
            env.storage().persistent().get(&TKey(addr)).unwrap_or(0)
        }
    }
}

// --------------- Mock Invoice (unbounded face value) ---------------
//
// Stands in for the pool's configured invoice contract to prove that
// fund_invoice's funded-amount multiplication rejects a pathologically large
// cross-contract face_value with the typed PoolError::Overflow rather than an
// untyped arithmetic abort (issue #585). The real invoice contract caps
// face_value at MAX_FACE_VALUE = u128::MAX / 10_000, which cannot overflow
// the (10000 - discount_bps) scaling; the pool must not rely on that
// external bound.

#[contract]
pub struct MockHugeFaceInvoice;

#[contractimpl]
impl MockHugeFaceInvoice {
    pub fn configure(
        env: Env,
        issuer: Address,
        buyer: Address,
        funding_asset: Address,
        face_value: u128,
        discount_bps: u32,
    ) {
        env.storage()
            .instance()
            .set(&InvKey(Symbol::new(&env, "issuer")), &issuer);
        env.storage()
            .instance()
            .set(&InvKey(Symbol::new(&env, "buyer")), &buyer);
        env.storage()
            .instance()
            .set(&InvKey(Symbol::new(&env, "asset")), &funding_asset);
        env.storage()
            .instance()
            .set(&InvKey(Symbol::new(&env, "face")), &face_value);
        env.storage()
            .instance()
            .set(&InvKey(Symbol::new(&env, "disc")), &discount_bps);
    }

    pub fn get_status(_env: Env, _invoice_id: BytesN<32>) -> u32 {
        1 // Listed
    }

    pub fn get_funding_terms(env: Env, _invoice_id: BytesN<32>) -> (u32, u128, u32) {
        (
            1, // Listed
            env.storage()
                .instance()
                .get(&InvKey(Symbol::new(&env, "face")))
                .unwrap(),
            env.storage()
                .instance()
                .get(&InvKey(Symbol::new(&env, "disc")))
                .unwrap(),
        )
    }

    pub fn get_issuer(env: Env, _invoice_id: BytesN<32>) -> Address {
        env.storage()
            .instance()
            .get(&InvKey(Symbol::new(&env, "issuer")))
            .unwrap()
    }

    pub fn get_buyer(env: Env, _invoice_id: BytesN<32>) -> Address {
        env.storage()
            .instance()
            .get(&InvKey(Symbol::new(&env, "buyer")))
            .unwrap()
    }

    pub fn get_funding_asset(env: Env, _invoice_id: BytesN<32>) -> Address {
        env.storage()
            .instance()
            .get(&InvKey(Symbol::new(&env, "asset")))
            .unwrap()
    }

    pub fn get_funding_context(env: Env, _invoice_id: BytesN<32>) -> crate::FundingContext {
        crate::FundingContext {
            status: 1, // Listed
            face_value: env
                .storage()
                .instance()
                .get(&InvKey(Symbol::new(&env, "face")))
                .unwrap(),
            discount_bps: env
                .storage()
                .instance()
                .get(&InvKey(Symbol::new(&env, "disc")))
                .unwrap(),
            issuer: env
                .storage()
                .instance()
                .get(&InvKey(Symbol::new(&env, "issuer")))
                .unwrap(),
            buyer: env
                .storage()
                .instance()
                .get(&InvKey(Symbol::new(&env, "buyer")))
                .unwrap(),
            funding_asset: env
                .storage()
                .instance()
                .get(&InvKey(Symbol::new(&env, "asset")))
                .unwrap(),
        }
    }

    pub fn get_face_value(env: Env, _invoice_id: BytesN<32>) -> u128 {
        env.storage()
            .instance()
            .get(&InvKey(Symbol::new(&env, "face")))
            .unwrap()
    }

    pub fn get_discount_bps(env: Env, _invoice_id: BytesN<32>) -> u32 {
        env.storage()
            .instance()
            .get(&InvKey(Symbol::new(&env, "disc")))
            .unwrap()
    }
}

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MockEscrowError {
    LockCalled = 99,
}

#[contract]
pub struct MockUnexpectedEscrow;

#[contractimpl]
impl MockUnexpectedEscrow {
    pub fn lock(env: Env, _invoice_id: BytesN<32>, _amount: u128, _issuer: Address) -> bool {
        soroban_sdk::panic_with_error!(&env, MockEscrowError::LockCalled);
    }
}

#[contracttype]
pub struct InvKey(Symbol);

struct TestEnv {
    env: Env,
    pool: PoolContractClient<'static>,
    pool_id: Address,
    invoice: RealInvoiceClient<'static>,
    registry: MockRegistryClient<'static>,
    usdc_id: Address,
    xlm_id: Address,
    escrow_id: Address,
    admin: Address,
    issuer: Address,
    buyer: Address,
    lp: Address,
}

fn setup() -> TestEnv {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);

    let registry_id = env.register(MockRegistry, ());
    let registry = MockRegistryClient::new(&env, &registry_id);
    registry.register(&issuer);
    registry.register(&buyer);

    let usdc_id = env.register(MockToken, ());
    let xlm_id = env.register(MockToken, ());

    let lp_bal_key = TKey(lp.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&lp_bal_key, &100_000_000_000_000i128);
    });
    env.as_contract(&xlm_id, || {
        env.storage()
            .persistent()
            .set(&lp_bal_key, &100_000_000_000_000i128);
    });
    let buyer_bal_key = TKey(buyer.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&buyer_bal_key, &100_000_000_000_000i128);
    });
    env.as_contract(&xlm_id, || {
        env.storage()
            .persistent()
            .set(&buyer_bal_key, &100_000_000_000_000i128);
    });

    // #827: deploy and init are atomic — every protocol instance is seeded
    // by its constructor. The pool's address is pre-generated so the escrow
    // can be constructed with its pool wiring first.
    let pool_id = Address::generate(&env);

    let invoice_id = env.register(RealInvoice, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        RealEscrow,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );

    let invoice = RealInvoiceClient::new(&env, &invoice_id);
    let _escrow = RealEscrowClient::new(&env, &escrow_id);

    env.register_at(
        &pool_id,
        PoolContract,
        (
            admin.clone(),
            invoice_id.clone(),
            escrow_id.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            admin.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            String::from_str(&env, TEST_SHARE_NAME),
            String::from_str(&env, TEST_SHARE_SYMBOL),
            DEFAULT_SHARE_DECIMALS,
        ),
    );
    let pool = PoolContractClient::new(&env, &pool_id);

    invoice.add_supported_asset(&usdc_id);
    invoice.add_supported_asset(&xlm_id);

    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    let agent_registry_id = env.register(MockAgentRegistry, ());
    let agent_registry = MockAgentRegistryClient::new(&env, &agent_registry_id);
    agent_registry.register_agent(
        &test_agent_id(&env),
        &trusttrove_invoice::Agent {
            active: true,
            pubkey: test_agent_pubkey(&env),
        },
    );
    invoice.set_agent_registry_contract(&agent_registry_id);

    // Raise cap to 100% so existing tests (which fund at 98% utilization) still pass
    pool.set_max_utilization(&admin, &10000);

    TestEnv {
        env,
        pool,
        pool_id,
        invoice,
        registry,
        usdc_id,
        xlm_id,
        escrow_id,
        admin,
        issuer,
        buyer,
        lp,
    }
}

/// #827: deployment is atomic — a pool instance is seeded by the protocol
/// constructor. The pre-#827 "uninitialized" tests targeted a deployed but
/// unconfigured instance; the equivalent state is reproduced here by
/// constructing the pool with valid mock wiring and evicting the instance
/// keys the constructor seeds.
fn deploy_pool_with_evicted_config() -> (Env, Address, PoolContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let registry_id = env.register(MockRegistry, ());
    let usdc_id = env.register(MockToken, ());
    let invoice_id = env.register(RealInvoice, (admin.clone(), registry_id.clone()));

    let pool_id = Address::generate(&env);
    let escrow_id = env.register(
        RealEscrow,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );

    env.register_at(
        &pool_id,
        PoolContract,
        (
            admin.clone(),
            invoice_id.clone(),
            escrow_id.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            admin.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            String::from_str(&env, TEST_SHARE_NAME),
            String::from_str(&env, TEST_SHARE_SYMBOL),
            DEFAULT_SHARE_DECIMALS,
        ),
    );

    env.as_contract(&pool_id, || {
        for key in [
            DataKey::Admin,
            DataKey::InvoiceContract,
            DataKey::EscrowContract,
            DataKey::FundingAsset,
            DataKey::RegistryContract,
            DataKey::TotalShares,
            DataKey::TotalDeposits,
            DataKey::TotalFunded,
            DataKey::TotalYieldDistributed,
            DataKey::ActiveInvoiceCount,
            DataKey::MaxUtilizationBps,
            DataKey::FeeBps,
            DataKey::TotalLossRealised,
            DataKey::ProtocolFeeBps,
            DataKey::TreasuryAddress,
            DataKey::MinInitialDeposit,
            DataKey::ShareName,
            DataKey::ShareSymbol,
            DataKey::ShareDecimals,
        ] {
            env.storage().instance().remove(&key);
        }
    });

    let pool = PoolContractClient::new(&env, &pool_id);
    (env, pool_id, pool)
}

fn create_and_list(te: &TestEnv, funding_asset: &Address) -> BytesN<32> {
    create_and_list_with_params(te, funding_asset, 10_000_000_000, 200)
}

fn create_and_list_with_params(
    te: &TestEnv,
    funding_asset: &Address,
    face_value: u128,
    discount_bps: u32,
) -> BytesN<32> {
    create_and_list_with_params_and_term(te, funding_asset, face_value, discount_bps, 86400)
}

fn create_and_list_with_params_and_term(
    te: &TestEnv,
    funding_asset: &Address,
    face_value: u128,
    discount_bps: u32,
    term: u64,
) -> BytesN<32> {
    let due_date = te.env.ledger().timestamp() + term;
    let invoice_id =
        te.invoice
            .create(&te.issuer, &te.buyer, &face_value, &due_date, funding_asset);
    attest_invoice(te, &invoice_id);
    te.invoice.list_for_financing(&invoice_id, &discount_bps);
    invoice_id
}

#[test]
fn test_fund_invoice_escrow_lock_failure_preserves_state() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    let failing_escrow_id = te.env.register(failing_escrow::MockFailingEscrow, ());
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::EscrowContract, &failing_escrow_id);
    });

    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let funded_key = DataKey::FundedInvoice(invoice_id.clone());
    let stats_before = te.pool.get_stats();
    let pool_balance_before = usdc.balance(&te.pool_id);
    let invoice_status_before = te.invoice.get_status(&invoice_id);

    let result = te.pool.try_fund_invoice(&invoice_id);
    assert!(
        result.is_err(),
        "fund_invoice must revert when escrow.lock fails"
    );

    let stats_after = te.pool.get_stats();
    assert_eq!(
        stats_after.total_deposits, stats_before.total_deposits,
        "total deposits must not change when escrow.lock fails"
    );
    assert_eq!(
        stats_after.total_funded, stats_before.total_funded,
        "total funded must not change when escrow.lock fails"
    );
    assert_eq!(
        stats_after.active_invoice_count, stats_before.active_invoice_count,
        "active invoice count must not change when escrow.lock fails"
    );
    assert_eq!(
        stats_after.available_liquidity, stats_before.available_liquidity,
        "available liquidity must not change when escrow.lock fails"
    );
    assert!(
        !te.env.as_contract(&te.pool_id, || {
            te.env.storage().persistent().has(&funded_key)
        }),
        "no funded invoice entry may remain when escrow.lock fails"
    );
    assert_eq!(
        usdc.balance(&te.pool_id),
        pool_balance_before,
        "pool token balance must not change when escrow.lock fails"
    );
    assert_eq!(
        te.invoice.get_status(&invoice_id),
        invoice_status_before,
        "invoice status must not change when escrow.lock fails"
    );
}

fn fund_and_repay_invoice(te: &TestEnv) -> BytesN<32> {
    let invoice_id = create_and_list(te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);
    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);
    te.invoice.repay(&invoice_id);
    invoice_id
}

fn create_lp_with_balance(te: &TestEnv, balance: i128) -> Address {
    let lp = Address::generate(&te.env);
    let lp_bal_key = TKey(lp.clone());
    te.env.as_contract(&te.usdc_id, || {
        te.env.storage().persistent().set(&lp_bal_key, &balance);
    });
    lp
}

//  DEPOSIT TESTS
#[test]
fn test_first_deposit_issues_one_to_one_shares() {
    let te = setup();
    let shares = te.pool.deposit(&te.lp, &5_000_000_000);
    assert_eq!(shares, 5_000_000_000);

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 5_000_000_000);
    assert_eq!(pos.deposit_count, 1);
}

#[test]
fn test_second_deposit_issues_proportional_shares() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    let shares = te.pool.deposit(&te.lp, &5_000_000_000);
    assert_eq!(shares, 5_000_000_000);

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 15_000_000_000);
    assert_eq!(pos.deposit_count, 2);
}

#[test]
fn test_second_deposit_scales_by_share_price() {
    // Rewritten per #586: the previous body was byte-for-byte identical to
    // test_second_deposit_issues_proportional_shares (second deposit still at
    // share price 1.0). Here the second deposit happens AFTER a repayment has
    // raised the share price to 1.02 (10.2B deposits backing 10B shares), so
    // the returned share count must scale down precisely.
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    fund_and_repay_invoice(&te);

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_deposits, 10_200_000_000);
    assert_eq!(stats.total_shares, 10_000_000_000);

    // 5_000_000_000 * 10_000_000_000 / 10_200_000_000 = 4_901_960_784 (floored)
    let shares = te.pool.deposit(&te.lp, &5_000_000_000);
    assert_eq!(shares, 4_901_960_784);

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 14_901_960_784);
    assert_eq!(pos.deposit_count, 2);
}

#[test]
fn test_preview_deposit_matches_deposit_across_share_prices() {
    let amounts = [
        DEFAULT_MIN_INITIAL_DEPOSIT,
        DEFAULT_MIN_INITIAL_DEPOSIT * 2,
        5_000_000_000,
    ];

    // An empty pool mints one share per unit deposited.
    for amount in amounts {
        let te = setup();
        let preview = te.pool.preview_deposit(&amount);
        let issued = te.pool.deposit(&te.lp, &amount);
        assert_eq!(preview, issued);
        assert_eq!(preview, amount);
    }

    // A pool with a 1:1 share price preserves the same ratio for later deposits.
    for amount in amounts {
        let te = setup();
        te.pool.deposit(&te.lp, &10_000_000_000);
        let before = te.pool.get_stats();
        let preview = te.pool.preview_deposit(&amount);
        let after = te.pool.get_stats();
        assert_eq!(after.total_deposits, before.total_deposits);
        assert_eq!(after.total_shares, before.total_shares);
        let issued = te.pool.deposit(&te.lp, &amount);
        assert_eq!(preview, issued);
    }

    // Repayment yield raises the share price, so previews must use the same
    // floor rounding as a real deposit at that price.
    for amount in [2u128, 3, 1_000_000, 5_000_000_000] {
        let te = setup();
        te.pool.deposit(&te.lp, &10_000_000_000);
        fund_and_repay_invoice(&te);
        let preview = te.pool.preview_deposit(&amount);
        let issued = te.pool.deposit(&te.lp, &amount);
        assert_eq!(preview, issued);
    }
}

#[test]
fn test_preview_deposit_requires_no_authorization() {
    let te = setup();
    te.env.set_auths(&[]);

    assert_eq!(
        te.pool.preview_deposit(&DEFAULT_MIN_INITIAL_DEPOSIT),
        DEFAULT_MIN_INITIAL_DEPOSIT
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_preview_deposit_rejects_zero_amount() {
    let te = setup();
    te.pool.preview_deposit(&0);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_preview_deposit_rejects_dust_after_yield() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    fund_and_repay_invoice(&te);
    te.pool.preview_deposit(&1);
}

//  DUST ATTACK / 0-SHARE TESTS (issue #129)
// After the pool accrues yield the share price rises above 1.0. A deposit
// small enough that `usdc_amount * total_shares < total_deposits` would round
// down to 0 shares. Such a deposit must be rejected with `MinimumDeposit` (#14)
// rather than silently absorbing the depositor's funds.
#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_deposit_rejects_dust_when_zero_shares_after_yield() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    fund_and_repay_invoice(&te);

    // Share price is now 10.2B / 10B = 1.02
    let stats = te.pool.get_stats();
    assert_eq!(
        stats.total_deposits,
        DEFAULT_FACE_VALUE + DEFAULT_YIELD_AMOUNT
    );
    assert_eq!(stats.total_shares, 10_000_000_000);

    // 1 * 10B / 10.2B = 0 shares -> must be rejected, not absorbed.
    let lp2 = create_lp_with_balance(&te, 10_000_000_000);
    te.pool.deposit(&lp2, &1);
}

// A rejected dust deposit must not change pool accounting or move either the
// depositor's USDC or LP shares: the whole transaction reverts.
#[test]
fn test_zero_share_deposit_rejection_preserves_state_and_balances() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    fund_and_repay_invoice(&te);

    let lp2 = create_lp_with_balance(&te, 10_000_000_000);
    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let stats_before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&lp2);
    let lp_usdc_before = usdc.balance(&lp2);
    let pool_usdc_before = usdc.balance(&te.pool_id);

    // try_* returns Err on contract panic instead of unwinding the test.
    let res = te.pool.try_deposit(&lp2, &1);
    let error = std::format!("{res:?}");
    assert!(
        error.contains("#14"),
        "expected MinimumDeposit for a zero-share deposit: {error}"
    );

    // The failed call leaves aggregate accounting, LP shares, and both sides
    // of the token transfer exactly as they were.
    let stats_after = te.pool.get_stats();
    let position_after = te.pool.get_lp_position(&lp2);
    assert_eq!(stats_after.total_deposits, stats_before.total_deposits);
    assert_eq!(stats_after.total_shares, stats_before.total_shares);
    assert_eq!(position_after.shares, position_before.shares);
    assert_eq!(position_after.usdc_value, position_before.usdc_value);
    assert_eq!(position_after.yield_earned, position_before.yield_earned);
    assert_eq!(position_after.deposit_count, position_before.deposit_count);
    assert_eq!(usdc.balance(&lp2), lp_usdc_before);
    assert_eq!(usdc.balance(&te.pool_id), pool_usdc_before);

    assert_eq!(position_after.shares, 0);
}

// The guard must not over-reject: a small deposit that still mints >= 1 share at
// the elevated price succeeds normally.
#[test]
fn test_smallest_valid_deposit_after_yield_issues_at_least_one_share() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    fund_and_repay_invoice(&te);

    // Share price 1.02: 2 * 10B / 10.2B = 1 share (floored), the minimum > 0.
    let lp2 = create_lp_with_balance(&te, 10_000_000_000);
    let shares = te.pool.deposit(&lp2, &2);
    assert_eq!(shares, 1);

    let pos = te.pool.get_lp_position(&lp2);
    assert_eq!(pos.shares, 1);
    assert_eq!(pos.deposit_count, 1);
}

// Core acceptance guarantee: across a sweep of deposit sizes against a pool with
// an inflated share price, every deposit either mints >= 1 share or is rejected.
// No deposit is ever accepted for 0 shares.
#[test]
fn test_no_deposit_ever_receives_zero_shares() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    fund_and_repay_invoice(&te);
    // Share price is 1.02; amounts of 1 round to 0 shares, >= 2 round to >= 1.

    let amounts = [1u128, 2, 3, 5, 10, 102, 1_000, 1_000_000];
    for amount in amounts {
        let lp = create_lp_with_balance(&te, 100_000_000_000i128);
        match te.pool.try_deposit(&lp, &amount) {
            Ok(Ok(shares)) => {
                // Accepted deposits must always mint at least one share.
                assert!(shares >= 1, "amount {amount} accepted for 0 shares");
                let pos = te.pool.get_lp_position(&lp);
                assert_eq!(pos.shares, shares);
            }
            _ => {
                // Rejected deposits must leave the depositor with no shares.
                let pos = te.pool.get_lp_position(&lp);
                assert_eq!(pos.shares, 0, "amount {amount} rejected but minted shares");
            }
        }
    }
}

// The initial deposit in an empty pool must be at least DEFAULT_MIN_INITIAL_DEPOSIT (1 USDC)
// to prevent share-price griefing attacks.
#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_first_deposit_below_minimum_panics_invalid_amount() {
    let te = setup();
    te.pool.deposit(&te.lp, &(DEFAULT_MIN_INITIAL_DEPOSIT - 1));
}

#[test]
fn test_first_deposit_at_minimum_succeeds() {
    let te = setup();
    let shares = te.pool.deposit(&te.lp, &DEFAULT_MIN_INITIAL_DEPOSIT);
    assert_eq!(shares, DEFAULT_MIN_INITIAL_DEPOSIT);
}

#[test]
fn test_first_deposit_above_minimum_succeeds() {
    let te = setup();
    let shares = te
        .pool
        .deposit(&te.lp, &(DEFAULT_MIN_INITIAL_DEPOSIT + 10_000_000));
    assert_eq!(shares, DEFAULT_MIN_INITIAL_DEPOSIT + 10_000_000);
}

// Two independently initialized pool instances, each configured with its own
// `min_initial_deposit` at `initialize` time, must each enforce their own
// floor rather than sharing a single hardcoded constant (issue #744).
#[test]
fn test_two_pool_instances_enforce_their_own_configured_minimum() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let lp = Address::generate(&env);
    let registry_id = env.register(MockRegistry, ());

    // A low-decimals asset pool, configured with a small minimum.
    let asset_a = env.register(MockToken, ());
    let low_min: u128 = 100;
    let pool_a_id = build_pool_with_min_deposit(&env, &admin, &asset_a, &registry_id, low_min);
    let pool_a = PoolContractClient::new(&env, &pool_a_id);

    // A high-decimals asset pool, configured with the original USDC-scale minimum.
    let asset_b = env.register(MockToken, ());
    let high_min: u128 = DEFAULT_MIN_INITIAL_DEPOSIT;
    let pool_b_id = build_pool_with_min_deposit(&env, &admin, &asset_b, &registry_id, high_min);
    let pool_b = PoolContractClient::new(&env, &pool_b_id);

    fund_lp(&env, &asset_a, &lp);
    fund_lp(&env, &asset_b, &lp);

    // Pool A's low minimum accepts a deposit that pool B would reject.
    assert!(pool_a.try_deposit(&lp, &low_min).is_ok());

    // A second, fresh instance for asset_b so the below-minimum deposit
    // exercises the empty-pool branch of the check.
    let pool_b_fresh_id =
        build_pool_with_min_deposit(&env, &admin, &asset_b, &registry_id, high_min);
    let pool_b_fresh = PoolContractClient::new(&env, &pool_b_fresh_id);
    fund_lp(&env, &asset_b, &lp);
    assert!(pool_b_fresh.try_deposit(&lp, &low_min).is_err());
    assert!(pool_b.try_deposit(&lp, &high_min).is_ok());
}

/// Deploys a fresh constructed pool instance funding `asset`, configured
/// with `min_initial_deposit`.
fn build_pool_with_min_deposit(
    env: &Env,
    admin: &Address,
    asset: &Address,
    registry_id: &Address,
    min_initial_deposit: u128,
) -> Address {
    let invoice_id = env.register(RealInvoice, (admin.clone(), registry_id.clone()));

    // #827: pre-generate the pool address so the escrow wiring can point at
    // it before the pool instance exists; the constructor seeds the pool.
    let pool_id = Address::generate(env);
    let escrow_id = env.register(RealEscrow, (admin.clone(), pool_id.clone(), asset.clone()));

    env.register_at(
        &pool_id,
        PoolContract,
        (
            admin.clone(),
            invoice_id.clone(),
            escrow_id.clone(),
            asset.clone(),
            registry_id.clone(),
            admin.clone(),
            min_initial_deposit,
            String::from_str(env, TEST_SHARE_NAME),
            String::from_str(env, TEST_SHARE_SYMBOL),
            DEFAULT_SHARE_DECIMALS,
        ),
    );
    pool_id
}

/// Credits `lp` with a large token balance on `asset`'s `MockToken`.
fn fund_lp(env: &Env, asset: &Address, lp: &Address) {
    env.as_contract(asset, || {
        env.storage()
            .persistent()
            .set(&TKey(lp.clone()), &100_000_000_000_000i128);
    });
}

//  WITHDRAW TESTS
#[test]
fn test_withdraw_returns_correct_usdc() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    let usdc = te.pool.withdraw(&te.lp, &5_000_000_000);
    assert_eq!(usdc, 5_000_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_withdraw_fails_if_insufficient_liquidity() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);

    te.pool.withdraw(&te.lp, &300_000_000);
}

#[test]
fn test_withdraw_updates_initial_deposit_and_yield_on_multiple_partial_withdrawals() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let first_return = te.pool.withdraw(&te.lp, &5_000_000_000);
    assert_eq!(first_return, 5_000_000_000);

    let init_dep_key = DataKey::LPInitialDeposit(te.lp.clone());
    let remaining_init_dep: u128 = te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .persistent()
            .get(&init_dep_key)
            .unwrap_or(0)
    });
    assert_eq!(remaining_init_dep, 5_000_000_000);

    let second_return = te.pool.withdraw(&te.lp, &5_000_000_000);
    assert_eq!(second_return, 5_000_000_000);

    let final_init_dep: Option<u128> = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get(&init_dep_key)
    });
    assert!(final_init_dep.is_none());

    let lp_pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(lp_pos.shares, 0);
    assert_eq!(lp_pos.yield_earned, 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_withdraw_zero_shares_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    te.pool.withdraw(&te.lp, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_withdraw_more_than_owned_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    te.pool.withdraw(&te.lp, &20_000_000_000);
}

#[test]
fn test_preview_withdraw_matches_withdraw_without_yield() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    // The generated Soroban client takes scalar arguments by reference, even though the contract entry point uses u128 by value.
    // Preview is a public read and succeeds with auth disabled.
    te.env.set_auths(&[]);
    let preview = te.pool.preview_withdraw(&5_000_000_000);
    te.env.mock_all_auths_allowing_non_root_auth();
    let actual = te.pool.withdraw(&te.lp, &5_000_000_000);
    assert_eq!(preview, actual);
}

#[test]
fn test_preview_withdraw_matches_withdraw_after_yield() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    fund_and_repay_invoice(&te);

    let preview = te.pool.preview_withdraw(&5_000_000_000);
    let actual = te.pool.withdraw(&te.lp, &5_000_000_000);
    assert_eq!(preview, actual);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_preview_withdraw_rejects_zero_shares() {
    let te = setup();
    te.pool.preview_withdraw(&0);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_preview_withdraw_fails_if_insufficient_liquidity() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);

    te.pool.preview_withdraw(&300_000_000);
}
//  TRANSFER TESTS
#[test]
fn test_transfer_succeeds() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);
    te.pool.transfer_shares(&te.lp, &recipient, &5_000_000_000);

    let lp_position = te.pool.get_lp_position(&te.lp);
    assert_eq!(lp_position.shares, 5_000_000_000);

    let recipient_position = te.pool.get_lp_position(&recipient);
    assert_eq!(recipient_position.shares, 5_000_000_000);
}

#[test]
fn test_transfer_same_address_no_op() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let before = te.pool.get_lp_position(&te.lp);
    te.pool.transfer_shares(&te.lp, &te.lp, &5_000_000_000);
    let after = te.pool.get_lp_position(&te.lp);

    assert_eq!(before.shares, after.shares);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_transfer_zero_amount_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);
    te.pool.transfer_shares(&te.lp, &recipient, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #23)")]
fn test_transfer_insufficient_balance_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);
    te.pool.transfer_shares(&te.lp, &recipient, &20_000_000_000);
}

//  FUND INVOICE TESTS
#[test]
fn test_fund_invoice_dust_face_value_never_reaches_pool() {
    // Issue #824 moved this boundary upstream: `face_value = 1` used to be
    // creatable/listable and only died on the pool's zero-amount guard.
    // The invoice contract now rejects dust at `create` (InvalidFaceValue
    // #6), so a rounding-to-zero listing can never reach `fund_invoice`.
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let due_date = te.env.ledger().timestamp() + 86400;
    let result = te
        .invoice
        .try_create(&te.issuer, &te.buyer, &1u128, &due_date, &te.usdc_id);
    assert!(
        std::format!("{result:?}").contains("#6"),
        "dust face value must be rejected by invoice.create with InvalidFaceValue: {result:?}"
    );
    assert!(result.is_err());

    let before = te.pool.get_stats();
    let after = te.pool.get_stats();
    assert_eq!(after.total_funded, before.total_funded);
    assert_eq!(after.active_invoice_count, before.active_invoice_count);
    assert_eq!(after.available_liquidity, before.available_liquidity);
}

#[test]
fn test_fund_invoice_allows_min_face_value_at_max_discount() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    // MIN_FACE_VALUE is the new funding floor: at the maximum 5000 bps
    // discount it still yields funded_amount = MIN_FACE_VALUE / 2 = 50_000
    // stroops >= 1, so the pool's zero-amount guard cannot trip on a
    // legitimately created invoice (issue #824 boundary).
    let invoice_id =
        create_and_list_with_params(&te, &te.usdc_id, trusttrove_invoice::MIN_FACE_VALUE, 5000);

    let result = te.pool.fund_invoice(&invoice_id);
    assert!(result);

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, trusttrove_invoice::MIN_FACE_VALUE / 2);
    assert_eq!(stats.active_invoice_count, 1);
}

#[test]
fn test_fund_invoice_succeeds_for_normal_amount() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    let result = te.pool.fund_invoice(&invoice_id);
    assert!(result);

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(stats.active_invoice_count, 1);
}

#[test]
fn test_fund_invoice_reduces_available_liquidity() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    let before = te.pool.get_stats();
    let _ = te.pool.fund_invoice(&invoice_id);
    let after = te.pool.get_stats();

    assert_eq!(after.active_invoice_count, 1);
    assert!(after.total_funded > before.total_funded);
    assert!(after.available_liquidity < before.available_liquidity);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_fund_invoice_fails_when_insufficient_liquidity() {
    let te = setup();
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_fund_invoice_fails_asset_mismatch() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    // Create invoice with XLM asset, but pool handles USDC
    let invoice_id = create_and_list(&te, &te.xlm_id);
    te.pool.fund_invoice(&invoice_id);
}

//  REGISTRY REVOCATION RE-CHECK (registry+invoice+pool bug) //
// Design decision: registry revocation is prospective, not retroactive.
// `list_for_financing` and `fund_invoice` are the two points where new
// business is committed (an issuer lists, then the pool commits capital),
// so both re-verify the issuer and buyer against the registry. Once an
// invoice is actually Funded, its lifecycle (mark_shipped, confirm_delivery,
// repay, repay_early, trigger_default) proceeds regardless of any later
// revocation — the pool's capital is already committed and the repayment
// terms are already fixed, so unwinding an in-flight invoice on revocation
// would be disruptive and gameable (e.g. an issuer griefing LPs by getting
// itself revoked mid-term). See `test_revocation_after_funding_does_not_block_lifecycle`
// below for the documented in-flight behavior.

#[test]
#[should_panic(expected = "Error(Contract, #18)")]
fn test_fund_invoice_fails_when_issuer_revoked_after_listing() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    // Issuer was verified at create()/list_for_financing() time but is
    // revoked before the pool commits capital.
    te.registry.revoke(&te.issuer);

    te.pool.fund_invoice(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn test_fund_invoice_fails_when_buyer_revoked_after_listing() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    te.registry.revoke(&te.buyer);

    te.pool.fund_invoice(&invoice_id);
}

#[test]
fn test_revocation_after_funding_does_not_block_lifecycle() {
    // Mid-lifecycle revocation (post-Funded) must NOT retroactively affect
    // an in-flight invoice: shipment, delivery confirmation, and repayment
    // all proceed exactly as if the issuer/buyer were still verified. This
    // is the documented, deliberate behavior — revocation only gates new
    // commitments (list_for_financing, fund_invoice), not invoices already
    // funded.
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    let result = te.pool.fund_invoice(&invoice_id);
    assert!(
        result,
        "funding must succeed while both parties are verified"
    );

    // Revoke both issuer and buyer only after the pool has already
    // committed capital.
    te.registry.revoke(&te.issuer);
    te.registry.revoke(&te.buyer);
    assert!(!te.registry.is_verified(&te.issuer));
    assert!(!te.registry.is_verified(&te.buyer));

    // The rest of the lifecycle is unaffected by the revocation.
    assert!(te.invoice.mark_shipped(&invoice_id));
    assert!(te.invoice.confirm_delivery(&invoice_id, &te.issuer));
    assert!(te.invoice.confirm_delivery(&invoice_id, &te.buyer));

    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);
    assert!(te.invoice.repay(&invoice_id));

    let invoice = te.invoice.get(&invoice_id);
    assert_eq!(invoice.status, trusttrove_invoice::InvoiceStatus::Repaid);

    let stats = te.pool.get_stats();
    assert_eq!(stats.active_invoice_count, 0);
    assert_eq!(stats.total_funded, 0);
}

//  ISSUE #275: FUND INVOICE EDGE CASES
#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_fund_invoice_nonexistent_invoice_panics() {
    // Calling fund_invoice with a random invoice ID that doesn't exist
    // should propagate the NotFound (#2) error from the invoice contract's
    // get_status call.
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let fake_id = BytesN::from_array(&te.env, &[0u8; 32]);
    te.pool.fund_invoice(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_fund_invoice_unlisted_invoice_panics() {
    // An invoice in Created state (not yet listed) must be rejected by
    // fund_invoice with InvoiceNotListed (#8).
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let due_date = te.env.ledger().timestamp() + 86400;
    let invoice_id = te.invoice.create(
        &te.issuer,
        &te.buyer,
        &1_000_000_000,
        &due_date,
        &te.usdc_id,
    );
    // Do NOT list the invoice — status is Created (0)
    te.pool.fund_invoice(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_fund_invoice_already_funded_invoice_panics() {
    // After successfully funding an invoice, a second call to fund_invoice
    // must be rejected with AlreadyFunded (#16): the pool's own FundedInvoice
    // guard is evaluated first and is the authoritative record that this
    // invoice was already funded here. Refs: issue #444.
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    // Second funding attempt must be rejected by the pool's own guard.
    te.pool.fund_invoice(&invoice_id);
}

//  STATS TESTS
#[test]
fn test_get_stats_initial_state() {
    let te = setup();
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_deposits, 0);
    assert_eq!(stats.total_shares, 0);
    assert_eq!(stats.total_funded, 0);
    assert_eq!(stats.active_invoice_count, 0);
    assert_eq!(stats.available_liquidity, 0);
    assert_eq!(stats.utilization_rate_bps, 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_stats_panics_on_uninitialized_pool() {
    // A pool whose config keys are missing must not silently return
    // zero-filled stats — that would let callers mistake an uninitialized
    // pool for an empty-but-healthy one. Instead get_stats() must panic with
    // NotInitialized (#2). Post-#827 deploy is atomic, so the missing-config
    // state is reproduced by evicting the constructor's instance keys.
    let (_env, _pool_id, pool) = deploy_pool_with_evicted_config();
    let _ = pool.get_stats();
}

#[test]
fn test_get_stats_after_deposit() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_deposits, 100_000_000_000);
    assert_eq!(stats.total_shares, 100_000_000_000);
    assert_eq!(stats.available_liquidity, 100_000_000_000);
    assert_eq!(stats.utilization_rate_bps, 0);
}

#[test]
fn test_get_stats_after_funding() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);

    let stats = te.pool.get_stats();
    assert!(stats.total_funded > 0);
    assert!(stats.available_liquidity < 100_000_000_000);
    assert_eq!(stats.active_invoice_count, 1);
    assert!(stats.utilization_rate_bps > 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_get_stats_rejects_utilization_overflow() {
    let te = setup();
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &u128::MAX);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalFunded, &(u128::MAX / 10_000 + 1));
    });

    let _ = te.pool.get_stats();
}

/// Detect TotalFunded > TotalDeposits invariant violation in get_stats.
///
/// The invariant `TotalFunded <= TotalDeposits` must hold for a healthy pool.
/// If corrupt storage (or a logic bug) makes TotalFunded exceed TotalDeposits,
/// `get_stats` must fail with a typed `Overflow` error rather than
/// underflowing on `available = total_deposits - total_funded` in a public
/// read path. The underlying storage must not be modified by the read.
#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_get_stats_rejects_total_funded_exceeds_deposits() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000u128);
    let stats_before = te.pool.get_stats();
    assert_eq!(stats_before.total_deposits, 100_000_000_000u128);
    assert_eq!(stats_before.total_funded, 0u128);

    // Corrupt storage: make TotalFunded > TotalDeposits
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalFunded, &(100_000_000_001u128));
    });

    // get_stats must panic with Overflow (#13) instead of underflowing
    let _ = te.pool.get_stats();

    // Storage must be unchanged by the read (no writes in get_stats)
    let stats_after = te.pool.get_stats();
    assert_eq!(stats_after.total_deposits, stats_before.total_deposits);
    assert_eq!(stats_after.total_funded, 100_000_000_001u128);
}

//  SHARE-PRICE OVERFLOW TESTS (issue #584) //
// The share-price multiplications in deposit/withdraw/get_lp_position must
// panic with the typed PoolError::Overflow (#13) instead of a raw Rust
// arithmetic-overflow abort (the workspace release profile sets
// overflow-checks = true), matching utilization_bps_or_panic. The boundary is
// driven by injecting u128-scaled storage values — the same technique the
// get_stats/get_utilization_rate/fund_invoice overflow tests use.

// `usdc_amount * total_shares` overflows u128 when total_shares is inflated
// to u128::MAX, so deposit must panic with Overflow (#13). The check runs
// before the token transfer, so the depositor's USDC is never pulled.
#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_deposit_rejects_share_price_overflow() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalShares, &u128::MAX);
    });

    te.pool.deposit(&te.lp, &10_000_000_000);
}

// Boundary complement: the largest representable product (2 * (u128::MAX / 2)
// == u128::MAX - 1) must still be accepted — checked_mul must not over-reject
// legal share-price math.
#[test]
fn test_deposit_accepts_multiplication_at_overflow_boundary() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalShares, &(u128::MAX / 2));
    });

    let shares = te.pool.deposit(&te.lp, &2);
    assert_eq!(shares, 2 * (u128::MAX / 2) / 10_000_000_000);
}

// `shares * total_deposits` overflows u128 when total_deposits is inflated to
// u128::MAX, so withdraw must panic with Overflow (#13) before transferring
// or burning anything.
#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_withdraw_rejects_share_price_overflow() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &u128::MAX);
    });

    te.pool.withdraw(&te.lp, &10_000_000_000);
}

// `lp_shares * total_deposits` overflows u128 when total_deposits is inflated
// to u128::MAX, so get_lp_position must panic with Overflow (#13) rather than
// report a wrapped-around position value.
#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_get_lp_position_rejects_share_price_overflow() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &u128::MAX);
    });

    let _ = te.pool.get_lp_position(&te.lp);
}

// Regression test for the public view boundary: a corrupted pool state that
// pushes `lp_shares * total_deposits` past `u128::MAX` must fail with the
// typed `PoolError::Overflow` (#13), not with a wrapped-around position value.
//
// Unlike the test above (which inflates only `TotalDeposits` on top of a real
// deposit), this one seeds total shares, total deposits and the LP's own share
// balance explicitly, so the multiplication overflows purely from the seeded
// state. It also checks that the failed read leaves the pool state untouched:
// `get_lp_position` is a view, so an overflow must not corrupt anything.
#[test]
fn test_get_lp_position_seeded_overflow_returns_typed_error() {
    let te = setup();

    // lp_shares * total_deposits = u128::MAX * u128::MAX overflows u128.
    let lp_shares: u128 = u128::MAX;
    let total_shares: u128 = u128::MAX;
    let total_deposits: u128 = u128::MAX;

    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .persistent()
            .set(&DataKey::LPShares(te.lp.clone()), &lp_shares);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalShares, &total_shares);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &total_deposits);
    });

    // The view must fail with the typed overflow error.
    match te.pool.try_get_lp_position(&te.lp) {
        Err(Ok(err)) => assert_eq!(
            err,
            soroban_sdk::Error::from(PoolError::Overflow),
            "get_lp_position must fail with the typed Overflow error"
        ),
        _ => panic!("get_lp_position must fail with the typed Overflow error"),
    }

    // The failed read must not have changed any state.
    let shares_after: u128 = te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .persistent()
            .get(&DataKey::LPShares(te.lp.clone()))
            .unwrap()
    });
    assert_eq!(shares_after, lp_shares);
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_shares, total_shares);
    assert_eq!(stats.total_deposits, total_deposits);
}

//  LP POSITION TESTS
#[test]
fn test_lp_position_empty() {
    let te = setup();
    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 0);
    assert_eq!(pos.usdc_value, 0);
    assert_eq!(pos.yield_earned, 0);
    assert_eq!(pos.deposit_count, 0);
}

#[test]
fn test_lp_position_after_deposit() {
    let te = setup();
    te.pool.deposit(&te.lp, &50_000_000_000);
    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 50_000_000_000);
    assert_eq!(pos.usdc_value, 50_000_000_000);
    assert_eq!(pos.deposit_count, 1);
}

//  UTILIZATION RATE TESTS
#[test]
fn test_utilization_rate_zero_when_no_deposits() {
    let te = setup();
    assert_eq!(te.pool.get_utilization_rate(), 0);
}

#[test]
fn test_utilization_rate_zero_when_no_funding() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    assert_eq!(te.pool.get_utilization_rate(), 0);
}

#[test]
fn test_utilization_rate_after_funding() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);
    let rate = te.pool.get_utilization_rate();
    assert!(rate > 0);
    assert!(rate < 10000);
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_get_utilization_rate_rejects_overflow() {
    let te = setup();
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &u128::MAX);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalFunded, &(u128::MAX / 10_000 + 1));
    });

    let _ = te.pool.get_utilization_rate();
}

#[test]
fn test_utilization_rate_calculates_correctly() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    // Raise cap to 100% so funding doesn't get rejected
    te.pool.set_max_utilization(&te.admin, &10000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);
    assert_eq!(
        te.pool.get_utilization_rate(),
        (DEFAULT_FUNDED_AMOUNT * 10000 / 10_000_000_000) as u32
    );
}

//  MAX UTILIZATION TESTS
#[test]
fn test_default_max_utilization_in_stats() {
    // Fresh pool without setup override to verify the constructor default is 8500
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let registry_id = env.register(MockRegistry, ());
    let usdc_id = env.register(MockToken, ());
    let pool_id = Address::generate(&env);
    let invoice_id = env.register(RealInvoice, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        RealEscrow,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );
    env.register_at(
        &pool_id,
        PoolContract,
        (
            admin.clone(),
            invoice_id.clone(),
            escrow_id.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            admin.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            String::from_str(&env, TEST_SHARE_NAME),
            String::from_str(&env, TEST_SHARE_SYMBOL),
            DEFAULT_SHARE_DECIMALS,
        ),
    );
    let pool = PoolContractClient::new(&env, &pool_id);
    let stats = pool.get_stats();
    assert_eq!(stats.max_utilization_bps, 8500);
}

#[test]
fn test_updated_max_utilization_reflected_in_stats() {
    let te = setup();
    te.pool.set_max_utilization(&te.admin, &9000);
    let stats = te.pool.get_stats();
    assert_eq!(stats.max_utilization_bps, 9000);
}

// set_max_utilization must emit a max_utilization_updated event carrying the
// old and new caps so off-chain indexers can observe risk-parameter changes
// without polling get_stats() (issue #582).
#[test]
fn test_set_max_utilization_emits_event() {
    let te = setup();
    // setup() already raised the cap to 10000, so the old cap here is 10000.
    te.pool.set_max_utilization(&te.admin, &8500);

    let events = te.env.events().all();
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(topics.len(), 1);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "max_utilization_updated")
    );
    assert_eq!(
        <(u32, u32)>::try_from_val(&te.env, &data).unwrap(),
        (10000, 8500)
    );

    // A later successful change reports the just-replaced value as its old
    // cap, preserving a complete transition history for event consumers.
    te.pool.set_max_utilization(&te.admin, &9000);
    let events = te.env.events().all();
    let (_, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(topics.len(), 1);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "max_utilization_updated")
    );
    assert_eq!(
        <(u32, u32)>::try_from_val(&te.env, &data).unwrap(),
        (8500, 9000)
    );

    // The storage update itself is still reflected in get_stats.
    assert_eq!(te.pool.get_stats().max_utilization_bps, 9000);

    // A rejected update (> 10000) must not emit the event.
    assert!(te.pool.try_set_max_utilization(&te.admin, &10001).is_err());
    let events_after = te.env.events().all();
    // sdk >=22 buffers hold only the failed call's own events: none.
    assert_eq!(events_after.len(), 0);
}

//  GET_USDC_ASSET TESTS
#[test]
fn test_get_usdc_asset_returns_configured_asset() {
    let te = setup();
    assert_eq!(te.pool.get_usdc_asset(), te.usdc_id);
}

// get_funding_asset is the new, asset-generic name; get_usdc_asset (above) is
// kept only as a deprecated alias for pre-factory integrators, and both must
// return the same value (issue #743).
#[test]
fn test_get_funding_asset_returns_configured_asset() {
    let te = setup();
    assert_eq!(te.pool.get_funding_asset(), te.usdc_id);
    assert_eq!(te.pool.get_funding_asset(), te.pool.get_usdc_asset());
}

// get_usdc_asset delegates to Self::funding_asset(), whose instance read is
// guarded by a typed `PoolError::NotInitialized` panic rather than a bare
// `expect` string. This test pins the typed error when the pool has never been
// initialized (issue #591 originally documented the untyped panic; issue #441
// replaced it with the typed variant).
#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_usdc_asset_panics_when_uninitialized() {
    let (_env, _pool_id, pool) = deploy_pool_with_evicted_config();
    pool.get_usdc_asset();
}

// The canonical name must carry the same typed pre-initialization contract as
// the deprecated alias (issue #954).
#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_funding_asset_panics_when_uninitialized() {
    let (_env, _pool_id, pool) = deploy_pool_with_evicted_config();
    pool.get_funding_asset();
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_fund_invoice_rejects_utilization_overflow() {
    let te = setup();
    let invoice_id = create_and_list(&te, &te.usdc_id);
    // Set both TotalDeposits and TotalFunded near u128::MAX so that
    // `available = total_deposits - total_funded` does not underflow,
    // but `new_total_funded * 10_000` overflows in the utilization check.
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &u128::MAX);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalFunded, &(u128::MAX / 10_000 + 1));
    });

    let _ = te.pool.fund_invoice(&invoice_id);
}

#[test]
fn test_fund_invoice_rejects_discount_above_max_before_escrow_or_state_update() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    let invoice_id = BytesN::from_array(&te.env, &[8u8; 32]);
    let invoice_contract = te.env.register(MockHugeFaceInvoice, ());
    MockHugeFaceInvoiceClient::new(&te.env, &invoice_contract).configure(
        &te.issuer,
        &te.buyer,
        &te.usdc_id,
        &10_000_000_000,
        &10_001,
    );
    let escrow_id = te.env.register(MockUnexpectedEscrow, ());
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::InvoiceContract, &invoice_contract);
        te.env
            .storage()
            .instance()
            .set(&DataKey::EscrowContract, &escrow_id);
    });

    let stats_before = te.pool.get_stats();
    let result = te.pool.try_fund_invoice(&invoice_id);
    assert!(result.is_err());
    let error = std::format!("{result:?}");
    assert!(error.contains("#4"), "expected InvalidAmount: {error}");
    assert!(
        !error.contains("#99"),
        "escrow lock must not be called: {error}"
    );

    let stats_after = te.pool.get_stats();
    assert_eq!(stats_after.total_funded, stats_before.total_funded);
    assert_eq!(
        stats_after.active_invoice_count,
        stats_before.active_invoice_count
    );
    assert_eq!(
        stats_after.available_liquidity,
        stats_before.available_liquidity
    );
    assert_eq!(te.pool.get_funded_amount(&invoice_id), None);
}
// A pathologically large face_value read from the invoice contract (the pool
// does not bound cross-contract values itself) must trigger the typed
// PoolError::Overflow (#13) before pool or escrow state changes.
#[test]
fn test_fund_invoice_rejects_funded_amount_overflow_without_state_changes() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    // Stand-in invoice contract reporting a face_value that overflows u128
    // when scaled by (10000 - discount_bps). The issuer/buyer it reports are
    // the ones already verified in the pool's registry, so the overflow is
    // the first failure the funding path hits.
    let mock_id = te.env.register(MockHugeFaceInvoice, ());
    MockHugeFaceInvoiceClient::new(&te.env, &mock_id).configure(
        &te.issuer,
        &te.buyer,
        &te.usdc_id,
        &u128::MAX,
        &0,
    );

    // Point the pool's configured invoice contract at the stand-in — same
    // storage-injection technique as the other overflow tests in this file.
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::InvoiceContract, &mock_id);
    });

    let invoice_id = BytesN::from_array(&te.env, &[7u8; 32]);
    let escrow = RealEscrowClient::new(&te.env, &te.escrow_id);
    let stats_before = te.pool.get_stats();
    let result = te.pool.try_fund_invoice(&invoice_id);
    assert!(
        result.is_err(),
        "overflowing funded amount must be rejected"
    );
    let error = std::format!("{result:?}");
    assert!(
        error.contains("#13"),
        "expected PoolError::Overflow: {error}"
    );

    let stats_after = te.pool.get_stats();
    assert_eq!(stats_after.total_deposits, stats_before.total_deposits);
    assert_eq!(stats_after.total_funded, stats_before.total_funded);
    assert_eq!(
        stats_after.available_liquidity,
        stats_before.available_liquidity
    );
    assert_eq!(
        stats_after.utilization_rate_bps,
        stats_before.utilization_rate_bps
    );
    assert_eq!(
        stats_after.total_yield_distributed,
        stats_before.total_yield_distributed
    );
    assert_eq!(
        stats_after.total_loss_realised,
        stats_before.total_loss_realised
    );
    assert_eq!(
        stats_after.active_invoice_count,
        stats_before.active_invoice_count
    );
    assert_eq!(stats_after.total_shares, stats_before.total_shares);
    assert_eq!(
        stats_after.max_utilization_bps,
        stats_before.max_utilization_bps
    );
    assert_eq!(te.pool.get_funded_amount(&invoice_id), None);
    assert_eq!(escrow.get_locked(&invoice_id), 0);
    assert_eq!(escrow.get_locked_at(&invoice_id), 0);
    assert!(escrow.get_history(&invoice_id).is_empty());
}
#[test]
fn test_fund_invoice_succeeds_at_exact_utilization_cap() {
    let te = setup();
    let exact_cap_bps = 9_800;
    te.pool.set_max_utilization(&te.admin, &exact_cap_bps);
    te.pool.deposit(&te.lp, &DEFAULT_FACE_VALUE);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    assert!(te.pool.fund_invoice(&invoice_id));

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(stats.utilization_rate_bps, exact_cap_bps);
    assert_eq!(stats.max_utilization_bps, exact_cap_bps);
    assert!(stats.utilization_rate_bps <= stats.max_utilization_bps);
}
#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_fund_invoice_rejects_above_cap() {
    let te = setup();
    // Restore cap to 8500; funding at 9800 utilization should fail
    te.pool.set_max_utilization(&te.admin, &8500);
    te.pool.deposit(&te.lp, &10_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_fund_invoice_rejects_when_utilization_cap_is_zero() {
    let te = setup();
    te.pool.set_max_utilization(&te.admin, &0);
    te.pool.deposit(&te.lp, &10_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
}

#[test]
fn test_fund_invoice_allowed_when_below_cap() {
    let te = setup();
    te.pool.set_max_utilization(&te.admin, &10000);
    te.pool.deposit(&te.lp, &10_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let result = te.pool.fund_invoice(&invoice_id);
    assert!(result);
}

#[test]
fn test_fund_invoice_is_permissionless() {
    // Verify that fund_invoice can be called by any address without admin authorization.
    // Setup normally (with mock_all_auths) so initialization succeeds, then test with no auths.
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    // Clear all mocked auths so that any require_auth() call would fail.
    // If admin.require_auth() was still in the code, this would panic.
    te.env.set_auths(&[]);
    let result = te.pool.fund_invoice(&invoice_id);
    assert!(
        result,
        "fund_invoice should succeed without any mocked auths (no admin auth required)"
    );

    // Verify the invoice was actually funded
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(stats.active_invoice_count, 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_set_max_utilization_above_10000_panics() {
    let te = setup();
    te.pool.set_max_utilization(&te.admin, &10001);
}

// set_max_utilization must reject callers other than the admin (#581). The
// admin-authorized path is already covered by
// test_updated_max_utilization_reflected_in_stats and
// test_set_max_utilization_emits_event.
#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_set_max_utilization_requires_admin_authorization() {
    let te = setup();
    let non_admin = Address::generate(&te.env);

    // Clear all mocked auths so the non-admin's require_auth() fails.
    te.env.set_auths(&[]);
    te.pool.set_max_utilization(&non_admin, &9000);
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_reducing_cap_mid_lifecycle_blocks_new_funding() {
    let te = setup();
    te.pool.set_max_utilization(&te.admin, &8500);
    te.pool.deposit(&te.lp, &100_000_000_000);
    // First funding: 9_800_000_000 / 100_000_000_000 = 980 bps < 8500 → ok
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // Lower cap below the utilization a second funding would cause
    // (980 bps already used; adding another 9.8B → 1960 bps)
    te.pool.set_max_utilization(&te.admin, &1000);
    // Second funding should push utilization to 1960 bps > 1000 → rejected
    let invoice_id2 = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id2);
}

#[test]
fn test_yield_increases_share_price_after_repayment() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    fund_and_repay_invoice(&te);

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 10_000_000_000);
    assert_eq!(pos.usdc_value, DEFAULT_FACE_VALUE + DEFAULT_YIELD_AMOUNT);
}

#[test]
fn test_two_lps_receive_proportional_yield() {
    let te = setup();
    let lp2 = create_lp_with_balance(&te, 100_000_000_000_000i128);

    te.pool.deposit(&te.lp, &10_000_000_000);
    te.pool.deposit(&lp2, &30_000_000_000);
    fund_and_repay_invoice(&te);

    let pos1 = te.pool.get_lp_position(&te.lp);
    let pos2 = te.pool.get_lp_position(&lp2);

    assert_eq!(pos1.shares, 10_000_000_000);
    assert_eq!(pos2.shares, 30_000_000_000);
    // With proportional yield distribution: LP1 gets 25% (10B/40B) of yield
    assert_eq!(
        pos1.usdc_value,
        10_000_000_000 + DEFAULT_YIELD_AMOUNT * 10_000_000_000 / (10_000_000_000 + 30_000_000_000)
    );
    // LP2 gets 75% (30B/40B) of yield
    assert_eq!(
        pos2.usdc_value,
        30_000_000_000 + DEFAULT_YIELD_AMOUNT * 30_000_000_000 / (10_000_000_000 + 30_000_000_000)
    );
}

#[test]
fn test_lp_position_reflects_current_share_price() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);

    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);
    te.invoice.repay(&invoice_id);

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.usdc_value, DEFAULT_FACE_VALUE + DEFAULT_YIELD_AMOUNT);
    assert_eq!(pos.shares, 10_000_000_000);
}

// Reproduces #630: repay_early() was previously only exercised against
// MockPool in the invoice crate's own tests, never against the real
// escrow+pool setup wired up here. This drives invoice.repay_early()
// end-to-end (buyer -> escrow -> pool) partway through the term and asserts
// the pool-side accounting effects (yield split into total_deposits /
// total_yield_distributed) and the buyer's discount refund match the
// elapsed/term-proportional split repay_early computes internally.
#[test]
fn test_repay_early_against_real_pool_and_escrow() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);

    // face_value=10_000_000_000, discount_bps=200 (2%)
    // funded_amount = 10_000_000_000 * 9800 / 10000 = 9_800_000_000
    // discount = 200_000_000; term = 86400s (due_date - funded_at)
    let face_value: u128 = 10_000_000_000;
    let discount: u128 = 200_000_000;
    let term: u64 = 86400;

    // Repay halfway through the term.
    let elapsed: u64 = term / 2;
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + elapsed);

    let earned_by_pool = discount * (elapsed as u128) / (term as u128);
    let refund_to_buyer = discount - earned_by_pool;

    let stats_before = te.pool.get_stats();
    let buyer_balance_before = MockTokenClient::new(&te.env, &te.usdc_id).balance(&te.buyer);

    let result = te.invoice.repay_early(&invoice_id);
    assert!(result);

    let stats_after = te.pool.get_stats();
    assert_eq!(
        stats_after.total_deposits,
        stats_before.total_deposits + earned_by_pool
    );
    assert_eq!(
        stats_after.total_yield_distributed,
        stats_before.total_yield_distributed + earned_by_pool
    );
    assert_eq!(stats_after.total_funded, 0);
    assert_eq!(stats_after.active_invoice_count, 0);

    let buyer_balance_after = MockTokenClient::new(&te.env, &te.usdc_id).balance(&te.buyer);
    assert_eq!(
        buyer_balance_after,
        buyer_balance_before - (face_value as i128) + (refund_to_buyer as i128)
    );

    // Escrow's lock record must be gone after release_to_pool.
    let escrow_client = RealEscrowClient::new(&te.env, &te.escrow_id);
    assert_eq!(escrow_client.get_locked(&invoice_id), 0);

    assert_eq!(te.invoice.get_status(&invoice_id), 5); // Repaid
}

// Issue #876: nonzero protocol fees must also flow through the real early
// repayment path, including the subsequent LP withdrawal.
#[test]
fn test_nonzero_protocol_fee_with_real_early_repayment_and_withdrawal() {
    let te = setup();
    let deposit = 100_000_000_000u128;
    te.pool.deposit(&te.lp, &deposit);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let treasury = Address::generate(&te.env);
    let fee_bps = 1_000u32;
    te.pool.set_protocol_fee(&fee_bps, &treasury);
    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);

    let elapsed = 43_200u64;
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + elapsed);
    let earned = DEFAULT_YIELD_AMOUNT * elapsed as u128 / 86_400;
    let fee = earned * fee_bps as u128 / 10_000;
    let lp_yield = earned - fee;
    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let treasury_before = usdc.balance(&treasury);

    assert!(te.invoice.repay_early(&invoice_id));
    assert_eq!(usdc.balance(&treasury) - treasury_before, fee as i128);
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_yield_distributed, lp_yield);
    assert_eq!(stats.total_deposits, deposit + lp_yield);
    assert_eq!(stats.total_funded, 0);
    assert_eq!(stats.active_invoice_count, 0);

    let position = te.pool.get_lp_position(&te.lp);
    let before_withdraw = usdc.balance(&te.lp);
    let returned = te.pool.withdraw(&te.lp, &position.shares);
    assert_eq!(returned, deposit + lp_yield);
    assert_eq!(usdc.balance(&te.lp) - before_withdraw, returned as i128);
    assert_eq!(te.pool.get_stats().total_deposits, 0);
    assert_eq!(te.invoice.get_status(&invoice_id), 5);
}

// Issue #878: interleave several real invoices while checking shared pool
// counters and the price used to admit a late LP after yield and loss.
#[test]
fn test_multi_invoice_mixed_repayment_default_and_late_depositor() {
    let te = setup();
    let lp2 = Address::generate(&te.env);
    let lp3 = Address::generate(&te.env);
    let lp2_key = TKey(lp2.clone());
    let lp3_key = TKey(lp3.clone());
    for (lp, key) in [(&lp2, &lp2_key), (&lp3, &lp3_key)] {
        te.env.as_contract(&te.usdc_id, || {
            te.env
                .storage()
                .persistent()
                .set(key, &100_000_000_000_000i128);
        });
        te.registry.register(lp);
    }
    te.pool.set_max_utilization(&te.admin, &6_000);
    te.pool.deposit(&te.lp, &40_000_000_000);
    te.pool.deposit(&lp2, &20_000_000_000);

    let repaid = create_and_list(&te, &te.usdc_id);
    let defaulted = create_and_list(&te, &te.usdc_id);
    let active = create_and_list(&te, &te.usdc_id);
    for id in [&repaid, &defaulted, &active] {
        te.pool.fund_invoice(id);
        let stats = te.pool.get_stats();
        assert_eq!(
            stats.available_liquidity,
            stats.total_deposits - stats.total_funded
        );
        assert!(stats.utilization_rate_bps <= stats.max_utilization_bps);
    }
    assert_eq!(te.pool.get_stats().active_invoice_count, 3);
    assert_eq!(te.pool.get_stats().total_funded, DEFAULT_FUNDED_AMOUNT * 3);

    te.invoice.mark_shipped(&repaid);
    te.invoice.confirm_delivery(&repaid, &te.issuer);
    te.invoice.confirm_delivery(&repaid, &te.buyer);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 43_200);
    assert!(te.invoice.repay_early(&repaid));
    let earned = DEFAULT_YIELD_AMOUNT / 2;
    let after_repay = te.pool.get_stats();
    assert_eq!(after_repay.total_yield_distributed, earned);
    assert_eq!(after_repay.active_invoice_count, 2);
    assert_eq!(
        after_repay.available_liquidity,
        after_repay.total_deposits - after_repay.total_funded
    );

    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 43_201);
    assert!(te.pool.handle_default(&defaulted));
    assert_eq!(te.invoice.get_status(&defaulted), 6);
    let after_default = te.pool.get_stats();
    assert_eq!(after_default.total_loss_realised, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(after_default.active_invoice_count, 1);
    assert_eq!(
        after_default.available_liquidity,
        after_default.total_deposits - after_default.total_funded
    );
    assert_eq!(after_default.total_yield_distributed, earned);

    let lp1_before = te.pool.get_lp_position(&te.lp);
    let lp2_before = te.pool.get_lp_position(&lp2);
    let before_late_deposit = te.pool.get_stats();
    te.pool.deposit(&lp3, &10_000_000_000);
    let lp3_position = te.pool.get_lp_position(&lp3);
    assert_eq!(
        lp3_position.shares,
        10_000_000_000 * before_late_deposit.total_shares / before_late_deposit.total_deposits
    );
    assert_eq!(lp3_position.yield_earned, 0);
    assert!(lp3_position.usdc_value <= 10_000_000_000);
    assert!(10_000_000_000 - lp3_position.usdc_value <= 1);
    assert_eq!(te.pool.get_lp_position(&te.lp).shares, lp1_before.shares);
    assert_eq!(te.pool.get_lp_position(&lp2).shares, lp2_before.shares);

    for (index, lp) in [&te.lp, &lp2, &lp3].iter().enumerate() {
        let position = te.pool.get_lp_position(lp);
        if position.shares > 0 {
            let stats = te.pool.get_stats();
            let position_value = position.shares * stats.total_deposits / stats.total_shares;
            let remaining_lps = (3 - index) as u128;
            let withdrawal_value =
                core::cmp::min(position_value, stats.available_liquidity / remaining_lps);
            let withdrawal_shares = withdrawal_value * stats.total_shares / stats.total_deposits;
            if withdrawal_shares > 0 {
                te.pool.withdraw(lp, &withdrawal_shares);
                let after = te.pool.get_stats();
                assert_eq!(
                    after.available_liquidity,
                    after.total_deposits - after.total_funded
                );
                assert_eq!(after.total_yield_distributed, earned);
                assert_eq!(after.total_loss_realised, DEFAULT_FUNDED_AMOUNT);
                assert_eq!(after.active_invoice_count, 1);
            }
        }
    }
    let final_stats = te.pool.get_stats();
    assert_eq!(
        final_stats.available_liquidity,
        final_stats.total_deposits - final_stats.total_funded
    );
    assert_eq!(final_stats.active_invoice_count, 1);
    assert_eq!(final_stats.total_yield_distributed, earned);
    assert_eq!(final_stats.total_loss_realised, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(te.invoice.get_status(&active), 2);
}
//  MULTI-LP TESTS
#[test]
fn test_multiple_lps_can_deposit() {
    let te = setup();
    let lp2 = Address::generate(&te.env);
    let lp2_bal_key = TKey(lp2.clone());
    te.env.as_contract(&te.usdc_id, || {
        te.env
            .storage()
            .persistent()
            .set(&lp2_bal_key, &100_000_000_000_000i128);
    });

    let s1 = te.pool.deposit(&te.lp, &10_000_000_000);
    let s2 = te.pool.deposit(&lp2, &20_000_000_000);

    assert_eq!(s1, 10_000_000_000);
    assert_eq!(s2, 20_000_000_000);

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_shares, 30_000_000_000);
    assert_eq!(stats.total_deposits, 30_000_000_000);
}

//  REPAYMENT TESTS
#[test]
fn test_receive_repayment() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // face_value=10_000_000_000, discount_bps=200
    // funded_amount = 10_000_000_000 * (10000 - 200) / 10000 = 9_800_000_000
    let yield_amount = DEFAULT_YIELD_AMOUNT;

    let before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&te.lp);
    let result = te.pool.receive_repayment(&invoice_id, &10_000_000_000);
    assert!(result);
    // Capture before further queries: sdk >=22 test Envs reset the event
    // buffer at every top-level invocation.
    let events = te.env.events().all();

    let after = te.pool.get_stats();
    let position_after = te.pool.get_lp_position(&te.lp);
    assert_eq!(after.total_deposits, before.total_deposits + yield_amount);
    assert_eq!(after.total_yield_distributed, yield_amount);
    assert_eq!(after.total_funded, 0);
    assert_eq!(after.active_invoice_count, 0);
    assert_eq!(
        position_after.usdc_value,
        position_before.usdc_value + yield_amount
    );

    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "repayment_received")
    );
    assert_eq!(
        BytesN::<32>::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        invoice_id
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (10_000_000_000, yield_amount, 0)
    );
}

#[test]
fn test_receive_repayment_exact_funded_amount_has_no_yield() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&te.lp);
    te.pool
        .receive_repayment(&invoice_id, &DEFAULT_FUNDED_AMOUNT);

    let after = te.pool.get_stats();
    let position_after = te.pool.get_lp_position(&te.lp);
    assert_eq!(after.total_deposits, before.total_deposits);
    assert_eq!(after.total_yield_distributed, 0);
    assert_eq!(after.total_funded, 0);
    assert_eq!(position_after.usdc_value, position_before.usdc_value);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_receive_repayment_requires_invoice_contract_authorization() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    te.env.set_auths(&[]);
    te.pool.receive_repayment(&invoice_id, &10_000_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_receive_repayment_panics_when_amount_below_funded() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // funded_amount = 9_800_000_000, sending less should panic (#4 = InvalidAmount)
    te.pool.receive_repayment(&invoice_id, &1_000_000_000);
}

// pool.receive_repayment_with_refund trusts whatever discount/refund split
// invoice_contract passes in: pool has no visibility into funded_at/due_date
// and never checks that `refund` is proportional to elapsed time. It only
// bounds `refund` to [0, amount - funded_amount]. This test demonstrates
// that behavior directly: called immediately after funding (elapsed = 0,
// so a time-proportional split would refund ~the full discount to the
// buyer and credit the pool ~nothing), an artificially inconsistent split
// that instead credits the pool the *entire* discount as yield (refund = 0)
// is accepted unconditionally, purely because it falls within the amount
// bound. Reconciling this split against invoice's actual elapsed/term is
// invoice_contract's responsibility, not pool's — see the "Trust boundary"
// note on `receive_repayment_with_refund`'s rustdoc.
#[test]
fn test_receive_repayment_with_refund_accepts_time_inconsistent_split() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // No time has elapsed since funding — due_date is still a full 86400s
    // away and `term` has barely started. A time-proportional split would
    // refund nearly the entire discount to the buyer. Instead, pass a split
    // that hands the pool the entire discount immediately (refund = 0).
    let full_repayment = DEFAULT_FACE_VALUE;
    let inconsistent_refund = 0u128;

    let before = te.pool.get_stats();
    let buyer_usdc_before = MockTokenClient::new(&te.env, &te.usdc_id).balance(&te.buyer);

    let result = te.pool.receive_repayment_with_refund(
        &invoice_id,
        &full_repayment,
        &inconsistent_refund,
        &te.buyer,
    );
    assert!(result);

    // Pool accepted the split unconditionally: the full discount (which a
    // time-proportional split would have mostly refunded to the buyer at
    // elapsed = 0) was instead distributed as LP yield, and the buyer
    // received no refund at all. Pool performed no elapsed/term check.
    let after = te.pool.get_stats();
    assert_eq!(
        after.total_yield_distributed,
        before.total_yield_distributed + DEFAULT_YIELD_AMOUNT
    );
    assert_eq!(after.total_funded, 0);
    let buyer_usdc_after = MockTokenClient::new(&te.env, &te.usdc_id).balance(&te.buyer);
    assert_eq!(buyer_usdc_after, buyer_usdc_before);
}

// Happy path for receive_repayment_with_refund (#583): the buyer receives
// exactly `refund` via the USDC transfer, only the remaining yield slice is
// credited to the pool, and the repayment_received event carries
// (amount, lp_yield, protocol_cut).
#[test]
fn test_receive_repayment_with_refund_happy_path() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // face_value=10_000_000_000, discount_bps=200 → funded=9_800_000_000.
    // Split the 200_000_000 surplus: 120_000_000 to the buyer, 80_000_000 to LPs.
    let amount = DEFAULT_FACE_VALUE;
    let refund = 120_000_000u128;
    let yield_amount = amount - DEFAULT_FUNDED_AMOUNT - refund;

    let before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&te.lp);
    let buyer_before = MockTokenClient::new(&te.env, &te.usdc_id).balance(&te.buyer);

    let result = te
        .pool
        .receive_repayment_with_refund(&invoice_id, &amount, &refund, &te.buyer);
    assert!(result);
    // Capture before further queries: sdk >=22 test Envs reset the event
    // buffer at every top-level invocation.
    let events = te.env.events().all();

    // Pool accounting reflects only the yield slice, not the refunded portion.
    let after = te.pool.get_stats();
    let position_after = te.pool.get_lp_position(&te.lp);
    assert_eq!(after.total_deposits, before.total_deposits + yield_amount);
    assert_eq!(
        after.total_yield_distributed,
        before.total_yield_distributed + yield_amount
    );
    assert_eq!(
        after.total_funded,
        before.total_funded - DEFAULT_FUNDED_AMOUNT
    );
    assert_eq!(after.active_invoice_count, before.active_invoice_count - 1);
    assert_eq!(
        position_after.usdc_value,
        position_before.usdc_value + yield_amount
    );

    // The buyer's USDC balance increased by exactly the refund.
    let buyer_after = MockTokenClient::new(&te.env, &te.usdc_id).balance(&te.buyer);
    assert_eq!(buyer_after, buyer_before + refund as i128);

    // The funded-invoice entry must be removed.
    let funded_key = DataKey::FundedInvoice(invoice_id.clone());
    assert!(!te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().has(&funded_key)
    }));

    // Event payload/topics for this entry point (from the capture above).
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "repayment_received")
    );
    assert_eq!(
        BytesN::<32>::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        invoice_id
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (amount, yield_amount, 0)
    );
}

// `refund` above the maximum (amount - funded_amount) must be rejected with
// InvalidAmount (#4): the bound keeps the pool's yield non-negative and stops
// invoice_contract from refunding more than the repayment surplus.
#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_receive_repayment_with_refund_rejects_refund_above_max() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let amount = DEFAULT_FACE_VALUE;
    // max refund = amount - funded = 200_000_000; send one stroop more.
    let refund = amount - DEFAULT_FUNDED_AMOUNT + 1;

    te.pool
        .receive_repayment_with_refund(&invoice_id, &amount, &refund, &te.buyer);
}

// refund == 0 must behave exactly like receive_repayment: the whole surplus
// goes to LP yield, the buyer receives nothing (the `if refund > 0` guard
// skips only the token transfer), and the repayment_received event still fires.
#[test]
fn test_receive_repayment_with_refund_zero_refund_matches_receive_repayment() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let amount = DEFAULT_FACE_VALUE;

    let before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&te.lp);
    let buyer_before = MockTokenClient::new(&te.env, &te.usdc_id).balance(&te.buyer);

    let result = te
        .pool
        .receive_repayment_with_refund(&invoice_id, &amount, &0u128, &te.buyer);
    assert!(result);
    // Capture before further queries: sdk >=22 test Envs reset the event
    // buffer at every top-level invocation.
    let events = te.env.events().all();

    let after = te.pool.get_stats();
    let position_after = te.pool.get_lp_position(&te.lp);
    assert_eq!(
        after.total_deposits,
        before.total_deposits + DEFAULT_YIELD_AMOUNT
    );
    assert_eq!(
        after.total_yield_distributed,
        before.total_yield_distributed + DEFAULT_YIELD_AMOUNT
    );
    assert_eq!(after.total_funded, 0);
    assert_eq!(after.active_invoice_count, 0);
    assert_eq!(
        position_after.usdc_value,
        position_before.usdc_value + DEFAULT_YIELD_AMOUNT
    );

    let buyer_after = MockTokenClient::new(&te.env, &te.usdc_id).balance(&te.buyer);
    assert_eq!(buyer_after, buyer_before);

    // The event still fires even though no refund transfer happened
    // (buffer captured right after the call above).
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "repayment_received")
    );
    assert_eq!(
        BytesN::<32>::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        invoice_id
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (amount, DEFAULT_YIELD_AMOUNT, 0)
    );
}

// -----------------------------------------------------------------------------
// EVENT CATALOG SCHEMA TESTS (#1012): Verify documented repayment and default
// event topics, schemas, and examples against contract assertions.
// -----------------------------------------------------------------------------

#[test]
fn test_doc_example_receive_repayment_no_fee() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let amount = 10_000_000_000u128; // Face value
    let gross_yield = 200_000_000u128; // 200 bps discount on 10_000_000_000
    let protocol_cut = 0u128; // Fee disabled (0 bps)
    let lp_yield = gross_yield - protocol_cut;

    let ok = te.pool.receive_repayment(&invoice_id, &amount);
    assert!(ok);

    let events = te.env.events().all();
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "repayment_received")
    );
    assert_eq!(
        BytesN::<32>::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        invoice_id
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (amount, lp_yield, protocol_cut)
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (10_000_000_000, 200_000_000, 0)
    );
}

#[test]
fn test_doc_example_receive_repayment_with_protocol_fee() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let treasury = Address::generate(&te.env);
    let fee_bps = 1_000u32; // 10% protocol fee
    te.pool.set_protocol_fee(&fee_bps, &treasury);

    let amount = 10_000_000_000u128; // Face value
    let gross_yield = 200_000_000u128;
    let protocol_cut = gross_yield * fee_bps as u128 / 10_000; // 20_000_000
    let lp_yield = gross_yield - protocol_cut; // 180_000_000

    let ok = te.pool.receive_repayment(&invoice_id, &amount);
    assert!(ok);

    let events = te.env.events().all();
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "repayment_received")
    );
    assert_eq!(
        BytesN::<32>::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        invoice_id
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (amount, lp_yield, protocol_cut)
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (10_000_000_000, 180_000_000, 20_000_000)
    );
}

#[test]
fn test_doc_example_receive_repayment_with_refund_no_fee() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let amount = 10_000_000_000u128;
    let refund = 120_000_000u128; // 12 USDC refunded to buyer
    let net_yield = amount - DEFAULT_FUNDED_AMOUNT - refund; // 80_000_000
    let protocol_cut = 0u128;
    let lp_yield = net_yield;

    let ok = te
        .pool
        .receive_repayment_with_refund(&invoice_id, &amount, &refund, &te.buyer);
    assert!(ok);

    let events = te.env.events().all();
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "repayment_received")
    );
    assert_eq!(
        BytesN::<32>::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        invoice_id
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (amount, lp_yield, protocol_cut)
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (10_000_000_000, 80_000_000, 0)
    );
}

#[test]
fn test_doc_example_receive_repayment_with_refund_with_protocol_fee() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let treasury = Address::generate(&te.env);
    let fee_bps = 1_000u32; // 10%
    te.pool.set_protocol_fee(&fee_bps, &treasury);

    let amount = 10_000_000_000u128;
    let refund = 120_000_000u128;
    let net_yield = amount - DEFAULT_FUNDED_AMOUNT - refund; // 80_000_000
    let protocol_cut = net_yield * fee_bps as u128 / 10_000; // 8_000_000
    let lp_yield = net_yield - protocol_cut; // 72_000_000

    let ok = te
        .pool
        .receive_repayment_with_refund(&invoice_id, &amount, &refund, &te.buyer);
    assert!(ok);

    let events = te.env.events().all();
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "repayment_received")
    );
    assert_eq!(
        BytesN::<32>::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        invoice_id
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (amount, lp_yield, protocol_cut)
    );
    assert_eq!(
        <(u128, u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (10_000_000_000, 72_000_000, 8_000_000)
    );
}

#[test]
fn test_doc_example_handle_default_and_no_repayment_event() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&te.lp);

    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86400 + 60);

    let ok = te.pool.handle_default(&invoice_id);
    assert!(ok);
    // Capture before further queries: sdk >=22 test Envs reset the event
    // buffer at every top-level invocation.
    let events = te.env.events().all();

    let after = te.pool.get_stats();
    let position_after = te.pool.get_lp_position(&te.lp);

    // In the unrecoverable-loss path (Example 3A, escrow_released == false),
    // principal was already paid to issuer: TotalDeposits shrinks by funded_amount
    // so LPs absorb the loss via lower share price.
    assert_eq!(
        after.total_deposits,
        before.total_deposits - DEFAULT_FUNDED_AMOUNT
    );
    assert_eq!(
        after.total_funded,
        before.total_funded - DEFAULT_FUNDED_AMOUNT
    );
    assert_eq!(
        after.total_loss_realised,
        before.total_loss_realised + DEFAULT_FUNDED_AMOUNT
    );
    assert_eq!(
        position_after.usdc_value,
        position_before.usdc_value - DEFAULT_FUNDED_AMOUNT
    );

    let mut defaulted_event_found = false;
    let mut repayment_event_found = false;

    for (contract, topics, data) in events.iter() {
        if contract == te.pool_id && !topics.is_empty() {
            if let Ok(topic0) = Symbol::try_from_val(&te.env, &topics.get(0).unwrap()) {
                if topic0 == Symbol::new(&te.env, "invoice_defaulted") {
                    defaulted_event_found = true;
                    assert_eq!(
                        BytesN::<32>::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
                        invoice_id
                    );
                    assert_eq!(
                        u128::try_from_val(&te.env, &data).unwrap(),
                        DEFAULT_FUNDED_AMOUNT
                    );
                } else if topic0 == Symbol::new(&te.env, "repayment_received") {
                    repayment_event_found = true;
                }
            }
        }
    }

    assert!(
        defaulted_event_found,
        "invoice_defaulted event must be emitted"
    );
    assert!(
        !repayment_event_found,
        "handle_default must NOT emit repayment_received event"
    );
}

#[test]
fn test_doc_example_handle_default_recovered_principal() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let funded_amount = DEFAULT_FUNDED_AMOUNT; // 9_800_000_000

    // Simulate escrow still holding the principal (pre-#819 or unreleased escrow)
    // so that escrow.handle_default returns true and returns the principal to the pool.
    let escrow_bal_key = TKey(te.escrow_id.clone());
    te.env.as_contract(&te.usdc_id, || {
        let cur: i128 = te
            .env
            .storage()
            .persistent()
            .get(&escrow_bal_key)
            .unwrap_or(0);
        te.env
            .storage()
            .persistent()
            .set(&escrow_bal_key, &(cur + funded_amount as i128));
    });

    te.env.as_contract(&te.escrow_id, || {
        te.env.storage().persistent().set(
            &trusttrove_escrow::DataKey::Locked(invoice_id.clone()),
            &trusttrove_escrow::EscrowRecord {
                invoice_id: invoice_id.clone(),
                amount: funded_amount,
                locked_at: te.env.ledger().timestamp(),
                issuer: te.issuer.clone(),
            },
        );
        te.env
            .storage()
            .persistent()
            .remove(&trusttrove_escrow::DataKey::IssuerReleased(
                invoice_id.clone(),
            ));
    });

    let before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&te.lp);

    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86400 + 60);

    let ok = te.pool.handle_default(&invoice_id);
    assert!(ok);
    // Capture before further queries: sdk >=22 test Envs reset the event
    // buffer at every top-level invocation.
    let events = te.env.events().all();

    let after = te.pool.get_stats();
    let position_after = te.pool.get_lp_position(&te.lp);

    // In the recovered-principal path (Example 3B, escrow_released == true),
    // TotalDeposits is deliberately left unchanged to preserve LP capital and share value (issue #439).
    assert_eq!(
        after.total_deposits, before.total_deposits,
        "TotalDeposits must remain unchanged on recovered principal"
    );
    assert_eq!(
        after.total_funded,
        before.total_funded - funded_amount,
        "TotalFunded must decrease by funded_amount"
    );
    assert_eq!(
        after.total_loss_realised,
        before.total_loss_realised + funded_amount,
        "TotalLossRealised tracks defaulted funded principal"
    );
    assert_eq!(after.active_invoice_count, before.active_invoice_count - 1);
    assert_eq!(
        position_after.usdc_value, position_before.usdc_value,
        "LP position value must be preserved when principal is recovered"
    );

    let mut defaulted_event_found = false;
    let mut repayment_event_found = false;

    for (contract, topics, data) in events.iter() {
        if contract == te.pool_id && !topics.is_empty() {
            if let Ok(topic0) = Symbol::try_from_val(&te.env, &topics.get(0).unwrap()) {
                if topic0 == Symbol::new(&te.env, "invoice_defaulted") {
                    defaulted_event_found = true;
                    assert_eq!(
                        BytesN::<32>::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
                        invoice_id
                    );
                    // Payload is the defaulted funded principal
                    assert_eq!(u128::try_from_val(&te.env, &data).unwrap(), funded_amount);
                } else if topic0 == Symbol::new(&te.env, "repayment_received") {
                    repayment_event_found = true;
                }
            }
        }
    }

    assert!(
        defaulted_event_found,
        "invoice_defaulted event must be emitted with defaulted funded principal"
    );
    assert!(
        !repayment_event_found,
        "handle_default must NOT emit repayment_received event"
    );
}

//  ISSUE #969: REFUND DESTINATION MUST MATCH THE INVOICE BUYER //
// `receive_repayment_with_refund` is given the refund destination as a plain
// argument alongside the invoice-computed split. A miswired or compromised
// caller contract could direct the refund to another address, so the pool
// reads the canonical buyer back from the configured invoice contract
// (`get_buyer`) and rejects any mismatch with the typed
// `PoolError::BuyerMismatch` (#27) before any funds move or any accounting
// is finalized.

#[test]
fn test_receive_repayment_with_refund_rejects_foreign_destination_without_mutation() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let attacker = Address::generate(&te.env);
    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let before = te.pool.get_stats();
    let pool_before = usdc.balance(&te.pool_id);
    let buyer_before = usdc.balance(&te.buyer);
    let attacker_before = usdc.balance(&attacker);

    // Valid amounts, in-bounds refund — only the destination is wrong.
    let amount = DEFAULT_FACE_VALUE;
    let refund = 120_000_000u128;
    let result =
        te.pool
            .try_receive_repayment_with_refund(&invoice_id, &amount, &refund, &attacker);
    let error = std::format!("{result:?}");
    assert!(
        result.is_err(),
        "a refund to a foreign destination must be rejected: {error}"
    );
    assert!(
        error.contains("#27"),
        "expected PoolError::BuyerMismatch: {error}"
    );

    // No accounting was finalized.
    let after = te.pool.get_stats();
    assert_eq!(after.total_deposits, before.total_deposits);
    assert_eq!(
        after.total_yield_distributed,
        before.total_yield_distributed
    );
    assert_eq!(after.total_funded, before.total_funded);
    assert_eq!(after.active_invoice_count, before.active_invoice_count);

    // No funds moved.
    assert_eq!(
        usdc.balance(&attacker),
        attacker_before,
        "the foreign destination must receive nothing"
    );
    assert_eq!(usdc.balance(&te.buyer), buyer_before);
    assert_eq!(
        usdc.balance(&te.pool_id),
        pool_before,
        "the pool balance must not change on a rejected destination"
    );

    // The funded invoice entry survives; the invoice may still be settled
    // later through the canonical path.
    assert_eq!(
        te.pool.get_funded_amount(&invoice_id),
        Some(DEFAULT_FUNDED_AMOUNT)
    );

    // Positive control: the same split with the canonical buyer settles.
    assert!(te
        .pool
        .receive_repayment_with_refund(&invoice_id, &amount, &refund, &te.buyer));
    assert_eq!(usdc.balance(&te.buyer), buyer_before + refund as i128);
    assert_eq!(te.pool.get_funded_amount(&invoice_id), None);
}

// Even a zero-refund call must verify the destination: the argument is
// untrusted regardless of whether money would move through it.
#[test]
fn test_receive_repayment_with_refund_rejects_foreign_destination_even_with_zero_refund() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let attacker = Address::generate(&te.env);
    let before = te.pool.get_stats();

    let result = te.pool.try_receive_repayment_with_refund(
        &invoice_id,
        &DEFAULT_FACE_VALUE,
        &0u128,
        &attacker,
    );
    let error = std::format!("{result:?}");
    assert!(result.is_err(), "must reject foreign destination: {error}");
    assert!(
        error.contains("#27"),
        "expected PoolError::BuyerMismatch: {error}"
    );

    let after = te.pool.get_stats();
    assert_eq!(after.total_deposits, before.total_deposits);
    assert_eq!(after.total_funded, before.total_funded);
    assert_eq!(after.active_invoice_count, before.active_invoice_count);
    assert_eq!(
        te.pool.get_funded_amount(&invoice_id),
        Some(DEFAULT_FUNDED_AMOUNT)
    );
}

// #969 upgrade path: funded entries created before the buyer snapshot existed
// must not become unpayable. `backfill_funded_invoice_buyer` reads the buyer
// from the trusted invoice contract and restores the snapshot without moving
// funds, after which the ordinary refund path works again.
#[test]
fn test_backfill_funded_invoice_buyer_restores_legacy_refund_path() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // Simulate a pre-#969 funded entry: the `FundedInvoice` principal is live
    // but the buyer snapshot does not exist yet.
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .persistent()
            .remove(&DataKey::FundedInvoiceBuyer(invoice_id.clone()));
    });

    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let buyer_before = usdc.balance(&te.buyer);
    let refund = 120_000_000u128;

    let rejected = te.pool.try_receive_repayment_with_refund(
        &invoice_id,
        &DEFAULT_FACE_VALUE,
        &refund,
        &te.buyer,
    );
    let error = std::format!("{rejected:?}");
    assert!(rejected.is_err(), "legacy entry must fail closed: {error}");
    assert!(
        error.contains("#27"),
        "expected PoolError::BuyerMismatch before backfill: {error}"
    );

    assert!(
        te.pool.backfill_funded_invoice_buyer(&invoice_id),
        "the missing snapshot must be written"
    );
    assert!(
        !te.pool.backfill_funded_invoice_buyer(&invoice_id),
        "an existing snapshot must never be overwritten"
    );

    assert!(te.pool.receive_repayment_with_refund(
        &invoice_id,
        &DEFAULT_FACE_VALUE,
        &refund,
        &te.buyer
    ));
    assert_eq!(usdc.balance(&te.buyer), buyer_before + refund as i128);
    assert_eq!(te.pool.get_funded_amount(&invoice_id), None);
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_backfill_funded_invoice_buyer_rejects_unfunded_invoice() {
    let te = setup();
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.backfill_funded_invoice_buyer(&invoice_id);
}

// A failing refund transfer must not leave the pool partially settled.
//
// `receive_repayment_with_refund` settles the invoice first (LP yield
// credited to `TotalDeposits` / `TotalYieldDistributed`, `TotalFunded` and
// `ActiveInvoiceCount` decremented, `FundedInvoice` entry removed) and only
// then transfers the buyer's refund out of the pool. If that transfer fails,
// the whole call must revert: an invoice whose accounting was settled while
// the buyer was never refunded would leave the pool's totals inconsistent
// with its token balance and would silently drop the buyer's money.
//
// The token stub fails *after* applying its own balance moves, so the test
// proves the revert is atomic for both the pool's settlement accounting and
// the token's writes.
#[test]
fn test_receive_repayment_with_refund_rolls_back_when_refund_transfer_fails() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // Swap the pool's funding asset for the failing token stub. Funding
    // already happened against the real USDC mock, so only the refund
    // transfer is affected.
    let failing_token_id = te.env.register(failing_token::MockFailingToken, ());
    let failing_usdc = failing_token::MockFailingTokenClient::new(&te.env, &failing_token_id);
    te.env.as_contract(&failing_token_id, || {
        te.env
            .storage()
            .persistent()
            .set(&failing_token::TKey(te.pool_id.clone()), &1_000_000_000i128);
        te.env
            .storage()
            .persistent()
            .set(&failing_token::TKey(te.buyer.clone()), &500i128);
    });
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::FundingAsset, &failing_token_id);
    });

    let before = te.pool.get_stats();
    let funded_key = DataKey::FundedInvoice(invoice_id.clone());
    let funded_amount_before: Option<u128> = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get(&funded_key)
    });
    assert_eq!(
        funded_amount_before,
        Some(DEFAULT_FUNDED_AMOUNT),
        "invoice must be funded before the failing repayment"
    );
    let buyer_balance_before = failing_usdc.balance(&te.buyer);
    let pool_balance_before = failing_usdc.balance(&te.pool_id);
    failing_token::TRANSFER_ATTEMPTS.store(0, core::sync::atomic::Ordering::SeqCst);

    // face_value=10_000_000_000, discount_bps=200 → funded=9_800_000_000.
    // The surplus is split between the buyer's refund and the LP yield.
    let amount = DEFAULT_FACE_VALUE;
    let refund = 120_000_000u128;

    let result =
        te.pool
            .try_receive_repayment_with_refund(&invoice_id, &amount, &refund, &te.buyer);
    assert!(
        result.is_err(),
        "receive_repayment_with_refund must revert when the refund transfer fails"
    );
    assert_eq!(
        failing_token::TRANSFER_ATTEMPTS.load(core::sync::atomic::Ordering::SeqCst),
        1,
        "the refund transfer must be the call that fails"
    );

    // The settlement accounting must be rolled back with the failed transfer.
    let after = te.pool.get_stats();
    assert_eq!(
        after.total_deposits, before.total_deposits,
        "total deposits must not change when the refund transfer fails"
    );
    assert_eq!(
        after.total_funded, before.total_funded,
        "total funded must not change when the refund transfer fails"
    );
    assert_eq!(
        after.active_invoice_count, before.active_invoice_count,
        "active invoice count must not change when the refund transfer fails"
    );
    assert_eq!(
        after.total_yield_distributed, before.total_yield_distributed,
        "total yield distributed must not change when the refund transfer fails"
    );
    assert_eq!(
        after.total_shares, before.total_shares,
        "total shares must not change when the refund transfer fails"
    );
    assert_eq!(
        after.available_liquidity, before.available_liquidity,
        "available liquidity must not change when the refund transfer fails"
    );

    // The funded entry must survive the failed settlement, with the same
    // amount: the invoice is still outstanding and can be repaid again.
    let funded_amount_after: Option<u128> = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get(&funded_key)
    });
    assert_eq!(
        funded_amount_after,
        Some(DEFAULT_FUNDED_AMOUNT),
        "funded invoice entry must persist when the refund transfer fails"
    );

    // The buyer must not receive the refund, and the token's own balance
    // moves must be rolled back with the failed transfer.
    assert_eq!(
        failing_usdc.balance(&te.buyer),
        buyer_balance_before,
        "buyer balance must not change when the refund transfer fails"
    );
    assert_eq!(
        failing_usdc.balance(&te.pool_id),
        pool_balance_before,
        "pool token balance must not change when the refund transfer fails"
    );
}

// Mismatched repayment (active_count already zero) must NOT silently underflow the
// counter to u32::MAX — the contract panics with #17 (ActiveCountUnderflow) instead.
// Otherwise every subsequent `get_stats()` / utilization read is corrupted.
#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_receive_repayment_active_count_underflow_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    // Inject a phantom funded-invoice record but force active_count to 0 so the
    // `active_count.checked_sub(1)` branch is the one to trigger. The other
    // counters are kept consistent so the panic lands on ActiveCountUnderflow,
    // not on the u128 underflow in `total_funded - funded_amount`.
    let phantom_id = BytesN::from_array(&te.env, &[0xab; 32]);
    let funded_amount: u128 = DEFAULT_FUNDED_AMOUNT;
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .persistent()
            .set(&DataKey::FundedInvoice(phantom_id.clone()), &funded_amount);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalFunded, &funded_amount);
        te.env
            .storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &0u32);
    });

    te.pool.receive_repayment(&phantom_id, &funded_amount);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_receive_repayment_with_refund_active_count_underflow_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    let phantom_id = BytesN::from_array(&te.env, &[0xcd; 32]);
    let funded_amount: u128 = DEFAULT_FUNDED_AMOUNT;
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .persistent()
            .set(&DataKey::FundedInvoice(phantom_id.clone()), &funded_amount);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalFunded, &funded_amount);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &funded_amount);
        te.env
            .storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &0u32);
    });

    te.pool
        .receive_repayment_with_refund(&phantom_id, &funded_amount, &0, &te.buyer);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_handle_default_active_count_underflow_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    let phantom_id = BytesN::from_array(&te.env, &[0xef; 32]);
    let funded_amount: u128 = DEFAULT_FUNDED_AMOUNT;
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .persistent()
            .set(&DataKey::FundedInvoice(phantom_id.clone()), &funded_amount);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalFunded, &funded_amount);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &funded_amount);
        te.env
            .storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &0u32);
    });

    // Give escrow a matching lock record so escrow.handle_default() actually
    // releases funds (returns true) and execution reaches the pool-side
    // active-count underflow this test targets, rather than tripping the
    // EscrowDefaultNotReleased guard first.
    te.env.as_contract(&te.escrow_id, || {
        te.env.storage().persistent().set(
            &trusttrove_escrow::DataKey::Locked(phantom_id.clone()),
            &trusttrove_escrow::EscrowRecord {
                invoice_id: phantom_id.clone(),
                amount: funded_amount,
                locked_at: te.env.ledger().timestamp(),
                issuer: Address::generate(&te.env),
            },
        );
    });
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);

    te.pool.handle_default(&phantom_id);
}

// Pinned by #629 before issue #819: invoice.trigger_default's due-date gate
// (`now >= due_date`) had no awareness of escrow's independent
// DEFAULT_MIN_LOCK_SECONDS (60s) grace period measured from the escrow lock
// timestamp, so a default within that window reverted with
// EscrowError::NotAuthorized from escrow.handle_default. Since #819 the
// funding path releases the principal to the issuer at funding time, which
// removes the `Locked` record — escrow's grace period no longer gates
// defaults of normally funded invoices. This test now pins the post-#819
// behavior: a default 31s after funding succeeds through the pool's
// issuer-release branch (escrow.handle_default returns false, the
// DataKey::IssuerReleased marker authorizes it) and books the capital loss
// instead of reverting with escrow #3.
#[test]
fn test_default_within_escrow_grace_window_after_issuer_payout() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    // due_date reached only 30s after now (well under escrow's old 60s lock
    // grace period, and funding happens immediately after listing in this
    // test).
    let due_date = te.env.ledger().timestamp() + 30;
    let face_value: u128 = 10_000_000_000;
    let invoice_id = te
        .invoice
        .create(&te.issuer, &te.buyer, &face_value, &due_date, &te.usdc_id);
    attest_invoice(&te, &invoice_id);
    te.invoice.list_for_financing(&invoice_id, &200);
    te.pool.fund_invoice(&invoice_id);

    // Advance past due_date but still within escrow's old 60s lock grace
    // window. Escrow holds no lock record (the principal is with the issuer),
    // so escrow's grace check is never reached. Driven through the pool
    // directly: the top-level invoice.trigger_default chain remains pinned to
    // the separate self-re-entrancy issue (#627).
    te.env.ledger().set_timestamp(due_date + 1);

    assert!(te.pool.handle_default(&invoice_id));

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, 0);
    assert_eq!(stats.active_invoice_count, 0);
    assert_eq!(stats.total_loss_realised, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(
        stats.total_deposits,
        100_000_000_000 - DEFAULT_FUNDED_AMOUNT
    );
    assert_eq!(te.invoice.get_status(&invoice_id), 6);
}

//  DEFAULT TESTS
#[test]
fn test_handle_default() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // funded_amount = 10_000_000_000 * 9800 / 10000 = 9_800_000_000
    let funded_amount = DEFAULT_FUNDED_AMOUNT;

    let before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&te.lp);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);
    let result = te.pool.handle_default(&invoice_id);
    assert!(result);
    // Capture before further queries: sdk >=22 test Envs reset the event
    // buffer at every top-level invocation.
    let events = te.env.events().all();

    let after = te.pool.get_stats();
    let position_after = te.pool.get_lp_position(&te.lp);
    // Issue #819: the funding path released the principal to the issuer, so
    // escrow recovers nothing on default. `TotalDeposits` shrinks by the
    // funded amount to keep deposit accounting matched to the pool's token
    // balance (LPs absorb the loss through a lower share price); shares are
    // not burned.
    assert_eq!(after.total_deposits, before.total_deposits - funded_amount);
    assert_eq!(after.total_funded, 0);
    assert_eq!(after.active_invoice_count, 0);
    assert_eq!(after.total_shares, before.total_shares);
    assert_eq!(
        after.total_loss_realised,
        before.total_loss_realised + funded_amount
    );
    assert_eq!(
        position_after.usdc_value,
        position_before.usdc_value - funded_amount
    );

    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "invoice_defaulted")
    );
    assert_eq!(
        BytesN::<32>::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        invoice_id
    );
    assert_eq!(u128::try_from_val(&te.env, &data).unwrap(), funded_amount);
}

#[test]
fn test_handle_default_realizes_loss_without_burning_shares() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);

    let lp_before = te.pool.get_lp_position(&te.lp);
    let pool_before = te.pool.get_stats();

    assert_eq!(pool_before.total_deposits, 100_000_000_000);
    assert_eq!(pool_before.total_shares, 100_000_000_000);
    assert_eq!(lp_before.usdc_value, 100_000_000_000);

    let result = te.pool.handle_default(&invoice_id);
    assert!(result);

    let lp_after = te.pool.get_lp_position(&te.lp);
    let pool_after = te.pool.get_stats();

    // Issue #819: a default after the issuer payout is a real capital loss —
    // escrow returns nothing, so `TotalDeposits` shrinks by the funded
    // principal and the LP's share value falls with it. Shares themselves are
    // not burned (#439's no-share-burn property survives; its
    // deposits-preserved property assumed escrow still held the principal).
    assert_eq!(
        pool_after.total_deposits,
        pool_before.total_deposits - DEFAULT_FUNDED_AMOUNT
    );
    assert_eq!(pool_after.total_shares, pool_before.total_shares);
    assert_eq!(lp_after.shares, lp_before.shares);
    assert_eq!(
        lp_after.usdc_value,
        lp_before.usdc_value - DEFAULT_FUNDED_AMOUNT
    );
    assert_eq!(pool_after.total_funded, 0);
    assert_eq!(pool_after.active_invoice_count, 0);
    assert_eq!(
        pool_after.total_loss_realised,
        pool_before.total_loss_realised + DEFAULT_FUNDED_AMOUNT
    );
}

//  ISSUE #439 (revisited by #819): DEFAULT BOOKS A REAL LP CAPITAL LOSS
/// Because the funding path releases the principal to the issuer at funding
/// time (issue #819), escrow holds nothing to hand back on default. The pool
/// must therefore shrink `TotalDeposits` by the funded amount so deposit
/// accounting keeps matching the pool's token balance; the loss surfaces as a
/// lower share price rather than insolvency. The pre-#819 premise of this
/// test ("escrow returns the funded principal, so deposits are untouched")
/// only held while the issuer-payout bug left the principal stranded.
#[test]
fn test_default_after_issuer_payout_books_capital_loss_and_preserves_shares() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let before = te.pool.get_stats();
    let lp_before = te.pool.get_lp_position(&te.lp);
    assert_eq!(before.total_deposits, 100_000_000_000);
    assert_eq!(before.total_funded, DEFAULT_FUNDED_AMOUNT);

    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);
    assert!(te.pool.handle_default(&invoice_id));

    let after = te.pool.get_stats();
    let lp_after = te.pool.get_lp_position(&te.lp);

    // Shares are not burned, but LP capital is: deposits shrink by the
    // unrecoverable principal and per-LP value falls with them.
    assert_eq!(after.total_shares, before.total_shares);
    assert_eq!(lp_after.shares, lp_before.shares);
    assert_eq!(
        after.total_deposits,
        before.total_deposits - DEFAULT_FUNDED_AMOUNT
    );
    assert_eq!(
        lp_after.usdc_value,
        lp_before.usdc_value - DEFAULT_FUNDED_AMOUNT
    );

    assert_eq!(after.total_funded, 0);
    assert_eq!(
        after.total_deposits - after.total_funded,
        before.total_deposits - before.total_funded
    );
    assert_eq!(after.active_invoice_count, 0);
    assert_eq!(
        after.total_loss_realised,
        before.total_loss_realised + DEFAULT_FUNDED_AMOUNT
    );
}

#[test]
fn test_handle_default_updates_invoice_status() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // Invoice should be in Funded status (2)
    assert_eq!(te.invoice.get_status(&invoice_id), 2);

    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);
    let result = te.pool.handle_default(&invoice_id);
    assert!(result);

    // After handle_default, invoice should be Defaulted (6)
    assert_eq!(te.invoice.get_status(&invoice_id), 6);
}

// Reproduces #627: every other default-path test drives the flow by calling
// te.pool.handle_default() directly, bypassing the real production entry
// point. A permissionless caller only ever has invoice.trigger_default(),
// which locally marks the invoice Defaulted and then invokes
// pool.handle_default() (which in turn calls escrow.handle_default() and
// calls back into invoice.mark_defaulted()).
//
// Driving the chain from invoice.trigger_default() (rather than from
// pool.handle_default() as the top-level caller) surfaces a real bug: the
// invoice contract is still on the call stack when pool calls back into
// invoice.mark_defaulted(), and Soroban's runtime rejects that as
// self-re-entrancy ("Contract re-entry is not allowed"), regardless of
// mark_defaulted's idempotent no-op logic. This test pins that current
// behavior; see follow-up issue for fixing the underlying re-entrancy in
// InvoiceContract::trigger_default / PoolContract::handle_default.
#[test]
#[should_panic(expected = "Error(Context, InvalidAction)")]
fn test_trigger_default_drives_full_pool_and_escrow_chain() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // Invoice should be Funded (2) before default.
    assert_eq!(te.invoice.get_status(&invoice_id), 2);

    let escrow_client = RealEscrowClient::new(&te.env, &te.escrow_id);
    // Issue #819: funding released the locked principal to the issuer, so no
    // principal remains locked in escrow after `fund_invoice`.
    assert_eq!(escrow_client.get_locked(&invoice_id), 0);
    assert!(escrow_client.was_released_to_issuer(&invoice_id));

    // Advance past the invoice's due date and default grace period, as well as
    // escrow's DEFAULT_MIN_LOCK_SECONDS grace period (60s from funding).
    te.env.ledger().set_timestamp(
        te.env.ledger().timestamp() + 86400 + trusttrove_invoice::DEFAULT_GRACE_SECONDS + 1,
    );

    // Drive the real production entry point instead of calling
    // pool.handle_default() directly. This panics with a re-entrancy error
    // once pool calls back into invoice.mark_defaulted() (see comment above).
    te.invoice.trigger_default(&invoice_id);
}

#[test]
fn test_handle_default_rejects_double_default() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);

    assert!(te.pool.handle_default(&invoice_id));
    // Second call should panic with InvoiceNotFound since the funded key was removed
    let res = te.pool.try_handle_default(&invoice_id);
    assert!(res.is_err());
}

// If escrow has neither a lock record nor an issuer-release marker for an
// invoice by the time pool.handle_default runs (e.g. the lock was consumed
// out of band via release_to_pool after the marker was already burned),
// escrow.handle_default() returns false without transferring any tokens and
// the marker check cannot attribute the missing principal to the issuer.
// Pool must not proceed with loss accounting in that case — see
// EscrowDefaultNotReleased (#21). Issue #819 made the marker the "normal"
// no-release case, so this test must strip BOTH pieces of escrow evidence.
#[test]
fn test_handle_default_rejects_when_escrow_reports_no_release() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);

    // Simulate escrow having no lock record AND no issuer-release marker, so
    // escrow.handle_default() hits its `return false` path and the pool's
    // marker cross-check fails too.
    let locked_key = trusttrove_escrow::DataKey::Locked(invoice_id.clone());
    let released_key = trusttrove_escrow::DataKey::IssuerReleased(invoice_id.clone());
    te.env.as_contract(&te.escrow_id, || {
        te.env.storage().persistent().remove(&locked_key);
        te.env.storage().persistent().remove(&released_key);
    });

    let before = te.pool.get_stats();

    let res = te.pool.try_handle_default(&invoice_id);
    assert!(res.is_err());

    // Pool accounting must be untouched: escrow released nothing, so no loss
    // should be realised, funded/deposit totals must be unchanged, and the
    // funded invoice entry must still exist.
    let after = te.pool.get_stats();
    assert_eq!(after.total_deposits, before.total_deposits);
    assert_eq!(after.total_funded, before.total_funded);
    assert_eq!(after.total_loss_realised, before.total_loss_realised);
    assert_eq!(after.active_invoice_count, before.active_invoice_count);

    let funded_key = DataKey::FundedInvoice(invoice_id.clone());
    let still_funded = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().has(&funded_key)
    });
    assert!(still_funded);
}

/// Roll back handle_default when escrow default handling fails.
///
/// Uses an escrow stub that panics on `handle_default` for an existing
/// funded invoice. Asserts that pool totals, FundedInvoice entry, active
/// invoice count, and invoice status remain unchanged.
#[test]
fn test_handle_default_rolls_back_on_escrow_failure() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);

    let funded_amount = DEFAULT_FUNDED_AMOUNT;

    // Swap in a failing escrow that panics on handle_default
    let failing_escrow_id = te
        .env
        .register(failing_escrow_default::MockFailingEscrowDefault, ());
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::EscrowContract, &failing_escrow_id);
    });

    let stats_before = te.pool.get_stats();
    let invoice_status_before = te.invoice.get_status(&invoice_id);
    let funded_key = DataKey::FundedInvoice(invoice_id.clone());
    let funded_entry_before = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().has(&funded_key)
    });
    assert!(funded_entry_before);

    // Call handle_default — it must revert because escrow panics
    let res = te.pool.try_handle_default(&invoice_id);
    assert!(
        res.is_err(),
        "handle_default must revert when escrow.handle_default fails"
    );

    // Pool totals must be unchanged
    let stats_after = te.pool.get_stats();
    assert_eq!(
        stats_after.total_deposits, stats_before.total_deposits,
        "total_deposits must not change on escrow failure"
    );
    assert_eq!(
        stats_after.total_funded, stats_before.total_funded,
        "total_funded must not change on escrow failure"
    );
    assert_eq!(
        stats_after.total_loss_realised, stats_before.total_loss_realised,
        "total_loss_realised must not change on escrow failure"
    );
    assert_eq!(
        stats_after.active_invoice_count, stats_before.active_invoice_count,
        "active_invoice_count must not change on escrow failure"
    );
    assert_eq!(
        stats_after.total_shares, stats_before.total_shares,
        "total_shares must not change on escrow failure"
    );

    // FundedInvoice entry must still exist with the same amount
    let funded_entry_after = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().has(&funded_key)
    });
    assert!(funded_entry_after, "FundedInvoice entry must persist");
    let funded_amount_after: u128 = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get(&funded_key).unwrap()
    });
    assert_eq!(
        funded_amount_after, funded_amount,
        "FundedInvoice amount must be unchanged"
    );

    // Invoice status must remain Funded (2), not transitioned to Defaulted (6)
    let invoice_status_after = te.invoice.get_status(&invoice_id);
    assert_eq!(
        invoice_status_after, invoice_status_before,
        "invoice status must not change on escrow failure"
    );
    assert_eq!(invoice_status_after, 2, "invoice must stay Funded");
}

/// Issue #971: `handle_default` must not leave the pool reporting a loss while
/// the invoice remains active. The configured invoice is swapped for a stub
/// that fails `mark_defaulted` after escrow's `handle_default` has already
/// succeeded, so the failure lands exactly at the step that used to run
/// *after* the pool accounting writes.
#[test]
fn test_handle_default_rolls_back_when_mark_defaulted_fails() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);

    // Escrow is left as the real contract: its handle_default succeeds (the
    // funding path already paid the issuer, so escrow returns false with the
    // issuer-release marker set), which puts the failure squarely in
    // mark_defaulted.
    let escrow = RealEscrowClient::new(&te.env, &te.escrow_id);
    assert!(
        escrow.was_released_to_issuer(&invoice_id),
        "escrow must have released the principal to the issuer before the default"
    );
    assert_eq!(escrow.get_locked(&invoice_id), 0);

    let failing_invoice_id = te
        .env
        .register(failing_mark_defaulted::MockFailingMarkDefaultedInvoice, ());
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::InvoiceContract, &failing_invoice_id);
    });

    let stats_before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&te.lp);
    let invoice_status_before = te.invoice.get_status(&invoice_id);
    let funded_key = DataKey::FundedInvoice(invoice_id.clone());
    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let pool_balance_before = usdc.balance(&te.pool_id);

    let result = te.pool.try_handle_default(&invoice_id);
    assert!(
        result.is_err(),
        "handle_default must revert when invoice.mark_defaulted fails"
    );

    let stats_after = te.pool.get_stats();
    assert_eq!(
        stats_after.total_deposits, stats_before.total_deposits,
        "total_deposits must not shrink when mark_defaulted fails"
    );
    assert_eq!(
        stats_after.total_funded, stats_before.total_funded,
        "total_funded must not change when mark_defaulted fails"
    );
    assert_eq!(
        stats_after.total_loss_realised, stats_before.total_loss_realised,
        "no loss may be realised while the invoice is still active"
    );
    assert_eq!(
        stats_after.active_invoice_count, stats_before.active_invoice_count,
        "active_invoice_count must not change when mark_defaulted fails"
    );
    assert_eq!(
        stats_after.total_shares, stats_before.total_shares,
        "total_shares must not change when mark_defaulted fails"
    );
    assert_eq!(
        stats_after.total_yield_distributed, stats_before.total_yield_distributed,
        "total_yield_distributed must not change when mark_defaulted fails"
    );

    let position_after = te.pool.get_lp_position(&te.lp);
    assert_eq!(position_after.shares, position_before.shares);
    assert_eq!(
        position_after.usdc_value, position_before.usdc_value,
        "LP share value must not move when mark_defaulted fails"
    );

    let funded_entry_after: Option<u128> = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get(&funded_key)
    });
    assert_eq!(
        funded_entry_after,
        Some(DEFAULT_FUNDED_AMOUNT),
        "the funded invoice entry must survive the failed default"
    );

    assert_eq!(
        usdc.balance(&te.pool_id),
        pool_balance_before,
        "pool token balance must not change when mark_defaulted fails"
    );
    assert_eq!(
        te.invoice.get_status(&invoice_id),
        invoice_status_before,
        "invoice status must not change when mark_defaulted fails"
    );
    assert_eq!(
        te.invoice.get_status(&invoice_id),
        2,
        "invoice must stay Funded"
    );
}

/// Issue #971, fail-closed half: an invoice contract that reports the
/// `Defaulted` status was not persisted (without trapping) is rejected with the
/// typed `PoolError::InvoiceDefaultNotRecorded` (#29), and the pool still books
/// nothing.
#[test]
fn test_handle_default_rejects_unpersisted_default() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);

    let refusing_invoice_id = te.env.register(
        refusing_mark_defaulted::MockRefusingMarkDefaultedInvoice,
        (),
    );
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::InvoiceContract, &refusing_invoice_id);
    });

    let stats_before = te.pool.get_stats();
    let invoice_status_before = te.invoice.get_status(&invoice_id);

    let result = te.pool.try_handle_default(&invoice_id);
    let error = std::format!("{result:?}");
    assert!(
        result.is_err(),
        "a default the invoice did not persist must be rejected: {error}"
    );
    assert!(
        error.contains("#29"),
        "expected PoolError::InvoiceDefaultNotRecorded: {error}"
    );

    let stats_after = te.pool.get_stats();
    assert_eq!(stats_after.total_deposits, stats_before.total_deposits);
    assert_eq!(stats_after.total_funded, stats_before.total_funded);
    assert_eq!(
        stats_after.total_loss_realised,
        stats_before.total_loss_realised
    );
    assert_eq!(
        stats_after.active_invoice_count,
        stats_before.active_invoice_count
    );
    assert_eq!(
        te.pool.get_funded_amount(&invoice_id),
        Some(DEFAULT_FUNDED_AMOUNT)
    );
    assert_eq!(te.invoice.get_status(&invoice_id), invoice_status_before);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_handle_default_requires_invoice_contract_authorization() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);

    te.env.set_auths(&[]);
    te.pool.handle_default(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_handle_default_unknown_invoice_panics() {
    let te = setup();
    let dummy_id = BytesN::from_array(&te.env, &[0u8; 32]);
    te.pool.handle_default(&dummy_id);
}

// Issue #443: `handle_default` for an invoice the pool never funded must not
// mutate any pool state. The current implementation rejects the call with
// InvoiceNotFound (#10) *before* it touches any accounting — the issue's
// "returns false" description predates that guard, and the exact error code is
// pinned by `test_handle_default_unknown_invoice_panics` above — so this test
// pins the safety-critical half: the rejected call writes nothing.
#[test]
fn test_handle_default_unknown_invoice_leaves_pool_state_unchanged() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    // Fund a real invoice so the pool holds non-trivial accounting to protect.
    let funded_id = create_and_list(&te, &te.usdc_id);
    assert!(te.pool.fund_invoice(&funded_id));

    let stats_before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&te.lp);

    let unknown_id = BytesN::from_array(&te.env, &[7u8; 32]);
    let result = te.pool.try_handle_default(&unknown_id);
    assert!(
        result.is_err(),
        "handle_default on an unfunded invoice must be rejected"
    );

    // Pool accounting is byte-for-byte unchanged by the rejected call.
    let stats_after = te.pool.get_stats();
    assert_eq!(stats_after.total_deposits, stats_before.total_deposits);
    assert_eq!(stats_after.total_funded, stats_before.total_funded);
    assert_eq!(
        stats_after.active_invoice_count,
        stats_before.active_invoice_count
    );
    assert_eq!(
        stats_after.total_loss_realised,
        stats_before.total_loss_realised
    );
    assert_eq!(stats_after.total_shares, stats_before.total_shares);
    let position_after = te.pool.get_lp_position(&te.lp);
    assert_eq!(position_after.shares, position_before.shares);
    assert_eq!(position_after.usdc_value, position_before.usdc_value);

    // The unrelated funded invoice's entry is still present and intact.
    let funded_key = DataKey::FundedInvoice(funded_id.clone());
    let still_funded: u128 = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get(&funded_key).unwrap()
    });
    assert_eq!(still_funded, DEFAULT_FUNDED_AMOUNT);
}

#[test]
fn test_deposit_when_deposits_zero_but_shares_exist() {
    let te = setup();

    // LP1 deposits and keeps their shares. Since issue #439 makes a default
    // preserve `TotalDeposits`, the "deposits == 0 but shares exist" edge case
    // is now reproduced by zeroing `TotalDeposits` directly — this is the
    // division-by-zero path the deposit math must guard.
    te.pool.deposit(&te.lp, &DEFAULT_FUNDED_AMOUNT);
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &0u128);
    });

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_deposits, 0);
    assert!(stats.total_shares > 0);

    // Attempt new deposit, which should not panic and should issue 1-to-1 shares
    let lp2 = create_lp_with_balance(&te, 10_000_000_000);
    let new_shares = te.pool.deposit(&lp2, &5_000_000_000);
    assert_eq!(new_shares, 5_000_000_000);
}

//  ISSUE #269: DEPOSIT AFTER DEFAULT (SHARE PRICE < 1)
#[test]
fn test_deposit_after_default_share_price_recovery() {
    let te = setup();

    // LP1 deposits 10B USDC
    let shares1 = te.pool.deposit(&te.lp, &10_000_000_000);
    assert_eq!(shares1, 10_000_000_000);

    // Fund invoice (9.8B funded)
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);

    // Default: the 9.8B principal is with the issuer (paid at funding,
    // issue #819), so escrow returns nothing and the pool books the loss.
    let _ = te.pool.handle_default(&invoice_id);

    // Issue #819: a default after the issuer payout is a real capital loss —
    // TotalDeposits shrinks by the funded amount (share price 0.02), which is
    // the state this test wants deposit math to keep handling.
    let stats_after_default = te.pool.get_stats();
    assert_eq!(
        stats_after_default.total_deposits,
        10_000_000_000 - DEFAULT_FUNDED_AMOUNT
    );
    assert_eq!(stats_after_default.total_shares, 10_000_000_000); // unchanged

    // Force a sub-1.0 share price (0.2B / 10B = 0.02 USDC per share) directly
    // in storage: this is the state the deposit math must keep handling now
    // that defaults no longer produce it. Share price: 200M / 10B = 0.02
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &DEFAULT_YIELD_AMOUNT);
    });

    // LP2 deposits 10B USDC (new address)
    let lp2 = create_lp_with_balance(&te, 100_000_000_000);
    let shares2 = te.pool.deposit(&lp2, &10_000_000_000);

    // LP2 should get 10B / 0.02 = 500B shares (less per USDC than LP1)
    // Because share price is depressed below 1.0
    assert!(
        shares2 > 10_000_000_000,
        "LP2 should get more shares due to deflated share price"
    );

    // Verify LP1 and LP2 positions
    let pos1 = te.pool.get_lp_position(&te.lp);
    let pos2 = te.pool.get_lp_position(&lp2);

    assert_eq!(pos1.shares, 10_000_000_000);
    assert_eq!(pos1.usdc_value, DEFAULT_YIELD_AMOUNT); // 10B shares * 0.02 per share

    assert_eq!(pos2.shares, shares2);
    assert_eq!(pos2.usdc_value, 10_000_000_000); // LP2 deposited 10B

    // Verify final pool state
    let final_stats = te.pool.get_stats();
    assert_eq!(
        final_stats.total_deposits,
        DEFAULT_FACE_VALUE + DEFAULT_YIELD_AMOUNT
    ); // 200M + 10B
    assert_eq!(final_stats.total_shares, 10_000_000_000 + shares2);
}

//  ISSUE #270: WITHDRAW EXACT TOTAL SHARES TO ZERO
#[test]
fn test_withdraw_all_shares_to_zero_then_redeposit() {
    let te = setup();

    // Deposit 10B USDC
    let shares = te.pool.deposit(&te.lp, &10_000_000_000);
    assert_eq!(shares, 10_000_000_000);

    // Verify initial state
    let stats_before = te.pool.get_stats();
    assert_eq!(stats_before.total_shares, 10_000_000_000);
    assert_eq!(stats_before.total_deposits, 10_000_000_000);

    // Withdraw all shares
    let usdc_returned = te.pool.withdraw(&te.lp, &10_000_000_000);
    assert_eq!(usdc_returned, 10_000_000_000);

    // Verify total shares and deposits are now zero
    let stats_after_withdraw = te.pool.get_stats();
    assert_eq!(stats_after_withdraw.total_shares, 0);
    assert_eq!(stats_after_withdraw.total_deposits, 0);

    // Verify the returned LP position and LP-scoped accounting are cleared.
    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 0);
    assert_eq!(pos.usdc_value, 0);
    assert_eq!(pos.deposit_count, 0);

    let init_dep_key = DataKey::LPInitialDeposit(te.lp.clone());
    let dep_count_key = DataKey::LPDepositCount(te.lp.clone());
    assert!(!te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().has(&init_dep_key)
    }));
    assert!(!te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().has(&dep_count_key)
    }));

    // Re-deposit succeeds and is treated as a first-depositor (1:1 shares)
    let new_shares = te.pool.deposit(&te.lp, &5_000_000_000);
    assert_eq!(new_shares, 5_000_000_000);

    // Verify pool state after re-deposit
    let stats_after_redeposit = te.pool.get_stats();
    assert_eq!(stats_after_redeposit.total_shares, 5_000_000_000);
    assert_eq!(stats_after_redeposit.total_deposits, 5_000_000_000);

    let final_pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(final_pos.shares, 5_000_000_000);
    assert_eq!(final_pos.usdc_value, 5_000_000_000);
    assert_eq!(final_pos.deposit_count, 1);

    let fresh_principal: u128 = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get(&init_dep_key).unwrap()
    });
    assert_eq!(fresh_principal, 5_000_000_000);
    let fresh_count: u32 = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get(&dep_count_key).unwrap()
    });
    assert_eq!(fresh_count, 1);
}

//  ISSUE #258: RESET LP STATE ON FULL WITHDRAWAL
#[test]
fn test_full_withdraw_resets_lp_state() {
    let te = setup();
    let lp2 = create_lp_with_balance(&te, 100_000_000_000);

    te.pool.deposit(&te.lp, &10_000_000_000);
    te.pool.deposit(&lp2, &20_000_000_000);

    // Generate yield so LPInitialDeposit != LPShares
    fund_and_repay_invoice(&te);

    // Full withdrawal of all shares
    let shares = te.pool.get_lp_position(&te.lp).shares;
    assert!(shares > 0);
    te.pool.withdraw(&te.lp, &shares);

    // Verify LPInitialDeposit is removed from storage
    let init_dep_key = DataKey::LPInitialDeposit(te.lp.clone());
    assert!(!te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().has(&init_dep_key)
    }));

    // Verify LPDepositCount is removed from storage
    let dep_count_key = DataKey::LPDepositCount(te.lp.clone());
    assert!(!te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().has(&dep_count_key)
    }));

    // Verify get_lp_position returns 0 for both
    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 0);
    assert_eq!(pos.deposit_count, 0);
}

#[test]
fn test_full_withdraw_then_deposit_yield_accounting() {
    let te = setup();
    let lp2 = create_lp_with_balance(&te, 100_000_000_000);

    // First deposit cycle
    te.pool.deposit(&te.lp, &10_000_000_000);
    te.pool.deposit(&lp2, &20_000_000_000);

    // Generate yield
    fund_and_repay_invoice(&te);

    // Full withdrawal with yield — principal portion should be less than USDC returned
    let pos_before = te.pool.get_lp_position(&te.lp);
    let returned = te.pool.withdraw(&te.lp, &pos_before.shares);
    assert!(returned > 10_000_000_000); // Got yield

    // Verify yield_earned is tracked after full withdrawal
    let pos_after = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos_after.shares, 0);
    assert!(pos_after.yield_earned > 0);

    // Re-deposit — should start fresh with deposit_count = 1 (not 2)
    let new_shares = te.pool.deposit(&te.lp, &5_000_000_000);
    assert!(new_shares > 0);

    let final_pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(final_pos.shares, new_shares);
    assert_eq!(final_pos.deposit_count, 1); // Reset, not 2

    // Yield earned from previous cycle is preserved
    assert!(final_pos.yield_earned > 0);
}

#[test]
fn test_multi_deposit_average_basis_differs_from_fifo_yield() {
    let te = setup();
    let first_deposit = 10_000_000_000u128;

    // A 2% pool return raises the share price before this LP's second deposit.
    te.pool.deposit(&te.lp, &first_deposit);
    fund_and_repay_invoice(&te);
    let second_shares = te.pool.deposit(&te.lp, &first_deposit);
    assert_eq!(second_shares, 9_803_921_568);

    // Withdrawing half of the original lot would return 5.1B stroops. FIFO
    // assigns 5B stroops of principal and 100M of yield to that withdrawal.
    let shares_withdrawn = first_deposit / 2;
    let returned = te.pool.withdraw(&te.lp, &shares_withdrawn);
    assert_eq!(returned, 5_100_000_000);

    // The aggregate-basis model instead allocates about 5.0495B principal,
    // understating this withdrawal's FIFO yield by about 49.5M stroops.
    let position = te.pool.get_lp_position(&te.lp);
    assert_eq!(position.yield_earned, 50_495_050);
    assert_eq!(100_000_000 - position.yield_earned, 49_504_950);
}

#[test]
fn test_multi_lp_proportional_yield_with_mid_cycle_deposit() {
    let te = setup();

    // LP1 deposits 10B USDC
    let lp1_deposit = 10_000_000_000;
    let lp1_shares = te.pool.deposit(&te.lp, &lp1_deposit);
    assert_eq!(lp1_shares, lp1_deposit);

    // LP2 deposits 20B USDC (different amount, 2:1 ratio)
    let lp2 = create_lp_with_balance(&te, 100_000_000_000);
    let lp2_deposit = 20_000_000_000;
    let lp2_shares = te.pool.deposit(&lp2, &lp2_deposit);
    assert_eq!(lp2_shares, lp2_deposit);

    // Fund an invoice (9.8B funded out of 30B total)
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);

    // Verify funding occurred
    let stats_after_fund = te.pool.get_stats();
    assert!(stats_after_fund.total_funded > 0);

    // LP3 deposits 15B between fund and repay
    let lp3 = create_lp_with_balance(&te, 100_000_000_000);
    let lp3_deposit = 15_000_000_000;
    let lp3_shares = te.pool.deposit(&lp3, &lp3_deposit);

    // Repay the invoice with yield (200M yield on 9.8B funded)
    let face_value = 10_000_000_000;
    let yield_amount = DEFAULT_YIELD_AMOUNT;
    te.pool.receive_repayment(&invoice_id, &face_value);

    // Verify yield was added to total_deposits
    let stats_after_repay = te.pool.get_stats();
    assert_eq!(stats_after_repay.total_yield_distributed, yield_amount);

    // LP1 withdraws all shares and verifies proportional yield
    let lp1_return = te.pool.withdraw(&te.lp, &lp1_shares);
    let _lp1_pos = te.pool.get_lp_position(&te.lp);

    // LP2 withdraws all shares and verifies proportional yield
    let lp2_return = te.pool.withdraw(&lp2, &lp2_shares);
    let _lp2_pos = te.pool.get_lp_position(&lp2);

    // Verify LP1 and LP2 received gains proportional to their share of yield
    // LP1 had 1/3 of the pool before LP3 joined and funded, so should receive ~1/3 of yield
    // LP2 had 2/3 of the pool before LP3 joined and funded, so should receive ~2/3 of yield
    assert!(
        lp1_return >= lp1_deposit,
        "LP1 should receive at least their deposit"
    );
    assert!(
        lp2_return >= lp2_deposit,
        "LP2 should receive at least their deposit"
    );

    // LP2 should have received more yield than LP1 (2:1 ratio)
    let lp1_gain = lp1_return - lp1_deposit;
    let lp2_gain = lp2_return - lp2_deposit;
    assert!(
        lp2_gain > lp1_gain,
        "LP2 should have higher yield gain due to larger deposit"
    );

    // LP3 should have received minimal or no yield (deposited after fund)
    let lp3_return = te.pool.withdraw(&lp3, &lp3_shares);
    let lp3_gain = lp3_return.saturating_sub(lp3_deposit);

    // LP3 deposited after funding, so should not receive much yield
    assert!(
        lp3_gain <= lp2_gain,
        "LP3 should receive less yield than LP2"
    );
}

//  ISSUE #272: NEGATIVE AUTH TESTS // Note: These functions (receive_repayment, handle_default, fund_invoice) are guarded by
// cross-contract auth checks. Testing unauthorized access requires more complex mocking
// of contract-to-contract calls. The auth logic is enforced at the contract boundary
// and verified through integration tests. Individual contract auth is covered by the
// fact that mock_all_auths() is used during setup and cleared when specific auths are set.

//  ISSUE #274: INSUFFICIENT LIQUIDITY ON WITHDRAW
// LP deposits exactly the amount that will be funded (100% utilization),
// then tries to withdraw all shares — the pool has zero available liquidity
// so this must panic with InsufficientLiquidity (#5).
#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_withdraw_all_shares_panics_when_insufficient_liquidity_at_full_utilization() {
    let te = setup();

    // face_value=10_000_000_000, discount_bps=200 → funded_amount=9_800_000_000
    // Deposit exactly 9.8B so funding uses 100% of available liquidity
    te.pool.deposit(&te.lp, &DEFAULT_FUNDED_AMOUNT);

    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);

    // Verify the pool is at 100% utilization (no available liquidity)
    let stats = te.pool.get_stats();
    assert_eq!(
        stats.available_liquidity, 0,
        "pool should have zero available liquidity"
    );
    assert_eq!(stats.total_funded, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(stats.total_deposits, DEFAULT_FUNDED_AMOUNT);

    // Attempt to withdraw all 9.8B shares when available = 0 → InsufficientLiquidity
    te.pool.withdraw(&te.lp, &DEFAULT_FUNDED_AMOUNT);
}

// LP deposits more than the funded amount, so some liquidity remains available.
// A partial withdraw within that available liquidity succeeds.
#[test]
fn test_partial_withdraw_succeeds_within_available_liquidity() {
    let te = setup();

    // face_value=10_000_000_000, discount_bps=200 → funded_amount=9_800_000_000
    // Deposit 15B so 5.2B remains available after funding
    te.pool.deposit(&te.lp, &15_000_000_000);

    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);

    // Verify available liquidity
    let stats = te.pool.get_stats();
    let available_liquidity = 15_000_000_000 - DEFAULT_FUNDED_AMOUNT;
    assert_eq!(stats.available_liquidity, available_liquidity);
    assert_eq!(stats.total_funded, DEFAULT_FUNDED_AMOUNT);

    // Partial withdraw of exactly the available amount should succeed
    let returned = te.pool.withdraw(&te.lp, &available_liquidity);
    // Capture before further queries: sdk >=22 test Envs reset the event
    // buffer at every top-level invocation.
    let events = te.env.events().all();
    assert_eq!(returned, available_liquidity);

    // Verify pool state after partial withdraw
    let stats_after = te.pool.get_stats();
    assert_eq!(stats_after.total_deposits, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(stats_after.total_shares, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(stats_after.available_liquidity, 0);

    // Verify LP position updated correctly
    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(pos.usdc_value, DEFAULT_FUNDED_AMOUNT);

    // Verify events
    // Event payload: (usdc_amount, shares_burned) — buffer captured above.
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "lp_withdrawn")
    );
    assert_eq!(
        Address::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        te.lp
    );
    assert_eq!(
        <(u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (available_liquidity, available_liquidity)
    );
}

// LP deposits more than the funded amount and withdraws less than the available
// liquidity — verifies that the LP can withdraw a portion while leaving some
// liquidity in the pool for other operations.
#[test]
fn test_partial_withdraw_leaves_remaining_liquidity() {
    let te = setup();

    // Deposit 20B, funding uses 9.8B → 10.2B available
    te.pool.deposit(&te.lp, &20_000_000_000);

    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);

    let stats = te.pool.get_stats();
    let available_liquidity = 20_000_000_000 - DEFAULT_FUNDED_AMOUNT;
    assert_eq!(stats.available_liquidity, available_liquidity);

    // Partial withdraw 5B out of 10.2B available
    let returned = te.pool.withdraw(&te.lp, &5_000_000_000);
    assert_eq!(returned, 5_000_000_000);

    // Remaining liquidity should be 5.2B (10.2B - 5B)
    let stats_after = te.pool.get_stats();
    assert_eq!(
        stats_after.available_liquidity,
        available_liquidity - 5_000_000_000
    );
    assert_eq!(stats_after.total_deposits, 15_000_000_000);
    assert_eq!(stats_after.total_shares, 15_000_000_000);

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 15_000_000_000);
}

//  ISSUE #268: WITHDRAW AFTER REPAYMENT (SHARE PRICE > 1)
// Covers the "yield grows share price" invariant end-to-end: deposit, fund,
// repay, then withdraw the same shares and confirm the LP is paid back more
// USDC than they put in, with the surplus exactly matching the discount
// portion of yield distributed on repayment.
#[test]
fn test_withdraw_after_repayment_returns_more_than_deposited() {
    let te = setup();
    let deposit_amount = 10_000_000_000u128;
    te.pool.deposit(&te.lp, &deposit_amount);
    fund_and_repay_invoice(&te);

    // face_value=10_000_000_000, discount_bps=200 (create_and_list defaults):
    // funded_amount = 10_000_000_000 * 9800 / 10000 = 9_800_000_000
    // yield = face_value - funded_amount = 200_000_000, all of which accrues
    // to this LP since they are the pool's sole depositor.
    let expected_yield = DEFAULT_YIELD_AMOUNT;

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, deposit_amount);
    assert_eq!(pos.usdc_value, deposit_amount + expected_yield);

    let usdc_returned = te.pool.withdraw(&te.lp, &pos.shares);
    // Capture before further queries: sdk >=22 test Envs reset the event
    // buffer at every top-level invocation.
    let events = te.env.events().all();

    assert!(
        usdc_returned > deposit_amount,
        "withdrawal after yield-generating repayment should return more than was deposited"
    );
    assert_eq!(usdc_returned, deposit_amount + expected_yield);
    assert_eq!(usdc_returned - deposit_amount, expected_yield);

    // Pool is fully drained: no shares or deposits remain.
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_shares, 0);
    assert_eq!(stats.total_deposits, 0);

    // The LP's realised yield is tracked for future reporting.
    let final_pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(final_pos.yield_earned, expected_yield);

    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "lp_withdrawn")
    );
    assert_eq!(
        Address::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        te.lp
    );
    assert_eq!(
        <(u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (usdc_returned, deposit_amount)
    );
}

//  ISSUE #263: CONSTRUCTION ADDRESS COLLISION GUARD
// A fresh, valid construction with four distinct addresses must keep working;
// `setup()` (used throughout this file) already exercises this path, but this
// test makes the positive case explicit for the collision guard added below.
#[test]
fn test_constructor_accepts_distinct_addresses() {
    let te = setup();
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_shares, 0);
    assert_eq!(stats.total_deposits, 0);
}

// Every pairwise collision among (admin, invoice_contract, escrow_contract,
// usdc_asset, registry_contract) must be rejected with InvalidConfiguration
// (#15) so the handle_default gate can never collide with the admin path.
#[test]
fn test_constructor_rejects_each_pairwise_address_collision() {
    // #827: the collision guard now runs inside the protocol constructor, so
    // a rejected deployment surfaces as a panic from `register_at` — the
    // guard fires before any cross-contract wiring is read, so placeholder
    // addresses are fine here.
    let pairs = [
        (0, 1),
        (0, 2),
        (0, 3),
        (0, 4),
        (1, 2),
        (1, 3),
        (1, 4),
        (2, 3),
        (2, 4),
        (3, 4),
    ];
    for (i, j) in pairs {
        let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let env = Env::default();
            env.mock_all_auths();
            let mut addrs = [
                Address::generate(&env),
                Address::generate(&env),
                Address::generate(&env),
                Address::generate(&env),
                Address::generate(&env),
            ];
            let colliding = addrs[i].clone();
            addrs[j] = colliding;
            env.register_at(
                &Address::generate(&env),
                PoolContract,
                (
                    addrs[0].clone(),
                    addrs[1].clone(),
                    addrs[2].clone(),
                    addrs[3].clone(),
                    addrs[4].clone(),
                    addrs[0].clone(),
                    DEFAULT_MIN_INITIAL_DEPOSIT,
                    String::from_str(&env, TEST_SHARE_NAME),
                    String::from_str(&env, TEST_SHARE_SYMBOL),
                    DEFAULT_SHARE_DECIMALS,
                ),
            );
        }))
        .is_err();
        assert!(
            rejected,
            "collision between constructor params {i} and {j} should be rejected"
        );
    }
}

#[test]
fn test_constructor_rejects_treasury_aliases() {
    // Each attempt gets a fresh pool address so the test covers treasury
    // aliases to the pool, invoice, escrow, registry, and funding asset. The
    // alias guard runs inside the constructor (before the escrow
    // cross-check), so a rejected deployment surfaces as a panic.
    for role in 0..5u8 {
        let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let env = Env::default();
            env.mock_all_auths();
            let addrs = [
                Address::generate(&env),
                Address::generate(&env),
                Address::generate(&env),
                Address::generate(&env),
                Address::generate(&env),
            ];
            let pool_id = Address::generate(&env);
            let treasury = match role {
                0 => pool_id.clone(),
                1 => addrs[1].clone(),
                2 => addrs[2].clone(),
                3 => addrs[4].clone(),
                _ => addrs[3].clone(),
            };
            env.register_at(
                &pool_id,
                PoolContract,
                (
                    addrs[0].clone(),
                    addrs[1].clone(),
                    addrs[2].clone(),
                    addrs[3].clone(),
                    addrs[4].clone(),
                    treasury,
                    DEFAULT_MIN_INITIAL_DEPOSIT,
                    String::from_str(&env, TEST_SHARE_NAME),
                    String::from_str(&env, TEST_SHARE_SYMBOL),
                    DEFAULT_SHARE_DECIMALS,
                ),
            );
        }))
        .is_err();
        assert!(rejected, "treasury alias for role {role} was accepted");
    }
}

//  ISSUE #265: PREVENT ALREADYFUNDED SILENT SHADOWING
// If a `FundedInvoice` entry already exists for an invoice id, fund_invoice
// must reject the call with AlreadyFunded (#16) instead of silently
// overwriting the prior entry (which would double-lock escrow funds and
// double-count active_invoice_count for a single invoice).
#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_fund_invoice_rejects_replay_when_funded_invoice_entry_exists() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    // Simulate a stale FundedInvoice entry already present for this invoice
    // id while the invoice itself is still Listed.
    let funded_key = DataKey::FundedInvoice(invoice_id.clone());
    te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().set(&funded_key, &1u128);
    });

    te.pool.fund_invoice(&invoice_id);
}

// The normal (non-replayed) funding path must be unaffected by the guard.
#[test]
fn test_fund_invoice_succeeds_when_no_prior_funded_entry() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    let result = te.pool.fund_invoice(&invoice_id);
    assert!(result);

    let funded_key = DataKey::FundedInvoice(invoice_id.clone());
    let funded_amount: u128 = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get(&funded_key).unwrap()
    });
    assert_eq!(funded_amount, DEFAULT_FUNDED_AMOUNT);
}

//  ISSUE #444: DOUBLE FUNDING / FUND AFTER SETTLEMENT
// Calling `fund_invoice` twice for the same invoice is rejected by the pool's
// own `FundedInvoice` guard with AlreadyFunded (#16). That guard is what stops
// a replay from double-locking escrow funds or double-counting
// `active_invoice_count` for one invoice.
#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_fund_invoice_twice_panics_already_funded() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    assert!(te.pool.fund_invoice(&invoice_id));
    // Replay: the FundedInvoice entry committed by the first call is the
    // authoritative record, so this must fail with AlreadyFunded rather than
    // silently funding again.
    te.pool.fund_invoice(&invoice_id);
}

// Once an invoice is repaid its funded entry has been settled (and removed)
// and the invoice is no longer Listed, so it can never be funded again.
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_fund_invoice_after_repayment_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = fund_and_repay_invoice(&te);
    assert_eq!(
        te.invoice.get(&invoice_id).status,
        trusttrove_invoice::InvoiceStatus::Repaid
    );

    te.pool.fund_invoice(&invoice_id);
}

// After default the funded entry has been removed and the invoice is
// Defaulted, so re-funding is likewise rejected.
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_fund_invoice_after_default_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    assert!(te.pool.fund_invoice(&invoice_id));

    // Move past the default grace period and settle the default.
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);
    assert!(te.pool.handle_default(&invoice_id));
    assert_eq!(
        te.invoice.get(&invoice_id).status,
        trusttrove_invoice::InvoiceStatus::Defaulted
    );

    te.pool.fund_invoice(&invoice_id);
}

//  ISSUE #281: INSTANCE TTL EXTENSION
// Every state-changing entrypoint must extend the contract's instance TTL so
// an active pool never expires. New instance storage entries start with a
// small default ttl (well above the extend_ttl threshold), so the
// constructor/set_max_utilization() calls in `setup()` are no-ops for the
// ttl bump. Drive the ttl down below the threshold here, then confirm a
// state-changing call (deposit) bumps it back up close to the configured
// extend-to window.
#[test]
fn test_deposit_extends_instance_ttl_when_below_threshold() {
    // The default instance TTL (4096) is below TTL_THRESHOLD (500_000),
    // so the constructor extends it to ~TTL_EXTEND_TO. #827 made deploy and
    // init atomic, so there is no observable pre-initialize instance to read
    // from — the assertion below pins that construction extended the TTL.
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let registry_id = env.register(MockRegistry, ());
    let usdc_id = env.register(MockToken, ());

    let pool_id = Address::generate(&env);
    let invoice_id = env.register(RealInvoice, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        RealEscrow,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );

    env.register_at(
        &pool_id,
        PoolContract,
        (
            admin.clone(),
            invoice_id.clone(),
            escrow_id.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            admin.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            String::from_str(&env, TEST_SHARE_NAME),
            String::from_str(&env, TEST_SHARE_SYMBOL),
            DEFAULT_SHARE_DECIMALS,
        ),
    );

    // After construction: TTL should be bumped to ~TTL_EXTEND_TO.
    let ttl_after = env.as_contract(&pool_id, || env.storage().instance().get_ttl());
    assert!(
        ttl_after >= 1_999_000,
        "instance ttl should be extended close to EXTEND_TO, got {ttl_after}"
    );
}

//  ISSUE #588: LP ENTRY TTL EXTENSION ON get_lp_position
// A deposit-and-hold LP only ever has LPShares/LPDepositCount/LPInitialDeposit
// TTLs set at deposit time. get_lp_position must refresh those entries on
// read, otherwise they lapse and become archival-eligible even though the
// position is still live from the pool's economic perspective.
#[test]
fn test_get_lp_position_extends_ttl_for_deposit_and_hold_lp() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let keys = [
        DataKey::LPShares(te.lp.clone()),
        DataKey::LPDepositCount(te.lp.clone()),
        DataKey::LPInitialDeposit(te.lp.clone()),
    ];

    // Drive the entries below the write-path threshold. All three were set at
    // the same deposit, so one key's remaining TTL gauges the drain for all.
    let ttl_before_drain: u32 = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get_ttl(&keys[0])
    });
    te.env
        .ledger()
        .set_sequence_number(te.env.ledger().sequence() + ttl_before_drain - 50);

    for key in &keys {
        let ttl_before_read: u32 = te
            .env
            .as_contract(&te.pool_id, || te.env.storage().persistent().get_ttl(key));
        assert!(
            ttl_before_read < TTL_THRESHOLD,
            "TTL should be below threshold before read, got {ttl_before_read}"
        );
    }

    // The read refreshes every surviving LP entry, not just LPShares.
    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 10_000_000_000);

    for key in &keys {
        let ttl_after_read: u32 = te
            .env
            .as_contract(&te.pool_id, || te.env.storage().persistent().get_ttl(key));
        assert!(
            ttl_after_read >= 1_999_000,
            "get_lp_position should extend TTL close to EXTEND_TO, got {ttl_after_read}"
        );
    }
}

// LPYieldEarned only appears after a withdrawal realizes yield, so the
// deposit-and-hold case above cannot cover it. Verify it is refreshed too.
#[test]
fn test_get_lp_position_extends_ttl_for_yield_earned_entry() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    fund_and_repay_invoice(&te);
    // Reap 10M of realized yield (5B shares @ 1.002 price) so the
    // LPYieldEarned entry is created.
    te.pool.withdraw(&te.lp, &5_000_000_000);
    let pos = te.pool.get_lp_position(&te.lp);
    assert!(
        pos.yield_earned > 0,
        "expected realized yield, got {}",
        pos.yield_earned
    );

    let yield_key = DataKey::LPYieldEarned(te.lp.clone());
    let ttl_before_drain: u32 = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get_ttl(&yield_key)
    });
    te.env
        .ledger()
        .set_sequence_number(te.env.ledger().sequence() + ttl_before_drain - 50);

    let ttl_before_read: u32 = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get_ttl(&yield_key)
    });
    assert!(
        ttl_before_read < TTL_THRESHOLD,
        "TTL should be below threshold before read, got {ttl_before_read}"
    );

    let pos = te.pool.get_lp_position(&te.lp);
    assert!(pos.yield_earned > 0);

    let ttl_after_read: u32 = te.env.as_contract(&te.pool_id, || {
        te.env.storage().persistent().get_ttl(&yield_key)
    });
    assert!(
        ttl_after_read >= 1_999_000,
        "get_lp_position should extend LPYieldEarned TTL close to EXTEND_TO, got {ttl_after_read}"
    );
}

//  ISSUE #865: DEPOSIT / WITHDRAW INSTANCE TTL EXTENSION (regression of #281)
// `deposit` and `withdraw` refresh the LP persistent entries they write but
// never bump the contract instance itself, so a pool whose LP activity is
// only deposits/withdrawals lets the instance (admin, contract wiring, all
// totals) lapse toward expiry. Drive the instance TTL below the write-path
// threshold — keeping the mock token's own entries alive across the jump —
// and confirm each entrypoint bumps the instance back near EXTEND_TO.

/// Extend the mock token's instance and the LP/pool balance keys to the same
/// headroom the pool instance carries after `setup()`, so the ledger jump
/// below ages the pool instance below `TTL_THRESHOLD` without expiring the
/// token contract the transfers cross into.
fn keep_mock_token_alive_across_jump(te: &TestEnv) {
    let usdc = te.usdc_id.clone();
    let lp = te.lp.clone();
    let pool_id = te.pool_id.clone();
    te.env.as_contract(&usdc, || {
        te.env
            .storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
        for key in [TKey(lp.clone()), TKey(pool_id.clone())] {
            if te.env.storage().persistent().has(&key) {
                te.env
                    .storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
            }
        }
    });
}

#[test]
fn test_deposit_extends_instance_ttl_after_ledger_drain() {
    let te = setup();
    keep_mock_token_alive_across_jump(&te);

    // Everything extended at setup time (~initialize) has ~EXTEND_TO left,
    // so one jump drives the pool instance — and only the pool instance
    // matters for this assertion — down to ~50 ledgers.
    let ttl_before_drain: u32 = te
        .env
        .as_contract(&te.pool_id, || te.env.storage().instance().get_ttl());
    te.env
        .ledger()
        .set_sequence_number(te.env.ledger().sequence() + ttl_before_drain - 50);

    let ttl_before_deposit: u32 = te
        .env
        .as_contract(&te.pool_id, || te.env.storage().instance().get_ttl());
    assert!(
        ttl_before_deposit < TTL_THRESHOLD,
        "instance ttl should be below threshold before deposit, got {ttl_before_deposit}"
    );

    te.pool.deposit(&te.lp, &10_000_000_000);

    let ttl_after_deposit: u32 = te
        .env
        .as_contract(&te.pool_id, || te.env.storage().instance().get_ttl());
    assert!(
        ttl_after_deposit >= 1_999_000,
        "deposit should extend instance ttl close to EXTEND_TO, got {ttl_after_deposit}"
    );
}

#[test]
fn test_withdraw_extends_instance_ttl_after_ledger_drain() {
    let te = setup();
    // Establish an LP position (and the pool's token balance) before aging.
    te.pool.deposit(&te.lp, &10_000_000_000);
    keep_mock_token_alive_across_jump(&te);

    let ttl_before_drain: u32 = te
        .env
        .as_contract(&te.pool_id, || te.env.storage().instance().get_ttl());
    te.env
        .ledger()
        .set_sequence_number(te.env.ledger().sequence() + ttl_before_drain - 50);

    let ttl_before_withdraw: u32 = te
        .env
        .as_contract(&te.pool_id, || te.env.storage().instance().get_ttl());
    assert!(
        ttl_before_withdraw < TTL_THRESHOLD,
        "instance ttl should be below threshold before withdraw, got {ttl_before_withdraw}"
    );

    // Partial withdrawal: 4B of the 10B shares, price 1:1 so no yield path.
    let returned = te.pool.withdraw(&te.lp, &4_000_000_000);
    assert_eq!(returned, 4_000_000_000);

    let ttl_after_withdraw: u32 = te
        .env
        .as_contract(&te.pool_id, || te.env.storage().instance().get_ttl());
    assert!(
        ttl_after_withdraw >= 1_999_000,
        "withdraw should extend instance ttl close to EXTEND_TO, got {ttl_after_withdraw}"
    );
}

//  ISSUE #273: MISSING-CONFIG REGRESSION COVERAGE
// (#827: `test_double_initialize_panics` was removed — with deploy and init
// atomic there is no second initialization to reject; `AlreadyInitialized`
// is unreachable by construction.)
#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_deposit_before_initialize_panics() {
    // #827: deploy is atomic; the missing-config state this test targets is
    // reproduced by evicting the constructor's instance keys.
    let (env, pool_id, pool) = deploy_pool_with_evicted_config();
    let lp = Address::generate(&env);

    // Mock LP auth but the pool config is missing → must panic with
    // NotInitialized (#2).
    env.mock_auths(&[MockAuth {
        address: &lp,
        invoke: &MockAuthInvoke {
            contract: &pool_id,
            fn_name: "deposit",
            args: (lp.clone(), 10_000_000_000u128).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    pool.deposit(&lp, &10_000_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_withdraw_before_initialize_panics() {
    let (env, pool_id, pool) = deploy_pool_with_evicted_config();
    let lp = Address::generate(&env);

    // Mock LP auth but the pool config is missing → must panic with
    // NotInitialized (#2).
    env.mock_auths(&[MockAuth {
        address: &lp,
        invoke: &MockAuthInvoke {
            contract: &pool_id,
            fn_name: "withdraw",
            args: (lp.clone(), 1_000u128).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    pool.withdraw(&lp, &1_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_fund_invoice_before_initialize_panics() {
    let (env, _pool_id, pool) = deploy_pool_with_evicted_config();
    let invoice_id = BytesN::from_array(&env, &[0u8; 32]);

    // The pool config is missing → must panic with NotInitialized (#2).
    pool.fund_invoice(&invoice_id);
}

// --------------- Real Registry Integration ---------------
//
// Every other test in this file uses MockRegistry, a hand-rolled stand-in
// for registry-style verification. This test instead deploys the real
// RegistryContract alongside real invoice/escrow/pool contracts and drives
// a full register -> verify -> create -> list -> fund -> repay lifecycle
// through it, so the actual cross-contract is_verified call (argument
// shape, NotInitialized/NotFound panic semantics) is exercised against
// production code rather than the mock. Refs: issue #631.
mod real_registry_integration {
    use super::*;
    use soroban_sdk::{map, Map, String};
    use trusttrove_registry::{
        RegistryContract as RealRegistry, RegistryContractClient as RealRegistryClient,
        VerificationStatus,
    };

    #[test]
    fn test_real_registry_revoke_reinstate_gates_create_and_funding() {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&env);
        let issuer = Address::generate(&env);
        let buyer = Address::generate(&env);
        let lp = Address::generate(&env);

        // --- Deploy the real registry and drive register -> verify ---
        // #827: the registry is deployed already constructed.
        let registry_id = env.register(RealRegistry, (admin.clone(),));
        let registry = RealRegistryClient::new(&env, &registry_id);

        let metadata: Map<String, String> = map![
            &env,
            (
                String::from_str(&env, "name"),
                String::from_str(&env, "test")
            )
        ];
        registry.register_issuer(&issuer, &metadata);
        registry.register_buyer(&buyer, &metadata);

        // Newly registered profiles start unverified (#130) — must be
        // explicitly verified by the admin before is_verified() returns true.
        assert!(!registry.is_verified(&issuer));
        assert!(!registry.is_verified(&buyer));
        registry.verify_profile(&admin, &issuer, &true);
        registry.verify_profile(&admin, &buyer, &true);
        assert!(registry.is_verified(&issuer));
        assert!(registry.is_verified(&buyer));
        assert_eq!(
            registry.get_verification_status(&issuer),
            VerificationStatus::Verified
        );
        assert_eq!(
            registry.get_verification_status(&buyer),
            VerificationStatus::Verified
        );

        // --- Deploy real invoice, escrow, pool wired to the real registry ---
        let usdc_id = env.register(MockToken, ());
        let lp_bal_key = TKey(lp.clone());
        env.as_contract(&usdc_id, || {
            env.storage()
                .persistent()
                .set(&lp_bal_key, &100_000_000_000_000i128);
        });
        let buyer_bal_key = TKey(buyer.clone());
        env.as_contract(&usdc_id, || {
            env.storage()
                .persistent()
                .set(&buyer_bal_key, &100_000_000_000_000i128);
        });

        let pool_id = Address::generate(&env);

        let invoice_id_addr = env.register(RealInvoice, (admin.clone(), registry_id.clone()));
        let escrow_id = env.register(
            RealEscrow,
            (admin.clone(), pool_id.clone(), usdc_id.clone()),
        );

        let invoice = RealInvoiceClient::new(&env, &invoice_id_addr);
        let _escrow = RealEscrowClient::new(&env, &escrow_id);

        env.register_at(
            &pool_id,
            PoolContract,
            (
                admin.clone(),
                invoice_id_addr.clone(),
                escrow_id.clone(),
                usdc_id.clone(),
                registry_id.clone(),
                admin.clone(),
                DEFAULT_MIN_INITIAL_DEPOSIT,
                String::from_str(&env, TEST_SHARE_NAME),
                String::from_str(&env, TEST_SHARE_SYMBOL),
                DEFAULT_SHARE_DECIMALS,
            ),
        );
        let pool = PoolContractClient::new(&env, &pool_id);

        invoice.add_supported_asset(&usdc_id);
        invoice.set_pool_contract(&pool_id);
        invoice.set_escrow_contract(&escrow_id);
        pool.set_max_utilization(&admin, &10000);

        let agent_registry_id = env.register(MockAgentRegistry, ());
        let agent_registry = MockAgentRegistryClient::new(&env, &agent_registry_id);
        agent_registry.register_agent(
            &test_agent_id(&env),
            &trusttrove_invoice::Agent {
                active: true,
                pubkey: test_agent_pubkey(&env),
            },
        );
        invoice.set_agent_registry_contract(&agent_registry_id);

        // A profile revoked before invoice creation is rejected by the real
        // invoice contract, then becomes eligible again after reinstate.
        let face_value: u128 = 10_000_000_000;
        let discount_bps: u32 = 200;
        let due_date = env.ledger().timestamp() + 86400;

        registry.revoke(&issuer);
        assert_eq!(
            registry.get_verification_status(&issuer),
            VerificationStatus::Revoked
        );
        let create_while_revoked =
            invoice.try_create(&issuer, &buyer, &face_value, &due_date, &usdc_id);
        assert!(create_while_revoked.is_err());
        assert!(
            std::format!("{create_while_revoked:?}").contains("#4"),
            "revoked issuer should fail invoice.create with IssuerNotVerified: {create_while_revoked:?}"
        );
        registry.reinstate(&issuer);
        assert_eq!(
            registry.get_verification_status(&issuer),
            VerificationStatus::Verified
        );

        pool.deposit(&lp, &(face_value * 3));

        let invoice_id = invoice.create(&issuer, &buyer, &face_value, &due_date, &usdc_id);

        let payload = trusttrove_invoice::AttestationPayload {
            domain_separator: BytesN::from_array(
                &env,
                &trusttrove_invoice::ATTESTATION_DOMAIN_SEPARATOR,
            ),
            invoice_id: invoice_id.clone(),
            contract_address: invoice.address.clone(),
            network_id: env.ledger().network_id(),
            risk_score: 5000,
            evidence_hash: BytesN::from_array(&env, &[9u8; 32]),
            agent_id: test_agent_id(&env),
            nonce: 1,
            valid_until: u64::MAX,
        };
        let payload_bytes = payload.to_xdr(&env);
        let digest = env.crypto().keccak256(&payload_bytes).to_array();
        let (sig, recid) = test_agent_signing_key()
            .sign_prehash_recoverable(&digest)
            .unwrap();
        let mut sig_bytes = [0u8; 65];
        sig_bytes[..64].copy_from_slice(&sig.to_bytes());
        sig_bytes[64] = recid.to_byte();
        let signature = BytesN::from_array(&env, &sig_bytes);
        invoice.submit_attestation(&invoice_id, &payload_bytes, &signature);

        invoice.list_for_financing(&invoice_id, &discount_bps);

        // Revocation after listing blocks pool funding. The failed cross-
        // contract call must leave pool accounting exactly as it was.
        let stats_before_issuer_rejection = pool.get_stats();
        registry.revoke(&issuer);
        assert_eq!(
            registry.get_verification_status(&issuer),
            VerificationStatus::Revoked
        );
        let issuer_funding_rejection = pool.try_fund_invoice(&invoice_id);
        assert!(issuer_funding_rejection.is_err());
        assert!(
            std::format!("{issuer_funding_rejection:?}").contains("#18"),
            "revoked issuer should fail pool funding with IssuerNotVerified: {issuer_funding_rejection:?}"
        );
        let stats_after_issuer_rejection = pool.get_stats();
        assert_eq!(
            stats_after_issuer_rejection.total_deposits,
            stats_before_issuer_rejection.total_deposits
        );
        assert_eq!(
            stats_after_issuer_rejection.total_funded,
            stats_before_issuer_rejection.total_funded
        );
        assert_eq!(
            stats_after_issuer_rejection.active_invoice_count,
            stats_before_issuer_rejection.active_invoice_count
        );
        assert_eq!(
            stats_after_issuer_rejection.available_liquidity,
            stats_before_issuer_rejection.available_liquidity
        );
        registry.reinstate(&issuer);
        assert_eq!(
            registry.get_verification_status(&issuer),
            VerificationStatus::Verified
        );

        let funded = pool.fund_invoice(&invoice_id);
        assert!(funded);

        // Repeat the listing/funding gate for a revoked buyer.
        let second_due_date = env.ledger().timestamp() + 86400;
        let second_invoice_id =
            invoice.create(&issuer, &buyer, &face_value, &second_due_date, &usdc_id);
        let second_payload = trusttrove_invoice::AttestationPayload {
            domain_separator: BytesN::from_array(
                &env,
                &trusttrove_invoice::ATTESTATION_DOMAIN_SEPARATOR,
            ),
            invoice_id: second_invoice_id.clone(),
            contract_address: invoice.address.clone(),
            network_id: env.ledger().network_id(),
            risk_score: 5000,
            evidence_hash: BytesN::from_array(&env, &[8u8; 32]),
            agent_id: test_agent_id(&env),
            nonce: 2,
            valid_until: u64::MAX,
        };
        let second_payload_bytes = second_payload.to_xdr(&env);
        let second_digest = env.crypto().keccak256(&second_payload_bytes).to_array();
        let (second_sig, second_recid) = test_agent_signing_key()
            .sign_prehash_recoverable(&second_digest)
            .unwrap();
        let mut second_sig_bytes = [0u8; 65];
        second_sig_bytes[..64].copy_from_slice(&second_sig.to_bytes());
        second_sig_bytes[64] = second_recid.to_byte();
        let second_signature = BytesN::from_array(&env, &second_sig_bytes);
        invoice.submit_attestation(&second_invoice_id, &second_payload_bytes, &second_signature);
        invoice.list_for_financing(&second_invoice_id, &discount_bps);

        let stats_before_buyer_rejection = pool.get_stats();
        registry.revoke(&buyer);
        assert_eq!(
            registry.get_verification_status(&buyer),
            VerificationStatus::Revoked
        );
        let buyer_funding_rejection = pool.try_fund_invoice(&second_invoice_id);
        assert!(buyer_funding_rejection.is_err());
        assert!(
            std::format!("{buyer_funding_rejection:?}").contains("#19"),
            "revoked buyer should fail pool funding with BuyerNotVerified: {buyer_funding_rejection:?}"
        );
        let stats_after_buyer_rejection = pool.get_stats();
        assert_eq!(
            stats_after_buyer_rejection.total_deposits,
            stats_before_buyer_rejection.total_deposits
        );
        assert_eq!(
            stats_after_buyer_rejection.total_funded,
            stats_before_buyer_rejection.total_funded
        );
        assert_eq!(
            stats_after_buyer_rejection.active_invoice_count,
            stats_before_buyer_rejection.active_invoice_count
        );
        assert_eq!(
            stats_after_buyer_rejection.available_liquidity,
            stats_before_buyer_rejection.available_liquidity
        );
        registry.reinstate(&buyer);
        assert_eq!(
            registry.get_verification_status(&buyer),
            VerificationStatus::Verified
        );
        assert!(pool.fund_invoice(&second_invoice_id));

        let record = invoice.get(&invoice_id);
        assert_eq!(record.status, trusttrove_invoice::InvoiceStatus::Funded);

        invoice.mark_shipped(&invoice_id);
        invoice.confirm_delivery(&invoice_id, &issuer);
        invoice.confirm_delivery(&invoice_id, &buyer);

        let record = invoice.get(&invoice_id);
        assert_eq!(record.status, trusttrove_invoice::InvoiceStatus::Confirmed);

        invoice.repay(&invoice_id);

        let record = invoice.get(&invoice_id);
        assert_eq!(record.status, trusttrove_invoice::InvoiceStatus::Repaid);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #")]
    fn test_real_registry_is_verified_panics_when_not_initialized() {
        // Confirms the real registry's is_verified semantics: on an
        // uninitialized (or unregistered) address it returns `false` rather
        // than panicking, which invoice.create()'s require_verified() then
        // turns into an IssuerNotVerified panic. This is the behavior
        // invoice's require_verified relies on — it must match the mock's
        // unwrap_or(false) behavior used everywhere else in this file.
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&env);
        let issuer = Address::generate(&env);
        let buyer = Address::generate(&env);

        let registry_id = env.register(RealRegistry, (admin.clone(),));
        // #827: deploy is atomic, so the registry's missing-config state is
        // reproduced by evicting the key the constructor seeds.
        env.as_contract(&registry_id, || {
            env.storage()
                .instance()
                .remove(&trusttrove_registry::DataKey::Admin);
        });

        let usdc_id = env.register(MockToken, ());
        let invoice_id_addr = env.register(RealInvoice, (admin.clone(), registry_id.clone()));
        let invoice = RealInvoiceClient::new(&env, &invoice_id_addr);
        invoice.add_supported_asset(&usdc_id);

        let due_date = env.ledger().timestamp() + 86400;
        invoice.create(&issuer, &buyer, &10_000_000_000u128, &due_date, &usdc_id);
    }
}

//  CHECKS-EFFECTS-INTERACTIONS TESTS (issue #576)
#[test]
fn test_fund_invoice_commits_state_before_cross_contract_calls() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    // Verify initial state
    let stats_before = te.pool.get_stats();
    assert_eq!(stats_before.total_funded, 0);
    assert_eq!(stats_before.active_invoice_count, 0);

    // Fund the invoice
    let result = te.pool.fund_invoice(&invoice_id);
    assert!(result);

    // Verify pool state is correctly updated after funding.
    // This test documents that the checks-effects-interactions reorder
    // produces the same end-state as before: TotalFunded and
    // ActiveInvoiceCount are updated atomically with FundedInvoice.
    let stats_after = te.pool.get_stats();
    assert_eq!(stats_after.total_funded, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(stats_after.active_invoice_count, 1);
    assert_eq!(
        stats_after.available_liquidity,
        stats_before.total_deposits - DEFAULT_FUNDED_AMOUNT
    );
}

#[test]
fn test_fund_invoice_prevents_double_funding_via_funded_key_check() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    // First funding succeeds
    let result = te.pool.fund_invoice(&invoice_id);
    assert!(result);

    // Verify the FundedInvoice entry exists in persistent storage,
    // which is now committed before cross-contract calls.
    let funded_amount: u128 = te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .persistent()
            .get(&DataKey::FundedInvoice(invoice_id.clone()))
            .unwrap_or(0)
    });
    assert_eq!(funded_amount, DEFAULT_FUNDED_AMOUNT);

    // Second funding attempt is rejected - the AlreadyFunded guard
    // reads from persistent storage that was committed before
    // the cross-contract calls in the first funding.
    let result = te.pool.try_fund_invoice(&invoice_id);
    assert!(result.is_err());
}

//  CONSTRUCTION EVENT TESTS (issue #575)
#[test]
fn test_constructor_emits_pool_initialized_event() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let registry_id = env.register(MockRegistry, ());
    let usdc_id = env.register(MockToken, ());
    let pool_addr = Address::generate(&env);

    // #827: the constructor emits `pool_initialized` as part of the
    // deployment transaction; the test env retains constructor events.
    let invoice_id = env.register(RealInvoice, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        RealEscrow,
        (admin.clone(), pool_addr.clone(), usdc_id.clone()),
    );
    env.register_at(
        &pool_addr,
        PoolContract,
        (
            admin.clone(),
            invoice_id.clone(),
            escrow_id.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            admin.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            String::from_str(&env, TEST_SHARE_NAME),
            String::from_str(&env, TEST_SHARE_SYMBOL),
            DEFAULT_SHARE_DECIMALS,
        ),
    );

    let events = env.events().all();
    let mut found = false;
    for i in 0..events.len() {
        let (contract, topics, _data) = events.get(i).unwrap();
        if contract == pool_addr {
            let symbol = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
            if symbol == Symbol::new(&env, "pool_initialized") {
                assert_eq!(
                    Address::try_from_val(&env, &topics.get(1).unwrap()).unwrap(),
                    admin
                );
                assert_eq!(
                    Address::try_from_val(&env, &topics.get(2).unwrap()).unwrap(),
                    invoice_id
                );
                assert_eq!(
                    Address::try_from_val(&env, &topics.get(3).unwrap()).unwrap(),
                    escrow_id
                );
                assert_eq!(
                    Address::try_from_val(&env, &topics.get(4).unwrap()).unwrap(),
                    usdc_id
                );
                found = true;
                break;
            }
        }
    }
    assert!(found, "pool_initialized event not found");
}

//  PUBLIC GETTER TESTS (issue #578)
#[test]
fn test_get_admin_returns_correct_address() {
    let te = setup();
    assert_eq!(te.pool.get_admin(), te.admin);
}

#[test]
fn test_get_invoice_contract_returns_correct_address() {
    let te = setup();
    assert_eq!(te.pool.get_invoice_contract(), te.invoice.address);
}

#[test]
fn test_get_escrow_contract_returns_correct_address() {
    let te = setup();
    assert_eq!(te.pool.get_escrow_contract(), te.escrow_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_admin_panics_when_uninitialized() {
    let (_env, _pool_id, pool) = deploy_pool_with_evicted_config();
    let _ = pool.get_admin();
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_invoice_contract_panics_when_uninitialized() {
    let (_env, _pool_id, pool) = deploy_pool_with_evicted_config();
    let _ = pool.get_invoice_contract();
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_escrow_contract_panics_when_uninitialized() {
    let (_env, _pool_id, pool) = deploy_pool_with_evicted_config();
    let _ = pool.get_escrow_contract();
}

//  ISSUE #441: TYPED NotInitialized ERRORS //
// Every instance-storage read in the pool contract must fail with the typed
// `PoolError::NotInitialized` (#2) when the config keys are missing, never an
// untyped host panic. These tests cover the remaining entry points
// and views after the bare `unwrap()` / `expect()` reads were replaced
// (`get_admin`, `get_invoice_contract`, `get_escrow_contract` and
// `get_usdc_asset` are pinned alongside; `deposit` and `withdraw` already had
// typed-error tests). #973 pinned the same error family for every path that
// goes through the three dependency-address helpers: `fund_invoice`,
// `handle_default` and `get_usdc_asset`.

fn uninitialized_pool() -> (Env, PoolContractClient<'static>) {
    // #827: constructed-then-evicted config (see deploy_pool_with_evicted_config)
    // stands in for the pre-#827 never-initialized deployment.
    let (env, _pool_id, pool) = deploy_pool_with_evicted_config();
    (env, pool)
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_fee_bps_panics_with_typed_not_initialized() {
    let (_env, pool) = uninitialized_pool();
    pool.get_fee_bps();
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_treasury_address_panics_with_typed_not_initialized() {
    let (_env, pool) = uninitialized_pool();
    pool.get_treasury_address();
}

/// `get_treasury` falls back to the stored admin when no dedicated treasury is
/// configured, so its panic path is the admin read — it must be typed too.
#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_treasury_panics_with_typed_not_initialized() {
    let (_env, pool) = uninitialized_pool();
    pool.get_treasury();
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_receive_repayment_panics_with_typed_not_initialized() {
    let (env, pool) = uninitialized_pool();
    let invoice_id = BytesN::from_array(&env, &[0u8; 32]);
    pool.receive_repayment(&invoice_id, &1_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_receive_repayment_with_refund_panics_with_typed_not_initialized() {
    let (env, pool) = uninitialized_pool();
    let invoice_id = BytesN::from_array(&env, &[0u8; 32]);
    let party = Address::generate(&env);
    pool.receive_repayment_with_refund(&invoice_id, &1_000, &0, &party);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_fund_invoice_panics_with_typed_not_initialized() {
    let (env, pool) = uninitialized_pool();
    let invoice_id = BytesN::from_array(&env, &[0u8; 32]);
    pool.fund_invoice(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_handle_default_panics_with_typed_not_initialized() {
    let (env, pool) = uninitialized_pool();
    let invoice_id = BytesN::from_array(&env, &[0u8; 32]);
    pool.handle_default(&invoice_id);
}

// The three dependency-address helpers (`invoice_contract`, `escrow_contract`,
// `funding_asset`) all surface the same typed error family before
// initialization (#973): `fund_invoice`, `handle_default` and `get_usdc_asset`
// must each abort with `PoolError::NotInitialized` (#2) rather than an untyped
// host panic from a raw `expect`.
#[test]
fn test_pre_initialization_dependency_helpers_share_typed_error_family() {
    let (env, pool) = uninitialized_pool();
    let invoice_id = BytesN::from_array(&env, &[0u8; 32]);

    let fund_invoice_error = std::format!("{:?}", pool.try_fund_invoice(&invoice_id));
    assert!(
        fund_invoice_error.contains("#2"),
        "fund_invoice must fail with PoolError::NotInitialized: {fund_invoice_error}"
    );

    let handle_default_error = std::format!("{:?}", pool.try_handle_default(&invoice_id));
    assert!(
        handle_default_error.contains("#2"),
        "handle_default must fail with PoolError::NotInitialized: {handle_default_error}"
    );

    let get_usdc_asset_error = std::format!("{:?}", pool.try_get_usdc_asset());
    assert!(
        get_usdc_asset_error.contains("#2"),
        "get_usdc_asset must fail with PoolError::NotInitialized: {get_usdc_asset_error}"
    );
}

//  CONSTANTS LOCATION TESTS (issue #592)
// DEFAULT_MIN_INITIAL_DEPOSIT and DEFAULT_MAX_UTILIZATION_BPS must be importable from
// `constants` (re-exported via `pub use constants::*` in lib.rs) and hold the
// canonical values.
#[test]
fn test_min_initial_deposit_constant_value() {
    assert_eq!(DEFAULT_MIN_INITIAL_DEPOSIT, 10_000_000);
}

#[test]
fn test_default_max_utilization_bps_constant_value() {
    use crate::DEFAULT_MAX_UTILIZATION_BPS;
    assert_eq!(DEFAULT_MAX_UTILIZATION_BPS, 8500);
}

//  WITHDRAW DUST-GUARD TESTS (issue #593)
// Withdraw must reject when the computed USDC redemption rounds down to zero,
// just as deposit rejects when the computed share count rounds down to zero.
// This prevents an LP from burning shares for nothing.
#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_withdraw_rejects_dust_shares_returning_zero_usdc() {
    // To make (shares * total_deposits) / total_shares == 0 we need
    // shares * total_deposits < total_shares.
    // Set total_shares very large and total_deposits very small so a
    // withdrawal of 1 share rounds down to 0 USDC.
    let te = setup();
    let total_shares: u128 = 1_000_000_000_000;
    let total_deposits: u128 = 1;

    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalShares, &total_shares);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &total_deposits);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalFunded, &0u128);
        te.env
            .storage()
            .persistent()
            .set(&DataKey::LPShares(te.lp.clone()), &total_shares);
    });

    // 1 * 1 / 1_000_000_000_000 == 0 USDC -> must be rejected.
    te.pool.withdraw(&te.lp, &1);
}

// Dust-guard must not over-reject: a withdrawal that returns at least 1 USDC
// must succeed even when the share price is very small.
#[test]
fn test_withdraw_dust_guard_does_not_reject_nonzero_return() {
    let te = setup();
    // Standard deposit: 1 share == 1 stroop, returns > 0.
    te.pool.deposit(&te.lp, &10_000_000_000);
    let usdc = te.pool.withdraw(&te.lp, &1);
    assert!(
        usdc >= 1,
        "single-share withdraw must return at least 1 stroop"
    );
}

// A rejected dust withdrawal must leave pool state and LP shares unchanged.
#[test]
fn test_withdraw_dust_rejection_preserves_state() {
    let te = setup();
    let total_shares: u128 = 1_000_000_000_000;
    let total_deposits: u128 = 1;

    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalShares, &total_shares);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalDeposits, &total_deposits);
        te.env
            .storage()
            .instance()
            .set(&DataKey::TotalFunded, &0u128);
        te.env
            .storage()
            .persistent()
            .set(&DataKey::LPShares(te.lp.clone()), &total_shares);
    });

    let before = te.pool.get_stats();
    let res = te.pool.try_withdraw(&te.lp, &1);
    assert!(res.is_err(), "dust withdraw should be rejected");

    let after = te.pool.get_stats();
    assert_eq!(after.total_shares, before.total_shares);
    assert_eq!(after.total_deposits, before.total_deposits);
}

//  CHECKED SUBTRACTION TESTS (issue #594)
// handle_default: TotalFunded subtraction must not panic on valid data and must
// correctly reduce TotalFunded while booking the issuer-paid principal as an
// LP capital loss in TotalDeposits (#439 as revisited by #819).
#[test]
fn test_handle_default_total_funded_decremented_correctly() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let before = te.pool.get_stats();
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);
    te.pool.handle_default(&invoice_id);

    let after = te.pool.get_stats();
    assert_eq!(
        after.total_funded,
        before.total_funded - DEFAULT_FUNDED_AMOUNT,
        "TotalFunded must decrease by funded_amount"
    );
    // #439 pinned "deposits preserved"; issue #819 supersedes that for the
    // standard flow: the principal was already paid to the issuer at funding
    // time, so defaulting books a real LP capital loss and TotalDeposits
    // shrinks by the funded amount while TotalFunded is unwound.
    assert_eq!(
        after.total_deposits,
        before.total_deposits - DEFAULT_FUNDED_AMOUNT,
        "TotalDeposits must shrink by the unrecoverable funded_amount"
    );
}

// settle_repayment (via receive_repayment): TotalFunded subtraction must
// correctly reduce TotalFunded back to zero after a full repayment.
#[test]
fn test_settle_repayment_total_funded_decremented_correctly() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let before = te.pool.get_stats();
    assert_eq!(before.total_funded, DEFAULT_FUNDED_AMOUNT);

    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);
    te.invoice.repay(&invoice_id);

    let after = te.pool.get_stats();
    assert_eq!(
        after.total_funded, 0,
        "TotalFunded must be zero after full repayment"
    );
}

//  PROPERTY-BASED TESTS (issue #100) // Uses proptest's TestRunner API directly so rustfmt formats normally.
// Case budget is 10 per property to stay within CI time budgets for the
// Soroban in-process host.

// Any valid deposit amount must result in at least 1 share when the pool is
// empty (1:1 ratio), and the resulting share count must equal the deposit.
#[test]
fn prop_any_valid_initial_deposit_issues_shares_equal_to_amount() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(
            &(DEFAULT_MIN_INITIAL_DEPOSIT..=1_000_000_000_000u128),
            |deposit_amount| {
                let te = setup();
                let shares = te.pool.deposit(&te.lp, &deposit_amount);
                prop_assert_eq!(
                    shares,
                    deposit_amount,
                    "initial deposit must mint shares 1:1"
                );
                let stats = te.pool.get_stats();
                prop_assert_eq!(stats.total_shares, deposit_amount);
                prop_assert_eq!(stats.total_deposits, deposit_amount);
                Ok(())
            },
        )
        .unwrap();
}

// A deposit below DEFAULT_MIN_INITIAL_DEPOSIT on an empty pool must always be rejected
// with InvalidAmount (#4), regardless of the exact value.
#[test]
fn prop_initial_deposit_below_minimum_always_rejected() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u128..DEFAULT_MIN_INITIAL_DEPOSIT), |deposit_amount| {
            let te = setup();
            let result = te.pool.try_deposit(&te.lp, &deposit_amount);
            prop_assert!(
                result.is_err(),
                "deposit of {deposit_amount} below minimum must be rejected"
            );
            Ok(())
        })
        .unwrap();
}

// For any withdrawal of all shares after a deposit, the USDC returned must
// equal the deposited amount (no yield, no loss).
#[test]
fn prop_full_withdrawal_returns_exact_deposit_with_no_yield() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(
            &(DEFAULT_MIN_INITIAL_DEPOSIT..=1_000_000_000_000u128),
            |deposit_amount| {
                let te = setup();
                let shares = te.pool.deposit(&te.lp, &deposit_amount);
                let usdc_returned = te.pool.withdraw(&te.lp, &shares);
                prop_assert_eq!(
                    usdc_returned,
                    deposit_amount,
                    "full withdrawal must return exact deposit"
                );
                let stats = te.pool.get_stats();
                prop_assert_eq!(stats.total_shares, 0);
                prop_assert_eq!(stats.total_deposits, 0);
                Ok(())
            },
        )
        .unwrap();
}

// Once repayment raises the share price above 1, a depositor's immediate
// deposit and full withdrawal must not extract value from existing LPs.
#[test]
fn prop_deposit_then_withdraw_never_returns_more_than_deposited_above_par() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(
            &(
                20_000_000_000u128..=1_000_000_000_000u128,
                1u32..=500u32,
                DEFAULT_MIN_INITIAL_DEPOSIT..=1_000_000_000_000u128,
            ),
            |(initial_deposit, discount_bps, deposit_amount)| {
                let te = setup();
                te.pool.deposit(&te.lp, &initial_deposit);

                let invoice_id = create_and_list_with_params(
                    &te,
                    &te.usdc_id,
                    DEFAULT_FACE_VALUE,
                    discount_bps,
                );
                te.pool.fund_invoice(&invoice_id);
                te.invoice.mark_shipped(&invoice_id);
                te.invoice.confirm_delivery(&invoice_id, &te.issuer);
                te.invoice.confirm_delivery(&invoice_id, &te.buyer);
                te.env
                    .ledger()
                    .set_timestamp(te.env.ledger().timestamp() + 86401);
                te.invoice.repay(&invoice_id);

                let stats_before = te.pool.get_stats();
                prop_assert!(
                    stats_before.total_deposits > stats_before.total_shares,
                    "repayment must raise the share price above 1"
                );
                let existing_lp_value_before = te.pool.get_lp_position(&te.lp).usdc_value;
                let depositor = create_lp_with_balance(&te, 100_000_000_000_000);
                let shares_minted = te.pool.deposit(&depositor, &deposit_amount);
                let usdc_returned = te.pool.withdraw(&depositor, &shares_minted);

                prop_assert!(
                    usdc_returned <= deposit_amount,
                    "withdrawal returned {usdc_returned} for deposit {deposit_amount}"
                );
                prop_assert!(
                    stats_before.total_shares > 0,
                    "share price is undefined when there are no shares"
                );
                let ceil_share_price = stats_before
                    .total_deposits
                    .div_ceil(stats_before.total_shares);
                prop_assert!(
                    deposit_amount - usdc_returned <= ceil_share_price,
                    "round-trip loss {} exceeds ceil share price {ceil_share_price}",
                    deposit_amount - usdc_returned
                );
                let existing_lp_value_after = te.pool.get_lp_position(&te.lp).usdc_value;
                prop_assert!(
                    existing_lp_value_after >= existing_lp_value_before,
                    "existing LP value decreased from {existing_lp_value_before} to {existing_lp_value_after}"
                );
                Ok(())
            },
        )
        .unwrap();
}

// After a full withdrawal the LP's position must be empty: zero shares,
// zero USDC value, deposit count zeroed.
#[test]
fn prop_full_withdrawal_clears_lp_position() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(
            &(DEFAULT_MIN_INITIAL_DEPOSIT..=1_000_000_000_000u128),
            |deposit_amount| {
                let te = setup();
                let shares = te.pool.deposit(&te.lp, &deposit_amount);
                te.pool.withdraw(&te.lp, &shares);
                let pos = te.pool.get_lp_position(&te.lp);
                prop_assert_eq!(pos.shares, 0);
                prop_assert_eq!(pos.usdc_value, 0);
                prop_assert_eq!(pos.deposit_count, 0);
                Ok(())
            },
        )
        .unwrap();
}

// Deposit followed by repayment: total_deposits must increase by the yield
// amount, and total_funded must return to zero.
#[test]
fn prop_repayment_increases_deposits_by_yield_and_clears_funded() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u32..=500u32), |discount_bps| {
            let te = setup();
            te.pool.deposit(&te.lp, &100_000_000_000);
            let face_value: u128 = 10_000_000_000;
            let funded_amount = face_value * (10000 - discount_bps as u128) / 10000;
            let yield_amount = face_value * discount_bps as u128 / 10000;
            let invoice_id =
                create_and_list_with_params(&te, &te.usdc_id, face_value, discount_bps);
            te.pool.fund_invoice(&invoice_id);

            let before = te.pool.get_stats();
            prop_assert_eq!(before.total_funded, funded_amount);

            te.invoice.mark_shipped(&invoice_id);
            te.invoice.confirm_delivery(&invoice_id, &te.issuer);
            te.invoice.confirm_delivery(&invoice_id, &te.buyer);
            te.env
                .ledger()
                .set_timestamp(te.env.ledger().timestamp() + 86401);
            te.invoice.repay(&invoice_id);

            let after = te.pool.get_stats();
            prop_assert_eq!(after.total_funded, 0);
            prop_assert_eq!(
                after.total_deposits,
                before.total_deposits - funded_amount + face_value,
                "deposits must grow by yield_amount after repayment"
            );
            prop_assert_eq!(after.total_yield_distributed, yield_amount);
            Ok(())
        })
        .unwrap();
}

//  ISSUE #771: NONZERO PROTOCOL FEE SPLITS
#[test]
fn test_freshly_constructed_pool_has_zero_protocol_fee() {
    let te = setup();
    assert_eq!(te.pool.get_protocol_fee_bps(), 0);
    assert_eq!(te.pool.get_treasury(), te.admin);
}

#[test]
fn test_nonzero_protocol_fee_splits_receive_repayment() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let treasury = Address::generate(&te.env);
    let fee_bps = 1000u32; // 10% protocol fee (1000 bps)
    te.pool.set_protocol_fee(&fee_bps, &treasury);

    assert_eq!(te.pool.get_protocol_fee_bps(), 1000);
    assert_eq!(te.pool.get_treasury(), treasury);

    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let treasury_before = usdc.balance(&treasury);
    let before_stats = te.pool.get_stats();

    let amount = DEFAULT_FACE_VALUE;
    let yield_amount = DEFAULT_YIELD_AMOUNT;
    let expected_protocol_cut = yield_amount * (fee_bps as u128) / 10_000;
    let expected_lp_yield = yield_amount - expected_protocol_cut;

    let result = te.pool.receive_repayment(&invoice_id, &amount);
    assert!(result);

    let treasury_after = usdc.balance(&treasury);
    let after_stats = te.pool.get_stats();

    // Assert treasury address's USDC balance increased by exactly protocol_cut
    assert_eq!(
        treasury_after - treasury_before,
        expected_protocol_cut as i128
    );

    // Assert TotalYieldDistributed increased by exactly lp_yield
    assert_eq!(
        after_stats.total_yield_distributed - before_stats.total_yield_distributed,
        expected_lp_yield
    );
    assert_eq!(
        after_stats.total_deposits - before_stats.total_deposits,
        expected_lp_yield
    );
}

#[test]
fn test_nonzero_protocol_fee_splits_receive_repayment_with_refund() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let treasury = Address::generate(&te.env);
    let fee_bps = 500u32; // 5% protocol fee (500 bps)
    te.pool.set_protocol_fee(&fee_bps, &treasury);

    assert_eq!(te.pool.get_protocol_fee_bps(), 500);
    assert_eq!(te.pool.get_treasury(), treasury);

    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let treasury_before = usdc.balance(&treasury);
    let before_stats = te.pool.get_stats();

    let amount = DEFAULT_FACE_VALUE;
    let refund = 100_000_000u128; // 100M refund to buyer out of 200M surplus
    let yield_amount = amount - DEFAULT_FUNDED_AMOUNT - refund; // 100_000_000
    let expected_protocol_cut = yield_amount * (fee_bps as u128) / 10_000; // 5_000_000
    let expected_lp_yield = yield_amount - expected_protocol_cut; // 95_000_000

    let result = te
        .pool
        .receive_repayment_with_refund(&invoice_id, &amount, &refund, &te.buyer);
    assert!(result);

    let treasury_after = usdc.balance(&treasury);
    let after_stats = te.pool.get_stats();

    // Assert treasury address's USDC balance increased by exactly protocol_cut
    assert_eq!(
        treasury_after - treasury_before,
        expected_protocol_cut as i128
    );

    // Assert TotalYieldDistributed increased by exactly lp_yield
    assert_eq!(
        after_stats.total_yield_distributed - before_stats.total_yield_distributed,
        expected_lp_yield
    );
    assert_eq!(
        after_stats.total_deposits - before_stats.total_deposits,
        expected_lp_yield
    );
}

//  ISSUE #846: PROPT — FEE SPLIT CONSERVES YIELD
// For every fee_bps in 0..=MAX_PROTOCOL_FEE_BPS and every yield settled
// through `settle_repayment`, the protocol cut plus the yield credited back
// to LPs must reassemble the yield that arrived: the fee split only
// redistributes yield between treasury and LPs, it never mints or destroys
// any. Uses proptest's TestRunner API directly so rustfmt formats normally;
// the case budget matches the other property tests (10) to stay within CI
// time budgets for the Soroban in-process host.
#[test]
fn prop_protocol_fee_split_conserves_yield_for_all_fee_bps() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(
            &(0i128..=10_000_000_000_000i128, 0u32..=MAX_PROTOCOL_FEE_BPS),
            |(yield_amount, fee_bps)| {
                let te = setup();
                te.pool.deposit(&te.lp, &100_000_000_000);
                let invoice_id = create_and_list(&te, &te.usdc_id);
                te.pool.fund_invoice(&invoice_id);

                let treasury = Address::generate(&te.env);
                te.pool.set_protocol_fee(&fee_bps, &treasury);

                let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
                let treasury_before = usdc.balance(&treasury);
                let before = te.pool.get_stats();

                // Repay the funded principal plus the arbitrary yield under
                // test, so `settle_repayment` sees exactly `yield_amount` of
                // surplus to split.
                let amount = DEFAULT_FUNDED_AMOUNT + (yield_amount as u128);
                let settled = te.pool.receive_repayment(&invoice_id, &amount);
                prop_assert!(settled);

                let treasury_delta = usdc.balance(&treasury) - treasury_before;
                let after = te.pool.get_stats();
                let yield_delta =
                    (after.total_yield_distributed - before.total_yield_distributed) as i128;
                let deposits_delta = (after.total_deposits - before.total_deposits) as i128;

                let protocol_cut = (yield_amount * (fee_bps as i128)) / 10_000;
                let lp_yield = yield_amount - protocol_cut;

                // Yield conservation: the two halves of the split reassemble
                // the whole yield, both as computed and as observed in the
                // treasury balance and TotalYieldDistributed.
                prop_assert_eq!(protocol_cut + lp_yield, yield_amount);
                prop_assert_eq!(treasury_delta + yield_delta, yield_amount);

                // The cut can never exceed fee_bps of the yield.
                prop_assert!(
                    protocol_cut <= (yield_amount * (fee_bps as i128)) / 10_000,
                    "protocol cut exceeds bps cut"
                );

                // Treasury balance delta equals the protocol cut.
                prop_assert_eq!(treasury_delta, protocol_cut);

                // TotalYieldDistributed delta equals the LP yield.
                prop_assert_eq!(yield_delta, lp_yield);

                // TotalDeposits grows by exactly the LP share of the yield.
                prop_assert_eq!(deposits_delta, lp_yield);

                // Zero-fee edge case: nothing may reach the treasury and the
                // full yield must land in TotalDeposits.
                if fee_bps == 0 {
                    prop_assert_eq!(treasury_delta, 0);
                    prop_assert_eq!(deposits_delta, yield_amount);
                }

                Ok(())
            },
        )
        .unwrap();
}
//  ISSUE #774: GAS BENCHMARK FOR DEPOSIT / WITHDRAW
#[test]
fn test_gas_benchmark_deposit_and_withdraw() {
    extern crate std;
    let te = setup();
    let env = &te.env;

    // Measure deposit() resource cost
    env.cost_estimate().budget().reset_default();
    let cpu_before_deposit = env.cost_estimate().budget().cpu_instruction_cost();
    let mem_before_deposit = env.cost_estimate().budget().memory_bytes_cost();
    let shares = te.pool.deposit(&te.lp, &10_000_000_000);
    let cpu_after_deposit = env.cost_estimate().budget().cpu_instruction_cost();
    let mem_after_deposit = env.cost_estimate().budget().memory_bytes_cost();

    let deposit_cpu = cpu_after_deposit - cpu_before_deposit;
    let deposit_mem = mem_after_deposit - mem_before_deposit;

    // Measure withdraw() resource cost
    env.cost_estimate().budget().reset_default();
    let cpu_before_withdraw = env.cost_estimate().budget().cpu_instruction_cost();
    let mem_before_withdraw = env.cost_estimate().budget().memory_bytes_cost();
    let returned = te.pool.withdraw(&te.lp, &(shares / 2));
    let cpu_after_withdraw = env.cost_estimate().budget().cpu_instruction_cost();
    let mem_after_withdraw = env.cost_estimate().budget().memory_bytes_cost();

    let withdraw_cpu = cpu_after_withdraw - cpu_before_withdraw;
    let withdraw_mem = mem_after_withdraw - mem_before_withdraw;

    std::println!(
        "\n== GAS BENCHMARK ==\ndeposit():  CPU instructions: {}, Memory bytes: {}\nwithdraw(): CPU instructions: {}, Memory bytes: {}\n=====",
        deposit_cpu,
        deposit_mem,
        withdraw_cpu,
        withdraw_mem
    );

    assert!(deposit_cpu > 0);
    assert!(deposit_mem > 0);
    assert!(withdraw_cpu > 0);
    assert!(withdraw_mem > 0);
    assert_eq!(returned, 5_000_000_000);
}

//  ISSUE #772: NEGATIVE-AUTH FOR SET_PROTOCOL_FEE
// set_protocol_fee must reject callers other than the admin (#772), matching
// the pattern used by set_max_utilization.
#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_set_protocol_fee_requires_admin_authorization() {
    let te = setup();
    let treasury = Address::generate(&te.env);

    // Clear all mocked auths so the caller's require_auth() fails.
    te.env.set_auths(&[]);
    te.pool.set_protocol_fee(&500, &treasury);
}

#[test]
#[should_panic(expected = "Error(Contract, #22)")]
fn test_set_protocol_fee_above_max_cap_panics() {
    let te = setup();
    let treasury = Address::generate(&te.env);
    te.pool.set_protocol_fee(&2001, &treasury);
}

#[test]
fn test_set_protocol_fee_at_max_cap_succeeds() {
    let te = setup();
    let treasury = Address::generate(&te.env);
    let ok = te.pool.set_protocol_fee(&2000, &treasury);
    assert!(ok);
    assert_eq!(te.pool.get_protocol_fee_bps(), 2000);
    assert_eq!(te.pool.get_treasury(), treasury);
}

#[test]
fn test_set_protocol_fee_rejects_treasury_aliases() {
    let te = setup();
    let forbidden_treasuries = [
        te.pool_id.clone(),
        te.invoice.address.clone(),
        te.escrow_id.clone(),
        te.registry.address.clone(),
        te.usdc_id.clone(),
    ];

    for treasury in forbidden_treasuries {
        assert!(
            te.pool.try_set_protocol_fee(&500, &treasury).is_err(),
            "treasury alias was accepted"
        );
    }
}

//  ISSUE #770: DEFAULT-ZERO PROTOCOL FEE ACCOUNTING REGRESSION
#[test]
fn test_default_zero_protocol_fee_preserves_repayment_accounting_unchanged() {
    let te = setup();
    // Verify default protocol fee is zero and treasury is te.admin
    assert_eq!(te.pool.get_protocol_fee_bps(), 0);
    assert_eq!(te.pool.get_treasury(), te.admin);

    let initial_deposit = 100_000_000_000u128;
    let shares = te.pool.deposit(&te.lp, &initial_deposit);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let treasury = te.pool.get_treasury();
    let treasury_before = usdc.balance(&treasury);
    let before_stats = te.pool.get_stats();

    let amount = DEFAULT_FACE_VALUE; // 1_200_000_000
    let yield_amount = DEFAULT_YIELD_AMOUNT; // 200_000_000

    let result = te.pool.receive_repayment(&invoice_id, &amount);
    assert!(result);

    let treasury_after = usdc.balance(&treasury);
    let after_stats = te.pool.get_stats();

    // At default 0 bps fee, treasury cut is exactly 0
    assert_eq!(
        treasury_after, treasury_before,
        "treasury balance must remain completely unchanged at default 0 bps fee"
    );

    // 100% of yield goes to LPs: TotalYieldDistributed and TotalDeposits increase by full yield_amount
    assert_eq!(
        after_stats.total_yield_distributed - before_stats.total_yield_distributed,
        yield_amount,
        "TotalYieldDistributed must increase by full yield_amount"
    );
    assert_eq!(
        after_stats.total_deposits - before_stats.total_deposits,
        yield_amount,
        "TotalDeposits must increase by full yield_amount"
    );
    assert_eq!(after_stats.total_funded, 0);

    // LP withdrawing all shares receives initial_deposit + yield_amount (pre-fee identical behavior)
    let returned = te.pool.withdraw(&te.lp, &shares);
    assert_eq!(returned, initial_deposit + yield_amount);
}

#[test]
fn test_default_zero_protocol_fee_preserves_refunded_repayment_accounting_unchanged() {
    let te = setup();
    assert_eq!(te.pool.get_protocol_fee_bps(), 0);

    let initial_deposit = 100_000_000_000u128;
    te.pool.deposit(&te.lp, &initial_deposit);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let treasury = te.pool.get_treasury();
    let treasury_before = usdc.balance(&treasury);
    let before_stats = te.pool.get_stats();

    let amount = DEFAULT_FACE_VALUE; // 1_200_000_000
    let refund = 50_000_000u128;
    let expected_yield = amount - DEFAULT_FUNDED_AMOUNT - refund; // 150_000_000

    let result = te
        .pool
        .receive_repayment_with_refund(&invoice_id, &amount, &refund, &te.buyer);
    assert!(result);

    let treasury_after = usdc.balance(&treasury);
    let after_stats = te.pool.get_stats();

    // Treasury cut must be zero
    assert_eq!(treasury_after, treasury_before);

    // Full expected yield goes to LPs
    assert_eq!(
        after_stats.total_yield_distributed - before_stats.total_yield_distributed,
        expected_yield
    );
    assert_eq!(
        after_stats.total_deposits - before_stats.total_deposits,
        expected_yield
    );
}

//  ISSUE #765: PROTOCOL FEE STORAGE AND INITIALIZATION
#[test]
fn test_protocol_fee_storage_initialized_to_zero_and_admin_treasury() {
    let te = setup();
    // Freshly constructed pool must read back fee_bps == 0 and treasury == admin
    assert_eq!(te.pool.get_protocol_fee_bps(), 0);
    assert_eq!(te.pool.get_treasury(), te.admin);
}

#[test]
fn test_set_protocol_fee_updates_both_stored_values_and_reads_back() {
    let te = setup();
    let treasury = Address::generate(&te.env);
    let fee_bps = 750u32; // 7.5%

    let updated = te.pool.set_protocol_fee(&fee_bps, &treasury);
    assert!(updated);

    assert_eq!(te.pool.get_protocol_fee_bps(), 750);
    assert_eq!(te.pool.get_treasury(), treasury);
}

#[test]
fn test_protocol_fee_storage_initialized_with_custom_treasury() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let usdc_id = env.register(MockToken, ());
    let registry_id = env.register(MockRegistry, ());
    let custom_treasury = Address::generate(&env);

    let pool_id = Address::generate(&env);
    let invoice_id = env.register(RealInvoice, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        RealEscrow,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );
    env.register_at(
        &pool_id,
        PoolContract,
        (
            admin.clone(),
            invoice_id.clone(),
            escrow_id.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            custom_treasury.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            String::from_str(&env, TEST_SHARE_NAME),
            String::from_str(&env, TEST_SHARE_SYMBOL),
            DEFAULT_SHARE_DECIMALS,
        ),
    );
    let pool = PoolContractClient::new(&env, &pool_id);

    assert_eq!(pool.get_protocol_fee_bps(), 0);
    assert_eq!(pool.get_treasury(), custom_treasury);
}

#[test]
fn test_sep41_balance_and_total_supply() {
    let te = setup();
    let client = PoolContractClient::new(&te.env, &te.pool_id);

    let unknown_lp = Address::generate(&te.env);

    // Check total supply
    let initial_supply = client.total_supply();
    assert_eq!(initial_supply, 0);

    // Check unknown LP balance
    let unknown_balance = client.balance(&unknown_lp);
    assert_eq!(unknown_balance, 0);

    // Check known LP balance after deposit
    client.deposit(&te.lp, &100_000_000);

    let supply_after = client.total_supply();
    assert_eq!(supply_after, 100_000_000);

    let lp_balance = client.balance(&te.lp);
    assert_eq!(lp_balance, 100_000_000);
}

//  ISSUE #756: SEP-41 ALLOWANCE INTERFACE
// approve() records a grant that allowance() reports until it is spent,
// revoked, or expires.
#[test]
fn test_approve_sets_allowance_until_expiration() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let spender = Address::generate(&te.env);
    let expires = te.env.ledger().sequence() + 100;

    assert_eq!(te.pool.allowance(&te.lp, &spender), 0);
    te.pool.approve(&te.lp, &spender, &5_000_000_000, &expires);
    assert_eq!(te.pool.allowance(&te.lp, &spender), 5_000_000_000);

    // A second approve overwrites the grant (SEP-41 semantics) rather than
    // adding to it, and it can move the expiry.
    te.pool
        .approve(&te.lp, &spender, &1_000_000_000, &(expires + 50));
    assert_eq!(te.pool.allowance(&te.lp, &spender), 1_000_000_000);

    // The grant is per (owner, spender) pair: another spender sees nothing.
    assert_eq!(te.pool.allowance(&te.lp, &Address::generate(&te.env)), 0);
}

// The whole point of the allowance: a spender moves shares, the grant shrinks
// by exactly what moved, and nobody's total share supply changes.
#[test]
fn test_transfer_from_decrements_allowance() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let spender = Address::generate(&te.env);
    let recipient = Address::generate(&te.env);
    let expires = te.env.ledger().sequence() + 100;
    te.pool.approve(&te.lp, &spender, &5_000_000_000, &expires);

    te.pool
        .transfer_from(&spender, &te.lp, &recipient, &2_000_000_000);

    assert_eq!(te.pool.allowance(&te.lp, &spender), 3_000_000_000);
    assert_eq!(te.pool.get_lp_position(&recipient).shares, 2_000_000_000);
    assert_eq!(te.pool.get_lp_position(&te.lp).shares, 8_000_000_000);
    // Moving shares must not mint or burn them.
    assert_eq!(te.pool.get_stats().total_shares, 10_000_000_000);

    // Spending the rest takes the grant to exactly zero.
    te.pool
        .transfer_from(&spender, &te.lp, &recipient, &3_000_000_000);
    assert_eq!(te.pool.allowance(&te.lp, &spender), 0);
    assert_eq!(te.pool.get_lp_position(&recipient).shares, 5_000_000_000);
}

#[test]
#[should_panic(expected = "Error(Contract, #24)")]
fn test_transfer_from_fails_once_allowance_exhausted() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let spender = Address::generate(&te.env);
    let recipient = Address::generate(&te.env);
    let expires = te.env.ledger().sequence() + 100;
    te.pool.approve(&te.lp, &spender, &1_000, &expires);
    te.pool.transfer_from(&spender, &te.lp, &recipient, &1_000);

    // Grant fully spent: the next move is rejected, and it is rejected before
    // any share leaves the owner.
    te.pool.transfer_from(&spender, &te.lp, &recipient, &1);
}

#[test]
fn test_allowance_reads_zero_after_expiration() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let spender = Address::generate(&te.env);
    let expires = te.env.ledger().sequence() + 10;
    te.pool.approve(&te.lp, &spender, &5_000_000_000, &expires);
    assert_eq!(te.pool.allowance(&te.lp, &spender), 5_000_000_000);

    // The grant is still live on its last ledger...
    te.env
        .ledger()
        .set_sequence_number(te.env.ledger().sequence() + 10);
    assert_eq!(te.pool.allowance(&te.lp, &spender), 5_000_000_000);

    // ...and dead from the next one onwards, without anyone having to prune it.
    te.env
        .ledger()
        .set_sequence_number(te.env.ledger().sequence() + 1);
    assert_eq!(te.pool.allowance(&te.lp, &spender), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #24)")]
fn test_transfer_from_rejects_expired_allowance() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let spender = Address::generate(&te.env);
    let recipient = Address::generate(&te.env);
    let expires = te.env.ledger().sequence() + 5;
    te.pool.approve(&te.lp, &spender, &5_000_000_000, &expires);
    te.env
        .ledger()
        .set_sequence_number(te.env.ledger().sequence() + 6);

    te.pool.transfer_from(&spender, &te.lp, &recipient, &1);
}

#[test]
fn test_approve_with_zero_amount_revokes_allowance() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let spender = Address::generate(&te.env);
    let recipient = Address::generate(&te.env);
    let expires = te.env.ledger().sequence() + 100;
    te.pool.approve(&te.lp, &spender, &5_000_000_000, &expires);

    // Revoking needs no future expiry: amount 0 is the revoke signal.
    te.pool.approve(&te.lp, &spender, &0, &0);
    assert_eq!(te.pool.allowance(&te.lp, &spender), 0);
    assert!(te
        .pool
        .try_transfer_from(&spender, &te.lp, &recipient, &1)
        .is_err());
}

#[test]
#[should_panic(expected = "Error(Contract, #25)")]
fn test_approve_with_expiration_in_the_past_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let spender = Address::generate(&te.env);
    te.pool
        .approve(&te.lp, &spender, &1_000, &te.env.ledger().sequence());
}

#[test]
#[should_panic(expected = "Error(Contract, #24)")]
fn test_transfer_from_rejects_spender_without_grant() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let stranger = Address::generate(&te.env);
    let recipient = Address::generate(&te.env);
    te.pool.transfer_from(&stranger, &te.lp, &recipient, &1_000);
}

// approve() is a state change LPs and spenders both need to observe off-chain,
// so it emits `allowance_approved` with the new total grant and its expiry.
#[test]
fn test_approve_emits_allowance_approved_event() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let spender = Address::generate(&te.env);
    let expires = te.env.ledger().sequence() + 100;
    te.pool.approve(&te.lp, &spender, &5_000_000_000, &expires);

    let events = te.env.events().all();
    // sdk >=22: approve's own buffer holds exactly its allowance_approved event.
    assert_eq!(events.len(), 1);
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(topics.len(), 2);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "allowance_approved")
    );
    assert_eq!(
        Address::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        te.lp
    );
    assert_eq!(
        <(Address, i128, u32)>::try_from_val(&te.env, &data).unwrap(),
        (spender.clone(), 5_000_000_000, expires)
    );

    // A rejected approve (expiry already passed) must not emit either.
    assert!(te
        .pool
        .try_approve(&te.lp, &spender, &1_000, &te.env.ledger().sequence())
        .is_err());
    assert_eq!(te.env.events().all().len(), 0);
}

//  ISSUE #757: SEP-41 SHARE METADATA
#[test]
fn test_share_metadata_comes_from_constructor() {
    let te = setup();
    assert_eq!(te.pool.name(), String::from_str(&te.env, TEST_SHARE_NAME));
    assert_eq!(
        te.pool.symbol(),
        String::from_str(&te.env, TEST_SHARE_SYMBOL)
    );
    // Shares are denominated in the funding asset's base units, so the value
    // configured at construction is what `decimals()` reports.
    assert_eq!(te.pool.decimals(), DEFAULT_SHARE_DECIMALS);

    // Metadata lives on the instance, written once by the constructor.
    te.env.as_contract(&te.pool_id, || {
        let stored_name: String = te
            .env
            .storage()
            .instance()
            .get(&DataKey::ShareName)
            .unwrap();
        let stored_symbol: String = te
            .env
            .storage()
            .instance()
            .get(&DataKey::ShareSymbol)
            .unwrap();
        assert_eq!(stored_name, String::from_str(&te.env, TEST_SHARE_NAME));
        assert_eq!(stored_symbol, String::from_str(&te.env, TEST_SHARE_SYMBOL));
    });
}

// The per-asset `pool_factory` model is the reason metadata is a constructor
// argument instead of a constant: two pool instances are two different share
// tokens and must not render identically in a wallet.
#[test]
fn test_share_metadata_is_scoped_to_the_pool_instance() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let usdc_id = env.register(MockToken, ());
    let registry_id = env.register(MockRegistry, ());
    let pool_id = Address::generate(&env);

    let invoice_id = env.register(RealInvoice, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        RealEscrow,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );

    env.register_at(
        &pool_id,
        PoolContract,
        (
            admin.clone(),
            invoice_id.clone(),
            escrow_id.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            admin.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            String::from_str(&env, "TrusTrove XLM Pool Shares"),
            String::from_str(&env, "TT-XLM"),
            12u32,
        ),
    );
    let pool = PoolContractClient::new(&env, &pool_id);

    assert_eq!(
        pool.name(),
        String::from_str(&env, "TrusTrove XLM Pool Shares")
    );
    assert_eq!(pool.symbol(), String::from_str(&env, "TT-XLM"));
    // A non-7 value proves decimals is per-instance state, not a constant.
    assert_eq!(pool.decimals(), 12);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")]
fn test_constructor_rejects_empty_share_symbol() {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let usdc_id = env.register(MockToken, ());
    let registry_id = env.register(MockRegistry, ());
    let pool_id = Address::generate(&env);

    let invoice_id = env.register(RealInvoice, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        RealEscrow,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );

    // The empty-symbol guard runs inside the constructor, so the deployment
    // itself must fail with InvalidConfiguration (#15).
    env.register_at(
        &pool_id,
        PoolContract,
        (
            admin.clone(),
            invoice_id.clone(),
            escrow_id.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            admin.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            String::from_str(&env, "TrusTrove USDC Pool Shares"),
            String::from_str(&env, ""),
            DEFAULT_SHARE_DECIMALS,
        ),
    );
}

#[test]
fn test_share_metadata_unreadable_without_config() {
    // #827: missing-config state reproduced by evicting the constructor keys.
    let (_env, _pool_id, pool) = deploy_pool_with_evicted_config();

    assert!(pool.try_name().is_err());
    assert!(pool.try_symbol().is_err());
    // `decimals` falls back to the 7-decimal default instead of panicking, so
    // pools that predate the parameter still render sensibly.
    assert_eq!(pool.decimals(), DEFAULT_SHARE_DECIMALS);
}

//  ISSUE #758: DEPOSIT ROUTES THROUGH mint()
// The `mint()` extraction must be invisible from the outside: same shares, same
// bookkeeping, and exactly one event with the same payload shape.
#[test]
fn test_deposit_mints_through_shared_helper_without_changing_behaviour() {
    let te = setup();

    let shares = te.pool.deposit(&te.lp, &10_000_000_000);
    assert_eq!(shares, 10_000_000_000);

    // One event, unchanged: `lp_deposited(lp, usdc_amount, shares_issued)` —
    // in deposit's own per-invocation buffer (sdk >=22).
    let events = te.env.events().all();
    assert_eq!(events.len(), 1);
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(topics.len(), 2);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "lp_deposited")
    );
    assert_eq!(
        <(u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (10_000_000_000u128, 10_000_000_000u128)
    );

    // mint() bumps the total and the LP's balance together...
    assert_eq!(te.pool.get_stats().total_shares, 10_000_000_000);
    assert_eq!(te.pool.get_lp_position(&te.lp).shares, 10_000_000_000);

    // ...and a second deposit through the same helper is additive.
    te.pool.deposit(&te.lp, &5_000_000_000);
    assert_eq!(te.pool.get_stats().total_shares, 15_000_000_000);
    assert_eq!(te.pool.get_lp_position(&te.lp).shares, 15_000_000_000);
}

// `total_shares` in stats and the LP's share balance must agree, since that is
// the invariant `mint()`/`burn()` now own.
#[test]
fn test_share_supply_stays_consistent_across_mint_and_burn() {
    let te = setup();
    let other = Address::generate(&te.env);
    te.pool.deposit(&te.lp, &10_000_000_000);

    // top up the mock token for a second LP
    te.env.as_contract(&te.usdc_id, || {
        te.env
            .storage()
            .persistent()
            .set(&TKey(other.clone()), &100_000_000_000_000i128);
    });
    te.pool.deposit(&other, &20_000_000_000);

    assert_eq!(te.pool.get_stats().total_shares, 30_000_000_000);
    assert_eq!(
        te.pool.get_lp_position(&te.lp).shares + te.pool.get_lp_position(&other).shares,
        30_000_000_000
    );

    te.pool.withdraw(&te.lp, &4_000_000_000);
    assert_eq!(te.pool.get_stats().total_shares, 26_000_000_000);
    assert_eq!(te.pool.get_lp_position(&te.lp).shares, 6_000_000_000);
}

//  ISSUE #767: protocol_fee_updated EVENT
// A fee/treasury change is high-impact for LPs, so it must be observable
// without polling `get_protocol_fee_bps`, and the event must carry the *old*
// value so an indexer can report what changed.
#[test]
fn test_set_protocol_fee_emits_event_with_old_and_new_values() {
    let te = setup();
    let treasury = Address::generate(&te.env);

    te.pool.set_protocol_fee(&750, &treasury);

    let events = te.env.events().all();
    // sdk >=22: this buffer holds only the set_protocol_fee call's event.
    assert_eq!(events.len(), 1);
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(topics.len(), 1);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "protocol_fee_updated")
    );
    // setup() initialized the fee to 0 bps, so the first change reports 0 -> 750.
    assert_eq!(
        <(u32, u32, Address)>::try_from_val(&te.env, &data).unwrap(),
        (0u32, 750u32, treasury.clone())
    );

    // A second change reports the value it replaced, not the original one.
    te.pool.set_protocol_fee(&1500, &treasury);
    let events = te.env.events().all();
    let (_contract, _topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(
        <(u32, u32, Address)>::try_from_val(&te.env, &data).unwrap(),
        (750u32, 1500u32, treasury.clone())
    );

    // A rejected change (above MAX_PROTOCOL_FEE_BPS) must not emit anything,
    // and must leave the stored fee at 1500.
    assert!(te.pool.try_set_protocol_fee(&2001, &treasury).is_err());
    // A rejected change emits nothing in its own per-invocation buffer.
    assert_eq!(te.env.events().all().len(), 0);
    assert_eq!(te.pool.get_protocol_fee_bps(), 1500);
}

// Re-pointing the treasury is reported in the same event even when the fee
// itself does not change, so monitoring sees the destination move.
#[test]
fn test_set_protocol_fee_event_reports_treasury_change_with_unchanged_fee() {
    let te = setup();
    let treasury = Address::generate(&te.env);
    te.pool.set_protocol_fee(&500, &treasury);

    let new_treasury = Address::generate(&te.env);
    te.pool.set_protocol_fee(&500, &new_treasury);

    let events = te.env.events().all();
    let (_contract, _topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(
        <(u32, u32, Address)>::try_from_val(&te.env, &data).unwrap(),
        (500u32, 500u32, new_treasury.clone())
    );
    assert_eq!(te.pool.get_treasury(), new_treasury);
    assert_eq!(te.pool.get_protocol_fee_bps(), 500);
}

#[test]
fn test_shared_fixture_setup_full_protocol() {
    let proto = setup_full_protocol();
    assert_eq!(proto.pool.get_admin(), proto.admin);
    assert_eq!(proto.pool.get_stats().total_funded, 0);
    assert_eq!(proto.invoice.get_admin(), Some(proto.admin));
}

//  ISSUE #842: MULTI-LP SHARE-SUPPLY CONSERVATION //
// The share ledger lives in two places: `total_supply()` (instance
// `TotalShares`) and each LP's `balance()` (persistent `LPShares`). Deposit
// and withdraw must move both together; `transfer`/`transfer_from` must move
// neither. The property below drives random interleavings of all four entry
// points across 3-5 LPs and re-checks the invariants after every step, so a
// sequence can never mint, burn, or strand a share unnoticed.

// Case budget matches the other properties in this file (see the note at the
// top of the property-test section) to stay within CI time for the in-process
// Soroban host.
const PROP_CASES: u32 = 10;
const PROP_MAX_STEPS: usize = 12;
const PROP_SOLVENCY_MAX_STEPS: usize = 8;

// Amount bound: wide enough that a single step can exceed any balance built
// up earlier in the sequence (driving the rejected-step paths), and far below
// anything that could overflow the pool's u128 scaling.
const PROP_MAX_AMOUNT: u128 = 10_000_000_000_000;

// Every generated LP starts with more USDC than the whole sequence can spend,
// so deposits fail on pool rules rather than on the mock token's balance.
const PROP_LP_USDC: i128 = 100_000_000_000_000;

// Largest LP set a case can generate; cases use the first `lp_count` slots.
const PROP_MAX_LPS: usize = 5;

// Opening position each LP takes before the random sequence starts: above
// `DEFAULT_MIN_INITIAL_DEPOSIT`, small enough that random steps routinely
// exceed it in both directions.
const PROP_WARMUP_DEPOSIT: u128 = 1_000_000_000;

/// One step of a generated sequence. LP selectors are raw bytes reduced
/// modulo the case's LP count, so every value addresses a real participant.
#[derive(Clone, Debug)]
enum LpStep {
    Deposit {
        lp: u8,
        amount: u128,
    },
    Withdraw {
        lp: u8,
        shares: u128,
    },
    Transfer {
        from: u8,
        to: u8,
        amount: u128,
    },
    TransferFrom {
        owner: u8,
        spender: u8,
        to: u8,
        amount: u128,
        // Re-approve the grant for `amount` right before the move (SEP-41
        // `approve` overwrites), so some steps start from a fresh allowance
        // and others spend whatever earlier grant is still live.
        fresh_grant: bool,
    },
}

/// Outcome of one step, normalised across the four `try_*` return types so
/// the property loop has a single match to reason about.
enum StepOutcome {
    Succeeded,
    Rejected(Result<soroban_sdk::Error, InvokeError>),
}

fn classify<T, E>(
    res: Result<Result<T, E>, Result<soroban_sdk::Error, InvokeError>>,
) -> StepOutcome {
    match res {
        Ok(_) => StepOutcome::Succeeded,
        Err(err) => StepOutcome::Rejected(err),
    }
}

/// Share/USDC sizes come in two buckets: a small one that usually fits inside
/// an existing balance (keeping the accepted withdraw/transfer paths
/// reachable) and a large one that regularly exceeds one (keeping the typed
/// rejections reachable).
fn step_amount_strategy() -> impl Strategy<Value = u128> {
    prop_oneof![
        2 => 0u128..=1_000_000,
        1 => (PROP_MAX_AMOUNT / 2)..=PROP_MAX_AMOUNT,
    ]
}

/// Deposit sizes: mostly large enough to mint shares from an empty pool, with
/// a minority of zero/below-minimum sizes for the typed rejections.
fn deposit_amount_strategy() -> impl Strategy<Value = u128> {
    prop_oneof![
        2 => DEFAULT_MIN_INITIAL_DEPOSIT..=PROP_MAX_AMOUNT,
        1 => 0u128..DEFAULT_MIN_INITIAL_DEPOSIT,
    ]
}

fn lp_step_strategy() -> impl Strategy<Value = LpStep> {
    prop_oneof![
        (any::<u8>(), deposit_amount_strategy())
            .prop_map(|(lp, amount)| LpStep::Deposit { lp, amount }),
        (any::<u8>(), step_amount_strategy())
            .prop_map(|(lp, shares)| LpStep::Withdraw { lp, shares }),
        (any::<u8>(), any::<u8>(), step_amount_strategy())
            .prop_map(|(from, to, amount)| LpStep::Transfer { from, to, amount }),
        (
            any::<u8>(),
            any::<u8>(),
            any::<u8>(),
            step_amount_strategy(),
            // Bias towards a fresh grant so the accepted `transfer_from` path
            // stays reachable; the remainder spends (or misses) earlier grants
            // and exercises `InsufficientAllowance`.
            prop_oneof![2 => Just(true), 1 => Just(false)]
        )
            .prop_map(
                |(owner, spender, to, amount, fresh_grant)| LpStep::TransferFrom {
                    owner,
                    spender,
                    to,
                    amount,
                    fresh_grant,
                },
            ),
    ]
}

/// Tops a freshly generated address up with mock USDC so its deposits are
/// bounded by pool rules instead of by the token balance.
fn fund_prop_lp(te: &TestEnv, lp: &Address) {
    te.env.as_contract(&te.usdc_id, || {
        te.env
            .storage()
            .persistent()
            .set(&TKey(lp.clone()), &PROP_LP_USDC);
    });
}

/// Captures the share ledger exactly as the property states it: one balance
/// per participating LP plus the pool-wide supply.
fn share_snapshot(te: &TestEnv, lps: &[Address]) -> ([i128; PROP_MAX_LPS], i128) {
    let mut balances = [0i128; PROP_MAX_LPS];
    for (slot, lp) in lps.iter().enumerate() {
        balances[slot] = te.pool.balance(lp);
    }
    (balances, te.pool.total_supply())
}

/// After any step: SEP-41 `balance()` must agree with `get_lp_position()`,
/// and the balances of the whole LP set must add up to `total_supply()`.
fn check_share_invariants(te: &TestEnv, lps: &[Address]) -> Result<(), TestCaseError> {
    let mut sum: i128 = 0;
    for lp in lps {
        let balance = te.pool.balance(lp);
        let position = te.pool.get_lp_position(lp);
        prop_assert_eq!(
            balance,
            position.shares as i128,
            "balance() and get_lp_position().shares diverge for {:?}",
            lp
        );
        sum += balance;
    }
    prop_assert_eq!(
        sum,
        te.pool.total_supply(),
        "sum of LP balances must equal total supply"
    );
    Ok(())
}

/// Dispatches one generated step through its `try_*` entry point so both the
/// accepted and the rejected path go through the same invariant checks.
fn run_step(te: &TestEnv, lps: &[Address], step: &LpStep) -> StepOutcome {
    let at = |raw: &u8| &lps[*raw as usize % lps.len()];
    match step {
        LpStep::Deposit { lp, amount } => classify(te.pool.try_deposit(at(lp), amount)),
        LpStep::Withdraw { lp, shares } => classify(te.pool.try_withdraw(at(lp), shares)),
        LpStep::Transfer { from, to, amount } => classify(te.pool.try_transfer_shares(
            at(from),
            at(to),
            &(*amount as i128),
        )),
        LpStep::TransferFrom {
            owner,
            spender,
            to,
            amount,
            fresh_grant,
        } => {
            if *fresh_grant {
                // The grant is setup for the move, not part of the property:
                // its own result is ignored so a zero amount still revokes
                // (SEP-41) and the transfer_from below reports the typed
                // `InsufficientAllowance` failure instead.
                let expiration_ledger = te.env.ledger().sequence() + 1_000;
                let _ = te.pool.try_approve(
                    at(owner),
                    at(spender),
                    &(*amount as i128),
                    &expiration_ledger,
                );
            }
            classify(
                te.pool
                    .try_transfer_from(at(spender), at(owner), at(to), &(*amount as i128)),
            )
        }
    }
}

// Core property: over any bounded interleaving of deposit/withdraw/transfer/
// transfer_from across 3-5 LPs, every accepted step preserves
// `sum(balance(lp)) == total_supply()` and `balance(lp) ==
// get_lp_position(lp).shares`, and every rejected step leaves the whole
// share ledger byte-for-byte unchanged.
#[test]
fn prop_multi_lp_share_supply_conserved_across_op_sequences() {
    // `TestRunner::run` takes an `Fn`, so the step counters use interior
    // mutability; `std` is pulled in here for them (crate is `no_std`).
    extern crate std;

    let mut runner = TestRunner::new(ProptestConfig::with_cases(PROP_CASES));
    let accepted_steps = std::cell::Cell::new(0usize);
    let rejected_steps = std::cell::Cell::new(0usize);

    runner
        .run(
            &(
                3usize..=PROP_MAX_LPS,
                prop::collection::vec(lp_step_strategy(), 1..=PROP_MAX_STEPS),
            ),
            |(lp_count, steps)| {
                let te = setup();
                let all_lps = [
                    Address::generate(&te.env),
                    Address::generate(&te.env),
                    Address::generate(&te.env),
                    Address::generate(&te.env),
                    Address::generate(&te.env),
                ];
                let lps = &all_lps[..lp_count];
                for lp in lps {
                    fund_prop_lp(&te, lp);
                }

                // Empty pool starts at zero supply, zero balances.
                check_share_invariants(&te, lps)?;

                // Warm start: every LP takes an opening position so the
                // generated steps move shares that actually exist instead of
                // mostly probing the no-position rejections.
                for lp in lps {
                    te.pool.deposit(lp, &PROP_WARMUP_DEPOSIT);
                }
                check_share_invariants(&te, lps)?;

                for (index, step) in steps.iter().enumerate() {
                    let before = share_snapshot(&te, lps);
                    match run_step(&te, lps, step) {
                        StepOutcome::Succeeded => {
                            accepted_steps.set(accepted_steps.get() + 1);
                            check_share_invariants(&te, lps)?;
                        }
                        StepOutcome::Rejected(err) => {
                            rejected_steps.set(rejected_steps.get() + 1);
                            match err {
                                Ok(contract_error) => prop_assert!(
                                    contract_error.is_type(soroban_sdk::xdr::ScErrorType::Contract),
                                    "step {} ({:?}) failed outside the typed PoolError space: {:?}",
                                    index,
                                    step,
                                    contract_error
                                ),
                                Err(host_error) => prop_assert!(
                                    false,
                                    "step {} ({:?}) failed with a host error instead of a typed PoolError: {:?}",
                                    index,
                                    step,
                                    host_error
                                ),
                            }
                            let after = share_snapshot(&te, lps);
                            prop_assert_eq!(
                                after,
                                before,
                                "step {} ({:?}) was rejected but moved balances or total supply",
                                index,
                                step
                            );
                        }
                    }
                }
                Ok(())
            },
        )
        .unwrap();

    // The generator must actually exercise both sides of the property; a
    // sequence run in which everything succeeded (or everything failed) would
    // make the assertions above vacuous.
    let accepted = accepted_steps.get();
    let rejected = rejected_steps.get();
    assert!(
        accepted > 0 && rejected > 0,
        "expected both accepted and rejected steps, got {} accepted / {} rejected",
        accepted,
        rejected
    );
}

//  ISSUE #873: STANDARD SEP-41 transfer ENTRY POINT ======//
// The share-token surface was missing the standard `transfer(from, to, amount)`
// entry point (a prior duplicate-definition collision removed it alongside the
// `transfer_shares` logic it clashed with, leaving `move_shares` orphaned from
// a public caller). These tests pin the SEP-41 behavior on the restored
// function — most importantly that a generic `soroban_sdk::token::Client`
// can drive it with no pool-specific wrapper.

/// The required generic-client test: any SEP-41 consumer must be able to move
/// shares with a plain `soroban_sdk::token::Client` pointed at the pool
/// address, with no pool-specific client in sight.
#[test]
fn test_sep41_generic_token_client_transfer_moves_shares() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);

    // The generic SEP-41 client, constructed from the pool address alone.
    let shares_token = soroban_sdk::token::Client::new(&te.env, &te.pool_id);
    shares_token.transfer(&te.lp, &recipient, &5_000_000_000);

    assert_eq!(
        te.pool.get_lp_position(&te.lp).shares,
        5_000_000_000,
        "sender's shares must be debited"
    );
    assert_eq!(
        te.pool.get_lp_position(&recipient).shares,
        5_000_000_000,
        "recipient's shares must be credited"
    );
    // Total share supply is unaffected by a transfer.
    assert_eq!(te.pool.get_stats().total_shares, 10_000_000_000);
}

/// `transfer` must credit a recipient who has never interacted with the pool.
#[test]
fn test_transfer_to_recipient_without_position_creates_one() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);
    te.pool.transfer(&te.lp, &recipient, &2_500_000_000);

    let lp_position = te.pool.get_lp_position(&te.lp);
    assert_eq!(lp_position.shares, 7_500_000_000);
    assert_eq!(te.pool.get_lp_position(&recipient).shares, 2_500_000_000);
}

/// The generic client's transfer must enforce `from`'s authorization: with no
/// signatures provided, the host rejects the transaction.
#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_sep41_generic_token_client_transfer_requires_from_auth() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);
    let shares_token = soroban_sdk::token::Client::new(&te.env, &te.pool_id);

    // Clear all mocked auths so from.require_auth() fails.
    te.env.set_auths(&[]);
    shares_token.transfer(&te.lp, &recipient, &1_000_000_000);
}

/// Balance checks must hold on the standard path: transferring more shares
/// than `from` owns panics with `InsufficientBalance`.
#[test]
#[should_panic(expected = "Error(Contract, #23)")]
fn test_transfer_insufficient_balance_panics_via_generic_client() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);
    let shares_token = soroban_sdk::token::Client::new(&te.env, &te.pool_id);
    shares_token.transfer(&te.lp, &recipient, &20_000_000_000);
}

/// A sender with no share position at all panics with `NoShares`.
#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_transfer_from_address_without_shares_panics() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let stranger = Address::generate(&te.env);
    let recipient = Address::generate(&te.env);
    let shares_token = soroban_sdk::token::Client::new(&te.env, &te.pool_id);
    shares_token.transfer(&stranger, &recipient, &1);
}

/// Zero and negative amounts are rejected on the standard entry point.
#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_transfer_zero_amount_panics_via_generic_client() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);
    let shares_token = soroban_sdk::token::Client::new(&te.env, &te.pool_id);
    shares_token.transfer(&te.lp, &recipient, &0);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_transfer_negative_amount_panics_via_generic_client() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);
    let shares_token = soroban_sdk::token::Client::new(&te.env, &te.pool_id);
    shares_token.transfer(&te.lp, &recipient, &-5);
}

/// Both SEP-41 entry points emit the standard `transfer(from, to, amount)`
/// event shape, so generic indexers can watch share movements.
#[test]
fn test_transfer_and_transfer_from_emit_standard_transfer_event() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);
    let spender = Address::generate(&te.env);
    let expires = te.env.ledger().sequence() + 100;

    te.pool.transfer(&te.lp, &recipient, &1_000_000_000);
    let events = te.env.events().all();
    // sdk >=22: transfer's own buffer holds exactly its transfer event.
    assert_eq!(events.len(), 1);
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(topics.len(), 2);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "transfer")
    );
    assert_eq!(
        Address::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        te.lp
    );
    assert_eq!(
        <(Address, i128)>::try_from_val(&te.env, &data).unwrap(),
        (recipient.clone(), 1_000_000_000)
    );

    // The allowance path emits the identical standard event shape (approve
    // itself emits `allowance_approved`, hence the +2).
    te.pool.approve(&te.lp, &spender, &2_000_000_000, &expires);
    let events_after_approve = te.env.events().all();
    te.pool
        .transfer_from(&spender, &te.lp, &recipient, &2_000_000_000);
    let events = te.env.events().all();
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    // sdk >=22 keeps each top-level call's events in its own buffer:
    // approve emitted `allowance_approved` (1 event), transfer_from emitted
    // the standard `transfer` event (1 event).
    assert_eq!(events_after_approve.len(), 1);
    assert_eq!(
        Symbol::try_from_val(
            &te.env,
            &events_after_approve.get(0).unwrap().1.get(0).unwrap()
        )
        .unwrap(),
        Symbol::new(&te.env, "allowance_approved")
    );
    assert_eq!(events.len(), 1);
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "transfer")
    );
    assert_eq!(
        <(Address, i128)>::try_from_val(&te.env, &data).unwrap(),
        (recipient.clone(), 2_000_000_000)
    );
}

/// `transfer` and `transfer_shares` must be interchangeable: both entry
/// points move shares identically (transfer_shares is the documented
/// non-standard legacy alias).
#[test]
fn test_transfer_matches_legacy_transfer_shares() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let recipient = Address::generate(&te.env);
    te.pool.transfer_shares(&te.lp, &recipient, &5_000_000_000);

    let second_recipient = Address::generate(&te.env);
    te.pool.transfer(&te.lp, &second_recipient, &5_000_000_000);

    assert_eq!(te.pool.get_lp_position(&recipient).shares, 5_000_000_000);
    assert_eq!(
        te.pool.get_lp_position(&second_recipient).shares,
        5_000_000_000
    );
    assert_eq!(te.pool.get_lp_position(&te.lp).shares, 0);
}

/// Self-transfers remain a no-op on the standard entry point.
#[test]
fn test_transfer_same_address_no_op_via_generic_client() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let before = te.pool.get_lp_position(&te.lp);
    let shares_token = soroban_sdk::token::Client::new(&te.env, &te.pool_id);
    shares_token.transfer(&te.lp, &te.lp, &5_000_000_000);
    let after = te.pool.get_lp_position(&te.lp);

    assert_eq!(before.shares, after.shares);
    assert_eq!(te.pool.get_stats().total_shares, 10_000_000_000);
}

// ====== EXPIRE-LISTING VS FUND-INVOICE RACE TESTS //
// These tests cover the interaction between invoice.expiry_listing() and
// pool.fund_invoice() when a listed invoice's expiry window passes.
// The threat model (docs/THREAT_MODEL.md, attack vector 6) treats this
// as an "accepted race condition" but we pin the deterministic outcomes
// on either side of the deadline so future refactors cannot silently flip
// the boundary behavior.
//
// Note: invoice.expire_listing() requires issuer or admin auth. The
// expire path's authorization has a separate open bug (see invoice crate
// issue tracker); this test uses mock_all_auths_allowing_non_root_auth()
// for the expire_listing call only to exercise the cross-contract flow.

#[test]
fn test_expire_listing_then_fund_invoice_fails_invoice_not_listed() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    // Create and list an invoice with a short expiry window
    let due_date = te.env.ledger().timestamp() + 86400;
    let invoice_id = te.invoice.create(
        &te.issuer,
        &te.buyer,
        &10_000_000_000,
        &due_date,
        &te.usdc_id,
    );
    attest_invoice(&te, &invoice_id);
    te.invoice.list_for_financing(&invoice_id, &200);

    // Verify invoice is Listed
    assert_eq!(te.invoice.get_status(&invoice_id), 1);

    // Set a short expiry window (100 seconds)
    te.invoice.set_expiry_window(&100);

    // Advance time to exactly listed_at + expiry_window
    // listed_at is set to current timestamp when list_for_financing is called
    let listed_at = te.env.ledger().timestamp();
    let deadline = listed_at + 100;
    te.env.ledger().set_timestamp(deadline);

    // Expire the listing - using mock_all_auths to bypass the separate auth bug
    te.env.mock_all_auths_allowing_non_root_auth();
    let expired = te.invoice.expire_listing(&invoice_id, &te.issuer);
    assert!(expired);

    // Verify invoice status is now Expired (7)
    assert_eq!(te.invoice.get_status(&invoice_id), 7);

    // Verify invoice counts show the invoice under Expired
    let counts = te.invoice.get_counts();
    assert_eq!(counts.get(String::from_str(&te.env, "Listed")), Some(0));
    assert_eq!(counts.get(String::from_str(&te.env, "Expired")), Some(1));

    // Capture pool stats before attempting to fund
    let stats_before = te.pool.get_stats();

    // Try to fund the expired invoice - must fail with InvoiceNotListed (#8)
    let result = te.pool.try_fund_invoice(&invoice_id);
    assert!(
        result.is_err(),
        "fund_invoice must fail for expired invoice"
    );

    // Verify pool totals and active_invoice_count are unchanged
    let stats_after = te.pool.get_stats();
    assert_eq!(stats_after.total_deposits, stats_before.total_deposits);
    assert_eq!(stats_after.total_funded, stats_before.total_funded);
    assert_eq!(
        stats_after.active_invoice_count,
        stats_before.active_invoice_count
    );
    assert_eq!(
        stats_after.available_liquidity,
        stats_before.available_liquidity
    );
    assert_eq!(stats_after.total_shares, stats_before.total_shares);
}

// Boundary case: at deadline - 1 second, funding succeeds and expiry is rejected.
// This documents that funding wins at the exact deadline second (current_time < deadline).
#[test]
fn test_fund_invoice_succeeds_at_deadline_minus_one_expiry_rejected() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    // Create and list an invoice
    let due_date = te.env.ledger().timestamp() + 86400;
    let invoice_id = te.invoice.create(
        &te.issuer,
        &te.buyer,
        &10_000_000_000,
        &due_date,
        &te.usdc_id,
    );
    attest_invoice(&te, &invoice_id);
    te.invoice.list_for_financing(&invoice_id, &200);

    // Set a short expiry window (100 seconds)
    te.invoice.set_expiry_window(&100);

    // Advance time to deadline - 1 (one second before expiry)
    let listed_at = te.env.ledger().timestamp();
    let deadline = listed_at + 100;
    te.env.ledger().set_timestamp(deadline - 1);

    // Funding should succeed at deadline - 1
    let funded = te.pool.fund_invoice(&invoice_id);
    assert!(
        funded,
        "funding must succeed one second before expiry deadline"
    );

    // Verify invoice is now Funded (2)
    assert_eq!(te.invoice.get_status(&invoice_id), 2);

    // Verify pool state updated
    let stats = te.pool.get_stats();
    assert_eq!(stats.active_invoice_count, 1);
    assert_eq!(stats.total_funded, 9_800_000_000);

    // Now try to expire - must fail with ListingNotExpired (#14)
    // because the invoice is no longer Listed (it's Funded)
    let result = te.invoice.try_expire_listing(&invoice_id, &te.issuer);
    assert!(
        result.is_err(),
        "expire_listing must fail for funded invoice"
    );
}

/// Proves that share-price appreciation is correctly reflected in LP positions
/// after a share transfer via the SEP-41 interface (Issue #763).
#[test]
fn test_share_price_appreciation_after_transfer() {
    let te = setup();

    // LP A deposits funds
    let lp_a_initial_deposit = 100_000_000_000u128;
    let lp_a_shares_before = te.pool.get_lp_position(&te.lp).shares;
    te.pool.deposit(&te.lp, &lp_a_initial_deposit);
    let lp_a_shares_after_deposit = te.pool.get_lp_position(&te.lp).shares;
    assert_eq!(
        lp_a_shares_after_deposit - lp_a_shares_before,
        lp_a_initial_deposit
    );

    // Accrue yield via fund and repay cycle
    fund_and_repay_invoice(&te);

    // After repayment, total_deposits increases relative to total_shares
    let stats_after_repay = te.pool.get_stats();
    assert!(stats_after_repay.total_deposits > stats_after_repay.total_shares);

    // LP A transfers half of their shares to LP B
    let lp_b = Address::generate(&te.env);
    let lp_a_shares = te.pool.get_lp_position(&te.lp).shares;
    let transfer_amount = (lp_a_shares / 2) as i128;
    assert!(transfer_amount > 0);

    te.pool.transfer(&te.lp, &lp_b, &transfer_amount);

    // Get LP positions after transfer
    let lp_a_pos = te.pool.get_lp_position(&te.lp);
    let lp_b_pos = te.pool.get_lp_position(&lp_b);

    // Verify share balances reflect the transfer
    assert_eq!(lp_a_pos.shares, lp_a_shares - (transfer_amount as u128));
    assert_eq!(lp_b_pos.shares, transfer_amount as u128);

    // Both positions must reflect the appreciated per-share value proportional
    // to their post-transfer balances.
    let expected_a = (lp_a_pos.shares as u128) * stats_after_repay.total_deposits
        / stats_after_repay.total_shares;
    let expected_b = (lp_b_pos.shares as u128) * stats_after_repay.total_deposits
        / stats_after_repay.total_shares;

    assert_eq!(lp_a_pos.usdc_value, expected_a);
    assert_eq!(lp_b_pos.usdc_value, expected_b);

    // Per-share value (usdc_value / shares) must be identical for both LPs
    assert_eq!(
        lp_a_pos.usdc_value / lp_a_pos.shares,
        lp_b_pos.usdc_value / lp_b_pos.shares
    );
    assert!(lp_a_pos.usdc_value > lp_a_pos.shares);
    assert!(lp_b_pos.usdc_value > lp_b_pos.shares);
}

//  ISSUE #844: UTILIZATION / FUNDING ACCOUNTING //
// Invariants, across arbitrary sequences of deposit / fund_invoice /
// receive_repayment / handle_default / withdraw with a random cap:
//   * `get_stats().total_funded <= total_deposits`
//   * `available_liquidity == total_deposits - total_funded`
//   * after a successful `fund_invoice`, `get_utilization_rate() <=
//     get_stats().max_utilization_bps`
//   * a rejected step leaves the accounting byte-for-byte unchanged.

#[derive(Clone, Debug)]
enum FundStep {
    Deposit(u128),
    Fund(u8),
    Repay(u128),
    Default,
    Withdraw(u128),
    SetCap(u32),
}

fn fund_step_strategy() -> impl Strategy<Value = FundStep> {
    prop_oneof![
        deposit_amount_strategy().prop_map(FundStep::Deposit),
        any::<u8>().prop_map(FundStep::Fund),
        step_amount_strategy().prop_map(FundStep::Repay),
        Just(FundStep::Default),
        step_amount_strategy().prop_map(FundStep::Withdraw),
        (0u32..=10_000u32).prop_map(FundStep::SetCap),
    ]
}

fn run_fund_step(te: &TestEnv, invoices: &[BytesN<32>; 3], step: &FundStep) -> StepOutcome {
    match step {
        FundStep::Deposit(amount) => classify(te.pool.try_deposit(&te.lp, amount)),
        FundStep::Fund(index) => {
            let id = invoices[*index as usize % invoices.len()].clone();
            classify(te.pool.try_fund_invoice(&id))
        }
        FundStep::Repay(amount) => {
            let id = invoices[0].clone();
            classify(te.pool.try_receive_repayment(&id, amount))
        }
        FundStep::Default => classify(te.pool.try_handle_default(&invoices[0])),
        FundStep::Withdraw(shares) => classify(te.pool.try_withdraw(&te.lp, shares)),
        FundStep::SetCap(bps) => classify(te.pool.try_set_max_utilization(&te.admin, bps)),
    }
}

/// `(total_deposits, total_funded, available_liquidity, max_utilization_bps)`.
fn fund_snapshot(te: &TestEnv) -> (u128, u128, u128, u32) {
    let stats = te.pool.get_stats();
    (
        stats.total_deposits,
        stats.total_funded,
        stats.available_liquidity,
        stats.max_utilization_bps,
    )
}

/// After any step: never more funded than deposited, and available liquidity is
/// exactly deposits minus funded.
fn check_fund_invariants(te: &TestEnv) -> Result<(), TestCaseError> {
    let stats = te.pool.get_stats();
    prop_assert!(
        stats.total_funded <= stats.total_deposits,
        "total_funded {} exceeds total_deposits {}",
        stats.total_funded,
        stats.total_deposits
    );
    prop_assert_eq!(
        stats.available_liquidity,
        stats.total_deposits - stats.total_funded,
        "available_liquidity must equal total_deposits - total_funded"
    );
    Ok(())
}

#[test]
fn prop_utilization_cap_and_funding_accounting_hold_across_op_sequences() {
    extern crate std;

    let mut runner = TestRunner::new(ProptestConfig::with_cases(PROP_CASES));
    let accepted_steps = std::cell::Cell::new(0usize);
    let rejected_steps = std::cell::Cell::new(0usize);

    runner
        .run(
            &(
                // Sometimes a very tight cap (mostly rejections), sometimes a
                // full cap (so funding can succeed and the cap assertion bites).
                prop_oneof![1 => 0u32..=500u32, 1 => 1u32..=10_000u32],
                prop::collection::vec(fund_step_strategy(), 1..=PROP_MAX_STEPS),
            ),
            |(cap_bps, steps)| {
                let te = setup();
                te.pool.set_max_utilization(&te.admin, &cap_bps);

                // A small set of fundable invoices plus enough LP liquidity.
                let invoices = [
                    create_and_list(&te, &te.usdc_id),
                    create_and_list(&te, &te.usdc_id),
                    create_and_list(&te, &te.usdc_id),
                ];
                fund_prop_lp(&te, &te.lp);
                te.pool.deposit(&te.lp, &PROP_WARMUP_DEPOSIT);

                check_fund_invariants(&te)?;

                for (index, step) in steps.iter().enumerate() {
                    let before = fund_snapshot(&te);
                    let outcome = run_fund_step(&te, &invoices, step);

                    // The core invariants hold after every step.
                    check_fund_invariants(&te)?;

                    match outcome {
                        StepOutcome::Succeeded => {
                            accepted_steps.set(accepted_steps.get() + 1);
                            if let FundStep::Fund(_) = step {
                                let util = te.pool.get_utilization_rate();
                                let cap = te.pool.get_stats().max_utilization_bps;
                                prop_assert!(
                                    util <= cap,
                                    "step {}: funding pushed utilization {} above cap {}",
                                    index,
                                    util,
                                    cap
                                );
                            }
                        }
                        StepOutcome::Rejected(_) => {
                            rejected_steps.set(rejected_steps.get() + 1);
                            prop_assert_eq!(
                                fund_snapshot(&te),
                                before,
                                "step {} ({:?}) was rejected but moved accounting",
                                index,
                                step
                            );
                        }
                    }
                }
                Ok(())
            },
        )
        .unwrap();

    // The generator must exercise both sides; otherwise the assertions above
    // would be vacuous.
    assert!(
        accepted_steps.get() > 0 && rejected_steps.get() > 0,
        "expected both accepted and rejected steps, got {} accepted / {} rejected",
        accepted_steps.get(),
        rejected_steps.get()
    );
}

//  ISSUE #719: SUGGESTED DISCOUNT VIEW //
// `get_suggested_discount_bps` interpolates linearly from the floor at 0%
// utilization to the ceiling at 100%. The ceiling is pinned to 5000 bps so a
// suggestion can never exceed the cap `invoice::list_for_financing` enforces.

#[test]
fn test_suggested_discount_curve_constants() {
    assert_eq!(SUGGESTED_DISCOUNT_FLOOR_BPS, 100);
    // The ceiling sits exactly at the invoice contract's hard 5000 bps cap so a
    // suggestion is never rejected by `list_for_financing`.
    assert_eq!(SUGGESTED_DISCOUNT_CEILING_BPS, 5_000);
}

/// Low utilization (idle pool, nothing funded) → the floor.
#[test]
fn test_get_suggested_discount_bps_at_low_utilization_returns_floor() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    assert_eq!(te.pool.get_utilization_rate(), 0);
    assert_eq!(
        te.pool.get_suggested_discount_bps(),
        SUGGESTED_DISCOUNT_FLOOR_BPS
    );
}

/// Mid utilization (exactly 50% of deposits funded) → the curve midpoint.
#[test]
fn test_get_suggested_discount_bps_at_mid_utilization_is_midpoint() {
    let te = setup();
    // Deposits = 2 × funded_amount, so utilization is exactly 5000 bps.
    te.pool.deposit(&te.lp, &(DEFAULT_FUNDED_AMOUNT * 2));
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    assert_eq!(te.pool.get_utilization_rate(), 5_000);
    assert_eq!(
        te.pool.get_suggested_discount_bps(),
        SUGGESTED_DISCOUNT_FLOOR_BPS
            + (SUGGESTED_DISCOUNT_CEILING_BPS - SUGGESTED_DISCOUNT_FLOOR_BPS) / 2
    );
}

/// Near-cap utilization (every deposited unit funded) → the ceiling.
#[test]
fn test_get_suggested_discount_bps_at_full_utilization_returns_ceiling() {
    let te = setup();
    te.pool.deposit(&te.lp, &DEFAULT_FUNDED_AMOUNT);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    assert_eq!(te.pool.get_utilization_rate(), 10_000);
    assert_eq!(
        te.pool.get_suggested_discount_bps(),
        SUGGESTED_DISCOUNT_CEILING_BPS
    );
}

/// The view is read-only and authless: it must succeed with no mocked auths
/// and leave every accounting field untouched.
#[test]
fn test_get_suggested_discount_bps_is_read_only_and_authless() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let before = te.pool.get_stats();

    // Clear all mocked auths: a read-only view must not need authorization.
    te.env.set_auths(&[]);
    let suggestion = te.pool.get_suggested_discount_bps();
    assert_eq!(suggestion, SUGGESTED_DISCOUNT_FLOOR_BPS);

    let after = te.pool.get_stats();
    assert_eq!(after.total_deposits, before.total_deposits);
    assert_eq!(after.total_funded, before.total_funded);
    assert_eq!(after.total_shares, before.total_shares);
    assert_eq!(after.active_invoice_count, before.active_invoice_count);
}

/// An uninitialized pool has no deposits, so utilization reads 0% and the view
/// returns the floor instead of panicking.
#[test]
fn test_get_suggested_discount_bps_uninitialized_pool_returns_floor() {
    let (_env, pool) = uninitialized_pool();
    assert_eq!(
        pool.get_suggested_discount_bps(),
        SUGGESTED_DISCOUNT_FLOOR_BPS
    );
}
//  ISSUE #440: TRANSFER OWNERSHIP
#[test]
fn test_pool_transfer_ownership_changes_admin_and_emits_event() {
    let te = setup();
    let new_admin = Address::generate(&te.env);

    te.pool.transfer_ownership(&new_admin);
    // Capture before further queries: sdk >=22 test Envs reset the event
    // buffer at every top-level invocation.
    let events = te.env.events().all();

    assert_eq!(te.pool.get_admin(), new_admin);

    // ownership_transferred(old_admin, new_admin) — buffer captured above.
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "ownership_transferred")
    );
    assert_eq!(
        Address::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        te.admin
    );
    assert_eq!(Address::try_from_val(&te.env, &data).unwrap(), new_admin);
}

/// Dual auth: only `non_admin` signed the call, so the stored admin's
/// `require_auth()` rejects the transfer — an unauthorized caller cannot move
/// ownership (issue #440).
#[test]
fn test_pool_transfer_ownership_rejects_unauthorized_caller() {
    let te = setup();
    let new_admin = Address::generate(&te.env);
    let non_admin = Address::generate(&te.env);

    te.env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &te.pool_id,
            fn_name: "transfer_ownership",
            args: (new_admin.clone(),).into_val(&te.env),
            sub_invokes: &[],
        },
    }]);
    assert!(te.pool.try_transfer_ownership(&new_admin).is_err());
}

/// Dual auth also requires the *incoming* admin to consent: with only the
/// current admin's signature, `new_admin.require_auth()` rejects the transfer
/// so ownership cannot be forced onto an address that never signed.
#[test]
fn test_pool_transfer_ownership_requires_new_admin_consent() {
    let te = setup();
    let new_admin = Address::generate(&te.env);

    te.env.mock_auths(&[MockAuth {
        address: &te.admin,
        invoke: &MockAuthInvoke {
            contract: &te.pool_id,
            fn_name: "transfer_ownership",
            args: (new_admin.clone(),).into_val(&te.env),
            sub_invokes: &[],
        },
    }]);
    assert!(te.pool.try_transfer_ownership(&new_admin).is_err());
    // Admin unchanged after the rejected transfer.
    assert_eq!(te.pool.get_admin(), te.admin);
}

//  ISSUE #714: EMERGENCY PAUSE / UNPAUSE
#[test]
fn test_pause_blocks_state_changes_and_unpause_restores_them() {
    let te = setup();

    // Live: deposits flow.
    te.pool.deposit(&te.lp, &10_000_000_000);
    assert_eq!(te.pool.get_stats().total_deposits, 10_000_000_000);

    te.pool.pause();

    // State-changing entry points are rejected while paused...
    assert!(te.pool.try_deposit(&te.lp, &10_000_000_000).is_err());
    assert!(te.pool.try_transfer(&te.lp, &te.issuer, &1i128).is_err());
    assert!(te.pool.try_set_max_utilization(&te.admin, &9000).is_err());

    // ...while read-only views keep working.
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_deposits, 10_000_000_000);
    assert_eq!(te.pool.get_admin(), te.admin);
    assert_eq!(te.pool.balance(&te.lp), 10_000_000_000);
    let _ = te.pool.get_lp_position(&te.lp);
    let _ = te.pool.get_utilization_rate();

    te.pool.unpause();

    // Unpaused: state changes flow again.
    te.pool.deposit(&te.lp, &10_000_000_000);
    assert_eq!(te.pool.get_stats().total_deposits, 20_000_000_000);
}

/// Pins the exact breaker error: `PauseError::ContractPaused` from the shared
/// crate surfaces as `Error(Contract, #1)` on every guarded entry point.
#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_deposit_reverts_while_paused_with_contract_paused_error() {
    let te = setup();
    te.pool.pause();
    te.pool.deposit(&te.lp, &10_000_000_000);
}

/// Negative auth: only a non-admin signed `pause`, so the stored admin's
/// `require_auth()` must reject it — a non-admin cannot engage the breaker.
#[test]
fn test_pause_requires_admin_authorization() {
    let te = setup();
    let non_admin = Address::generate(&te.env);

    te.env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &te.pool_id,
            fn_name: "pause",
            args: soroban_sdk::Vec::new(&te.env),
            sub_invokes: &[],
        },
    }]);
    assert!(te.pool.try_pause().is_err());
}

/// Negative auth for disengaging: a non-admin cannot unpause either, so a
/// paused pool cannot be resumed by anyone but the admin.
#[test]
fn test_unpause_requires_admin_authorization() {
    let te = setup();
    te.pool.pause();
    let non_admin = Address::generate(&te.env);

    te.env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &te.pool_id,
            fn_name: "unpause",
            args: soroban_sdk::Vec::new(&te.env),
            sub_invokes: &[],
        },
    }]);
    assert!(te.pool.try_unpause().is_err());
}

#[test]
fn test_pause_and_unpause_emit_events() {
    let te = setup();

    te.pool.pause();
    let events = te.env.events().all();
    let (contract, topics, _) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "paused")
    );
    assert_eq!(
        Address::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        te.admin
    );

    te.pool.unpause();
    let events = te.env.events().all();
    let (contract, topics, _) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "unpaused")
    );
    assert_eq!(
        Address::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        te.admin
    );
}

/// Transfer of ownership is itself a state change, so the breaker gates it
/// too: while paused, `transfer_ownership` reverts and the admin is unchanged.
#[test]
fn test_pool_transfer_ownership_blocked_while_paused() {
    let te = setup();
    let new_admin = Address::generate(&te.env);

    te.pool.pause();
    assert!(te.pool.try_transfer_ownership(&new_admin).is_err());
    assert_eq!(te.pool.get_admin(), te.admin);
}
//  ISSUE #722: BATCH FUND INVOICE
#[test]
fn test_batch_fund_invoice_funds_every_entry() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    let ids = vec![
        &te.env,
        create_and_list(&te, &te.usdc_id),
        create_and_list(&te, &te.usdc_id),
        create_and_list(&te, &te.usdc_id),
    ];

    let funded = te.pool.batch_fund_invoice(&ids);
    assert_eq!(funded.len(), 3);

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, DEFAULT_FUNDED_AMOUNT * 3);
    assert_eq!(stats.active_invoice_count, 3);

    // Every invoice actually reached the funded state.
    for id in ids.iter() {
        assert_eq!(te.invoice.get_status(&id), 2); // Funded
    }
}

/// The required stop-behavior: funding halts — rather than partially draining
/// the pool — once funding the next entry would exceed `max_utilization_bps`.
#[test]
fn test_batch_fund_invoice_stops_when_utilization_cap_would_be_exceeded() {
    let te = setup();
    // Deposits = 3 × funded_amount, so each funded invoice adds ~3333 bps of
    // utilization: two entries reach 6666 bps (allowed by the 7000 cap) and the
    // third would reach 10000 bps, breaching it.
    te.pool.deposit(&te.lp, &29_400_000_000);
    te.pool.set_max_utilization(&te.admin, &7_000);

    let ids = vec![
        &te.env,
        create_and_list(&te, &te.usdc_id),
        create_and_list(&te, &te.usdc_id),
        create_and_list(&te, &te.usdc_id),
        create_and_list(&te, &te.usdc_id),
    ];

    let funded = te.pool.batch_fund_invoice(&ids);

    // Two entries fit within the 6600 bps cap; the third would exceed it.
    assert_eq!(funded.len(), 2);
    let before_cap_entry = &ids.get(2).unwrap();
    assert_eq!(funded.get(0).unwrap(), ids.get(0).unwrap());
    assert_eq!(funded.get(1).unwrap(), ids.get(1).unwrap());

    let stats = te.pool.get_stats();
    // Stopped cleanly: the third invoice was left untouched, not partially
    // funded, and the fourth was never attempted.
    assert_eq!(stats.total_funded, DEFAULT_FUNDED_AMOUNT * 2);
    assert_eq!(stats.active_invoice_count, 2);
    assert_eq!(te.invoice.get_status(before_cap_entry), 1); // still Listed
    assert_eq!(te.invoice.get_status(&ids.get(3).unwrap()), 1); // Listed
    assert_eq!(stats.utilization_rate_bps, 6_666);
    assert!(stats.utilization_rate_bps <= 7_000);
}

/// A capacity stop is not a panic: the batch returns a partial funded-list.
#[test]
fn test_batch_fund_invoice_stops_on_insufficient_liquidity() {
    let te = setup();
    // Exactly enough for one invoice at 100% utilization.
    te.pool.deposit(&te.lp, &DEFAULT_FUNDED_AMOUNT);

    let ids = vec![
        &te.env,
        create_and_list(&te, &te.usdc_id),
        create_and_list(&te, &te.usdc_id),
    ];

    let funded = te.pool.batch_fund_invoice(&ids);
    assert_eq!(funded.len(), 1);
    assert_eq!(funded.get(0).unwrap(), ids.get(0).unwrap());
    assert_eq!(te.invoice.get_status(&ids.get(1).unwrap()), 1); // Listed
    assert_eq!(te.pool.get_stats().active_invoice_count, 1);
}

/// Eligibility failures are NOT treated as a capacity stop: an ineligible entry
/// still reverts the whole batch with the same typed error `fund_invoice` raises.
#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_batch_fund_invoice_reverts_on_already_funded_entry() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let first = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&first);

    let second = create_and_list(&te, &te.usdc_id);
    let ids = vec![&te.env, second, first];
    te.pool.batch_fund_invoice(&ids);
}

/// An unlisted invoice reverts the batch too (`InvoiceNotListed` = #8).
#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_batch_fund_invoice_reverts_on_unlisted_entry() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    let listed = create_and_list(&te, &te.usdc_id);
    // Created but never listed.
    let due_date = te.env.ledger().timestamp() + 86400;
    let unlisted = te.invoice.create(
        &te.issuer,
        &te.buyer,
        &DEFAULT_FACE_VALUE,
        &due_date,
        &te.usdc_id,
    );

    let ids = vec![&te.env, listed, unlisted];
    te.pool.batch_fund_invoice(&ids);
}

#[test]
fn test_batch_fund_invoice_on_empty_batch_is_a_noop() {
    let te = setup();
    let ids = vec![&te.env];
    let funded = te.pool.batch_fund_invoice(&ids);
    assert!(funded.is_empty());
    assert_eq!(te.pool.get_stats().total_funded, 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #26)")]
fn test_batch_fund_invoice_rejects_oversized_batch() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let id = create_and_list(&te, &te.usdc_id);
    let mut ids: Vec<BytesN<32>> = Vec::new(&te.env);
    for _ in 0..=MAX_BATCH_SIZE {
        ids.push_back(id.clone());
    }
    te.pool.batch_fund_invoice(&ids);
}

/// A full 50-entry batch is permitted; the budget is lifted only because each
/// entry performs several cross-contract calls.
#[test]
fn test_batch_fund_invoice_allows_exactly_max_batch_size() {
    let te = setup();
    te.env.cost_estimate().budget().reset_unlimited();
    te.pool.deposit(&te.lp, &1_000_000_000_000);

    let mut ids: Vec<BytesN<32>> = Vec::new(&te.env);
    for _ in 0..MAX_BATCH_SIZE {
        ids.push_back(create_and_list(&te, &te.usdc_id));
    }

    let funded = te.pool.batch_fund_invoice(&ids);
    assert_eq!(funded.len(), MAX_BATCH_SIZE);
    assert_eq!(te.pool.get_stats().active_invoice_count, MAX_BATCH_SIZE);
}

// ============== GET_FUNDED_AMOUNT VIEW TESTS (issue #839) ==============

// The four states of `DataKey::FundedInvoice`, each reached through the
// contract's real fund / repayment / default entry points rather than by
// writing storage directly, so the assertions exercise the same lifecycle an
// on-chain indexer would observe.

// Unfunded: a listed invoice the pool has never funded has no
// `FundedInvoice` entry, so the view reports `None`.
#[test]
fn test_get_funded_amount_returns_none_for_unfunded_invoice() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    assert_eq!(te.pool.get_funded_amount(&invoice_id), None);

    // An invoice id the pool has never seen at all also reports `None`
    // (the storage lookup never panics for unknown ids).
    let unknown_id = BytesN::from_array(&te.env, &[0xAB; 32]);
    assert_eq!(te.pool.get_funded_amount(&unknown_id), None);
}

// Funded: after `fund_invoice` commits capital, the view reports exactly
// the amount that was funded.
#[test]
fn test_get_funded_amount_returns_amount_while_funded() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // face_value=10_000_000_000, discount_bps=200
    // funded_amount = 10_000_000_000 * (10000 - 200) / 10000 = 9_800_000_000
    assert_eq!(
        te.pool.get_funded_amount(&invoice_id),
        Some(DEFAULT_FUNDED_AMOUNT)
    );
}

// Repaid: the full production repayment path (ship -> confirm ->
// `invoice.repay`, which calls back into `receive_repayment` and
// `settle_repayment`) removes the `FundedInvoice` entry, so the view
// returns to `None`.
#[test]
fn test_get_funded_amount_returns_none_after_repayment() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    assert_eq!(
        te.pool.get_funded_amount(&invoice_id),
        Some(DEFAULT_FUNDED_AMOUNT)
    );

    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);
    te.invoice.repay(&invoice_id);

    assert_eq!(te.pool.get_funded_amount(&invoice_id), None);
}

// Defaulted: the default path removes the `FundedInvoice` entry too, so
// the view returns `None` after `handle_default` completes. This drives
// `pool.handle_default` (the entry point `invoice.trigger_default`
// invokes) like the rest of the default-path suite; the direct
// `invoice.trigger_default` chain is currently pinned to panic on
// self-re-entrancy by `test_trigger_default_drives_full_pool_and_escrow_chain`.
#[test]
fn test_get_funded_amount_returns_none_after_default() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    assert_eq!(
        te.pool.get_funded_amount(&invoice_id),
        Some(DEFAULT_FUNDED_AMOUNT)
    );

    // Advance a beat and default; since #819 escrow holds no lock record for
    // funded invoices (the principal went to the issuer at funding), so
    // `pool.handle_default` runs through the issuer-release branch.
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);
    assert!(te.pool.handle_default(&invoice_id));

    assert_eq!(te.pool.get_funded_amount(&invoice_id), None);
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_upgrade_requires_admin_auth() {
    let te = setup();
    te.env.set_auths(&[]);
    te.pool.upgrade(&BytesN::from_array(&te.env, &[0; 32]));
}

// --------------- Counting mock invoice (issue #833) ---------------
//
// Stand-in for the pool's invoice contract that tallies every cross-contract
// call `fund_invoice` makes into it. Used to prove the funding path now reads
// the invoice exactly once (`get_funding_context`) plus the terminal
// `mark_funded`, instead of four separate reads plus `mark_funded`.
//
// Wrapped in its own module because `#[contractimpl]` generates per-method
// dispatch symbols from the *method* name only: identical entrypoint names
// (`configure`, `get_issuer`, …) in two mocks in one module would collide.
mod counting_invoice {
    use crate::FundingContext;
    use soroban_sdk::{contract, contractimpl, Address, BytesN, Env, Symbol};

    #[contract]
    pub struct MockCountingInvoice;

    #[contractimpl]
    impl MockCountingInvoice {
        fn bump(env: &Env, name: &str) {
            let key = super::InvKey(Symbol::new(env, name));
            let count: u32 = env.storage().instance().get(&key).unwrap_or(0);
            env.storage().instance().set(&key, &(count + 1));
        }

        pub fn configure_counting(
            env: Env,
            issuer: Address,
            buyer: Address,
            funding_asset: Address,
            face_value: u128,
            discount_bps: u32,
        ) {
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "issuer")), &issuer);
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "buyer")), &buyer);
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "asset")), &funding_asset);
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "face")), &face_value);
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "disc")), &discount_bps);
        }

        pub fn call_count(env: Env, name: Symbol) -> u32 {
            env.storage()
                .instance()
                .get(&super::InvKey(name))
                .unwrap_or(0)
        }

        // Entrypoint names below must match what the pool's production code
        // invokes: `get_funding_context` and `mark_funded` exactly; the four
        // legacy names exist only so their counters stay at zero.
        pub fn get_funding_context(env: Env, _invoice_id: BytesN<32>) -> FundingContext {
            Self::bump(&env, "ctx_n");
            FundingContext {
                status: 1, // Listed
                face_value: env
                    .storage()
                    .instance()
                    .get(&super::InvKey(Symbol::new(&env, "face")))
                    .unwrap(),
                discount_bps: env
                    .storage()
                    .instance()
                    .get(&super::InvKey(Symbol::new(&env, "disc")))
                    .unwrap(),
                issuer: env
                    .storage()
                    .instance()
                    .get(&super::InvKey(Symbol::new(&env, "issuer")))
                    .unwrap(),
                buyer: env
                    .storage()
                    .instance()
                    .get(&super::InvKey(Symbol::new(&env, "buyer")))
                    .unwrap(),
                funding_asset: env
                    .storage()
                    .instance()
                    .get(&super::InvKey(Symbol::new(&env, "asset")))
                    .unwrap(),
            }
        }

        pub fn get_funding_terms(env: Env, _invoice_id: BytesN<32>) -> (u32, u128, u32) {
            Self::bump(&env, "terms_n");
            (
                1,
                env.storage()
                    .instance()
                    .get(&super::InvKey(Symbol::new(&env, "face")))
                    .unwrap(),
                env.storage()
                    .instance()
                    .get(&super::InvKey(Symbol::new(&env, "disc")))
                    .unwrap(),
            )
        }

        pub fn get_issuer(env: Env, _invoice_id: BytesN<32>) -> Address {
            Self::bump(&env, "issuer_n");
            env.storage()
                .instance()
                .get(&super::InvKey(Symbol::new(&env, "issuer")))
                .unwrap()
        }

        pub fn get_buyer(env: Env, _invoice_id: BytesN<32>) -> Address {
            Self::bump(&env, "buyer_n");
            env.storage()
                .instance()
                .get(&super::InvKey(Symbol::new(&env, "buyer")))
                .unwrap()
        }

        pub fn get_funding_asset(env: Env, _invoice_id: BytesN<32>) -> Address {
            Self::bump(&env, "asset_n");
            env.storage()
                .instance()
                .get(&super::InvKey(Symbol::new(&env, "asset")))
                .unwrap()
        }

        pub fn mark_funded(
            env: Env,
            _invoice_id: BytesN<32>,
            _pool: Address,
            _asset: Address,
            _amount: u128,
        ) -> bool {
            Self::bump(&env, "mark_n");
            true
        }
    }
}

/// #833: `fund_invoice` must touch the invoice contract exactly twice —
/// one batched `get_funding_context` read plus the terminal `mark_funded`.
/// Before the fix it was five calls (`get_funding_terms`, `get_issuer`,
/// `get_buyer`, `get_funding_asset`, `mark_funded`). The old read entrypoints
/// are still implemented on the mock and their counters prove the funding
/// path no longer invokes them.
#[test]
fn test_fund_invoice_reads_invoice_once_via_get_funding_context() {
    let te = setup();
    let mock_id = te.env.register(counting_invoice::MockCountingInvoice, ());
    counting_invoice::MockCountingInvoiceClient::new(&te.env, &mock_id).configure_counting(
        &te.issuer,
        &te.buyer,
        &te.usdc_id,
        &DEFAULT_FACE_VALUE,
        &DEFAULT_DISCOUNT_BPS,
    );
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::InvoiceContract, &mock_id);
    });
    te.pool.deposit(&te.lp, &100_000_000_000);

    let invoice_id = BytesN::from_array(&te.env, &[42u8; 32]);
    assert!(te.pool.fund_invoice(&invoice_id));

    let count = |name: &str| -> u32 {
        te.env.as_contract(&mock_id, || {
            te.env
                .storage()
                .instance()
                .get::<_, u32>(&InvKey(Symbol::new(&te.env, name)))
                .unwrap_or(0)
        })
    };
    assert_eq!(count("ctx_n"), 1, "exactly one batched invoice read");
    assert_eq!(count("mark_n"), 1);
    assert_eq!(count("terms_n"), 0, "get_funding_terms must not be called");
    assert_eq!(count("issuer_n"), 0, "get_issuer must not be called");
    assert_eq!(count("buyer_n"), 0, "get_buyer must not be called");
    assert_eq!(count("asset_n"), 0, "get_funding_asset must not be called");
    assert_eq!(
        count("ctx_n")
            + count("terms_n")
            + count("issuer_n")
            + count("buyer_n")
            + count("asset_n")
            + count("mark_n"),
        2,
        "total invoice-contract calls per funding should be 2 (was 5 before #833)"
    );

    // State transitions on the pool side are unchanged by the consolidation.
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(stats.active_invoice_count, 1);
    let funded: u128 = te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .persistent()
            .get(&DataKey::FundedInvoice(invoice_id.clone()))
            .unwrap()
    });
    assert_eq!(funded, DEFAULT_FUNDED_AMOUNT);
}

/// #833 budget regression guard. Baseline on plain `upstream/main` @
/// `7d2c9198` (four separate invoice reads) for this exact
/// deposit + create_and_list + fund_invoice sequence measured
/// cpu = 1,503,282 and memory = 281,310 host budget units. With the single
/// batched read the call must land strictly below that baseline; host budget
/// accounting is deterministic, so these ceilings are stable.
///
/// Re-pinned by #819: `fund_invoice` now also performs the issuer payout
/// (`escrow.release_to_issuer`) after `escrow.lock`, restoring the flow from
/// 5f6dc7c. That is one extra cross-contract call (~23k cpu / ~25k mem), so
/// the pre-payout baseline no longer bounds the corrected flow. The ceilings
/// below are the re-measured deterministic budget of deposit + create_and_list
/// + fund_invoice WITH the payout; the #833 batching guarantee itself is still
///   enforced by the invoice-call-count assertions in
///   `test_fund_invoice_uses_single_batched_read` (2 invoice calls, not 5).
///
/// Re-measured on the `b4a32a0f` (#865/#867) rebase: the invoice reads
/// `fund_invoice` batches now carry the per-entrypoint instance-TTL bumps
/// #865 added, so the corrected flow costs cpu = 1,562,040 / mem = 318,929
/// on this branch; the ceilings below pin that re-measurement.
///
/// Re-pinned by #969: `fund_invoice` now also snapshots the canonical buyer
/// (`DataKey::FundedInvoiceBuyer`, one persistent write + TTL bump) so
/// `receive_repayment_with_refund` can verify the refund destination without
/// re-entering the invoice contract mid-settlement. That is ~56k cpu / ~18k
/// mem of storage work, so the earlier #865 ceilings no longer bound the flow.
/// The #829 yield floor adds one instance-storage read
/// (`DataKey::MinDiscountBps`) to `fund_one` on top of that. The ceilings
/// below leave headroom for both changes on top of the previous #969
/// re-measurement (cpu = 1,617,956 / mem = 336,743); the #833 batching
/// guarantee itself is still enforced by the invoice-call-count assertions in
/// `test_fund_invoice_uses_single_batched_read` (2 invoice calls, not 5).
#[test]
fn test_fund_invoice_host_budget_below_four_call_baseline() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    te.env.cost_estimate().budget().reset_default();
    assert!(te.pool.fund_invoice(&invoice_id));
    let cpu = te.env.cost_estimate().budget().cpu_instruction_cost();
    let mem = te.env.cost_estimate().budget().memory_bytes_cost();
    std::println!("FUND833_BUDGET cpu={cpu} mem={mem}");

    assert!(
        cpu < 1_625_000,
        "fund_invoice (batched read + #819 issuer payout + #829 discount floor + #969 buyer snapshot) must stay within its re-pinned CPU budget: {cpu}"
    );
    assert!(
        mem < 345_000,
        "fund_invoice (batched read + #819 issuer payout + #969 buyer snapshot) must stay within its re-pinned memory budget: {mem}"
    );
}

//  ISSUE #845: SOLVENCY PROPTEST — POOL TOKEN BALANCE ALWAYS COVERS AVAILABLE LIQUIDITY
// Sibling of the #844 property above: #844 checks the pool's internal counters against
// each other across funding-op sequences; this one checks that the counters are actually
// backed by the funding-asset balance:
//
//     usdc.balance(pool) >= total_deposits - total_funded
//
// after EVERY step of a random operation sequence. `withdraw` pays LPs out of the token
// balance using only the counters (`preview_withdraw_amount` bounds redemptions by
// `total_deposits - total_funded`), so any counter/balance drift either fails
// withdrawals (counters exceed the balance) or, worse, pays an LP out of another LP's
// money (balance exceeds the counters, stranding the difference).
//
// The generated sequence covers the five operations from the issue, driven through the
// real contracts the way production drives them:
//   * `pool.deposit` / `pool.withdraw` directly (LP surface);
//   * `pool.fund_invoice` directly (`escrow.lock` pulls the funded principal out of the
//     pool's balance);
//   * full repayment via `invoice.repay` and early repayment via `invoice.repay_early`,
//     both of which settle through `pool.receive_repayment_with_refund` called BY the
//     real invoice contract, so the buyer refund and the protocol-fee transfer to the
//     treasury are real balance movements, not bookkeeping;
//   * default via `invoice.trigger_default` -> `pool.handle_default` ->
//     `escrow.handle_default` (locked principal returns to the pool balance).
// Each sequence runs with a random `protocol_fee_bps <= MAX_PROTOCOL_FEE_BPS`.
//
// The acceptance criteria also require the treasury's balance change to equal the sum of
// `protocol_cut`s. Instead of re-deriving the contract's fee math (which would only
// prove the test agrees with itself), the cut is read from every `repayment_received`
// event the pool emits during the sequence and summed; the treasury's final balance must
// equal that sum exactly.
//
// Every step dispatches through its `try_*` entry point: rejections (insufficient
// liquidity, utilization cap, wrong invoice status, escrow default grace) are legal
// outcomes and the invariant must survive them too, so the invariant is checked after
// every step regardless of the step's result.
//
// Case budget and sequence length reuse the file's existing property constants
// (`PROP_CASES` = 10, `PROP_MAX_STEPS` = 12): every step here performs real cross-
// contract settlement (escrow + invoice), which is several times the per-step cost of
// the share-ledger properties in #842, so the same modest budget is the deliberate
// choice for the CI time bound of the in-process Soroban host.

/// Listed invoices prepared per sequence: enough that funding steps routinely have a
/// target, small enough that the warm liquidity deposits cover several of them.
const SOLVENCY_INVOICES: usize = 3;

/// Opening deposit per LP. Sized so the whole invoice set can be funded (so `Fund`
/// steps succeed and later repay/default steps have real positions to settle) while
/// random withdrawals remain large enough to hit the liquidity guard.
const SOLVENCY_WARM_DEPOSIT: u128 = 400_000_000_000;

/// One step of a generated sequence. `lp`/`invoice` selectors are raw bytes reduced
/// modulo the case's participant/invoice counts, so every value addresses a real one.
#[derive(Clone, Debug)]
enum SolvencyStep {
    Deposit {
        lp: u8,
        amount: u128,
    },
    Withdraw {
        lp: u8,
        shares: u128,
    },
    Fund {
        invoice: u8,
    },
    /// Ship + confirm the full lifecycle, advance time, `invoice.repay` — settles at
    /// `receive_repayment_with_refund` with refund 0 once past the due date.
    Repay {
        invoice: u8,
        advance: u64,
    },
    /// Ship + confirm, advance time, `invoice.repay_early` — the pro-rata branch of
    /// `receive_repayment_with_refund` that moves the buyer refund out of the pool.
    RepayEarly {
        invoice: u8,
        advance: u64,
    },
    /// `invoice.trigger_default` — routes to `pool.handle_default` ->
    /// `escrow.handle_default`, returning the locked principal to the pool balance.
    Default {
        invoice: u8,
        advance: u64,
    },
}

/// Deposit sizes: mostly fundable, with a minority of zero/below-minimum sizes for the
/// typed rejections (same shape as `deposit_amount_strategy` used by #842/#844).
fn solvency_deposit_strategy() -> impl Strategy<Value = u128> {
    prop_oneof![
        4 => DEFAULT_MIN_INITIAL_DEPOSIT..=PROP_MAX_AMOUNT,
        1 => 0u128..DEFAULT_MIN_INITIAL_DEPOSIT,
    ]
}

/// Share sizes: usually redeemable against a warm position, regularly oversized so the
/// `InsufficientShares`/`NoShares` rejections stay reachable.
fn solvency_shares_strategy() -> impl Strategy<Value = u128> {
    prop_oneof![
        2 => 1u128..SOLVENCY_WARM_DEPOSIT,
        1 => PROP_MAX_AMOUNT / 2..=PROP_MAX_AMOUNT,
    ]
}

/// Ledger advance per settlement step: usually a small tick (keeps `repay_early` and
/// the refund branch reachable while the invoice is still within its 1-day term),
/// sometimes a jump past the term (keeps on-time `repay` and `trigger_default`, whose
/// due-date gate plus the escrow's 60-second default grace both need real elapsed
/// time).
fn solvency_advance_strategy() -> impl Strategy<Value = u64> {
    prop_oneof![
        3 => 0u64..3_600,
        2 => 90_000u64..302_400,
    ]
}

fn solvency_step_strategy() -> impl Strategy<Value = SolvencyStep> {
    prop_oneof![
        (any::<u8>(), solvency_deposit_strategy())
            .prop_map(|(lp, amount)| SolvencyStep::Deposit { lp, amount }),
        (any::<u8>(), solvency_shares_strategy())
            .prop_map(|(lp, shares)| SolvencyStep::Withdraw { lp, shares }),
        any::<u8>().prop_map(|invoice| SolvencyStep::Fund { invoice }),
        (any::<u8>(), solvency_advance_strategy())
            .prop_map(|(invoice, advance)| SolvencyStep::Repay { invoice, advance }),
        (any::<u8>(), solvency_advance_strategy())
            .prop_map(|(invoice, advance)| SolvencyStep::RepayEarly { invoice, advance }),
        (any::<u8>(), solvency_advance_strategy())
            .prop_map(|(invoice, advance)| SolvencyStep::Default { invoice, advance }),
    ]
}

/// Dispatches one generated step, always through the real counterparties: settlement
/// and default go through the invoice contract, funding and the LP surface through the
/// pool client. Lifecycle transitions inside `Repay`/`RepayEarly`/`Default` may
/// legitimately fail (wrong status, already shipped); those results are ignored — the
/// property is about the invariant, not about the step succeeding.
fn run_solvency_step(
    te: &TestEnv,
    lps: &[Address; 2],
    invoices: &[BytesN<32>; SOLVENCY_INVOICES],
    step: &SolvencyStep,
) -> StepOutcome {
    let lp_at = |raw: &u8| &lps[*raw as usize % lps.len()];
    let invoice_at = |raw: &u8| invoices[*raw as usize % invoices.len()].clone();
    let advance_time = |delta: u64| {
        te.env
            .ledger()
            .set_timestamp(te.env.ledger().timestamp() + delta);
    };
    match step {
        SolvencyStep::Deposit { lp, amount } => classify(te.pool.try_deposit(lp_at(lp), amount)),
        SolvencyStep::Withdraw { lp, shares } => classify(te.pool.try_withdraw(lp_at(lp), shares)),
        SolvencyStep::Fund { invoice } => classify(te.pool.try_fund_invoice(&invoice_at(invoice))),
        SolvencyStep::Repay { invoice, advance } => {
            let id = invoice_at(invoice);
            let _ = te.invoice.try_mark_shipped(&id);
            let _ = te.invoice.try_confirm_delivery(&id, &te.issuer);
            let _ = te.invoice.try_confirm_delivery(&id, &te.buyer);
            advance_time(*advance);
            classify(te.invoice.try_repay(&id))
        }
        SolvencyStep::RepayEarly { invoice, advance } => {
            let id = invoice_at(invoice);
            let _ = te.invoice.try_mark_shipped(&id);
            let _ = te.invoice.try_confirm_delivery(&id, &te.issuer);
            let _ = te.invoice.try_confirm_delivery(&id, &te.buyer);
            advance_time(*advance);
            classify(te.invoice.try_repay_early(&id))
        }
        SolvencyStep::Default { invoice, advance } => {
            let id = invoice_at(invoice);
            advance_time(*advance);
            classify(te.invoice.try_trigger_default(&id))
        }
    }
}

/// The solvency property itself: the pool's funding-asset balance must cover the
/// available liquidity the counters advertise, and the counters themselves must stay in
/// the order the subtraction requires. Re-checked after every generated step, accepted
/// or rejected.
fn check_solvency_invariant(te: &TestEnv, usdc: &MockTokenClient) -> Result<(), TestCaseError> {
    let stats = te.pool.get_stats();
    prop_assert!(
        stats.total_funded <= stats.total_deposits,
        "total_funded {} exceeds total_deposits {}",
        stats.total_funded,
        stats.total_deposits
    );
    prop_assert_eq!(
        stats.available_liquidity,
        stats.total_deposits - stats.total_funded,
        "available_liquidity must equal total_deposits - total_funded"
    );
    let balance = usdc.balance(&te.pool_id);
    prop_assert!(
        balance >= stats.available_liquidity as i128,
        "insolvent: pool USDC balance {balance} below available liquidity {}",
        stats.available_liquidity
    );
    Ok(())
}

/// Sums the `protocol_cut` field of `repayment_received` events emitted by the pool
/// since the previous scan (`seen` advances in place), so the treasury assertion reads
/// the fee the contract actually booked instead of a re-derivation of its math.
/// Sums the `protocol_cut` field of `repayment_received` events carried by a
/// step's own per-invocation event buffer (sdk >=22 test Envs reset the
/// buffer at every top-level invocation, so the caller captures it right
/// after `run_solvency_step`), so the treasury assertion reads the fee the
/// contract actually booked instead of a re-derivation of its math.
fn solvency_new_protocol_cuts(
    te: &TestEnv,
    events: &soroban_sdk::Vec<(
        soroban_sdk::Address,
        soroban_sdk::Vec<soroban_sdk::Val>,
        soroban_sdk::Val,
    )>,
) -> u128 {
    let name = Symbol::new(&te.env, "repayment_received");
    let mut cuts = 0u128;
    for (contract, topics, data) in events.iter() {
        if contract != te.pool_id || topics.is_empty() {
            continue;
        }
        let Ok(topic) = Symbol::try_from_val(&te.env, &topics.get(0).unwrap()) else {
            continue;
        };
        if topic != name {
            continue;
        }
        if let Ok((_amount, _lp_yield, cut)) = <(u128, u128, u128)>::try_from_val(&te.env, &data) {
            cuts += cut;
        }
    }
    cuts
}

// Core property: for any random `protocol_fee_bps <= MAX_PROTOCOL_FEE_BPS` and any
// bounded interleaving of deposit / fund_invoice / invoice-driven repayment (full and
// early) / invoice-driven default / withdraw, the pool's real USDC balance always
// covers `total_deposits - total_funded`, and the treasury's final balance equals the
// sum of the protocol cuts booked on the `repayment_received` events.
#[test]
fn prop_pool_token_balance_always_covers_available_liquidity() {
    // `TestRunner::run` takes an `Fn`, so the step counters use interior mutability.
    extern crate std;
    let accepted_steps = std::cell::Cell::new(0usize);
    let rejected_steps = std::cell::Cell::new(0usize);

    let mut runner = TestRunner::new(ProptestConfig::with_cases(PROP_CASES));
    runner
        .run(
            &(
                // Most sequences run a live protocol fee (so the treasury transfer and
                // the cut-vs-balance assertion bite); some run fee 0 (full yield stays
                // with the LPs).
                prop_oneof![1 => Just(0u32), 3 => 1u32..=MAX_PROTOCOL_FEE_BPS],
                prop::collection::vec(solvency_step_strategy(), 1..=PROP_SOLVENCY_MAX_STEPS),
            ),
            |(fee_bps, steps)| {
                let te = setup();
                // Tarpaulin instrumentation can exhaust the default Soroban host budget.
                // Preserve the full property sequence and its solvency assertions.
                te.env.cost_estimate().budget().reset_unlimited();
                let usdc = MockTokenClient::new(&te.env, &te.usdc_id);

                // One random fee configuration per sequence (the issue's
                // `protocol_fee_bps <= MAX_PROTOCOL_FEE_BPS`), pointed at a freshly
                // generated treasury whose balance starts at exactly zero.
                let treasury = Address::generate(&te.env);
                te.pool.set_protocol_fee(&fee_bps, &treasury);

                // Two LPs topped far above anything the sequence can deposit, so steps
                // fail on pool rules rather than on the token balance.
                let lps = [te.lp.clone(), create_lp_with_balance(&te, PROP_LP_USDC)];
                for lp in &lps {
                    te.pool.deposit(lp, &SOLVENCY_WARM_DEPOSIT);
                }

                let invoices = [
                    create_and_list(&te, &te.usdc_id),
                    create_and_list(&te, &te.usdc_id),
                    create_and_list(&te, &te.usdc_id),
                ];

                let mut protocol_cuts = 0u128;

                check_solvency_invariant(&te, &usdc)?;

                for step in steps.iter() {
                    match run_solvency_step(&te, &lps, &invoices, step) {
                        StepOutcome::Succeeded => {
                            accepted_steps.set(accepted_steps.get() + 1);
                        }
                        StepOutcome::Rejected(_) => {
                            rejected_steps.set(rejected_steps.get() + 1);
                        }
                    }

                    // Capture the step's final call events before the
                    // invariant's get_stats query clears the buffer (sdk >=22).
                    let step_events = te.env.events().all();
                    // The invariant holds after every step, accepted or rejected.
                    check_solvency_invariant(&te, &usdc)?;
                    protocol_cuts += solvency_new_protocol_cuts(&te, &step_events);
                }

                // Treasury conservation from the AC: every booked protocol cut actually
                // arrived, and nothing else did.
                prop_assert_eq!(
                    usdc.balance(&treasury),
                    protocol_cuts as i128,
                    "treasury balance must equal the sum of protocol cuts booked on repayment_received events"
                );
                Ok(())
            },
        )
        .unwrap();

    // The generator must actually exercise both sides; a run where everything
    // succeeded (or everything failed) would make the per-step checks vacuous.
    assert!(
        accepted_steps.get() > 0 && rejected_steps.get() > 0,
        "expected both accepted and rejected steps, got {} accepted / {} rejected",
        accepted_steps.get(),
        rejected_steps.get()
    );
}

// ---------------------------------------------------------------------------
//  ISSUE #829: ADMIN-SETTABLE min_discount_bps YIELD FLOOR
// ---------------------------------------------------------------------------

#[test]
fn test_min_discount_bps_defaults_to_zero_and_funding_is_unchanged() {
    let te = setup();
    assert_eq!(te.pool.get_min_discount_bps(), 0);

    // The griefing case the floor exists for: a 1-bps invoice. With the
    // default floor of 0 it still funds, proving pre-#829 behaviour is
    // unchanged.
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list_with_params(&te, &te.usdc_id, 10_000_000_000, 1);
    assert!(te.pool.fund_invoice(&invoice_id));
    assert_eq!(te.pool.get_min_discount_bps(), 0);
}

#[test]
fn test_set_min_discount_at_max_cap_succeeds_and_reads_back() {
    let te = setup();
    let ok = te.pool.set_min_discount(&5000);
    assert!(ok);
    assert_eq!(te.pool.get_min_discount_bps(), 5000);
}

#[test]
#[should_panic(expected = "Error(Contract, #15)")]
fn test_set_min_discount_above_max_cap_panics() {
    let te = setup();
    te.pool.set_min_discount(&5001);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_set_min_discount_requires_admin_authorization() {
    let te = setup();
    // Clear all mocked auths so the stored admin's require_auth() fails.
    te.env.set_auths(&[]);
    te.pool.set_min_discount(&300);
}

/// Self-authorizing non-admin: only `non_admin` signed the call, so the
/// stored admin's `require_auth()` rejects it — mirroring the
/// `set_protocol_fee` negative-auth pattern from #772.
#[test]
fn test_set_min_discount_rejects_self_authorizing_non_admin() {
    let te = setup();
    let non_admin = Address::generate(&te.env);
    te.env.mock_auths(&[MockAuth {
        address: &non_admin,
        invoke: &MockAuthInvoke {
            contract: &te.pool_id,
            fn_name: "set_min_discount",
            args: (300u32,).into_val(&te.env),
            sub_invokes: &[],
        },
    }]);
    assert!(te.pool.try_set_min_discount(&300).is_err());
    assert_eq!(te.pool.get_min_discount_bps(), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #28)")]
fn test_fund_invoice_discount_below_floor_rejected() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    te.pool.set_min_discount(&300);
    let invoice_id = create_and_list_with_params(&te, &te.usdc_id, 10_000_000_000, 200);
    te.pool.fund_invoice(&invoice_id);
}

#[test]
fn test_fund_invoice_discount_at_floor_accepted() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    te.pool.set_min_discount(&300);
    let invoice_id = create_and_list_with_params(&te, &te.usdc_id, 10_000_000_000, 300);
    assert!(te.pool.fund_invoice(&invoice_id));
    assert_eq!(te.pool.get_min_discount_bps(), 300);
}

/// The floor is an eligibility failure, not a capacity shortfall, so
/// `batch_fund_invoice` must surface `DiscountBelowMinimum` rather than
/// stopping silently (matching the panic-in-both-modes rule of `fund_one`).
#[test]
#[should_panic(expected = "Error(Contract, #28)")]
fn test_batch_fund_invoice_discount_below_floor_rejected() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    te.pool.set_min_discount(&300);
    let invoice_id = create_and_list_with_params(&te, &te.usdc_id, 10_000_000_000, 200);
    let ids = vec![&te.env, invoice_id];
    te.pool.batch_fund_invoice(&ids);
}

#[test]
fn test_set_min_discount_emits_event_with_old_and_new_values() {
    let te = setup();

    te.pool.set_min_discount(&750);

    let events = te.env.events().all();
    // sdk >=22: this buffer holds only the set_min_discount call's event.
    assert_eq!(events.len(), 1);
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.pool_id);
    assert_eq!(topics.len(), 1);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "min_discount_updated")
    );
    // setup() leaves the floor at its default 0 bps, so the first change
    // reports 0 -> 750.
    assert_eq!(
        <(u32, u32)>::try_from_val(&te.env, &data).unwrap(),
        (0u32, 750u32)
    );

    // A second change reports the value it replaced, not the original one.
    te.pool.set_min_discount(&1500);
    let events = te.env.events().all();
    let (_contract, _topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(
        <(u32, u32)>::try_from_val(&te.env, &data).unwrap(),
        (750u32, 1500u32)
    );

    // A rejected change (above MAX_MIN_DISCOUNT_BPS) must not emit anything,
    // and must leave the stored floor at 1500.
    assert!(te.pool.try_set_min_discount(&5001).is_err());
    // A rejected change emits nothing in its own per-invocation buffer.
    assert_eq!(te.env.events().all().len(), 0);
    assert_eq!(te.pool.get_min_discount_bps(), 1500);
}

// --------------- Configurable failing invoice (issue #964) ---------------
//
// Stand-in for the pool's invoice contract whose individual view methods
// (`get_status`, `get_funding_asset`, `get_face_value`, `get_discount_bps`)
// can be configured to fail in turn. `get_funding_context` — the single
// batched read `fund_invoice` makes since #833 — delegates to those views, so
// a failure in any one of them aborts the pool's funding path before it
// writes any state. Used to prove a failing invoice view leaves the pool's
// accounting untouched: no `FundedInvoice` entry, no token movement, and an
// unchanged `TotalFunded`.
//
// Wrapped in its own module because `#[contractimpl]` generates per-method
// dispatch symbols from the *method* name only: identical entrypoint names in
// two mocks in one module would collide (same reason as `counting_invoice`).

mod failing_invoice {
    use crate::FundingContext;
    use soroban_sdk::{contract, contractimpl, Address, BytesN, Env, Symbol};

    #[contract]
    pub struct MockFailingInvoice;

    // Failure modes accepted by `set_failure_mode`.
    pub const FAIL_NONE: u32 = 0;
    pub const FAIL_GET_STATUS: u32 = 1;
    pub const FAIL_GET_FUNDING_ASSET: u32 = 2;
    pub const FAIL_GET_FACE_VALUE: u32 = 3;
    pub const FAIL_GET_DISCOUNT_BPS: u32 = 4;

    #[contractimpl]
    impl MockFailingInvoice {
        fn fail_mode(env: &Env) -> u32 {
            env.storage()
                .instance()
                .get(&super::InvKey(Symbol::new(env, "fail_mode")))
                .unwrap_or(0)
        }

        fn maybe_fail(env: &Env, method: &str, mode: u32) {
            if Self::fail_mode(env) == mode {
                panic!("invoice view {method} failed");
            }
        }

        pub fn configure(
            env: Env,
            issuer: Address,
            buyer: Address,
            funding_asset: Address,
            face_value: u128,
            discount_bps: u32,
        ) {
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "issuer")), &issuer);
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "buyer")), &buyer);
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "asset")), &funding_asset);
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "face")), &face_value);
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "disc")), &discount_bps);
        }

        pub fn set_failure_mode(env: Env, mode: u32) {
            env.storage()
                .instance()
                .set(&super::InvKey(Symbol::new(&env, "fail_mode")), &mode);
        }

        pub fn get_status(env: Env, _invoice_id: BytesN<32>) -> u32 {
            Self::maybe_fail(&env, "get_status", FAIL_GET_STATUS);
            1 // Listed
        }

        pub fn get_funding_asset(env: Env, _invoice_id: BytesN<32>) -> Address {
            Self::maybe_fail(&env, "get_funding_asset", FAIL_GET_FUNDING_ASSET);
            env.storage()
                .instance()
                .get(&super::InvKey(Symbol::new(&env, "asset")))
                .unwrap()
        }

        pub fn get_face_value(env: Env, _invoice_id: BytesN<32>) -> u128 {
            Self::maybe_fail(&env, "get_face_value", FAIL_GET_FACE_VALUE);
            env.storage()
                .instance()
                .get(&super::InvKey(Symbol::new(&env, "face")))
                .unwrap()
        }

        pub fn get_discount_bps(env: Env, _invoice_id: BytesN<32>) -> u32 {
            Self::maybe_fail(&env, "get_discount_bps", FAIL_GET_DISCOUNT_BPS);
            env.storage()
                .instance()
                .get(&super::InvKey(Symbol::new(&env, "disc")))
                .unwrap()
        }

        pub fn get_issuer(env: Env, _invoice_id: BytesN<32>) -> Address {
            env.storage()
                .instance()
                .get(&super::InvKey(Symbol::new(&env, "issuer")))
                .unwrap()
        }

        pub fn get_buyer(env: Env, _invoice_id: BytesN<32>) -> Address {
            env.storage()
                .instance()
                .get(&super::InvKey(Symbol::new(&env, "buyer")))
                .unwrap()
        }

        pub fn get_funding_context(env: Env, invoice_id: BytesN<32>) -> FundingContext {
            let status = Self::get_status(env.clone(), invoice_id.clone());
            let face_value = Self::get_face_value(env.clone(), invoice_id.clone());
            let discount_bps = Self::get_discount_bps(env.clone(), invoice_id.clone());
            let funding_asset = Self::get_funding_asset(env.clone(), invoice_id.clone());
            let issuer = Self::get_issuer(env.clone(), invoice_id.clone());
            let buyer = Self::get_buyer(env.clone(), invoice_id.clone());
            FundingContext {
                status,
                face_value,
                discount_bps,
                issuer,
                buyer,
                funding_asset,
            }
        }

        pub fn mark_funded(
            _env: Env,
            _invoice_id: BytesN<32>,
            _pool: Address,
            _asset: Address,
            _amount: u128,
        ) -> bool {
            true
        }
    }
}

/// #964: `fund_invoice` must preserve its accounting when any invoice view
/// call fails. The configurable `MockFailingInvoice` stub fails
/// `get_status`, `get_funding_asset`, `get_face_value`, and
/// `get_discount_bps` in turn; because the pool's batched `get_funding_context`
/// read delegates to those views, each failure aborts the funding path before
/// it writes any state. After every failure the pool must show no
/// `FundedInvoice` entry, no token movement (pool / escrow / issuer balances),
/// and an unchanged `TotalFunded`.
#[test]
fn test_fund_invoice_view_failure_preserves_accounting() {
    let te = setup();
    let mock_id = te.env.register(failing_invoice::MockFailingInvoice, ());
    let mock = failing_invoice::MockFailingInvoiceClient::new(&te.env, &mock_id);
    mock.configure(
        &te.issuer,
        &te.buyer,
        &te.usdc_id,
        &DEFAULT_FACE_VALUE,
        &DEFAULT_DISCOUNT_BPS,
    );
    te.env.as_contract(&te.pool_id, || {
        te.env
            .storage()
            .instance()
            .set(&DataKey::InvoiceContract, &mock_id);
    });
    te.pool.deposit(&te.lp, &100_000_000_000);

    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);
    let invoice_id = BytesN::from_array(&te.env, &[7u8; 32]);

    let failure_modes = [
        (failing_invoice::FAIL_GET_STATUS, "get_status"),
        (failing_invoice::FAIL_GET_FUNDING_ASSET, "get_funding_asset"),
        (failing_invoice::FAIL_GET_FACE_VALUE, "get_face_value"),
        (failing_invoice::FAIL_GET_DISCOUNT_BPS, "get_discount_bps"),
    ];

    for (mode, name) in failure_modes {
        let stats_before = te.pool.get_stats();
        let pool_bal_before = usdc.balance(&te.pool_id);
        let escrow_bal_before = usdc.balance(&te.escrow_id);
        let issuer_bal_before = usdc.balance(&te.issuer);

        mock.set_failure_mode(&mode);
        let result = te.pool.try_fund_invoice(&invoice_id);
        assert!(
            result.is_err(),
            "fund_invoice must revert when the {name} view fails"
        );

        // No FundedInvoice state may be created.
        let funded_key = DataKey::FundedInvoice(invoice_id.clone());
        assert!(
            !te.env.as_contract(&te.pool_id, || {
                te.env.storage().persistent().has(&funded_key)
            }),
            "no FundedInvoice state may exist after the {name} view fails"
        );

        // No tokens may move.
        assert_eq!(
            usdc.balance(&te.pool_id),
            pool_bal_before,
            "pool balance must not change when the {name} view fails"
        );
        assert_eq!(
            usdc.balance(&te.escrow_id),
            escrow_bal_before,
            "escrow balance must not change when the {name} view fails"
        );
        assert_eq!(
            usdc.balance(&te.issuer),
            issuer_bal_before,
            "issuer balance must not change when the {name} view fails"
        );

        // TotalFunded (and the active count) must be unchanged.
        let stats_after = te.pool.get_stats();
        assert_eq!(
            stats_after.total_funded, stats_before.total_funded,
            "total_funded must not change when the {name} view fails"
        );
        assert_eq!(
            stats_after.active_invoice_count, stats_before.active_invoice_count,
            "active_invoice_count must not change when the {name} view fails"
        );

        mock.set_failure_mode(&failing_invoice::FAIL_NONE);
    }

    // Sanity check: with every view healthy, the same stub funds cleanly,
    // proving the reverts above came from the failing views and not from a
    // misconfigured stub.
    assert!(te.pool.fund_invoice(&invoice_id));
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, DEFAULT_FUNDED_AMOUNT);
    assert_eq!(stats.active_invoice_count, 1);
}
