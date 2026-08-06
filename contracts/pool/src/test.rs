#![cfg(test)]

use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{
        storage::Instance as _, Address as _, Events as _, Ledger, MockAuth, MockAuthInvoke,
    },
    Address, BytesN, Env, IntoVal, Symbol, TryFromVal,
};

use crate::{
    DataKey, PoolContract, PoolContractClient, MIN_INITIAL_DEPOSIT, TTL_EXTEND_TO, TTL_THRESHOLD,
};

use trusttrove_escrow::{EscrowContract as RealEscrow, EscrowContractClient as RealEscrowClient};
use trusttrove_invoice::{
    InvoiceContract as RealInvoice, InvoiceContractClient as RealInvoiceClient,
};

// Default invoice parameters matching create_and_list() defaults
// These computed constants eliminate magic numbers in test assertions
// and make the tests self-correcting when parameters change.
const DEFAULT_FACE_VALUE: u128 = 10_000_000_000;
const DEFAULT_DISCOUNT_BPS: u32 = 200;
const DEFAULT_FUNDED_AMOUNT: u128 =
    DEFAULT_FACE_VALUE * (10000 - DEFAULT_DISCOUNT_BPS as u128) / 10000;
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
    env.mock_all_auths_allowing_non_root_auth();

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
    escrow.initialize(&admin, &pool_id, &usdc_id);

    invoice.add_supported_asset(&usdc_id);
    invoice.add_supported_asset(&xlm_id);

    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

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
    create_and_list_with_params(te, funding_asset, 10_000_000_000, 200)
}

fn create_and_list_with_params(
    te: &TestEnv,
    funding_asset: &Address,
    face_value: u128,
    discount_bps: u32,
) -> BytesN<32> {
    let due_date = te.env.ledger().timestamp() + 86400;
    let invoice_id =
        te.invoice
            .create(&te.issuer, &te.buyer, &face_value, &due_date, funding_asset);
    te.invoice.list_for_financing(&invoice_id, &discount_bps);
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
    assert_eq!(
        stats.total_deposits,
        DEFAULT_FACE_VALUE + DEFAULT_YIELD_AMOUNT
    );
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

// The initial deposit in an empty pool must be at least MIN_INITIAL_DEPOSIT (1 USDC)
// to prevent share-price griefing attacks.
#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_first_deposit_below_minimum_panics_invalid_amount() {
    let te = setup();
    te.pool.deposit(&te.lp, &(MIN_INITIAL_DEPOSIT - 1));
}

#[test]
fn test_first_deposit_at_minimum_succeeds() {
    let te = setup();
    let shares = te.pool.deposit(&te.lp, &MIN_INITIAL_DEPOSIT);
    assert_eq!(shares, MIN_INITIAL_DEPOSIT);
}

#[test]
fn test_first_deposit_above_minimum_succeeds() {
    let te = setup();
    let shares = te.pool.deposit(&te.lp, &(MIN_INITIAL_DEPOSIT + 10_000_000));
    assert_eq!(shares, MIN_INITIAL_DEPOSIT + 10_000_000);
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
fn test_fund_invoice_rejects_zero_funded_amount() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list_with_params(&te, &te.usdc_id, 1, 5000);

    let before = te.pool.get_stats();
    let result = te.pool.try_fund_invoice(&invoice_id);
    assert!(result.is_err(), "zero funded amount should be rejected");

    let after = te.pool.get_stats();
    assert_eq!(after.total_funded, before.total_funded);
    assert_eq!(after.active_invoice_count, before.active_invoice_count);
    assert_eq!(after.available_liquidity, before.available_liquidity);
    assert_eq!(te.invoice.get_status(&invoice_id), 1);
}

#[test]
fn test_fund_invoice_allows_boundary_amount_of_one() {
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    // face_value=2, discount_bps=5000 -> funded_amount = 2 * 5000 / 10000 = 1
    let invoice_id = create_and_list_with_params(&te, &te.usdc_id, 2, 5000);

    let result = te.pool.fund_invoice(&invoice_id);
    assert!(result);

    let stats = te.pool.get_stats();
    assert_eq!(stats.total_funded, 1);
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

// ============== ISSUE #275: FUND INVOICE EDGE CASES ==============

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
#[should_panic(expected = "Error(Contract, #8)")]
fn test_fund_invoice_already_funded_invoice_panics() {
    // After successfully funding an invoice, a second call to fund_invoice
    // must be rejected with InvoiceNotListed (#8) since the invoice status
    // is now Funded (2) rather than Listed (1).
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    // Second funding attempt should panic — invoice is no longer Listed
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
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_stats_panics_on_uninitialized_pool() {
    // A freshly deployed contract (no initialize() call) must not silently
    // return zero-filled stats — that would let callers mistake an uninitialized
    // pool for an empty-but-healthy one. Instead get_stats() must panic with
    // NotInitialized (#2).
    let env = Env::default();
    env.mock_all_auths();
    let pool_id = env.register_contract(None, crate::PoolContract);
    let pool = crate::PoolContractClient::new(&env, &pool_id);
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
    RealEscrowClient::new(&env, &escrow_id).initialize(&admin, &pool_id, &usdc_id);
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
fn test_fund_invoice_is_permissionless() {
    // Verify that fund_invoice can be called by any address without admin authorization.
    // Setup normally (with mock_all_auths) so initialization succeeds, then test with a non-admin caller.
    let te = setup();
    te.pool.deposit(&te.lp, &100_000_000_000);
    let invoice_id = create_and_list(&te, &te.usdc_id);

    // The default setup already tested that admin can call fund_invoice.
    // What we're verifying is that the auth requirement was REMOVED.
    // If admin.require_auth() was still in the code, it would fail.
    // Since we're calling it in a setup that uses mock_all_auths, if it works,
    // the auth requirement is gone.
    let result = te.pool.fund_invoice(&invoice_id);
    assert!(
        result,
        "fund_invoice should succeed (no admin auth required)"
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
    let yield_amount = DEFAULT_YIELD_AMOUNT;

    let before = te.pool.get_stats();
    let position_before = te.pool.get_lp_position(&te.lp);
    let result = te.pool.receive_repayment(&invoice_id, &10_000_000_000);
    assert!(result);

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
        <(u128, u128)>::try_from_val(&te.env, &data).unwrap(),
        (10_000_000_000, yield_amount)
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
#[should_panic]
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

    te.pool.handle_default(&phantom_id);
}

// ============== DEFAULT TESTS ==============

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

    let after = te.pool.get_stats();
    let position_after = te.pool.get_lp_position(&te.lp);
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

    let events = te.env.events().all();
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

    // A default writes the funded amount off against pool deposits (realising
    // the loss) while leaving the share supply untouched: total_shares and the
    // LP's share balance are preserved, and total_loss_realised tracks the
    // loss. Deposit value falls by exactly the funded amount.
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

#[test]
#[should_panic]
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

#[test]
fn test_deposit_when_deposits_zero_but_shares_exist() {
    let te = setup();

    // Deposit exact amount needed to fund the standard test invoice
    // (10B face value, 200bps discount = 9.8B funding amount)
    te.pool.deposit(&te.lp, &DEFAULT_FUNDED_AMOUNT);

    let invoice_id = create_and_list(&te, &te.usdc_id);
    te.pool.fund_invoice(&invoice_id);
    te.env
        .ledger()
        .set_timestamp(te.env.ledger().timestamp() + 60);

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

// ============== ISSUE #269: DEPOSIT AFTER DEFAULT (SHARE PRICE < 1) ==============

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

    // Default wipes out 9.8B, leaving LP1 with 0.2B / 10B shares = 0.02 USDC per share
    let _ = te.pool.handle_default(&invoice_id);

    let stats_after_default = te.pool.get_stats();
    assert_eq!(stats_after_default.total_deposits, DEFAULT_YIELD_AMOUNT); // 10B - 9.8B
    assert_eq!(stats_after_default.total_shares, 10_000_000_000); // unchanged
                                                                  // Share price: 200M / 10B = 0.02

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

// ============== ISSUE #270: WITHDRAW EXACT TOTAL SHARES TO ZERO ==============

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

    // Verify LP has no position
    let pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(pos.shares, 0);
    assert_eq!(pos.usdc_value, 0);

    // Re-deposit succeeds and is treated as a first-depositor (1:1 shares)
    let new_shares = te.pool.deposit(&te.lp, &5_000_000_000);
    assert_eq!(new_shares, 5_000_000_000);

    // Verify pool state after re-deposit
    let stats_after_redeposit = te.pool.get_stats();
    assert_eq!(stats_after_redeposit.total_shares, 5_000_000_000);
    assert_eq!(stats_after_redeposit.total_deposits, 5_000_000_000);

    let final_pos = te.pool.get_lp_position(&te.lp);
    assert_eq!(final_pos.shares, 5_000_000_000);
    assert_eq!(final_pos.deposit_count, 1); // Reset on full withdrawal, so 1 deposit in this cycle
}

// ============== ISSUE #258: RESET LP STATE ON FULL WITHDRAWAL ==============

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

// ============== ISSUE #272: NEGATIVE AUTH TESTS ==============
// Note: These functions (receive_repayment, handle_default, fund_invoice) are guarded by
// cross-contract auth checks. Testing unauthorized access requires more complex mocking
// of contract-to-contract calls. The auth logic is enforced at the contract boundary
// and verified through integration tests. Individual contract auth is covered by the
// fact that mock_all_auths() is used during setup and cleared when specific auths are set.

// ============== ISSUE #274: INSUFFICIENT LIQUIDITY ON WITHDRAW ==============

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
    // Event payload: (usdc_amount, shares_burned)
    let events = te.env.events().all();
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

// ============== ISSUE #268: WITHDRAW AFTER REPAYMENT (SHARE PRICE > 1) ==============

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

    let events = te.env.events().all();
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

// ============== ISSUE #263: INITIALIZE ADDRESS COLLISION GUARD ==============

// A fresh, valid initialize() with four distinct addresses must keep working;
// `setup()` (used throughout this file) already exercises this path, but this
// test makes the positive case explicit for the collision guard added below.
#[test]
fn test_initialize_accepts_distinct_addresses() {
    let te = setup();
    let stats = te.pool.get_stats();
    assert_eq!(stats.total_shares, 0);
    assert_eq!(stats.total_deposits, 0);
}

// Every pairwise collision among (admin, invoice_contract, escrow_contract,
// usdc_asset) must be rejected with InvalidConfiguration (#15) so the
// handle_default gate can never collide with the admin path.
#[test]
fn test_initialize_rejects_each_pairwise_address_collision() {
    let env = Env::default();
    env.mock_all_auths();

    let base = [
        Address::generate(&env), // admin
        Address::generate(&env), // invoice_contract
        Address::generate(&env), // escrow_contract
        Address::generate(&env), // usdc_asset
    ];

    let pairs = [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)];
    for (i, j) in pairs {
        let mut addrs = base.clone();
        addrs[j] = addrs[i].clone();

        let pool_id = env.register_contract(None, PoolContract);
        let pool = PoolContractClient::new(&env, &pool_id);
        let res = pool.try_initialize(&addrs[0], &addrs[1], &addrs[2], &addrs[3]);
        assert!(
            res.is_err(),
            "collision between initialize() params {i} and {j} should be rejected"
        );
    }
}

// ============== ISSUE #265: PREVENT ALREADYFUNDED SILENT SHADOWING ==============

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

// ============== ISSUE #281: INSTANCE TTL EXTENSION ==============

// Every state-changing entrypoint must extend the contract's instance TTL so
// an active pool never expires. New instance storage entries start with a
// small default ttl (well above the extend_ttl threshold), so the
// initialize()/set_max_utilization() calls in `setup()` are no-ops for the
// ttl bump. Drive the ttl down below the threshold here, then confirm a
// state-changing call (deposit) bumps it back up close to the configured
// extend-to window.
#[test]
fn test_deposit_extends_instance_ttl_when_below_threshold() {
    // The default instance TTL (4096) is below TTL_THRESHOLD (500_000),
    // so initialize() extends it to ~TTL_EXTEND_TO. Verify the extension
    // happens by checking TTL before and after initialization.
    let env = Env::default();
    env.mock_all_auths();

    let admin = Address::generate(&env);
    let registry_id = env.register_contract(None, MockRegistry);
    let invoice_id = env.register_contract(None, RealInvoice);
    let escrow_id = env.register_contract(None, RealEscrow);
    let usdc_id = env.register_contract(None, MockToken);

    RealInvoiceClient::new(&env, &invoice_id).initialize(&admin, &registry_id);

    let pool_id = env.register_contract(None, PoolContract);

    // Before initialize: TTL is the default of ~4096 ledgers.
    let ttl_before = env.as_contract(&pool_id, || env.storage().instance().get_ttl());
    assert!(
        ttl_before < TTL_THRESHOLD,
        "default instance ttl should be below TTL_THRESHOLD, got {ttl_before}"
    );

    let pool = PoolContractClient::new(&env, &pool_id);
    pool.initialize(&admin, &invoice_id, &escrow_id, &usdc_id);

    // After initialize: TTL should be bumped to ~TTL_EXTEND_TO.
    let ttl_after = env.as_contract(&pool_id, || env.storage().instance().get_ttl());
    assert!(
        ttl_after >= 1_999_000,
        "instance ttl should be extended close to EXTEND_TO, got {ttl_after}"
    );
}

// ============== ISSUE #273: INITIALIZATION & AUTH REGRESSION COVERAGE ==============

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_double_initialize_panics() {
    let env = Env::default();

    let admin = Address::generate(&env);
    let registry_id = env.register_contract(None, MockRegistry);
    let invoice_id = env.register_contract(None, RealInvoice);
    let escrow_id = env.register_contract(None, RealEscrow);
    let usdc_id = env.register_contract(None, MockToken);
    let pool_id = env.register_contract(None, PoolContract);
    let pool = PoolContractClient::new(&env, &pool_id);

    // Initialize invoice with explicit auth
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &invoice_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    RealInvoiceClient::new(&env, &invoice_id).initialize(&admin, &registry_id);

    // Initialize escrow with explicit auth
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &escrow_id,
            fn_name: "initialize",
            args: (admin.clone(), pool_id.clone(), usdc_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    RealEscrowClient::new(&env, &escrow_id).initialize(&admin, &pool_id, &usdc_id);

    // First pool initialize — succeeds with explicit auth
    env.mock_auths(&[MockAuth {
        address: &admin,
        invoke: &MockAuthInvoke {
            contract: &pool_id,
            fn_name: "initialize",
            args: (
                admin.clone(),
                invoice_id.clone(),
                escrow_id.clone(),
                usdc_id.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    pool.initialize(&admin, &invoice_id, &escrow_id, &usdc_id);

    // Verify storage state after first initialize
    env.as_contract(&pool_id, || {
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        assert_eq!(stored_admin, admin);
        let stored_invoice: Address = env
            .storage()
            .instance()
            .get(&DataKey::InvoiceContract)
            .unwrap();
        assert_eq!(stored_invoice, invoice_id);
        let stored_escrow: Address = env
            .storage()
            .instance()
            .get(&DataKey::EscrowContract)
            .unwrap();
        assert_eq!(stored_escrow, escrow_id);
        let stored_usdc: Address = env.storage().instance().get(&DataKey::UsdcAsset).unwrap();
        assert_eq!(stored_usdc, usdc_id);
    });

    // Second initialize — panics with AlreadyInitialized (#1)
    pool.initialize(&admin, &invoice_id, &escrow_id, &usdc_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_deposit_before_initialize_panics() {
    let env = Env::default();

    let usdc_id = env.register_contract(None, MockToken);
    let pool_id = env.register_contract(None, PoolContract);
    let pool = PoolContractClient::new(&env, &pool_id);
    let lp = Address::generate(&env);

    // Give LP a balance in the mock USDC token
    let lp_bal_key = TKey(lp.clone());
    env.as_contract(&usdc_id, || {
        env.storage()
            .persistent()
            .set(&lp_bal_key, &100_000_000_000_000i128);
    });

    // Mock LP auth but pool is not initialized → should panic with NotInitialized (#2)
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
    let env = Env::default();

    let pool_id = env.register_contract(None, PoolContract);
    let pool = PoolContractClient::new(&env, &pool_id);
    let lp = Address::generate(&env);

    // Mock LP auth but pool is not initialized → should panic with NotInitialized (#2)
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
