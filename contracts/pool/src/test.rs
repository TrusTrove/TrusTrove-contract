#![cfg(test)]

use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Ledger},
    Address, BytesN, Env,
};

use crate::{DataKey, PoolContract, PoolContractClient};

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
    pool_id: Address,
    invoice: RealInvoiceClient<'static>,
    usdc_id: Address,
    xlm_id: Address,
    admin: Address,
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

    // Raise cap to 100% so existing tests (which fund at 98% utilization) still pass
    pool.set_max_utilization(&admin, &10000);

    TestEnv {
        env,
        pool,
        pool_id,
        invoice,
        usdc_id,
        xlm_id,
        admin,
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

// ============== DUST ATTACK / 0-SHARE TESTS (issue #129) ==============

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
    assert_eq!(stats.total_deposits, 10_200_000_000);
    assert_eq!(stats.total_shares, 10_000_000_000);

    // 1 * 10B / 10.2B = 0 shares -> must be rejected, not absorbed.
    let lp2 = create_lp_with_balance(&te, 10_000_000_000);
    te.pool.deposit(&lp2, &1);
}

// A rejected dust deposit must not change pool accounting and must not take the
// depositor's USDC: the whole transaction reverts. This proves no funds are lost.
#[test]
fn test_dust_deposit_rejection_preserves_state_and_funds() {
    let te = setup();
    te.pool.deposit(&te.lp, &10_000_000_000);
    fund_and_repay_invoice(&te);

    let before = te.pool.get_stats();
    let lp2 = create_lp_with_balance(&te, 10_000_000_000);

    // try_* returns Err on contract panic instead of unwinding the test.
    let res = te.pool.try_deposit(&lp2, &1);
    assert!(res.is_err(), "dust deposit should be rejected");

    // Pool deposits/shares are unchanged: the 1 unit was never absorbed.
    let after = te.pool.get_stats();
    assert_eq!(after.total_deposits, before.total_deposits);
    assert_eq!(after.total_shares, before.total_shares);

    // The rejected depositor holds no shares.
    let pos = te.pool.get_lp_position(&lp2);
    assert_eq!(pos.shares, 0);
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

// The first deposit (total_shares == 0) is always 1:1 and never hits the guard,
// even for the smallest possible amount.
#[test]
fn test_first_deposit_of_one_unit_succeeds() {
    let te = setup();
    let shares = te.pool.deposit(&te.lp, &1);
    assert_eq!(shares, 1);
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
    // Raise cap to 100% so funding doesn't get rejected
    te.pool.set_max_utilization(&te.admin, &10000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let _ = te.pool.fund_invoice(&invoice_id);
    assert_eq!(te.pool.get_utilization_rate(), 9800);
}

// ============== MAX UTILIZATION TESTS ==============

#[test]
fn test_default_max_utilization_in_stats() {
    // Fresh pool without setup override to verify initialize default is 8500
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let registry_id = env.register_contract(None, MockRegistry);
    let invoice_id = env.register_contract(None, RealInvoice);
    let escrow_id = env.register_contract(None, RealEscrow);
    let usdc_id = env.register_contract(None, MockToken);
    RealInvoiceClient::new(&env, &invoice_id).initialize(&admin, &registry_id);
    let pool_id = env.register_contract(None, PoolContract);
    let pool = PoolContractClient::new(&env, &pool_id);
    pool.initialize(&admin, &invoice_id, &escrow_id, &usdc_id);
    RealEscrowClient::new(&env, &escrow_id).initialize(&admin, &pool_id, &invoice_id, &usdc_id);
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
fn test_fund_invoice_allowed_when_below_cap() {
    let te = setup();
    te.pool.set_max_utilization(&te.admin, &10000);
    te.pool.deposit(&te.lp, &10_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    let result = te.pool.fund_invoice(&invoice_id);
    assert!(result);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_set_max_utilization_above_10000_panics() {
    let te = setup();
    te.pool.set_max_utilization(&te.admin, &10001);
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
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 86401);
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
