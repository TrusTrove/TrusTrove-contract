//! Issue #969 — the refund destination of `receive_repayment_with_refund`
//! must be verified against the invoice contract's canonical buyer.
//!
//! The refund *amount* is invoice-computed, but the *destination* travels as
//! plain calldata alongside it: a miswired or compromised caller contract
//! could otherwise direct the refund to another address. This suite wires the
//! real registry + invoice + escrow + pool contracts end to end, then attempts
//! a repayment settlement with a foreign destination and asserts that no
//! funds move, no accounting is finalized, and the invoice is untouched —
//! before proving the canonical destination still settles.

extern crate std;

use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, xdr::ToXdr, Address, BytesN,
    Env, Map, String, Symbol,
};

use trusttrove_escrow::EscrowContract;
use trusttrove_invoice::{
    Agent, AttestationPayload, InvoiceContract, InvoiceContractClient, ATTESTATION_DOMAIN_SEPARATOR,
};
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
    pub fn get_agent(env: Env, agent_id: Symbol) -> Option<Agent> {
        env.storage().persistent().get(&AgentKey(agent_id))
    }

    pub fn register_agent(env: Env, agent_id: Symbol, agent: Agent) {
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
    let payload = AttestationPayload {
        domain_separator: BytesN::from_array(env, &ATTESTATION_DOMAIN_SEPARATOR),
        invoice_id: invoice_id.clone(),
        contract_address: invoice_client.address.clone(),
        network_id: env.ledger().network_id(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(env, &[9u8; 32]),
        agent_id: test_agent_id(env),
        nonce: 1,
        valid_until: u64::MAX,
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
// funded_amount = FACE_VALUE * (10000 - 200) / 10000
const FUNDED_AMOUNT: u128 = FACE_VALUE * (10000 - DISCOUNT_BPS as u128) / 10000;
const LP_DEPOSIT: u128 = 12_000_000_000;

struct RefundWiring {
    env: Env,
    invoice: InvoiceContractClient<'static>,
    pool: PoolContractClient<'static>,
    pool_id: Address,
    usdc: MockTokenClient<'static>,
    buyer: Address,
    invoice_id: BytesN<32>,
}

/// Full-protocol wiring: real registry, invoice, escrow, and pool contracts
/// with a funded invoice, mirroring `setup_repay_wiring()` in
/// `tests/repay_auth.rs`. The pool is seeded with `LP_DEPOSIT` and the
/// invoice is funded, so the pool holds `LP_DEPOSIT - FUNDED_AMOUNT` and the
/// funded entry is live.
fn setup_refund_wiring() -> RefundWiring {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);
    let usdc_id = env.register(MockToken, ());
    for address in [&lp, &buyer] {
        let key = TKey(address.clone());
        env.as_contract(&usdc_id, || {
            env.storage()
                .persistent()
                .set(&key, &100_000_000_000_000i128);
        });
    }

    // Post-#827 wiring: every contract takes its config through the generated
    // constructor, so there is no separate `initialize` call. The pool is
    // registered last because its constructor cross-checks the escrow's
    // funding asset and the invoice needs the pool address.
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let registry = RegistryContractClient::new(&env, &registry_id);
    let metadata: Map<String, String> = Map::new(&env);
    registry.register_issuer(&issuer, &metadata);
    registry.register_buyer(&buyer, &metadata);
    registry.verify_profile(&admin, &issuer, &true);
    registry.verify_profile(&admin, &buyer, &true);

    let pool_id = Address::generate(&env);
    let invoice_addr = env.register(InvoiceContract, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        EscrowContract,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );
    let invoice = InvoiceContractClient::new(&env, &invoice_addr);
    invoice.add_supported_asset(&usdc_id);
    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    let agent_reg_id = env.register(MockAgentRegistry, ());
    MockAgentRegistryClient::new(&env, &agent_reg_id).register_agent(
        &test_agent_id(&env),
        &Agent {
            active: true,
            pubkey: test_agent_pubkey(&env),
        },
    );
    invoice.set_agent_registry_contract(&agent_reg_id);

    let pool = PoolContractClient::new(&env, &pool_id);
    env.register_at(
        &pool_id,
        PoolContract,
        (
            admin.clone(),
            invoice_addr.clone(),
            escrow_id.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            admin.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            String::from_str(&env, "TrusTrove USDC Pool Shares"),
            String::from_str(&env, "TT-USDC"),
            DEFAULT_SHARE_DECIMALS,
        ),
    );
    pool.set_max_utilization(&admin, &10000);

    pool.deposit(&lp, &LP_DEPOSIT);

    let due_date = env.ledger().timestamp() + 30 * 86400;
    let invoice_id = invoice.create(&issuer, &buyer, &FACE_VALUE, &due_date, &usdc_id);
    attest_invoice(&env, &invoice, &invoice_id);
    invoice.list_for_financing(&invoice_id, &DISCOUNT_BPS);
    pool.fund_invoice(&invoice_id);

    let usdc = MockTokenClient::new(&env, &usdc_id);
    RefundWiring {
        env,
        invoice,
        pool,
        pool_id,
        usdc,
        buyer,
        invoice_id,
    }
}

/// Issue #969, AC — an attempt to refund an address other than the invoice's
/// canonical buyer must be rejected with `PoolError::BuyerMismatch` (#27):
/// no funds move, no accounting is finalized, and the invoice is untouched.
#[test]
fn test_refund_to_foreign_address_is_rejected_without_moving_funds() {
    let w = setup_refund_wiring();
    let attacker = Address::generate(&w.env);

    let stats_before = w.pool.get_stats();
    let pool_before = w.usdc.balance(&w.pool_id);
    let buyer_before = w.usdc.balance(&w.buyer);
    let attacker_before = w.usdc.balance(&attacker);
    let status_before = w.invoice.get(&w.invoice_id).status;

    // In-bounds amounts; only the destination is foreign.
    let refund = 120_000_000u128;
    let result =
        w.pool
            .try_receive_repayment_with_refund(&w.invoice_id, &FACE_VALUE, &refund, &attacker);
    let error = std::format!("{result:?}");
    assert!(
        result.is_err(),
        "a refund to a foreign destination must be rejected: {error}"
    );
    assert!(
        error.contains("#27"),
        "expected PoolError::BuyerMismatch (#27): {error}"
    );

    // No funds moved.
    assert_eq!(
        w.usdc.balance(&attacker),
        attacker_before,
        "the foreign destination must receive nothing"
    );
    assert_eq!(
        w.usdc.balance(&w.buyer),
        buyer_before,
        "the canonical buyer must be untouched by the rejected attempt"
    );
    assert_eq!(
        w.usdc.balance(&w.pool_id),
        pool_before,
        "the pool balance must not change on a rejected destination"
    );

    // No accounting was finalized.
    let stats_after = w.pool.get_stats();
    assert_eq!(stats_after.total_deposits, stats_before.total_deposits);
    assert_eq!(
        stats_after.total_yield_distributed,
        stats_before.total_yield_distributed
    );
    assert_eq!(stats_after.total_funded, stats_before.total_funded);
    assert_eq!(
        stats_after.active_invoice_count,
        stats_before.active_invoice_count
    );
    assert_eq!(
        w.pool.get_funded_amount(&w.invoice_id),
        Some(FUNDED_AMOUNT),
        "the funded invoice entry must survive the rejected settlement"
    );

    // The invoice itself is untouched: still awaiting repayment.
    assert_eq!(
        w.invoice.get(&w.invoice_id).status,
        status_before,
        "the invoice status must not change on a rejected destination"
    );
}

/// Positive control for the test above: the same split with the canonical
/// buyer as the destination settles — the destination check pins the invoice
/// buyer, it does not block legitimate repayments.
#[test]
fn test_refund_to_canonical_buyer_settles() {
    let w = setup_refund_wiring();

    let refund = 120_000_000u128;
    let buyer_before = w.usdc.balance(&w.buyer);
    let pool_before = w.usdc.balance(&w.pool_id);

    assert!(w
        .pool
        .receive_repayment_with_refund(&w.invoice_id, &FACE_VALUE, &refund, &w.buyer));

    // The buyer received exactly the refund. This call settles the pool
    // directly (bypassing escrow's face-value release, which only
    // `invoice.repay` triggers), so the pool's token balance moves only by
    // the refund it pays out.
    assert_eq!(w.usdc.balance(&w.buyer), buyer_before + refund as i128);
    assert_eq!(
        w.usdc.balance(&w.pool_id),
        pool_before - refund as i128,
        "the pool pays exactly the refund out of its own balance"
    );

    let yield_to_lp = FACE_VALUE - FUNDED_AMOUNT - refund;
    let stats = w.pool.get_stats();
    assert_eq!(stats.total_funded, 0);
    assert_eq!(stats.active_invoice_count, 0);
    assert_eq!(
        stats.total_deposits,
        LP_DEPOSIT + yield_to_lp,
        "only the non-refunded yield slice is credited to the pool"
    );
    assert_eq!(w.pool.get_funded_amount(&w.invoice_id), None);
}
