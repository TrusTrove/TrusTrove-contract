//! Fuzz target for EscrowContract::lock

use arbitrary::Arbitrary;
use soroban_sdk::{testutils::Address as _, Address, Env};
use trusttrove_fuzz::{EscrowTestEnv, generate_invoice_id};

#[derive(Arbitrary, Debug)]
struct EscrowLockInput {
    amount: u128,
}

fn main() {
    let input = EscrowLockInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_escrow_lock(input);
}

fn fuzz_escrow_lock(input: EscrowLockInput) {
    let te = EscrowTestEnv::new();
    
    // Generate reasonable amount
    let amount = (input.amount % 1_000_000_000_000).max(1);
    let invoice_id = generate_invoice_id(&te.env, 1);
    
    // Try lock
    let result = te.escrow.try_lock(&invoice_id, &amount);
    
    // Verify invariants
    if let Ok(Ok(locked)) = result {
        assert!(locked, "lock returned false but didn't error");
        
        let locked_amount = te.escrow.get_locked(&invoice_id);
        assert_eq!(locked_amount, amount);
        
        let locked_at = te.escrow.get_locked_at(&invoice_id);
        assert_eq!(locked_at, te.env.ledger().timestamp());
    }
}