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

fn fund_and_repay_invoice(te: &TestEnv) -> BytesN<32> {
    let invoice_id = create_and_list(te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);
    te.invoice.mark_shipped(&invoice_id);
    te.invoice.confirm_delivery(&invoice_id, &te.issuer);
    te.invoice.confirm_delivery(&invoice_id, &te.buyer);
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

// ============== DEPOSIT TESTS ==============

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
#[should_panic(expected = "Error(Contract, #5)")]
fn test_withdraw_fails_if_insufficient_liquidity() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);

    // Withdraw more shares than available liquidity can satisfy after funding
    te.pool.withdraw(&te.lp, &300_000_000);
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
fn test_utilization_rate_calculates_correctly() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);
    assert_eq!(te.pool.get_utilization_rate(), 9800);
}

#[test]
fn test_yield_increases_share_price_after_repayment() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    fund_and_repay_invoice(&te);

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 10_000_000_000);
    assert_eq!(pos.usdc_value, 10_200_000_000);
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
    assert_eq!(pos1.usdc_value, 10_050_000_000);
    // LP2 gets 75% (30B/40B) of yield
    assert_eq!(pos2.usdc_value, 30_150_000_000);
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
    te.invoice.repay(&invoice_id);

    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.usdc_value, 10_200_000_000);
    assert_eq!(pos.shares, 10_000_000_000);
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

// ============== REPAYMENT TESTS ==============

#[test]
fn test_receive_repayment() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // face_value=10_000_000_000, discount_bps=200
    // funded_amount = 10_000_000_000 * (10000 - 200) / 10000 = 9_800_000_000
    let yield_amount = 200_000_000;

    let before = te.pool.get_stats();
    let result = te.pool.receive_repayment(&invoice_id, &10_000_000_000);
    assert!(result);

    let after = te.pool.get_stats();
    assert_eq!(after.total_deposits, before.total_deposits + yield_amount);
    assert_eq!(after.total_yield_distributed, yield_amount);
    assert_eq!(after.total_funded, 0);
    assert_eq!(after.active_invoice_count, 0);
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

// ============== DEFAULT TESTS ==============

#[test]
fn test_handle_default() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // funded_amount = 10_000_000_000 * 9800 / 10000 = 9_800_000_000
    let funded_amount = 9_800_000_000;

    let before = te.pool.get_stats();
    let result = te.pool.handle_default(&invoice_id);
    assert!(result);

    let after = te.pool.get_stats();
    assert_eq!(after.total_deposits, before.total_deposits - funded_amount);
    assert_eq!(after.total_funded, 0);
    assert_eq!(after.active_invoice_count, 0);
}

#[test]
fn test_handle_default_unknown_invoice_returns_false() {
    let te = setup();
    let dummy_id = BytesN::from_array(&te.env, &[0u8; 32]);
    let result = te.pool.handle_default(&dummy_id);
    assert!(!result);
}

#[test]
fn test_deposit_when_deposits_zero_but_shares_exist() {
    let te = setup();

    // Deposit exact amount needed to fund the standard test invoice
    // (10B face value, 200bps discount = 9.8B funding amount)
    te.pool.deposit(&te.lp, &9_800_000_000);

    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);

    // Trigger default, wiping out all pool deposits
    te.pool.handle_default(&invoice_id);

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_deposits, 0);
    assert!(stats.total_shares > 0);

    // Attempt new deposit, which should not panic and should issue 1-to-1 shares
    let lp2 = create_lp_with_balance(&te, 10_000_000_000);
    let new_shares = te.pool.deposit(&lp2, &5_000_000_000);
    assert_eq!(new_shares, 5_000_000_000);
}
