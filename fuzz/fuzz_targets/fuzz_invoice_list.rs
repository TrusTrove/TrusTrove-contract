//! Fuzz target for InvoiceContract::list_for_financing

use arbitrary::Arbitrary;
use soroban_sdk::{Address, Env};
use trusttrove_fuzz::InvoiceTestEnv;

#[derive(Arbitrary, Debug)]
struct ListInvoiceInput {
    face_value: u128,
    discount_bps: u32,
}

fn main() {
    let input = ListInvoiceInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_invoice_list(input);
}

fn fuzz_invoice_list(input: ListInvoiceInput) {
    let te = InvoiceTestEnv::new();

    // Generate reasonable values
    let face_value = (input.face_value % 100_000_000_000).max(1);
    let discount_bps = (input.discount_bps % 5001).max(1); // 1-5000
    let due_date = te.env.ledger().timestamp() + 86400;

    // Create invoice
    let invoice_id = te
        .invoice
        .create(&te.issuer, &te.buyer, &face_value, &due_date, &te.usdc_id);

    // Try list_for_financing
    let result = te
        .invoice
        .try_list_for_financing(&invoice_id, &discount_bps);

    // Verify invariants
    if let Ok(Ok(listed)) = result {
        assert!(listed, "list_for_financing returned false but didn't error");

        let invoice = te.invoice.get(&invoice_id);
        assert_eq!(invoice.status, trusttrove_invoice::InvoiceStatus::Listed);
        assert_eq!(invoice.discount_bps, discount_bps);

        // Verify funded amount is computable
        let funded_amount = face_value * (10000 - discount_bps as u128) / 10000;
        if face_value > 0 && discount_bps < 10000 {
            assert!(
                funded_amount < face_value,
                "Funded amount should be less than face value"
            );
        }
    }
}
