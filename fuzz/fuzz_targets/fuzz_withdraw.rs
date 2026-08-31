//! Fuzz target for PoolContract::withdraw

use arbitrary::Arbitrary;
use soroban_sdk::{testutils::Address as _, Address, Env};
use trusttrove_fuzz::PoolTestEnv;
use trusttrove_pool::MIN_INITIAL_DEPOSIT;

#[derive(Arbitrary, Debug)]
struct WithdrawInput {
    deposit_amount: u128,
    withdraw_shares: u128,
}

fn main() {
    let input = WithdrawInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_withdraw(input);
}

fn fuzz_withdraw(input: WithdrawInput) {
    let te = PoolTestEnv::new();

    // Ensure minimum initial deposit
    let deposit_amount = input.deposit_amount.max(10_000);

    // Give the LP enough balance
    let lp_bal_key = trusttrove_fuzz::BalanceKey(te.lp.clone());
    te.env.as_contract(&te.usdc_id, || {
        te.env
            .storage()
            .persistent()
            .set(&lp_bal_key, &(deposit_amount as i128 * 10));
    });

    // First deposit
    let shares = te.pool.deposit(&te.lp, &deposit_amount);
    assert!(shares > 0);

    // Try withdraw - should either succeed or fail gracefully
    let result = te.pool.try_withdraw(&te.lp, &input.withdraw_shares);

    // Verify invariants
    if let Ok(Ok(usdc)) = result {
        // If withdraw succeeded, usdc should be > 0 and <= deposit
        assert!(usdc > 0, "Withdraw succeeded but usdc = 0");
        assert!(
            usdc <= deposit_amount,
            "Withdraw returned more than deposited"
        );

        let pos = te.pool.get_lp_position(&te.lp);
        assert!(pos.shares <= shares, "Shares increased after withdraw");

        // Verify pool stats consistency
        let stats = te.pool.get_stats();
        assert!(stats.total_deposits >= deposit_amount - usdc);
    }
}
