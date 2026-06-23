#![cfg(test)]

use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, Address, BytesN, Env,
};

use crate::{PoolContract, PoolContractClient};

use trusttrove_escrow::{EscrowContract as RealEscrow, EscrowContractClient as RealEscrowClient};
use trusttrove_invoice::{
    InvoiceContract as RealInvoice, InvoiceContractClient as RealInvoiceClient,
};

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

struct TestEnv {
    env: Env,
    pool: PoolContractClient<'static>,
    invoice: RealInvoiceClient<'static>,
    usdc_id: Address,
    xlm_id: Address,
    issuer: Address,
    buyer: Address,
    lp: Address,
}

fn setup() -> TestEnv {
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
    let xlm_id = env.register_contract(None, MockToken);

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

    let invoice_id = env.register_contract(None, RealInvoice);
    let escrow_id = env.register_contract(None, RealEscrow);
    let pool_id = env.register_contract(None, PoolContract);

    let invoice = RealInvoiceClient::new(&env, &invoice_id);
    invoice.initialize(&admin, &registry_id);

    let pool = PoolContractClient::new(&env, &pool_id);
    pool.initialize(&admin, &invoice_id, &escrow_id, &usdc_id);

    let escrow = RealEscrowClient::new(&env, &escrow_id);
    escrow.initialize(&admin, &pool_id, &invoice_id, &usdc_id);

    invoice.set_pool_contract(&pool_id);

    TestEnv {
        env,
        pool,
        invoice,
        usdc_id,
        xlm_id,
        issuer,
        buyer,
        lp,
    }
}

fn create_and_list(te: &TestEnv, funding_asset: &Address) -> BytesN<32> {
    let due_date = te.env.ledger().timestamp() + 86400;
    let invoice_id = te.invoice.create(
        &te.issuer,
        &te.buyer,
        &10_000_000_000,
        &due_date,
        funding_asset,
    );
    te.invoice.list_for_financing(&invoice_id, &200);
    invoice_id
}

// ============== DEPOSIT TESTS ==============

#[test]
fn test_deposit_issues_correct_shares() {
    let te = setup();
    let shares = te.pool.deposit(&te.lp, &1_000_000_000);
    assert_eq!(shares, 1_000_000_000);
}

#[test]
fn test_first_deposit_is_one_to_one() {
    let te = setup();
    let shares = te.pool.deposit(&te.lp, &5_000_000_000);
    assert_eq!(shares, 5_000_000_000);

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 5_000_000_000);
    assert_eq!(pos.deposit_count, 1);
}

#[test]
fn test_second_deposit_scales_by_share_price() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);

    let shares = te.pool.deposit(&te.lp, &5_000_000_000);
    assert_eq!(shares, 5_000_000_000);

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 15_000_000_000);
    assert_eq!(pos.deposit_count, 2);
}

// ============== WITHDRAW TESTS ==============

#[test]
fn test_withdraw_returns_correct_usdc() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    let usdc = te.pool.withdraw(&te.lp, &5_000_000_000);
    assert_eq!(usdc, 5_000_000_000);
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

// ============== FUND INVOICE TESTS ==============

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

// ============== STATS TESTS ==============

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

// ============== LP POSITION TESTS ==============

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

// ============== UTILIZATION RATE TESTS ==============

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
#[should_panic(expected = "Error(Contract, #12)")]
fn test_fund_invoice_fails_when_utilization_cap_exceeded() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000); // 10k USDC
    
    // face_value = 10k, funding required = 9.8k
    // 98% utilization exceeds default 85% cap
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
}

#[test]
fn test_set_max_utilization_succeeds() {
    let te = setup();
    assert_eq!(te.pool.get_max_utilization(), 8500);
    
    te.pool.set_max_utilization(&10000);
    assert_eq!(te.pool.get_max_utilization(), 10000);
    
    te.pool.deposit(&te.lp, &10_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    
    // Should succeed because cap is now 100%
    assert!(te.pool.fund_invoice(&invoice_id));
    
    let stats = te.pool.get_stats();
    assert_eq!(stats.max_utilization_bps, 10000);
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_set_max_utilization_fails_if_invalid() {
    let te = setup();
    te.pool.set_max_utilization(&10001);
}

// ============== MULTI-LP TESTS ==============

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

// ============== YIELD HISTORY TESTS ==============

/// Helper: drives an invoice through the full lifecycle: create → list → fund → ship → confirm → repay.
/// Returns the invoice_id and the yield amount (face_value - funded_amount).
fn full_repayment_flow(te: &TestEnv) -> (BytesN<32>, u128) {
    let invoice_id = create_and_list(te, &te.usdc_id);

    // face_value = 10_000_000_000, discount_bps = 200 (2%)
    // funded_amount = 10_000_000_000 * (10000 - 200) / 10000 = 9_800_000_000
    // yield = 10_000_000_000 - 9_800_000_000 = 200_000_000
    let funded_amount: u128 = 10_000_000_000 * (10000 - 200) / 10000;
    let yield_amount: u128 = 10_000_000_000 - funded_amount;

    te.pool.fund_invoice(&invoice_id);
    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);
    te.invoice.repay(&invoice_id);

    (invoice_id, yield_amount)
}

#[test]
fn test_yield_history_empty_initially() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    let history = te.pool.get_lp_yield_history(&te.lp);
    assert_eq!(history.len(), 0);
}

#[test]
fn test_yield_history_after_single_repayment() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    let (invoice_id, yield_amount) = full_repayment_flow(&te);

    let history = te.pool.get_lp_yield_history(&te.lp);
    assert_eq!(history.len(), 1);

    let event = history.get(0).unwrap();
    assert_eq!(event.invoice_id, invoice_id);
    assert_eq!(event.yield_amount, yield_amount);
    // Single LP owns 100% of shares
    assert_eq!(event.lp_share_bps, 10000);
}

#[test]
fn test_yield_history_multiple_lps_proportional() {
    let te = setup();

    // Set up LP2
    let lp2 = Address::generate(&te.env);
    let lp2_bal_key = TKey(lp2.clone());
    te.env.as_contract(&te.usdc_id, || {
        te.env
            .storage()
            .persistent()
            .set(&lp2_bal_key, &100_000_000_000_000i128);
    });

    // LP1 deposits 30B, LP2 deposits 70B → LP1 gets 30%, LP2 gets 70%
    te.pool.deposit(&te.lp, &30_000_000_000);
    te.pool.deposit(&lp2, &70_000_000_000);

    let (invoice_id, yield_amount) = full_repayment_flow(&te);

    // LP1: 30% share
    let history1 = te.pool.get_lp_yield_history(&te.lp);
    assert_eq!(history1.len(), 1);
    let ev1 = history1.get(0).unwrap();
    assert_eq!(ev1.invoice_id, invoice_id);
    assert_eq!(ev1.lp_share_bps, 3000);
    assert_eq!(ev1.yield_amount, yield_amount * 30_000_000_000 / 100_000_000_000);

    // LP2: 70% share
    let history2 = te.pool.get_lp_yield_history(&lp2);
    assert_eq!(history2.len(), 1);
    let ev2 = history2.get(0).unwrap();
    assert_eq!(ev2.invoice_id, invoice_id);
    assert_eq!(ev2.lp_share_bps, 7000);
    assert_eq!(ev2.yield_amount, yield_amount * 70_000_000_000 / 100_000_000_000);

    // Yield shares should sum to the total yield (within rounding)
    assert!(ev1.yield_amount + ev2.yield_amount <= yield_amount);
}

#[test]
fn test_yield_history_multiple_repayments() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);

    let (invoice_id_1, yield_1) = full_repayment_flow(&te);
    let (invoice_id_2, yield_2) = full_repayment_flow(&te);

    let history = te.pool.get_lp_yield_history(&te.lp);
    assert_eq!(history.len(), 2);

    let ev1 = history.get(0).unwrap();
    assert_eq!(ev1.invoice_id, invoice_id_1);
    assert_eq!(ev1.yield_amount, yield_1);
    assert_eq!(ev1.lp_share_bps, 10000);

    let ev2 = history.get(1).unwrap();
    assert_eq!(ev2.invoice_id, invoice_id_2);
    assert_eq!(ev2.yield_amount, yield_2);
    assert_eq!(ev2.lp_share_bps, 10000);
}

#[test]
fn test_yield_history_multiple_lps_multiple_repayments() {
    let te = setup();

    // Set up LP2
    let lp2 = Address::generate(&te.env);
    let lp2_bal_key = TKey(lp2.clone());
    te.env.as_contract(&te.usdc_id, || {
        te.env
            .storage()
            .persistent()
            .set(&lp2_bal_key, &100_000_000_000_000i128);
    });

    // LP1 deposits 40B, LP2 deposits 60B → 40/60 split
    te.pool.deposit(&te.lp, &40_000_000_000);
    te.pool.deposit(&lp2, &60_000_000_000);

    let total_shares_before = 100_000_000_000u128;

    let (inv_id_1, yield_1) = full_repayment_flow(&te);

    // After first repayment, total_deposits increases by yield_1 but shares stay the same.
    // So the share ratio stays 40/60 for the second repayment too.
    let (inv_id_2, yield_2) = full_repayment_flow(&te);

    // LP1 history: 2 events, both at 40% share (4000 bps)
    let h1 = te.pool.get_lp_yield_history(&te.lp);
    assert_eq!(h1.len(), 2);

    let e1_0 = h1.get(0).unwrap();
    assert_eq!(e1_0.invoice_id, inv_id_1);
    assert_eq!(e1_0.lp_share_bps, 4000);
    assert_eq!(e1_0.yield_amount, yield_1 * 40_000_000_000 / total_shares_before);

    let e1_1 = h1.get(1).unwrap();
    assert_eq!(e1_1.invoice_id, inv_id_2);
    assert_eq!(e1_1.lp_share_bps, 4000);
    assert_eq!(e1_1.yield_amount, yield_2 * 40_000_000_000 / total_shares_before);

    // LP2 history: 2 events, both at 60% share (6000 bps)
    let h2 = te.pool.get_lp_yield_history(&lp2);
    assert_eq!(h2.len(), 2);

    let e2_0 = h2.get(0).unwrap();
    assert_eq!(e2_0.invoice_id, inv_id_1);
    assert_eq!(e2_0.lp_share_bps, 6000);
    assert_eq!(e2_0.yield_amount, yield_1 * 60_000_000_000 / total_shares_before);

    let e2_1 = h2.get(1).unwrap();
    assert_eq!(e2_1.invoice_id, inv_id_2);
    assert_eq!(e2_1.lp_share_bps, 6000);
    assert_eq!(e2_1.yield_amount, yield_2 * 60_000_000_000 / total_shares_before);
}
