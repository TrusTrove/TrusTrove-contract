//! Fuzz target for RegistryContract::register_issuer and register_buyer

use arbitrary::Arbitrary;
use soroban_sdk::{testutils::Address as _, Address, Env, Map, String};
use trusttrove_fuzz::RegistryTestEnv;
use trusttrove_registry::Role;

#[derive(Arbitrary, Debug)]
struct RegisterInput {
    is_buyer: bool,
    metadata_size: u8,
}

fn main() {
    let input = RegisterInput::arbitrary(&mut arbitrary::Unstructured::new(&[])).unwrap();
    fuzz_registry_register(input);
}

fn fuzz_registry_register(input: RegisterInput) {
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
    
    // Try register
    let result = if input.is_buyer {
        te.registry.try_register_buyer(&address, &metadata)
    } else {
        te.registry.try_register_issuer(&address, &metadata)
    };
    
    // Verify invariants
    if let Ok(Ok(registered)) = result {
        assert!(registered, "register returned false but didn't error");
        
        let profile = te.registry.get_profile(&address);
        if input.is_buyer {
            assert_eq!(profile.role(), Role::Buyer);
        } else {
            assert_eq!(profile.role(), Role::Issuer);
        }
        assert!(!profile.verified());
        assert_eq!(profile.metadata, metadata);
        
        // Verify cannot re-register
        let result2 = if input.is_buyer {
            te.registry.try_register_buyer(&address, &metadata)
        } else {
            te.registry.try_register_issuer(&address, &metadata)
        };
        assert!(result2.is_err(), "Re-register should fail");
    }
}