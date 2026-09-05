//! Fuzz target for PoolContract::deposit

use arbitrary::Arbitrary;
use soroban_sdk::{testutils::Address as _, Address, Env};
use trusttrove_fuzz::PoolTestEnv;
use trusttrove_pool::MIN_INITIAL_DEPOSIT;

#[derive(Arbitrary, Debug)]
struct DepositInput {
    amount: u128,
}

fn main() {
    let input = DepositInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_deposit(input);
}

fn fuzz_deposit(input: DepositInput) {
    let te = PoolTestEnv::new();

    // Ensure minimum initial deposit
    let amount = input.amount.max(MIN_INITIAL_DEPOSIT);

    // Give the LP enough balance
    let lp_bal_key = trusttrove_fuzz::BalanceKey(te.lp.clone());
    te.env.as_contract(&te.usdc_id, || {
        te.env
            .storage()
            .persistent()
            .set(&lp_bal_key, &(amount as i128 * 10));
    });

    // Try deposit - should either succeed or fail gracefully
    let result = te.pool.try_deposit(&te.lp, &amount);

    // Verify invariants
    if let Ok(Ok(shares)) = result {
        // If deposit succeeded, shares should be > 0
        assert!(shares > 0, "Deposit succeeded but shares = 0");

        let pos = te.pool.get_lp_position(&te.lp);
        assert_eq!(pos.shares, shares);
        assert_eq!(pos.deposit_count, 1);

        // Verify pool stats
        let stats = te.pool.get_stats();
        assert_eq!(stats.total_deposits, amount);
        assert_eq!(stats.total_shares, shares);
        assert_eq!(stats.available_liquidity, amount);
    }
}
