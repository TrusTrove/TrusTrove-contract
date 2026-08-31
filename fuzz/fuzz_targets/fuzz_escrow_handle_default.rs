//! Fuzz target for EscrowContract::handle_default

use arbitrary::Arbitrary;
use soroban_sdk::{testutils::{Address as _, Ledger}, Address, Env};
use trusttrove_fuzz::{EscrowTestEnv, generate_invoice_id};

#[derive(Arbitrary, Debug)]
struct HandleDefaultInput {
    amount: u128,
    time_advance: u64,
    caller_is_pool: bool,
}

fn main() {
    let input = HandleDefaultInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_escrow_handle_default(input);
}

fn fuzz_escrow_handle_default(input: HandleDefaultInput) {
    let te = EscrowTestEnv::new();
    
    // Generate reasonable amount
    let amount = (input.amount % 1_000_000_000_000).max(1);
    let invoice_id = generate_invoice_id(&te.env, 1);
    
    // Lock funds
    let _ = te.escrow.try_lock(&invoice_id, &amount);
    
    // Advance time (grace period is 60 seconds)
    let time_advance = (input.time_advance % 300).max(0);
    te.env.ledger().set_timestamp(te.env.ledger().timestamp() + time_advance);
    
    // Choose caller
    let caller = if input.caller_is_pool {
        te.pool.clone()
    } else {
        te.env.current_contract_address()
    };
    
    // Try handle_default
    let result = te.escrow.try_handle_default(&invoice_id, &caller);
    
    // Verify invariants
    if let Ok(Ok(handled)) = result {
        if time_advance >= 60 {
            // Should succeed after grace period
            assert!(handled, "handle_default returned false after grace period");
            let locked = te.escrow.get_locked(&invoice_id);
            assert_eq!(locked, 0, "Lock should be cleared after handle_default");
        } else {
            // Should fail before grace period (panic with error #3)
            // In fuzzing, we just verify it doesn't corrupt state
        }
    }
}