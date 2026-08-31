//! Fuzz target for InvoiceContract::mark_funded

use arbitrary::Arbitrary;
use soroban_sdk::{testutils::Address as _, Address, Env, Symbol};
use trusttrove_fuzz::{InvoiceTestEnv, mock_pool_with_asset, attest_invoice};
use trusttrove_invoice::InvoiceStatus;

#[derive(Arbitrary, Debug)]
struct MarkFundedInput {
    face_value: u128,
    discount_bps: u32,
    funded_amount: u128,
}

fn main() {
    let input = MarkFundedInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_invoice_mark_funded(input);
}

fn fuzz_invoice_mark_funded(input: MarkFundedInput) {
    let te = InvoiceTestEnv::new();
    
    // Generate reasonable values
    let face_value = (input.face_value % 100_000_000_000).max(1);
    let discount_bps = (input.discount_bps % 5000).max(1);
    let due_date = te.env.ledger().timestamp() + 86400;
    
    // Create and list invoice
    let invoice_id = te.invoice.create(&te.issuer, &te.buyer, &face_value, &due_date, &te.usdc_id);
    
    // Attest and list using shared attest function
    attest_invoice(&te.env, &te.invoice, &invoice_id);
    te.invoice.list_for_financing(&invoice_id, &discount_bps);
    
    // Setup pool
    let pool = mock_pool_with_asset(&te.env, &te.usdc_id);
    te.invoice.set_pool_contract(&pool);
    
    // Compute expected funded amount
    let expected_funded = face_value * (10000 - discount_bps as u128) / 10000;
    let funded_amount = if expected_funded > 0 { expected_funded } else { 1 };
    
    // Try mark_funded
    let result = te.invoice.try_mark_funded(&invoice_id, &pool, &te.usdc_id, &funded_amount);
    
    // Verify invariants
    if let Ok(Ok(marked)) = result {
        assert!(marked, "mark_funded returned false but didn't error");
        
        let invoice = te.invoice.get(&invoice_id);
        assert_eq!(invoice.status, trusttrove_invoice::InvoiceStatus::Funded);
        assert_eq!(invoice.funding_pool, Some(pool));
    }
}