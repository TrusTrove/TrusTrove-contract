//! Issue #820 — real-authorization coverage for the invoice repayment path.
//!
//! Every repayment test in this suite previously ran under
//! `env.mock_all_auths_allowing_non_root_auth()`, which only *records*
//! authorizations and never verifies them against an auth tree. The tests
//! below instead install an explicit `env.mock_auths(...)` tree carrying only
//! the authorizations a real buyer-signed transaction would present, so the
//! full multi-hop chain `buyer → invoice.repay → escrow.release_to_pool →
//! pool.receive_repayment_with_refund` is validated with real Soroban auth
//! semantics.

extern crate std;

use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, testutils::Ledger as _,
    testutils::MockAuth, testutils::MockAuthInvoke, xdr::ToXdr, Address, BytesN, Env, IntoVal, Map,
    String, Symbol, Val, Vec,
};

use trusttrove_escrow::{EscrowContract, EscrowContractClient};
use trusttrove_invoice::{InvoiceContract, InvoiceContractClient, InvoiceStatus};
use trusttrove_pool::{
    PoolContract, PoolContractClient, DEFAULT_MIN_INITIAL_DEPOSIT, DEFAULT_SHARE_DECIMALS,
};
use trusttrove_registry::{RegistryContract, RegistryContractClient};

// --------------- Mock USDC Token (same shim as tests/lifecycle.rs) ---------------

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

// --------------- Mock Agent Registry ---------------

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

const TEST_AGENT_SEED: [u8; 32] = [7u8; 32];

fn test_agent_signing_key() -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_slice(&TEST_AGENT_SEED).unwrap()
}

fn test_agent_pubkey(env: &Env) -> BytesN<65> {
    let point = test_agent_signing_key()
        .verifying_key()
        .to_encoded_point(false);
    let mut bytes = [0u8; 65];
    bytes.copy_from_slice(point.as_bytes());
    BytesN::from_array(env, &bytes)
}

fn test_agent_id(env: &Env) -> Symbol {
    Symbol::new(env, "test_agent")
}

fn attest_invoice(env: &Env, invoice_client: &InvoiceContractClient, invoice_id: &BytesN<32>) {
    let payload = trusttrove_invoice::AttestationPayload {
        domain_separator: BytesN::from_array(
            env,
            &trusttrove_invoice::ATTESTATION_DOMAIN_SEPARATOR,
        ),
        invoice_id: invoice_id.clone(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(env, &[9u8; 32]),
        agent_id: test_agent_id(env),
        nonce: 1,
    };
    let payload_bytes = payload.to_xdr(env);
    let digest = env.crypto().keccak256(&payload_bytes).to_array();
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(env, &sig_bytes);

    invoice_client.submit_attestation(invoice_id, &payload_bytes, &signature);
}

// --------------- Wiring ---------------

const FACE_VALUE: u128 = 10_000_000_000;
const DISCOUNT_BPS: u32 = 200;

struct RepayWiring {
    env: Env,
    invoice: InvoiceContractClient<'static>,
    invoice_addr: Address,
    escrow: EscrowContractClient<'static>,
    escrow_id: Address,
    pool_id: Address,
    usdc_id: Address,
    buyer: Address,
    invoice_id: BytesN<32>,
    due_date: u64,
}

/// End-to-end wiring identical to `setup_fee_lifecycle()` in
/// `tests/lifecycle.rs`, producing a `Confirmed` invoice whose escrow lock is
/// live. Setup keeps the blanket mock (it mirrors the existing suite); every
/// repayment call below is then issued under an *explicit* auth tree.
fn setup_repay_wiring() -> RepayWiring {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);
    let usdc_id = env.register_contract(None, MockToken);
    for address in [&lp, &buyer] {
        let key = TKey(address.clone());
        env.as_contract(&usdc_id, || {
            env.storage()
                .persistent()
                .set(&key, &100_000_000_000_000i128);
        });
    }

    let registry_id = env.register_contract(None, RegistryContract);
    let registry = RegistryContractClient::new(&env, &registry_id);
    registry.initialize(&admin);
    let metadata: Map<String, String> = Map::new(&env);
    registry.register_issuer(&issuer, &metadata);
    registry.register_buyer(&buyer, &metadata);
    registry.verify_profile(&admin, &issuer, &true);
    registry.verify_profile(&admin, &buyer, &true);

    let invoice_addr = env.register_contract(None, InvoiceContract);
    let escrow_id = env.register_contract(None, EscrowContract);
    let pool_id = env.register_contract(None, PoolContract);
    let invoice = InvoiceContractClient::new(&env, &invoice_addr);
    invoice.initialize(&admin, &registry_id);
    invoice.add_supported_asset(&usdc_id);
    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    let agent_reg_id = env.register_contract(None, MockAgentRegistry);
    MockAgentRegistryClient::new(&env, &agent_reg_id).register_agent(
        &test_agent_id(&env),
        &trusttrove_invoice::Agent {
            active: true,
            pubkey: test_agent_pubkey(&env),
        },
    );
    invoice.set_agent_registry_contract(&agent_reg_id);

    let escrow = EscrowContractClient::new(&env, &escrow_id);
    escrow.initialize(&admin, &pool_id, &usdc_id);
    let pool = PoolContractClient::new(&env, &pool_id);
    pool.initialize(
        &admin,
        &invoice_addr,
        &escrow_id,
        &usdc_id,
        &registry_id,
        &admin,
        &DEFAULT_MIN_INITIAL_DEPOSIT,
        &String::from_str(&env, "TrusTrove USDC Pool Shares"),
        &String::from_str(&env, "TT-USDC"),
        &DEFAULT_SHARE_DECIMALS,
    );
    pool.set_max_utilization(&admin, &10000);

    // Seed pool liquidity, then drive the invoice to a `Confirmed` state with
    // a live escrow lock — the exact pre-state every existing repayment test
    // uses before calling `repay`/`repay_partial`.
    pool.deposit(&lp, &12_000_000_000);

    let due_date = env.ledger().timestamp() + 30 * 86400;
    let invoice_id = invoice.create(&issuer, &buyer, &FACE_VALUE, &due_date, &usdc_id);
    attest_invoice(&env, &invoice, &invoice_id);
    invoice.list_for_financing(&invoice_id, &DISCOUNT_BPS);
    pool.fund_invoice(&invoice_id);
    invoice.mark_shipped(&invoice_id);
    invoice.confirm_delivery(&invoice_id, &issuer);
    invoice.confirm_delivery(&invoice_id, &buyer);

    RepayWiring {
        env,
        invoice,
        invoice_addr,
        escrow,
        escrow_id,
        pool_id,
        usdc_id,
        buyer,
        invoice_id,
        due_date,
    }
}

fn vec1(env: &Env, v: Val) -> Vec<Val> {
    let mut args = Vec::new(env);
    args.push_back(v);
    args
}

/// Positive control: the same full-term repayment settles when the blanket
/// mock from the existing suite is used (this mirrors `tests/lifecycle.rs`, so
/// the wiring itself is proven good and any failure below is auth-specific).
#[test]
fn test_repay_settles_under_blanket_mock_control() {
    let w = setup_repay_wiring();
    w.env.ledger().set_timestamp(w.due_date);
    assert!(w.invoice.repay(&w.invoice_id));
    assert_eq!(w.invoice.get(&w.invoice_id).status, InvoiceStatus::Repaid);
}

/// Issue #820, AC step 1 — recorded reproduction result: on pristine `main`
/// (`aa7acc257961aad10e8621040225ebcdc17c6981`) this exact call failed under
/// the same explicit buyer-only tree with
/// `Err(Ok(Error(Context, InvalidAction)))`, because `release_to_pool` started
/// with `require_pool_auth` and no real transaction can carry an
/// authorization rooted at the pool contract address.
///
/// With the #628 principal restored (escrow now authorizes the invoice
/// contract read from the pool's own configuration), the identical realistic
/// tree settles the invoice.
#[test]
fn test_repay_settles_with_explicit_buyer_only_auth() {
    let w = setup_repay_wiring();
    w.env.ledger().set_timestamp(w.due_date);

    // Explicit auth tree for what a real buyer-signed transaction carries:
    // the buyer's signature over the invoice call, declaring the sub-contract
    // calls the invoice makes on its behalf. Deliberately absent: any
    // authorization rooted at the pool *contract address* — a real
    // transaction cannot carry one (the pool implements no `__check_auth__`),
    // and `mock_auths` would only accept it by re-registering the pool address
    // with the test-only `MockAuthContract` stub, i.e. exactly the blind spot
    // issue #637 warned about.
    let release_to_pool_sub = MockAuthInvoke {
        contract: &w.escrow_id,
        fn_name: "release_to_pool",
        args: (w.invoice_id.clone(), FACE_VALUE).into_val(&w.env),
        sub_invokes: &[],
    };
    let receive_repayment_sub = MockAuthInvoke {
        contract: &w.pool_id,
        fn_name: "receive_repayment_with_refund",
        args: (w.invoice_id.clone(), FACE_VALUE, 0u128, w.buyer.clone()).into_val(&w.env),
        sub_invokes: &[],
    };
    let sub_invokes = [release_to_pool_sub, receive_repayment_sub];
    let repay_invoke = MockAuthInvoke {
        contract: &w.invoice_addr,
        fn_name: "repay",
        args: vec1(&w.env, w.invoice_id.clone().into_val(&w.env)),
        sub_invokes: &sub_invokes,
    };
    w.env.mock_auths(&[MockAuth {
        address: &w.buyer,
        invoke: &repay_invoke,
    }]);

    let res = w.invoice.try_repay(&w.invoice_id);
    std::println!(
        "#820 fixed: invoice.repay under explicit buyer-only auth => {:?}",
        res
    );
    assert!(res.is_ok(), "repay must settle with only buyer auths");
    assert_eq!(w.invoice.get(&w.invoice_id).status, InvoiceStatus::Repaid);

    // Settlement actually moved: the escrow lock was consumed and the face
    // value landed at the pool (pool held 12e9 − 9.8e9 funded = 2.2e9 before).
    assert_eq!(w.escrow.get_locked(&w.invoice_id), 0);
    let token = MockTokenClient::new(&w.env, &w.usdc_id);
    assert_eq!(token.balance(&w.pool_id), 12_200_000_000i128);
}

/// Same invariant for `repay_partial` when the partial payment happens to
/// settle the invoice in full (the branch that routes through
/// `escrow.release_to_pool`).
#[test]
fn test_repay_partial_settles_with_explicit_buyer_only_auth() {
    let w = setup_repay_wiring();
    w.env.ledger().set_timestamp(w.due_date);

    let release_to_pool_sub = MockAuthInvoke {
        contract: &w.escrow_id,
        fn_name: "release_to_pool",
        args: (w.invoice_id.clone(), FACE_VALUE).into_val(&w.env),
        sub_invokes: &[],
    };
    let receive_repayment_sub = MockAuthInvoke {
        contract: &w.pool_id,
        fn_name: "receive_repayment_with_refund",
        args: (w.invoice_id.clone(), FACE_VALUE, 0u128, w.buyer.clone()).into_val(&w.env),
        sub_invokes: &[],
    };
    let sub_invokes = [release_to_pool_sub, receive_repayment_sub];
    let repay_invoke = MockAuthInvoke {
        contract: &w.invoice_addr,
        fn_name: "repay_partial",
        args: (w.invoice_id.clone(), FACE_VALUE).into_val(&w.env),
        sub_invokes: &sub_invokes,
    };
    w.env.mock_auths(&[MockAuth {
        address: &w.buyer,
        invoke: &repay_invoke,
    }]);

    let res = w.invoice.try_repay_partial(&w.invoice_id, &FACE_VALUE);
    std::println!(
        "#820 fixed: invoice.repay_partial under explicit buyer-only auth => {:?}",
        res
    );
    assert!(
        res.is_ok(),
        "repay_partial must settle with only buyer auths"
    );
    assert_eq!(w.invoice.get(&w.invoice_id).status, InvoiceStatus::Repaid);
}

/// Negative control for the tests above: with the auth set emptied, the
/// buyer's own `require_auth` inside `repay_partial` rejects the call — the
/// positive tests are not passing "for free".
#[test]
fn test_repay_without_buyer_auth_is_rejected() {
    let w = setup_repay_wiring();
    w.env.ledger().set_timestamp(w.due_date);
    w.env.set_auths(&[]);

    let res = w.invoice.try_repay(&w.invoice_id);
    assert!(res.is_err(), "repay must require the buyer's auth");
    assert_ne!(w.invoice.get(&w.invoice_id).status, InvoiceStatus::Repaid);
}

/// Issue #820, AC: an unrelated caller cannot call `release_to_pool` — the
/// invoice contract's authorization is not present and cannot be forged by
/// whatever signature the caller does carry.
#[test]
fn test_release_to_pool_by_unrelated_caller_is_rejected() {
    let w = setup_repay_wiring();
    let attacker = Address::generate(&w.env);

    let release_invoke = MockAuthInvoke {
        contract: &w.escrow_id,
        fn_name: "release_to_pool",
        args: (w.invoice_id.clone(), FACE_VALUE).into_val(&w.env),
        sub_invokes: &[],
    };
    w.env.mock_auths(&[MockAuth {
        address: &attacker,
        invoke: &release_invoke,
    }]);

    let res = w.escrow.try_release_to_pool(&w.invoice_id, &FACE_VALUE);
    assert!(res.is_err(), "only the invoice contract may release");
    // Post-#819 the funded principal already left escrow at funding time
    // (`fund_invoice` → `release_to_issuer` consumed the Locked record and
    // wrote the `IssuerReleased` marker), so the untouched state the rejected
    // attacker call must not have perturbed is the *marker*, not the lock.
    // Every below-line is the same "no funds moved, no desync was possible"
    // invariant, restated for the payout-at-funding semantics #920 restores.
    assert_eq!(
        w.escrow.get_locked(&w.invoice_id),
        0,
        "the Locked record was consumed at funding and stays absent"
    );
    assert!(
        w.escrow.was_released_to_issuer(&w.invoice_id),
        "a rejected release_to_pool must not consume the IssuerReleased marker"
    );
    let token = MockTokenClient::new(&w.env, &w.usdc_id);
    let inv = w.invoice.get(&w.invoice_id);
    assert_eq!(
        token.balance(&w.escrow_id),
        0,
        "escrow holds nothing after the payout and still holds nothing after the rejection"
    );
    assert_eq!(
        token.balance(&inv.issuer),
        inv.funded_amount as i128,
        "the issuer's payout-at-funding must be intact after the rejected call"
    );
    // Pool received no attacker "release": still only 12e9 deposited − 9.8e9 funded.
    assert_eq!(token.balance(&w.pool_id), 2_200_000_000i128);
}
