extern crate std;

use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, testutils::Ledger as _,
    xdr::ToXdr, Address, BytesN, Env, Map, String, Symbol,
};

use trusttrove_escrow::{EscrowContract, EscrowContractClient};
use trusttrove_invoice::{InvoiceContract, InvoiceContractClient, InvoiceStatus};
use trusttrove_pool::{
    PoolContract, PoolContractClient, DEFAULT_MIN_INITIAL_DEPOSIT, DEFAULT_SHARE_DECIMALS,
};
use trusttrove_registry::{RegistryContract, RegistryContractClient};

// --------------- Mock USDC Token ---------------

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

struct FeeLifecycle {
    env: Env,
    usdc_id: Address,
    invoice: InvoiceContractClient<'static>,
    pool: PoolContractClient<'static>,
    escrow_id: Address,
    pool_id: Address,
    issuer: Address,
    buyer: Address,
    lp: Address,
    treasury: Address,
}

fn setup_fee_lifecycle() -> FeeLifecycle {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);
    let treasury = Address::generate(&env);
    let usdc_id = env.register(MockToken, ());
    for address in [&lp, &buyer] {
        let key = TKey(address.clone());
        env.as_contract(&usdc_id, || {
            env.storage()
                .persistent()
                .set(&key, &100_000_000_000_000i128);
        });
    }

    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let registry = RegistryContractClient::new(&env, &registry_id);
    let metadata: Map<String, String> = Map::new(&env);
    registry.register_issuer(&issuer, &metadata);
    registry.register_buyer(&buyer, &metadata);
    registry.verify_profile(&admin, &issuer, &true);
    registry.verify_profile(&admin, &buyer, &true);

    let pool_id = Address::generate(&env);
    let invoice_id = env.register(InvoiceContract, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        EscrowContract,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );
    let invoice = InvoiceContractClient::new(&env, &invoice_id);
    invoice.add_supported_asset(&usdc_id);
    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    let agent_reg_id = env.register(MockAgentRegistry, ());
    MockAgentRegistryClient::new(&env, &agent_reg_id).register_agent(
        &test_agent_id(&env),
        &trusttrove_invoice::Agent {
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
            invoice_id.clone(),
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

    FeeLifecycle {
        env,
        usdc_id,
        invoice,
        pool,
        escrow_id,
        pool_id,
        issuer,
        buyer,
        lp,
        treasury,
    }
}

fn create_fee_lifecycle_invoice(lifecycle: &FeeLifecycle) -> (BytesN<32>, u64) {
    let face_value = 10_000_000_000u128;
    let due_date = lifecycle.env.ledger().timestamp() + 30 * 86400;
    let invoice_id = lifecycle.invoice.create(
        &lifecycle.issuer,
        &lifecycle.buyer,
        &face_value,
        &due_date,
        &lifecycle.usdc_id,
    );
    attest_invoice(&lifecycle.env, &lifecycle.invoice, &invoice_id);
    lifecycle.invoice.list_for_financing(&invoice_id, &200);
    lifecycle.pool.fund_invoice(&invoice_id);
    lifecycle.invoice.mark_shipped(&invoice_id);
    lifecycle
        .invoice
        .confirm_delivery(&invoice_id, &lifecycle.issuer);
    lifecycle
        .invoice
        .confirm_delivery(&invoice_id, &lifecycle.buyer);
    (invoice_id, due_date)
}

struct FeeSettlementExpectation {
    shares: u128,
    deposit: u128,
    face_value: u128,
    refund: u128,
    protocol_cut: u128,
    lp_yield: u128,
    buyer_before: i128,
    treasury_before: i128,
}

fn assert_fee_settlement(lifecycle: &FeeLifecycle, expected: FeeSettlementExpectation) {
    let token = MockTokenClient::new(&lifecycle.env, &lifecycle.usdc_id);
    let lp_before_withdrawal = token.balance(&lifecycle.lp);
    let returned = lifecycle.pool.withdraw(&lifecycle.lp, &expected.shares);
    assert_eq!(returned, expected.deposit + expected.lp_yield);
    assert_eq!(
        token.balance(&lifecycle.lp) - lp_before_withdrawal,
        returned as i128
    );
    assert_eq!(
        token.balance(&lifecycle.buyer),
        expected.buyer_before - (expected.face_value - expected.refund) as i128
    );
    assert_eq!(
        token.balance(&lifecycle.treasury) - expected.treasury_before,
        expected.protocol_cut as i128
    );
    assert_eq!(token.balance(&lifecycle.pool_id), 0);
    // Issue #819: the 98% principal was released to the issuer at funding
    // time, and the 100% face-value repayment was routed buyer → escrow →
    // pool, so escrow is left with nothing.
    assert_eq!(token.balance(&lifecycle.escrow_id), 0);
    assert_eq!(token.balance(&lifecycle.issuer), 9_800_000_000);
}

fn expected_repayment_split(invoice: &trusttrove_invoice::Invoice, repaid_at: u64) -> (u128, u128) {
    let funded_at = invoice
        .funded_at
        .expect("funded invoice should record its funding timestamp");
    let discount = invoice.face_value - invoice.funded_amount;
    let term = invoice.due_date - funded_at;
    let elapsed = repaid_at - funded_at;
    let earned_by_pool = if term == 0 {
        discount
    } else {
        discount * (elapsed as u128) / (term as u128)
    };
    let refund_to_buyer = discount - earned_by_pool;
    (earned_by_pool, refund_to_buyer)
}

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

// --------------- Cross-Contract Integration Tests (Issue #313) ---------------

#[test]
fn test_cross_contract_invoice_pool_escrow_lifecycle() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);

    // 1. Deploy & initialize mock USDC token
    let usdc_id = env.register(MockToken, ());
    let lp_key = TKey(lp.clone());
    let buyer_key = TKey(buyer.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&lp_key, &100_000_000_000_000i128);
        env.storage()
            .persistent()
            .set(&buyer_key, &100_000_000_000_000i128);
    });

    // 2. Deploy real Registry contract & register participants
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let registry = RegistryContractClient::new(&env, &registry_id);

    let metadata: Map<String, String> = Map::new(&env);
    registry.register_issuer(&issuer, &metadata);
    registry.register_buyer(&buyer, &metadata);
    registry.verify_profile(&admin, &issuer, &true);
    registry.verify_profile(&admin, &buyer, &true);
    assert!(registry.is_verified(&issuer));
    assert!(registry.is_verified(&buyer));

    // 3. Deploy real Invoice, Escrow, and Pool contracts
    let pool_id = Address::generate(&env);
    let invoice_id = env.register(InvoiceContract, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        EscrowContract,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );

    let invoice = InvoiceContractClient::new(&env, &invoice_id);
    invoice.add_supported_asset(&usdc_id);
    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    // Configure Agent Registry for attestations
    let agent_reg_id = env.register(MockAgentRegistry, ());
    let agent_reg = MockAgentRegistryClient::new(&env, &agent_reg_id);
    agent_reg.register_agent(
        &test_agent_id(&env),
        &trusttrove_invoice::Agent {
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
            invoice_id.clone(),
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
    pool.set_max_utilization(&admin, &10000); // 100% cap

    // 4. Issuer creates invoice
    let face_value = 1_200_000_000u128; // 120 USDC
    let due_date = env.ledger().timestamp() + 86400 * 30;
    let inv_id = invoice.create(&issuer, &buyer, &face_value, &due_date, &usdc_id);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Created as u32);

    // 5. Attest & list invoice for financing (discount 1666 bps)
    attest_invoice(&env, &invoice, &inv_id);
    let discount_bps = 1666u32;
    invoice.list_for_financing(&inv_id, &discount_bps);

    // 6. LP deposits into Pool
    let deposit_amount = 10_000_000_000u128;
    let shares = pool.deposit(&lp, &deposit_amount);
    assert!(shares > 0);

    // 7. Pool funds invoice (cross-contract call into Escrow to lock and Invoice to mark funded)
    let funded = pool.fund_invoice(&inv_id);
    assert!(funded);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Funded as u32);

    // 8. Settle repayment
    let repaid = pool.receive_repayment(&inv_id, &face_value);
    assert!(repaid);

    // 9. LP withdraws principal + earned yield
    let returned = pool.withdraw(&lp, &shares);
    assert!(
        returned > deposit_amount,
        "LP must receive yield on top of deposit"
    );
}

#[test]
fn test_fee_bearing_early_repayment_with_lp_withdrawal() {
    let lifecycle = setup_fee_lifecycle();
    let deposit = 12_000_000_000u128;
    let shares = lifecycle.pool.deposit(&lifecycle.lp, &deposit);
    let fee_bps = 1000u32;
    lifecycle
        .pool
        .set_protocol_fee(&fee_bps, &lifecycle.treasury);
    let (invoice_id, _) = create_fee_lifecycle_invoice(&lifecycle);

    let token = MockTokenClient::new(&lifecycle.env, &lifecycle.usdc_id);
    let buyer_before = token.balance(&lifecycle.buyer);
    let treasury_before = token.balance(&lifecycle.treasury);
    let invoice = lifecycle.invoice.get(&invoice_id);
    let funded_at = invoice.funded_at.unwrap();
    let term = invoice.due_date - funded_at;
    let repaid_at = funded_at + term / 2;
    let (gross_yield, refund) = expected_repayment_split(&invoice, repaid_at);
    assert!(
        refund > 0,
        "early repayment should return part of the discount"
    );
    lifecycle.env.ledger().set_timestamp(repaid_at);

    assert!(lifecycle.invoice.repay_early(&invoice_id));

    let protocol_cut = gross_yield * fee_bps as u128 / 10_000;
    let lp_yield = gross_yield - protocol_cut;
    assert_fee_settlement(
        &lifecycle,
        FeeSettlementExpectation {
            shares,
            deposit,
            face_value: invoice.face_value,
            refund,
            protocol_cut,
            lp_yield,
            buyer_before,
            treasury_before,
        },
    );
}

#[test]
fn test_repay_early_at_funded_at_refunds_full_discount() {
    let lifecycle = setup_fee_lifecycle();
    let deposit = 12_000_000_000u128;
    let shares = lifecycle.pool.deposit(&lifecycle.lp, &deposit);
    let fee_bps = 1000u32;
    lifecycle
        .pool
        .set_protocol_fee(&fee_bps, &lifecycle.treasury);
    let (invoice_id, _) = create_fee_lifecycle_invoice(&lifecycle);

    let token = MockTokenClient::new(&lifecycle.env, &lifecycle.usdc_id);
    let buyer_before = token.balance(&lifecycle.buyer);
    let treasury_before = token.balance(&lifecycle.treasury);
    let invoice = lifecycle.invoice.get(&invoice_id);
    let funded_at = invoice.funded_at.unwrap();
    let discount = invoice.face_value - invoice.funded_amount;
    assert_eq!(discount, 200_000_000);

    // Nothing has elapsed: the pool must earn nothing and refund the full
    // discount to the buyer.
    lifecycle.env.ledger().set_timestamp(funded_at);
    assert_eq!(lifecycle.env.ledger().timestamp(), funded_at);
    assert!(lifecycle.invoice.repay_early(&invoice_id));

    let settled = lifecycle.invoice.get(&invoice_id);
    assert_eq!(settled.status, InvoiceStatus::Repaid);
    assert_eq!(settled.repaid_at, Some(funded_at));
    assert_eq!(settled.repaid_amount, invoice.face_value);
    assert_eq!(lifecycle.pool.get_stats().total_yield_distributed, 0);
    // A replay must not book the split a second time.
    assert!(lifecycle.invoice.try_repay(&invoice_id).is_err());
    assert_eq!(lifecycle.pool.get_stats().total_yield_distributed, 0);

    assert_fee_settlement(
        &lifecycle,
        FeeSettlementExpectation {
            shares,
            deposit,
            face_value: invoice.face_value,
            refund: discount,
            protocol_cut: 0,
            lp_yield: 0,
            buyer_before,
            treasury_before,
        },
    );
}

#[test]
fn test_repay_early_one_second_before_due_date_refunds_remainder_once() {
    let lifecycle = setup_fee_lifecycle();
    let deposit = 12_000_000_000u128;
    let shares = lifecycle.pool.deposit(&lifecycle.lp, &deposit);
    let fee_bps = 1000u32;
    lifecycle
        .pool
        .set_protocol_fee(&fee_bps, &lifecycle.treasury);
    let (invoice_id, _) = create_fee_lifecycle_invoice(&lifecycle);

    let token = MockTokenClient::new(&lifecycle.env, &lifecycle.usdc_id);
    let buyer_before = token.balance(&lifecycle.buyer);
    let treasury_before = token.balance(&lifecycle.treasury);
    let invoice = lifecycle.invoice.get(&invoice_id);
    let discount = invoice.face_value - invoice.funded_amount;
    let repaid_at = invoice.due_date - 1;
    let (gross_yield, refund) = expected_repayment_split(&invoice, repaid_at);
    assert!(refund > 0);
    assert_eq!(gross_yield + refund, discount);

    lifecycle.env.ledger().set_timestamp(repaid_at);
    assert!(lifecycle.invoice.repay_early(&invoice_id));

    let protocol_cut = gross_yield * fee_bps as u128 / 10_000;
    let lp_yield = gross_yield - protocol_cut;
    let settled = lifecycle.invoice.get(&invoice_id);
    assert_eq!(settled.status, InvoiceStatus::Repaid);
    assert_eq!(settled.repaid_at, Some(repaid_at));
    assert_eq!(
        lifecycle.pool.get_stats().total_yield_distributed,
        lp_yield,
        "the pool must book the complement of the refund exactly once"
    );
    assert!(lifecycle.invoice.try_repay_early(&invoice_id).is_err());
    assert_eq!(lifecycle.pool.get_stats().total_yield_distributed, lp_yield);

    assert_fee_settlement(
        &lifecycle,
        FeeSettlementExpectation {
            shares,
            deposit,
            face_value: invoice.face_value,
            refund,
            protocol_cut,
            lp_yield,
            buyer_before,
            treasury_before,
        },
    );
}

#[test]
fn test_repay_conserves_face_value_before_at_and_after_maturity() {
    let term = 30 * 86400;
    // Independent expectations for the invoice's 200_000_000 discount.
    for (elapsed, gross_yield, refund) in [
        (0, 0, 200_000_000),
        (term / 2, 100_000_000, 100_000_000),
        (term - 1, 199_999_922, 78),
        (term, 200_000_000, 0),
        (term + 1, 200_000_000, 0),
        (term * 2, 200_000_000, 0),
    ] {
        for partial_first in [false, true] {
            let lifecycle = setup_fee_lifecycle();
            let deposit = 12_000_000_000u128;
            let shares = lifecycle.pool.deposit(&lifecycle.lp, &deposit);
            let fee_bps = 1000u32;
            lifecycle
                .pool
                .set_protocol_fee(&fee_bps, &lifecycle.treasury);
            let (invoice_id, _) = create_fee_lifecycle_invoice(&lifecycle);

            let token = MockTokenClient::new(&lifecycle.env, &lifecycle.usdc_id);
            let buyer_before = token.balance(&lifecycle.buyer);
            let treasury_before = token.balance(&lifecycle.treasury);
            let invoice = lifecycle.invoice.get(&invoice_id);
            assert_eq!(invoice.face_value - invoice.funded_amount, 200_000_000);
            if partial_first {
                assert!(lifecycle
                    .invoice
                    .repay_partial(&invoice_id, &(invoice.face_value / 3)));
            }
            let repaid_at = invoice.funded_at.unwrap() + elapsed;
            lifecycle.env.ledger().set_timestamp(repaid_at);
            if partial_first {
                let remaining = lifecycle.invoice.get_remaining_balance(&invoice_id);
                assert!(lifecycle.invoice.repay_partial(&invoice_id, &remaining));
            } else {
                assert!(lifecycle.invoice.repay(&invoice_id));
            }

            let settled = lifecycle.invoice.get(&invoice_id);
            assert_eq!(settled.status, InvoiceStatus::Repaid);
            assert_eq!(settled.repaid_at, Some(repaid_at));
            assert_eq!(settled.repaid_amount, invoice.face_value);
            assert_eq!(settled.remaining_balance, 0);
            assert_eq!(
                invoice.funded_amount + gross_yield + refund,
                invoice.face_value
            );

            let protocol_cut = gross_yield * fee_bps as u128 / 10_000;
            let lp_yield = gross_yield - protocol_cut;
            assert_fee_settlement(
                &lifecycle,
                FeeSettlementExpectation {
                    shares,
                    deposit,
                    face_value: invoice.face_value,
                    refund,
                    protocol_cut,
                    lp_yield,
                    buyer_before,
                    treasury_before,
                },
            );
        }
    }
}

#[test]
#[should_panic]
fn test_cross_contract_unauthorized_pool_setter_rejected() {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let usdc_id = Address::generate(&env);
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let pool_id = Address::generate(&env);
    let invoice_id = env.register(InvoiceContract, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        EscrowContract,
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
            String::from_str(&env, "TrusTrove USDC Pool Shares"),
            String::from_str(&env, "TT-USDC"),
            DEFAULT_SHARE_DECIMALS,
        ),
    );

    let pool = PoolContractClient::new(&env, &pool_id);

    // #827: the pool is constructed with its admin auth mocked above; the
    // rejection under test is the setter. Drop every mocked authorization —
    // only the stored admin may change the cap.
    env.set_auths(&[]);
    let attacker = Address::generate(&env);
    pool.set_max_utilization(&attacker, &5000);
}

#[test]
fn test_cross_contract_partial_repayment_lifecycle() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);

    // 1. Deploy & initialize mock USDC token
    let usdc_id = env.register(MockToken, ());
    let lp_key = TKey(lp.clone());
    let buyer_key = TKey(buyer.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&lp_key, &100_000_000_000_000i128);
        env.storage()
            .persistent()
            .set(&buyer_key, &100_000_000_000_000i128);
    });

    // 2. Deploy real Registry contract & register participants
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let registry = RegistryContractClient::new(&env, &registry_id);

    let metadata: Map<String, String> = Map::new(&env);
    registry.register_issuer(&issuer, &metadata);
    registry.register_buyer(&buyer, &metadata);
    registry.verify_profile(&admin, &issuer, &true);
    registry.verify_profile(&admin, &buyer, &true);

    // 3. Deploy real Invoice, Escrow, and Pool contracts
    let pool_id = Address::generate(&env);
    let invoice_id = env.register(InvoiceContract, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        EscrowContract,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );

    let invoice = InvoiceContractClient::new(&env, &invoice_id);
    invoice.add_supported_asset(&usdc_id);
    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    let agent_reg_id = env.register(MockAgentRegistry, ());
    let agent_reg = MockAgentRegistryClient::new(&env, &agent_reg_id);
    agent_reg.register_agent(
        &test_agent_id(&env),
        &trusttrove_invoice::Agent {
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
            invoice_id.clone(),
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

    // 4. Issuer creates invoice
    let face_value = 1_200_000_000u128; // 120 USDC
    let due_date = env.ledger().timestamp() + 86400 * 30;
    let inv_id = invoice.create(&issuer, &buyer, &face_value, &due_date, &usdc_id);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Created as u32);

    // 5. Attest & list invoice for financing
    attest_invoice(&env, &invoice, &inv_id);
    let discount_bps = 1666u32;
    invoice.list_for_financing(&inv_id, &discount_bps);

    // 6. LP deposits into Pool
    let deposit_amount = 10_000_000_000u128;
    let shares = pool.deposit(&lp, &deposit_amount);
    assert!(shares > 0);

    // 7. Pool funds invoice
    let funded = pool.fund_invoice(&inv_id);
    assert!(funded);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Funded as u32);

    // Advance to confirmed
    invoice.mark_shipped(&inv_id);
    invoice.confirm_delivery(&inv_id, &issuer);
    invoice.confirm_delivery(&inv_id, &buyer);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Confirmed as u32);

    // Advance ledger timestamp to due_date so full yield accrues to the pool
    env.ledger().set_timestamp(due_date);

    // 8. Multi-step partial repayment sequence:
    // Step 8a: Repay partial (1st time)

    let part1 = 400_000_000u128;
    let res1 = invoice.repay_partial(&inv_id, &part1);
    assert!(res1);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Confirmed as u32);
    assert_eq!(invoice.get_remaining_balance(&inv_id), 800_000_000);
    assert_eq!(invoice.get_repaid_amount(&inv_id), 400_000_000);

    // Step 8b: Repay partial (2nd time)
    let part2 = 300_000_000u128;
    let res2 = invoice.repay_partial(&inv_id, &part2);
    assert!(res2);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Confirmed as u32);
    assert_eq!(invoice.get_remaining_balance(&inv_id), 500_000_000);
    assert_eq!(invoice.get_repaid_amount(&inv_id), 700_000_000);

    // Step 8c: Repay remaining balance -> invoice reaches Repaid
    let rem = 500_000_000u128;
    let res3 = invoice.repay_partial(&inv_id, &rem);
    assert!(res3);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Repaid as u32);
    assert_eq!(invoice.get_remaining_balance(&inv_id), 0);
    assert_eq!(invoice.get_repaid_amount(&inv_id), face_value);

    // 9. LP withdraws principal + earned yield
    let returned = pool.withdraw(&lp, &shares);
    assert!(
        returned > deposit_amount,
        "LP must receive yield on top of deposit"
    );
}

#[test]
#[should_panic(expected = "Contract re-entry is not allowed")]
fn test_partial_repayment_then_default_lifecycle() {
    // Policy for the repaid portion:
    // - Buyer's partial repayments are real funds that have reached escrow.
    // - On default, the unpaid portion (face_value - repaid_amount) is the true loss to LPs.
    // - The pool's total_loss_realised should increase by (funded_amount - repaid_amount), not the full funded_amount.
    // - Escrow should release ALL held funds (original funded_amount + partial repayments) to the pool.
    // - Pool token balance should reflect the partial repayments received.
    // - Escrow token balance should be zero after default handling.
    // - Invoice repaid_amount and remaining_balance should be preserved (not reset).
    //
    // Current behavior (buggy):
    // - pool.handle_default records loss of full funded_amount (overstates LP loss).
    // - escrow.handle_default only returns the original funded_amount, stranding partial repayments in escrow.
    // - Invoice trigger_default -> pool.handle_default -> invoice.mark_defaulted causes reentrancy error.

    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);

    // 1. Deploy & initialize mock USDC token
    let usdc_id = env.register(MockToken, ());
    let lp_key = TKey(lp.clone());
    let buyer_key = TKey(buyer.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&lp_key, &100_000_000_000_000i128);
        env.storage()
            .persistent()
            .set(&buyer_key, &100_000_000_000_000i128);
    });

    // 2. Deploy real Registry contract & register participants
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let registry = RegistryContractClient::new(&env, &registry_id);

    let metadata: Map<String, String> = Map::new(&env);
    registry.register_issuer(&issuer, &metadata);
    registry.register_buyer(&buyer, &metadata);
    registry.verify_profile(&admin, &issuer, &true);
    registry.verify_profile(&admin, &buyer, &true);

    // 3. Deploy real Invoice, Escrow, and Pool contracts
    let pool_id = Address::generate(&env);
    let invoice_id = env.register(InvoiceContract, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        EscrowContract,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );

    let invoice = InvoiceContractClient::new(&env, &invoice_id);
    invoice.add_supported_asset(&usdc_id);
    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    let agent_reg_id = env.register(MockAgentRegistry, ());
    let agent_reg = MockAgentRegistryClient::new(&env, &agent_reg_id);
    agent_reg.register_agent(
        &test_agent_id(&env),
        &trusttrove_invoice::Agent {
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
            invoice_id.clone(),
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

    // 4. Issuer creates invoice
    let face_value = 1_200_000_000u128; // 120 USDC
    let due_date = env.ledger().timestamp() + 86400 * 30;
    let inv_id = invoice.create(&issuer, &buyer, &face_value, &due_date, &usdc_id);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Created as u32);

    // 5. Attest & list invoice for financing (discount 1666 bps)
    attest_invoice(&env, &invoice, &inv_id);
    let discount_bps = 1666u32;
    invoice.list_for_financing(&inv_id, &discount_bps);

    // 6. LP deposits into Pool
    let deposit_amount = 10_000_000_000u128;
    let shares = pool.deposit(&lp, &deposit_amount);
    assert!(shares > 0);

    // 7. Pool funds invoice
    let funded = pool.fund_invoice(&inv_id);
    assert!(funded);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Funded as u32);

    // Get funded amount for later assertions
    let invoice_data = invoice.get(&inv_id);
    let funded_amount = invoice_data.funded_amount;
    let _funded_at = invoice_data.funded_at.unwrap();

    // Advance to confirmed
    invoice.mark_shipped(&inv_id);
    invoice.confirm_delivery(&inv_id, &issuer);
    invoice.confirm_delivery(&inv_id, &buyer);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Confirmed as u32);

    // 8. Buyer makes partial repayment (50% of face value)
    let partial_repayment = 600_000_000u128; // 60 USDC = 50% of 120 USDC
    let res = invoice.repay_partial(&inv_id, &partial_repayment);
    assert!(res);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Confirmed as u32);
    assert_eq!(
        invoice.get_remaining_balance(&inv_id),
        face_value - partial_repayment
    );
    assert_eq!(invoice.get_repaid_amount(&inv_id), partial_repayment);

    // Issue #819: the funded principal went to the issuer at funding time,
    // so escrow now holds only the buyer's partial repayment.
    let token = MockTokenClient::new(&env, &usdc_id);
    let escrow_balance_after_partial = token.balance(&escrow_id);
    assert_eq!(escrow_balance_after_partial, partial_repayment as i128);

    // 9. Advance time past due_date AND escrow grace period (60 seconds)
    // escrow DEFAULT_MIN_LOCK_SECONDS = 60, so we need locked_at + 60 < now
    // locked_at == funded_at (approximately), so advance to due_date + 60
    env.ledger().set_timestamp(due_date + 60);

    // 10. Trigger default - this currently fails with reentrancy error
    // because trigger_default -> pool.handle_default -> invoice.mark_defaulted
    // creates a reentrant call into invoice contract.
    let defaulted = invoice.trigger_default(&inv_id);
    assert!(defaulted);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Defaulted as u32);

    // 11. Assert invoice repaid_amount and remaining_balance are preserved
    assert_eq!(invoice.get_repaid_amount(&inv_id), partial_repayment);
    assert_eq!(
        invoice.get_remaining_balance(&inv_id),
        face_value - partial_repayment
    );

    // 12. Assert pool stats reflect correct loss (funded_amount - repaid_amount), not full funded_amount
    let pool_stats = pool.get_stats();
    let expected_loss = funded_amount.saturating_sub(partial_repayment);
    assert_eq!(
        pool_stats.total_loss_realised, expected_loss,
        "LP loss should only be the unpaid funded amount"
    );
    assert_eq!(
        pool_stats.total_funded, 0,
        "no active invoices after default"
    );
    assert_eq!(
        pool_stats.active_invoice_count, 0,
        "active invoice count should be zero"
    );

    // 13. Assert pool token balance reflects partial repayment received
    let pool_token_balance = token.balance(&pool_id);
    assert_eq!(
        pool_token_balance,
        (deposit_amount as i128) - (expected_loss as i128)
    );

    // 14. Assert escrow token balance is zero (all funds released to pool)
    let escrow_token_balance = token.balance(&escrow_id);
    assert_eq!(
        escrow_token_balance, 0,
        "escrow should not strand funds after default"
    );

    // 15. LP withdrawal should reflect correct principal after loss
    let _lp_position_before = pool.get_lp_position(&lp);
    let returned = pool.withdraw(&lp, &shares);
    assert_eq!(returned, deposit_amount - expected_loss);
}

#[test]
#[should_panic(expected = "LP loss should only be the unpaid funded amount")]
fn test_partial_repayment_then_default_pool_loss_calculation() {
    // This test bypasses the reentrancy bug in invoice.trigger_default by calling
    // pool.handle_default and invoice.mark_defaulted directly.
    //
    // Policy for the repaid portion (same as above):
    // - Buyer's partial repayments are real funds that have reached escrow.
    // - On default, the unpaid portion (face_value - repaid_amount) is the true loss to LPs.
    // - The pool's total_loss_realised should increase by (funded_amount - repaid_amount), not the full funded_amount.
    // - Escrow should release ALL held funds (original funded_amount + partial repayments) to the pool.
    //
    // Current buggy behavior in pool.handle_default:
    // - Reads funded_amount from FundedInvoice entry (original funded amount)
    // - Records total_loss_realised += funded_amount (full amount, ignoring partial repayments)
    // - Reduces total_deposits by funded_amount (should only reduce by funded_amount - partial_repayment)
    // - escrow.handle_default only returns the original funded_amount, stranding partial repayments in escrow.

    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);

    // 1. Deploy & initialize mock USDC token
    let usdc_id = env.register(MockToken, ());
    let lp_key = TKey(lp.clone());
    let buyer_key = TKey(buyer.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&lp_key, &100_000_000_000_000i128);
        env.storage()
            .persistent()
            .set(&buyer_key, &100_000_000_000_000i128);
    });

    // 2. Deploy real Registry contract & register participants
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let registry = RegistryContractClient::new(&env, &registry_id);

    let metadata: Map<String, String> = Map::new(&env);
    registry.register_issuer(&issuer, &metadata);
    registry.register_buyer(&buyer, &metadata);
    registry.verify_profile(&admin, &issuer, &true);
    registry.verify_profile(&admin, &buyer, &true);

    // 3. Deploy real Invoice, Escrow, and Pool contracts
    let pool_id = Address::generate(&env);
    let invoice_id = env.register(InvoiceContract, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        EscrowContract,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );

    let invoice = InvoiceContractClient::new(&env, &invoice_id);
    invoice.add_supported_asset(&usdc_id);
    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    let agent_reg_id = env.register(MockAgentRegistry, ());
    let agent_reg = MockAgentRegistryClient::new(&env, &agent_reg_id);
    agent_reg.register_agent(
        &test_agent_id(&env),
        &trusttrove_invoice::Agent {
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
            invoice_id.clone(),
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

    // 4. Issuer creates invoice
    let face_value = 1_200_000_000u128; // 120 USDC
    let due_date = env.ledger().timestamp() + 86400 * 30;
    let inv_id = invoice.create(&issuer, &buyer, &face_value, &due_date, &usdc_id);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Created as u32);

    // 5. Attest & list invoice for financing (discount 1666 bps)
    attest_invoice(&env, &invoice, &inv_id);
    let discount_bps = 1666u32;
    invoice.list_for_financing(&inv_id, &discount_bps);

    // 6. LP deposits into Pool
    let deposit_amount = 10_000_000_000u128;
    let shares = pool.deposit(&lp, &deposit_amount);
    assert!(shares > 0);

    // 7. Pool funds invoice
    let funded = pool.fund_invoice(&inv_id);
    assert!(funded);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Funded as u32);

    // Get funded amount for later assertions
    let invoice_data = invoice.get(&inv_id);
    let funded_amount = invoice_data.funded_amount;
    let _funded_at = invoice_data.funded_at.unwrap();

    // Advance to confirmed
    invoice.mark_shipped(&inv_id);
    invoice.confirm_delivery(&inv_id, &issuer);
    invoice.confirm_delivery(&inv_id, &buyer);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Confirmed as u32);

    // 8. Buyer makes partial repayment (50% of face value)
    let partial_repayment = 600_000_000u128; // 60 USDC = 50% of 120 USDC
    let res = invoice.repay_partial(&inv_id, &partial_repayment);
    assert!(res);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Confirmed as u32);
    assert_eq!(
        invoice.get_remaining_balance(&inv_id),
        face_value - partial_repayment
    );
    assert_eq!(invoice.get_repaid_amount(&inv_id), partial_repayment);

    // Issue #819: the funded principal went to the issuer at funding time,
    // so escrow now holds only the buyer's partial repayment.
    let token = MockTokenClient::new(&env, &usdc_id);
    let escrow_balance_after_partial = token.balance(&escrow_id);
    assert_eq!(escrow_balance_after_partial, partial_repayment as i128);

    // 9. Advance time past due_date AND escrow grace period (60 seconds)
    env.ledger().set_timestamp(due_date + 60);

    // 10. Call pool.handle_default directly (bypassing invoice.trigger_default reentrancy bug)
    let defaulted = pool.handle_default(&inv_id);
    assert!(defaulted);

    // 11. Call invoice.mark_defaulted directly to set invoice status (requires pool auth)
    let marked = invoice.mark_defaulted(&inv_id);
    assert!(marked);
    assert_eq!(invoice.get_status(&inv_id), InvoiceStatus::Defaulted as u32);

    // 12. Assert invoice repaid_amount and remaining_balance are preserved
    assert_eq!(invoice.get_repaid_amount(&inv_id), partial_repayment);
    assert_eq!(
        invoice.get_remaining_balance(&inv_id),
        face_value - partial_repayment
    );

    // 13. Assert pool stats reflect correct loss (funded_amount - repaid_amount), not full funded_amount
    let pool_stats = pool.get_stats();
    let expected_loss = funded_amount.saturating_sub(partial_repayment);
    assert_eq!(
        pool_stats.total_loss_realised, expected_loss,
        "LP loss should only be the unpaid funded amount"
    );
    assert_eq!(
        pool_stats.total_funded, 0,
        "no active invoices after default"
    );
    assert_eq!(
        pool_stats.active_invoice_count, 0,
        "active invoice count should be zero"
    );

    // 14. Assert pool token balance reflects partial repayment received
    let pool_token_balance = token.balance(&pool_id);
    assert_eq!(
        pool_token_balance,
        (deposit_amount as i128) - (expected_loss as i128)
    );

    // 15. Assert escrow token balance is zero (all funds released to pool)
    let escrow_token_balance = token.balance(&escrow_id);
    assert_eq!(
        escrow_token_balance, 0,
        "escrow should not strand funds after default"
    );

    // 16. LP withdrawal should reflect correct principal after loss
    let returned = pool.withdraw(&lp, &shares);
    assert_eq!(returned, deposit_amount - expected_loss);
}

// --------------- Issue #819: issuer payout at funding time ---------------

// Helper: create, attest, list and fund an invoice on a FeeLifecycle env
// without advancing it through shipping/confirmation.
fn fund_one_invoice(lifecycle: &FeeLifecycle, face_value: u128, discount_bps: u32) -> BytesN<32> {
    let due_date = lifecycle.env.ledger().timestamp() + 30 * 86400;
    let invoice_id = lifecycle.invoice.create(
        &lifecycle.issuer,
        &lifecycle.buyer,
        &face_value,
        &due_date,
        &lifecycle.usdc_id,
    );
    attest_invoice(&lifecycle.env, &lifecycle.invoice, &invoice_id);
    lifecycle
        .invoice
        .list_for_financing(&invoice_id, &discount_bps);
    lifecycle.pool.fund_invoice(&invoice_id);
    invoice_id
}

#[test]
fn test_fund_invoice_pays_issuer_at_funding_time() {
    // Issue #819 acceptance criterion 1: after `fund_invoice`, the issuer's
    // USDC balance must equal the funded amount and escrow must not hold the
    // principal (the `escrow.lock` → `escrow.release_to_issuer` flow).
    let lifecycle = setup_fee_lifecycle();
    let deposit = 12_000_000_000u128;
    lifecycle.pool.deposit(&lifecycle.lp, &deposit);

    let token = MockTokenClient::new(&lifecycle.env, &lifecycle.usdc_id);
    // face_value 10_000_000_000, discount 200 bps -> funded_amount 9_800_000_000
    let face_value = 10_000_000_000u128;
    let funded_amount = 9_800_000_000u128;
    let invoice_id = fund_one_invoice(&lifecycle, face_value, 200);

    let invoice = lifecycle.invoice.get(&invoice_id);
    assert_eq!(invoice.funded_amount, funded_amount);

    // The issuer received the full funded amount.
    assert_eq!(token.balance(&lifecycle.issuer), funded_amount as i128);
    // Escrow holds no stranded principal.
    assert_eq!(token.balance(&lifecycle.escrow_id), 0);
    let escrow = EscrowContractClient::new(&lifecycle.env, &lifecycle.escrow_id);
    assert_eq!(escrow.get_locked(&invoice_id), 0);
    // And escrow recorded the issuer release.
    assert!(escrow.was_released_to_issuer(&invoice_id));
}

#[test]
fn test_repay_after_issuer_payout_leaves_no_stranded_principal() {
    // Issue #819: once the principal is released to the issuer at funding,
    // the repayment path (buyer -> escrow -> pool via release_to_pool) must
    // still work and leave escrow with zero balance for the invoice.
    let lifecycle = setup_fee_lifecycle();
    let deposit = 12_000_000_000u128;
    lifecycle.pool.deposit(&lifecycle.lp, &deposit);

    let face_value = 10_000_000_000u128;
    let funded_amount = 9_800_000_000u128;
    let invoice_id = fund_one_invoice(&lifecycle, face_value, 200);

    lifecycle.invoice.mark_shipped(&invoice_id);
    lifecycle
        .invoice
        .confirm_delivery(&invoice_id, &lifecycle.issuer);
    lifecycle
        .invoice
        .confirm_delivery(&invoice_id, &lifecycle.buyer);

    let due_date = lifecycle.invoice.get(&invoice_id).due_date;
    lifecycle.env.ledger().set_timestamp(due_date);
    assert!(lifecycle.invoice.repay(&invoice_id));

    let token = MockTokenClient::new(&lifecycle.env, &lifecycle.usdc_id);
    assert_eq!(
        lifecycle.invoice.get_status(&invoice_id),
        InvoiceStatus::Repaid as u32
    );
    // The buyer's face-value repayment passed through escrow to the pool:
    // nothing is left behind.
    assert_eq!(
        token.balance(&lifecycle.escrow_id),
        0,
        "escrow must not strand principal after repay"
    );
    // The issuer keeps exactly its funded advance.
    assert_eq!(token.balance(&lifecycle.issuer), funded_amount as i128);
    let escrow = EscrowContractClient::new(&lifecycle.env, &lifecycle.escrow_id);
    assert_eq!(escrow.get_locked(&invoice_id), 0);
    // The release marker was consumed by release_to_pool (replay guard).
    assert!(!escrow.was_released_to_issuer(&invoice_id));
}

#[test]
fn test_default_after_issuer_payout_does_not_strand_principal() {
    // Issue #819: on default the funded principal is with the issuer, not in
    // escrow. The pool must recognize the unrecoverable capital: only the
    // escrow-side release guard (#21) is satisfied by the issuer-release
    // marker, TotalFunded is unwound and TotalDeposits reflects the loss.
    // Driven via pool.handle_default directly (invoice.trigger_default is
    // pinned to the separate re-entrancy issue).
    let lifecycle = setup_fee_lifecycle();
    let deposit = 12_000_000_000u128;
    lifecycle.pool.deposit(&lifecycle.lp, &deposit);

    let face_value = 10_000_000_000u128;
    let funded_amount = 9_800_000_000u128;
    let invoice_id = fund_one_invoice(&lifecycle, face_value, 200);

    let due_date = lifecycle.invoice.get(&invoice_id).due_date;
    lifecycle.env.ledger().set_timestamp(due_date + 60);

    let before = lifecycle.pool.get_stats();
    assert_eq!(before.total_deposits, deposit);
    assert_eq!(before.total_funded, funded_amount);

    assert!(lifecycle.pool.handle_default(&invoice_id));

    let token = MockTokenClient::new(&lifecycle.env, &lifecycle.usdc_id);
    // The issuer keeps its advance; escrow holds nothing.
    assert_eq!(token.balance(&lifecycle.issuer), funded_amount as i128);
    assert_eq!(
        token.balance(&lifecycle.escrow_id),
        0,
        "escrow should not strand principal after default"
    );

    let after = lifecycle.pool.get_stats();
    assert_eq!(after.total_funded, 0);
    assert_eq!(after.active_invoice_count, 0);
    assert_eq!(
        after.total_loss_realised,
        before.total_loss_realised + funded_amount
    );
    // LP capital actually left the pool at funding time: deposits shrink by
    // the unrecoverable principal.
    assert_eq!(after.total_deposits, before.total_deposits - funded_amount);
    assert_eq!(
        token.balance(&lifecycle.pool_id) as u128,
        after.total_deposits,
        "pool token balance must match deposit accounting (no insolvency)"
    );
}
