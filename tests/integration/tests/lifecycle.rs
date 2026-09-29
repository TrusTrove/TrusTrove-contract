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
    registry.verify_profile(&issuer, &true);
    registry.verify_profile(&buyer, &true);

    let invoice_id = env.register_contract(None, InvoiceContract);
    let escrow_id = env.register_contract(None, EscrowContract);
    let pool_id = env.register_contract(None, PoolContract);
    let invoice = InvoiceContractClient::new(&env, &invoice_id);
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
        &invoice_id,
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
    // The issuer's original 98% advance remains in escrow; the 100% face-value
    // repayment is routed back to the pool, so this precisely accounts for the
    // escrow's balance without affecting LP yield or protocol-fee accounting.
    assert_eq!(token.balance(&lifecycle.escrow_id), 9_800_000_000);
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
    let usdc_id = env.register_contract(None, MockToken);
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
    let registry_id = env.register_contract(None, RegistryContract);
    let registry = RegistryContractClient::new(&env, &registry_id);
    registry.initialize(&admin);

    let metadata: Map<String, String> = Map::new(&env);
    registry.register_issuer(&issuer, &metadata);
    registry.register_buyer(&buyer, &metadata);
    registry.verify_profile(&issuer, &true);
    registry.verify_profile(&buyer, &true);
    assert!(registry.is_verified(&issuer));
    assert!(registry.is_verified(&buyer));

    // 3. Deploy real Invoice, Escrow, and Pool contracts
    let invoice_id = env.register_contract(None, InvoiceContract);
    let escrow_id = env.register_contract(None, EscrowContract);
    let pool_id = env.register_contract(None, PoolContract);

    let invoice = InvoiceContractClient::new(&env, &invoice_id);
    invoice.initialize(&admin, &registry_id);
    invoice.add_supported_asset(&usdc_id);
    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    // Configure Agent Registry for attestations
    let agent_reg_id = env.register_contract(None, MockAgentRegistry);
    let agent_reg = MockAgentRegistryClient::new(&env, &agent_reg_id);
    agent_reg.register_agent(
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
        &invoice_id,
        &escrow_id,
        &usdc_id,
        &registry_id,
        &admin,
        &DEFAULT_MIN_INITIAL_DEPOSIT,
        &String::from_str(&env, "TrusTrove USDC Pool Shares"),
        &String::from_str(&env, "TT-USDC"),
        &DEFAULT_SHARE_DECIMALS,
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
fn test_fee_bearing_full_term_repayment_with_lp_withdrawal() {
    let lifecycle = setup_fee_lifecycle();
    let deposit = 12_000_000_000u128;
    let shares = lifecycle.pool.deposit(&lifecycle.lp, &deposit);
    let fee_bps = 1000u32;
    lifecycle
        .pool
        .set_protocol_fee(&fee_bps, &lifecycle.treasury);
    let (invoice_id, due_date) = create_fee_lifecycle_invoice(&lifecycle);

    let token = MockTokenClient::new(&lifecycle.env, &lifecycle.usdc_id);
    let buyer_before = token.balance(&lifecycle.buyer);
    let treasury_before = token.balance(&lifecycle.treasury);
    lifecycle.env.ledger().set_timestamp(due_date);
    let invoice = lifecycle.invoice.get(&invoice_id);
    let (gross_yield, refund) = expected_repayment_split(&invoice, due_date);
    assert_eq!(refund, 0, "full-term repayment should return no discount");

    assert!(lifecycle.invoice.repay(&invoice_id));

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
#[should_panic]
fn test_cross_contract_unauthorized_pool_setter_rejected() {
    let env = Env::default();
    env.set_auths(&[]);

    let admin = Address::generate(&env);
    let registry_id = env.register_contract(None, RegistryContract);
    let invoice_id = env.register_contract(None, InvoiceContract);
    let escrow_id = env.register_contract(None, EscrowContract);
    let pool_id = env.register_contract(None, PoolContract);
    let usdc_id = Address::generate(&env);

    let pool = PoolContractClient::new(&env, &pool_id);
    pool.initialize(
        &admin,
        &invoice_id,
        &escrow_id,
        &usdc_id,
        &registry_id,
        &admin,
        &DEFAULT_MIN_INITIAL_DEPOSIT,
        &String::from_str(&env, "TrusTrove USDC Pool Shares"),
        &String::from_str(&env, "TT-USDC"),
        &DEFAULT_SHARE_DECIMALS,
    );

    // Non-admin call without authorization must fail
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
    let usdc_id = env.register_contract(None, MockToken);
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
    let registry_id = env.register_contract(None, RegistryContract);
    let registry = RegistryContractClient::new(&env, &registry_id);
    registry.initialize(&admin);

    let metadata: Map<String, String> = Map::new(&env);
    registry.register_issuer(&issuer, &metadata);
    registry.register_buyer(&buyer, &metadata);
    registry.verify_profile(&issuer, &true);
    registry.verify_profile(&buyer, &true);

    // 3. Deploy real Invoice, Escrow, and Pool contracts
    let invoice_id = env.register_contract(None, InvoiceContract);
    let escrow_id = env.register_contract(None, EscrowContract);
    let pool_id = env.register_contract(None, PoolContract);

    let invoice = InvoiceContractClient::new(&env, &invoice_id);
    invoice.initialize(&admin, &registry_id);
    invoice.add_supported_asset(&usdc_id);
    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    let agent_reg_id = env.register_contract(None, MockAgentRegistry);
    let agent_reg = MockAgentRegistryClient::new(&env, &agent_reg_id);
    agent_reg.register_agent(
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
        &invoice_id,
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
