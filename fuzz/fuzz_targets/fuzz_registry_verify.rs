//! Fuzz target for RegistryContract::verify_profile, revoke, reinstate

use arbitrary::Arbitrary;
use soroban_sdk::{testutils::Address as _, Address, Env, Map, String};
use trusttrove_fuzz::RegistryTestEnv;
use trusttrove_registry::{Role, VerificationStatus};

#[derive(Arbitrary, Debug)]
struct VerifyInput {
    is_buyer: bool,
    verify_cycles: u8,
    metadata_size: u8,
}

fn main() {
    let input = VerifyInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_registry_verify(input);
}

fn fuzz_registry_verify(input: VerifyInput) {
    let te = RegistryTestEnv::new();
    
    let address = Address::generate(&te.env);
    
    // Generate metadata
    let mut metadata = Map::new(&te.env);
    let size = (input.metadata_size % 20) as u32;
    for i in 0..size {
        let key = String::from_str(&te.env, &format!("key_{}", i));
        let value = String::from_str(&te.env, &format!("value_{}", i));
        metadata.set(key, value);
    }
    
    // Register first
    let _ = if input.is_buyer {
        te.registry.register_buyer(&address, &metadata)
    } else {
        te.registry.register_issuer(&address, &metadata)
    };
    
    // Perform verify/revoke cycles
    let cycles = (input.verify_cycles % 5) as u32 + 1;
    for _ in 0..cycles {
        // Verify
        let result = te.registry.try_verify_profile(&address, &true);
        if let Ok(Ok(verified)) = result {
            assert!(verified);
            assert!(te.registry.is_verified(&address));
            assert_eq!(
                te.registry.get_verification_status(&address),
                VerificationStatus::Verified
            );
        }
        
        // Revoke
        let result = te.registry.try_revoke(&address);
        if let Ok(Ok(revoked)) = result {
            assert!(revoked);
            assert!(!te.registry.is_verified(&address));
            assert_eq!(
                te.registry.get_verification_status(&address),
                VerificationStatus::Revoked
            );
        }
    }
    
    // Final state should be revoked
    assert!(!te.registry.is_verified(&address));
    assert_eq!(
        te.registry.get_verification_status(&address),
        VerificationStatus::Revoked
    );
    
    // Test reinstate
    let result = te.registry.try_reinstate(&address);
    if let Ok(Ok(reinstated)) = result {
        assert!(reinstated);
        assert!(te.registry.is_verified(&address));
        assert_eq!(
            te.registry.get_verification_status(&address),
            VerificationStatus::Verified
        );
    }
}