//! Fuzz target for PoolContract::fund_invoice

use arbitrary::Arbitrary;
use soroban_sdk::{testutils::Address as _, Address, Env};
use trusttrove_fuzz::{PoolTestEnv, attest_invoice};

#[derive(Arbitrary, Debug)]
struct FundInvoiceInput {
    face_value: u128,
    discount_bps: u32,
    pool_deposit: u128,
}

fn main() {
    let input = FundInvoiceInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_fund_invoice(input);
}

fn fuzz_fund_invoice(input: FundInvoiceInput) {
    let te = PoolTestEnv::new();
    
    // Generate reasonable values
    let face_value = (input.face_value % 100_000_000_000).max(10_000);
    let discount_bps = (input.discount_bps % 5000).max(1);
    let pool_deposit = input.pool_deposit.max(face_value * 2);
    
    // Fund the pool
    let lp_bal_key = trusttrove_fuzz::BalanceKey(te.lp.clone());
    te.env.as_contract(&te.usdc_id, || {
        te.env.storage().persistent().set(&lp_bal_key, &(pool_deposit as i128 * 10));
    });
    
    te.pool.deposit(&te.lp, &pool_deposit);
    
    // Create and list an invoice
    let due_date = te.env.ledger().timestamp() + 86400;
    let invoice_id = te.invoice.create(&te.issuer, &te.buyer, &face_value, &due_date, &te.usdc_id);
    attest_invoice(&te.env, &te.invoice, &invoice_id);
    te.invoice.list_for_financing(&invoice_id, &discount_bps);
    
    // Try fund_invoice
    let result = te.pool.try_fund_invoice(&invoice_id);
    
    // Verify invariants
    if let Ok(Ok(funded)) = result {
        // If funding succeeded, it should be true
        assert!(funded, "fund_invoice returned false but didn't error");
        
        let stats = te.pool.get_stats();
        let expected_funded = face_value * (10000 - discount_bps as u128) / 10000;
        
        if expected_funded > 0 {
            assert_eq!(stats.total_funded, expected_funded);
            assert_eq!(stats.active_invoice_count, 1);
            assert!(stats.available_liquidity < pool_deposit);
        }
    }
}