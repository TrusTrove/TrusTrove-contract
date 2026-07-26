#![cfg(test)]

use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Events as _, Ledger},
    Address, BytesN, Env, Symbol, TryFromVal,
};

use trusttrove_escrow::{EscrowContract, EscrowContractClient};
use trusttrove_invoice::{InvoiceContract, InvoiceContractClient};
use trusttrove_pool::{PoolContract, PoolContractClient};

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
            .extend_ttl(&RegKey(address), 100, 2_000_000);
    }
}

#[contracttype]
pub struct RegKey(Address);

struct IntegrationEnv {
    env: Env,
    pool: PoolContractClient<'static>,
    pool_id: Address,
    invoice: InvoiceContractClient<'static>,
    escrow: EscrowContractClient<'static>,
    escrow_id: Address,
    usdc_id: Address,
    issuer: Address,
    buyer: Address,
    lp: Address,
}

fn setup_with_auths() -> IntegrationEnv {
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);

    let registry_id = env.register_contract(None, MockRegistry);
    let registry = MockRegistryClient::new(&env, &registry_id);
    registry.register(&issuer);
    registry.register(&buyer);

    let usdc_id = env.register_contract(None, MockToken);

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

    let invoice_id = env.register_contract(None, InvoiceContract);
    let escrow_id = env.register_contract(None, EscrowContract);
    let pool_id = env.register_contract(None, PoolContract);

    let invoice = InvoiceContractClient::new(&env, &invoice_id);
    invoice.initialize(&admin, &registry_id);

    let pool = PoolContractClient::new(&env, &pool_id);
    pool.initialize(&admin, &invoice_id, &escrow_id, &usdc_id);

    let escrow = EscrowContractClient::new(&env, &escrow_id);
    escrow.initialize(&admin, &pool_id, &invoice_id, &usdc_id);

    invoice.set_pool_contract(&pool_id);
    pool.set_max_utilization(&admin, &10000);

    IntegrationEnv {
        env,
        pool,
        pool_id,
        invoice,
        escrow,
        escrow_id,
        usdc_id,
        issuer,
        buyer,
        lp,
    }
}

fn create_and_list(te: &IntegrationEnv) -> BytesN<32> {
    let due_date = te.env.ledger().timestamp() + 86400;
    let invoice_id = te.invoice.create(
        &te.issuer,
        &te.buyer,
        &10_000_000_000,
        &due_date,
        &te.usdc_id,
    );
    te.invoice.list_for_financing(&invoice_id, &200);
    invoice_id
}

fn has_event(
    env: &Env,
    events: &soroban_sdk::Vec<(
        Address,
        soroban_sdk::Vec<soroban_sdk::Val>,
        soroban_sdk::Val,
    )>,
    contract_id: &Address,
    event_name: &str,
) -> bool {
    for i in 0..events.len() {
        let (c, topics, _) = events.get(i).unwrap();
        if c != *contract_id {
            continue;
        }
        let topic0: Symbol = Symbol::try_from_val(env, &topics.get(0).unwrap()).unwrap();
        if topic0 == Symbol::new(env, event_name) {
            return true;
        }
    }
    false
}

// ==================== POSITIVE PATH: FULL LIFECYCLE ====================

#[test]
fn test_full_cross_contract_lifecycle() {
    let te = setup_with_auths();

    // 1. LP deposits into pool
    let shares = te.pool.deposit(&te.lp, &100_000_000_000);
    assert_eq!(shares, 100_000_000_000);
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_deposits, 100_000_000_000);
    assert_eq!(stats.total_shares, 100_000_000_000);

    // 2. Create and list invoice
    let invoice_id = create_and_list(&te);
    assert_eq!(te.invoice.get_status(&invoice_id), 1); // Listed

    // 3. Fund invoice — triggers escrow lock and invoice mark_funded
    let funded = te.pool.fund_invoice(&invoice_id);
    assert!(funded);

    // Verify pool state after funding
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, 9_800_000_000);
    assert_eq!(stats.active_invoice_count, 1);
    assert_eq!(stats.available_liquidity, 100_000_000_000 - 9_800_000_000);

    // Verify invoice state
    assert_eq!(te.invoice.get_status(&invoice_id), 2); // Funded
    assert_eq!(te.invoice.get_face_value(&invoice_id), 10_000_000_000);
    assert_eq!(te.invoice.get_funding_asset(&invoice_id), te.usdc_id);

    // Verify escrow lock
    let locked = te.escrow.get_locked(&invoice_id);
    assert_eq!(locked, 9_800_000_000);

    // 4. Mark shipped (issuer)
    te.invoice.mark_shipped(&invoice_id);
    assert_eq!(te.invoice.get_status(&invoice_id), 3); // Active

    // 5. Confirm delivery (both parties)
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    assert_eq!(te.invoice.get_status(&invoice_id), 3); // Still Active (only 1 confirmed)

    te.invoice.confirm_delivery(&invoice_id, &te.buyer);
    assert_eq!(te.invoice.get_status(&invoice_id), 4); // Confirmed

    let inv = te.invoice.get(&invoice_id);
    assert!(inv.issuer_confirmed);
    assert!(inv.buyer_confirmed);

    // 6. Repay — buyer transfers face_value + pool processes refund + escrow
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);
    let repaid = te.invoice.repay(&invoice_id);
    assert!(repaid);

    // Verify terminal states
    assert_eq!(te.invoice.get_status(&invoice_id), 5); // Repaid

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, 0);
    assert_eq!(stats.active_invoice_count, 0);
    assert_eq!(stats.total_deposits, 100_200_000_000); // original + 200M yield
    assert_eq!(stats.total_yield_distributed, 200_000_000);

    // Escrow record is cleaned up
    let locked = te.escrow.get_locked(&invoice_id);
    assert_eq!(locked, 0);

    // Verify events across all contracts
    let all_events = te.env.events().all();
    assert!(all_events.len() >= 10);
}

// ==================== YIELD VERIFICATION ====================

#[test]
fn test_lp_receives_yield_after_full_cycle() {
    let te = setup_with_auths();

    te.pool.deposit(&te.lp, &10_000_000_000);

    let pos_before = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos_before.shares, 10_000_000_000);
    assert_eq!(pos_before.usdc_value, 10_000_000_000);

    let invoice_id = create_and_list(&te);
    te.pool.fund_invoice(&invoice_id);
    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);
    te.invoice.repay(&invoice_id);

    let pos_after = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos_after.shares, 10_000_000_000);
    // face_value=10B, discount_bps=200 → yield = 10B * 200/10000 = 200M
    assert_eq!(pos_after.usdc_value, 10_200_000_000);
}

// ==================== DEFAULT LIFECYCLE ====================

#[test]
fn test_cross_contract_default_lifecycle() {
    let te = setup_with_auths();

    te.pool.deposit(&te.lp, &100_000_000_000);

    let invoice_id = create_and_list(&te);
    te.pool.fund_invoice(&invoice_id);

    // Invoice is funded; verify escrow has the lock
    assert_eq!(te.escrow.get_locked(&invoice_id), 9_800_000_000);

    // Fast forward past due date
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);

    // Trigger default through invoice contract
    let defaulted = te.invoice.trigger_default(&invoice_id);
    assert!(defaulted);

    // Verify states
    assert_eq!(te.invoice.get_status(&invoice_id), 6); // Defaulted

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, 0);
    assert_eq!(stats.active_invoice_count, 0);
    assert_eq!(stats.total_deposits, 100_000_000_000 - 9_800_000_000);
    assert_eq!(stats.available_liquidity, 100_000_000_000 - 9_800_000_000);

    // Escrow funds returned to pool
    let locked = te.escrow.get_locked(&invoice_id);
    assert_eq!(locked, 0);
}

// ==================== EVENT ASSERTIONS ====================

#[test]
fn test_events_emitted_during_funding() {
    let te = setup_with_auths();

    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te);
    let event_count_before = te.env.events().all().len();

    te.pool.fund_invoice(&invoice_id);

    let events_after = te.env.events().all();
    let new_events_count = events_after.len() - event_count_before;

    // Funding emits at least: pool invoice_funded, escrow funds_locked,
    // invoice invoice_funded
    assert!(
        new_events_count >= 3,
        "expected >= 3 events from fund_invoice (pool, escrow, invoice), got {new_events_count}"
    );

    // Verify pool event
    assert!(
        has_event(&te.env, &events_after, &te.pool_id, "invoice_funded"),
        "pool should emit invoice_funded event"
    );

    // Verify escrow event
    assert!(
        has_event(&te.env, &events_after, &te.escrow_id, "funds_locked"),
        "escrow should emit funds_locked event"
    );
}

#[test]
fn test_events_emitted_during_repayment() {
    let te = setup_with_auths();

    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te);
    te.pool.fund_invoice(&invoice_id);
    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);

    let event_count_before = te.env.events().all().len();
    te.invoice.repay(&invoice_id);
    let events_after = te.env.events().all();
    let new_events_count = events_after.len() - event_count_before;
    assert!(
        new_events_count >= 2,
        "expected >= 2 events from repay (invoice repaid, pool repayment_received), got {new_events_count}"
    );

    assert!(
        has_event(&te.env, &events_after, &te.pool_id, "repayment_received"),
        "pool should emit repayment_received event"
    );
}

// ==================== NEGATIVE AUTH TESTS ====================

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_receive_repayment_requires_invoice_contract_auth() {
    let te = setup_with_auths();

    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te);
    te.pool.fund_invoice(&invoice_id);

    // Clear mock auths — receive_repayment calls invoice_contract.require_auth()
    te.env.set_auths(&[]);
    te.pool.receive_repayment(&invoice_id, &10_000_000_000);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_handle_default_requires_invoice_contract_auth() {
    let te = setup_with_auths();

    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te);
    te.pool.fund_invoice(&invoice_id);

    te.env.set_auths(&[]);
    te.pool.handle_default(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_receive_repayment_with_refund_requires_invoice_contract_auth() {
    let te = setup_with_auths();

    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te);
    te.pool.fund_invoice(&invoice_id);

    te.env.set_auths(&[]);
    te.pool
        .receive_repayment_with_refund(&invoice_id, &10_000_000_000, &0, &te.buyer, &te.issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_fund_invoice_fails_on_asset_mismatch() {
    let te = setup_with_auths();

    te.pool.deposit(&te.lp, &100_000_000_000);

    // Create invoice with a different asset (not usdc)
    let due_date = te.env.ledger().timestamp() + 86400;
    let other_asset = Address::generate(&te.env);
    let invoice_id = te.invoice.create(
        &te.issuer,
        &te.buyer,
        &10_000_000_000,
        &due_date,
        &other_asset,
    );
    te.invoice.list_for_financing(&invoice_id, &200);

    te.pool.fund_invoice(&invoice_id);
}

// ==================== TWO-LP SCENARIO ====================

#[test]
fn test_two_lifecycles_with_two_lps() {
    let te = setup_with_auths();

    let lp2 = Address::generate(&te.env);
    let lp2_bal_key = TKey(lp2.clone());
    te.env.as_contract(&te.usdc_id, || {
        te.env
            .storage()
            .persistent()
            .set(&lp2_bal_key, &100_000_000_000_000i128);
    });

    te.pool.deposit(&te.lp, &10_000_000_000);
    te.pool.deposit(&lp2, &30_000_000_000);

    let invoice_id = create_and_list(&te);
    te.pool.fund_invoice(&invoice_id);
    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);
    te.invoice.repay(&invoice_id);

    let pos1 = te.pool.get_lp_position(&te.lp);
    let pos2 = te.pool.get_lp_position(&lp2);

    assert_eq!(pos1.shares, 10_000_000_000);
    assert_eq!(pos2.shares, 30_000_000_000);
    assert_eq!(pos1.usdc_value, 10_050_000_000); // 25% of 200M yield
    assert_eq!(pos2.usdc_value, 30_150_000_000); // 75% of 200M yield
}

// ==================== FUNDING REJECTION TESTS ====================

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_fund_invoice_fails_insufficient_liquidity() {
    let te = setup_with_auths();

    // No deposit = no liquidity
    let invoice_id = create_and_list(&te);
    te.pool.fund_invoice(&invoice_id);
}

#[test]
fn test_double_funding_rejected() {
    let te = setup_with_auths();

    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te);
    te.pool.fund_invoice(&invoice_id);

    let result = te.pool.try_fund_invoice(&invoice_id);
    assert!(result.is_err());
}

#[test]
fn test_handle_default_unknown_returns_false() {
    let te = setup_with_auths();

    let dummy_id = BytesN::from_array(&te.env, &[0u8; 32]);
    let result = te.pool.handle_default(&dummy_id);
    assert!(!result);
}
