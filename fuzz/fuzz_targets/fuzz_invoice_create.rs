//! Fuzz target for InvoiceContract::create

use arbitrary::Arbitrary;
use soroban_sdk::{testutils::Address as _, Address, Env};
use trusttrove_fuzz::InvoiceTestEnv;

#[derive(Arbitrary, Debug)]
struct CreateInvoiceInput {
    face_value: u128,
    due_offset: u64,
}

fn main() {
    let input = CreateInvoiceInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_invoice_create(input);
}

fn fuzz_invoice_create(input: CreateInvoiceInput) {
    let te = InvoiceTestEnv::new();
    
    // Generate reasonable values
    let face_value = (input.face_value % 100_000_000_000).max(1);
    let due_offset = (input.due_offset % 31_536_000).max(1); // Max 1 year
    let due_date = te.env.ledger().timestamp() + due_offset;
    
    // Try create
    let result = te.invoice.try_create(&te.issuer, &te.buyer, &face_value, &due_date, &te.usdc_id);
    
    // Verify invariants
    if let Ok(Ok(invoice_id)) = result {
        let invoice = te.invoice.get(&invoice_id);
        assert_eq!(invoice.issuer, te.issuer);
        assert_eq!(invoice.buyer, te.buyer);
        assert_eq!(invoice.face_value, face_value);
        assert_eq!(invoice.due_date, due_date);
        assert_eq!(invoice.funding_asset, te.usdc_id);
        assert_eq!(invoice.status, trusttrove_invoice::InvoiceStatus::Created);
        assert!(!invoice.issuer_confirmed);
        assert!(!invoice.buyer_confirmed);
    }
}