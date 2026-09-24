//! Fuzz target for EscrowContract::release_to_issuer and release_to_pool

use arbitrary::Arbitrary;
use soroban_sdk::{testutils::Address as _, Address, Env};
use trusttrove_fuzz::{generate_invoice_id, EscrowTestEnv};

#[derive(Arbitrary, Debug)]
struct EscrowReleaseInput {
    amount: u128,
    release_to_pool: bool,
    partial: bool,
}

fn main() {
    let input = EscrowReleaseInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_escrow_release(input);
}

fn fuzz_escrow_release(input: EscrowReleaseInput) {
    let te = EscrowTestEnv::new();

    // Generate reasonable amount
    let amount = (input.amount % 1_000_000_000_000).max(1);
    let invoice_id = generate_invoice_id(&te.env, 1);

    // First lock some funds
    let _ = te.escrow.try_lock(&invoice_id, &amount);

    if input.release_to_pool {
        // Test release_to_pool
        let repayment = if input.partial {
            (amount / 2).max(1)
        } else {
            amount
        };

        let result = te.escrow.try_release_to_pool(&invoice_id, &repayment);

        if let Ok(Ok(released)) = result {
            assert!(released, "release_to_pool returned false but didn't error");

            let locked = te.escrow.get_locked(&invoice_id);
            assert_eq!(locked, 0, "Lock should be cleared after release_to_pool");
        }
    } else {
        // Test release_to_issuer
        let issuer = Address::generate(&te.env);

        let result = te.escrow.try_release_to_issuer(&invoice_id, &issuer);

        if let Ok(Ok(released)) = result {
            assert!(
                released,
                "release_to_issuer returned false but didn't error"
            );

            let locked = te.escrow.get_locked(&invoice_id);
            assert_eq!(locked, 0, "Lock should be cleared after release_to_issuer");
        }
    }
}
