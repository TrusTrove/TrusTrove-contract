#![cfg(test)]

use crate::{DataKey, Profile, RegistryContract, RegistryContractClient, Role, VerificationStatus};
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use soroban_sdk::{map, testutils::Address as _, vec, Address, Env, String, Vec};

fn setup() -> (Env, RegistryContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register_contract(None, RegistryContract);
    let client = RegistryContractClient::new(&env, &contract_id);
    (env, client)
}

#[test]
fn test_initialize() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_register_issuer() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Acme Corp")
        )
    ];
    let result = client.register_issuer(&issuer, &metadata);
    assert!(result);
    let profile = client.get_profile(&issuer);
    assert_eq!(profile.role, crate::Role::Issuer);
    assert!(profile.verified);
}

#[test]
fn test_register_buyer() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let buyer = Address::generate(&env);
    let metadata = map![&env];
    let result = client.register_buyer(&buyer, &metadata);
    assert!(result);
    let profile = client.get_profile(&buyer);
    assert_eq!(profile.role, crate::Role::Buyer);
    assert!(profile.verified);
}

#[test]
fn test_is_verified_returns_true_for_registered() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert!(client.is_verified(&issuer));
}

#[test]
fn test_is_verified_returns_false_for_unknown() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    assert!(!client.is_verified(&unknown));
}

#[test]
fn test_revoke_sets_verified_false() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert!(client.is_verified(&issuer));
    let result = client.revoke(&issuer);
    assert!(result);
    assert!(!client.is_verified(&issuer));
}

#[test]
fn test_update_metadata_self_succeeds() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Acme Corp"),
        )
    ];
    client.register_issuer(&issuer, &metadata);

    let updated_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Acme LLC"),
        )
    ];
    let result = client.update_metadata(&issuer, &updated_metadata);
    assert!(result);

    let profile = client.get_profile(&issuer);
    assert_eq!(profile.metadata, updated_metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_update_metadata_unregistered_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    let metadata = map![&env];
    client.update_metadata(&unknown, &metadata);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_update_metadata_wrong_auth_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, RegistryContract);
    let client = RegistryContractClient::new(&env, &contract_id);

    let issuer = Address::generate(&env);
    let metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Acme Corp"),
        )
    ];
    let profile = Profile {
        address: issuer.clone(),
        role: Role::Issuer,
        verified: true,
        registered_at: env.ledger().timestamp(),
        metadata,
    };

    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::Profile(issuer.clone()), &profile);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Profile(issuer.clone()), 100, 2_000_000);
    });

    let updated_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Bad Actor"),
        )
    ];
    client.update_metadata(&issuer, &updated_metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_duplicate_registration_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.register_issuer(&issuer, &map![&env]);
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_double_initialize_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    client.initialize(&admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_get_profile_unknown_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    client.get_profile(&unknown);
}

#[test]
fn test_batch_register_issuers_empty_vec() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let entries = Vec::new(&env);
    let skipped = client.batch_register_issuers(&entries);
    assert_eq!(skipped.len(), 0);
}

#[test]
fn test_batch_register_issuers_all_new() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);
    let issuer3 = Address::generate(&env);

    let metadata1 = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Issuer 1")
        )
    ];
    let metadata2 = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Issuer 2")
        )
    ];
    let metadata3 = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Issuer 3")
        )
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), metadata1),
        (issuer2.clone(), metadata2),
        (issuer3.clone(), metadata3),
    ];

    let skipped = client.batch_register_issuers(&entries);
    assert_eq!(skipped.len(), 0);

    assert!(client.is_verified(&issuer1));
    assert!(client.is_verified(&issuer2));
    assert!(client.is_verified(&issuer3));

    assert_eq!(client.get_profile(&issuer1).role, crate::Role::Issuer);
    assert_eq!(client.get_profile(&issuer2).role, crate::Role::Issuer);
    assert_eq!(client.get_profile(&issuer3).role, crate::Role::Issuer);
}

#[test]
fn test_batch_register_issuers_all_duplicate() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);

    client.register_issuer(&issuer1, &map![&env]);
    client.register_issuer(&issuer2, &map![&env]);

    let entries = vec![
        &env,
        (issuer1.clone(), map![&env]),
        (issuer2.clone(), map![&env]),
    ];

    let skipped = client.batch_register_issuers(&entries);
    // Both were already registered — both are reported as skipped.
    assert_eq!(skipped.len(), 2);
    assert!(skipped.contains(&issuer1));
    assert!(skipped.contains(&issuer2));
}

#[test]
fn test_batch_register_issuers_mixed() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env); // existing
    let issuer2 = Address::generate(&env); // new
    let issuer3 = Address::generate(&env); // new

    client.register_issuer(&issuer1, &map![&env]);

    let entries = vec![
        &env,
        (issuer1.clone(), map![&env]),
        (issuer2.clone(), map![&env]),
        (issuer3.clone(), map![&env]),
    ];

    let skipped = client.batch_register_issuers(&entries);
    // Only issuer1 was already registered.
    assert_eq!(skipped.len(), 1);
    assert!(skipped.contains(&issuer1));

    assert!(client.is_verified(&issuer1));
    assert!(client.is_verified(&issuer2));
    assert!(client.is_verified(&issuer3));
}

#[test]
fn test_verify_profile_updates_status() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    assert!(client.is_verified(&issuer));

    // Revoke
    client.revoke(&issuer);
    assert!(!client.is_verified(&issuer));

    // Re-verify
    let result = client.verify_profile(&issuer, &true);
    assert!(result);
    assert!(client.is_verified(&issuer));

    // Un-verify again
    let result2 = client.verify_profile(&issuer, &false);
    assert!(result2);
    assert!(!client.is_verified(&issuer));
}

#[test]
#[should_panic]
fn test_verify_profile_unknown_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    client.verify_profile(&unknown, &true);
}

#[test]
fn test_get_verification_status_unregistered() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    assert_eq!(
        client.get_verification_status(&unknown),
        VerificationStatus::Unregistered
    );
}

#[test]
fn test_get_verification_status_verified() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Verified
    );
}

#[test]
fn test_get_verification_status_revoked() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.revoke(&issuer);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Revoked
    );
}

#[test]
fn test_get_verification_status_distinguishes_revoked_from_unregistered() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let never_registered = Address::generate(&env);
    let revoked = Address::generate(&env);

    client.register_issuer(&revoked, &map![&env]);
    client.revoke(&revoked);

    // is_verified returns false for both — indistinguishable
    assert!(!client.is_verified(&never_registered));
    assert!(!client.is_verified(&revoked));

    // get_verification_status tells them apart
    assert_eq!(
        client.get_verification_status(&never_registered),
        VerificationStatus::Unregistered
    );
    assert_eq!(
        client.get_verification_status(&revoked),
        VerificationStatus::Revoked
    );
}

#[test]
fn test_get_verification_status_re_verified_returns_verified() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.revoke(&issuer);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Revoked
    );
    client.verify_profile(&issuer, &true);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Verified
    );
}

// ============== ISSUE #61: TRANSFER OWNERSHIP ==============

#[test]
fn test_registry_transfer_ownership_changes_admin() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    client.initialize(&admin);
    client.transfer_ownership(&new_admin);
    assert_eq!(client.get_admin(), new_admin);
}

#[test]
#[should_panic]
fn test_registry_transfer_ownership_requires_both_auths() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    client.initialize(&admin);
    env.set_auths(&[]);
    client.transfer_ownership(&new_admin);
}

// ============== PROPERTY-BASED INVARIANT TESTS ==============

#[test]
fn prop_is_verified_always_consistent_with_get_verification_status_after_register() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(0u32..=1u32), |_seed| {
            let (env, client) = setup();
            let admin = Address::generate(&env);
            client.initialize(&admin);
            let address = Address::generate(&env);
            client.register_issuer(&address, &map![&env]);
            let verified = client.is_verified(&address);
            let status = client.get_verification_status(&address);
            prop_assert!(verified);
            prop_assert_eq!(status, VerificationStatus::Verified);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_revoke_always_sets_is_verified_false_and_status_revoked() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(0u32..=1u32), |_seed| {
            let (env, client) = setup();
            let admin = Address::generate(&env);
            client.initialize(&admin);
            let address = Address::generate(&env);
            client.register_issuer(&address, &map![&env]);
            client.revoke(&address);
            prop_assert!(!client.is_verified(&address));
            prop_assert_eq!(
                client.get_verification_status(&address),
                VerificationStatus::Revoked
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_unregistered_address_never_verified() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(0u32..=1u32), |_seed| {
            let (env, client) = setup();
            let admin = Address::generate(&env);
            client.initialize(&admin);
            let unknown = Address::generate(&env);
            prop_assert!(!client.is_verified(&unknown));
            prop_assert_eq!(
                client.get_verification_status(&unknown),
                VerificationStatus::Unregistered
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_re_verify_after_revoke_restores_verified_state() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(0u32..=1u32), |_seed| {
            let (env, client) = setup();
            let admin = Address::generate(&env);
            client.initialize(&admin);
            let address = Address::generate(&env);
            client.register_issuer(&address, &map![&env]);
            client.revoke(&address);
            prop_assert_eq!(
                client.get_verification_status(&address),
                VerificationStatus::Revoked
            );
            client.verify_profile(&address, &true);
            prop_assert!(client.is_verified(&address));
            prop_assert_eq!(
                client.get_verification_status(&address),
                VerificationStatus::Verified
            );
            Ok(())
        })
        .unwrap();
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_batch_register_issuers_exceeds_limit() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let mut entries = Vec::new(&env);
    for _ in 0..51 {
        let address = Address::generate(&env);
        entries.push_back((address, map![&env]));
    }
    client.batch_register_issuers(&entries);
}
