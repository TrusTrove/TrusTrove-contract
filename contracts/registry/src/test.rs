#![cfg(test)]

extern crate std;

use crate::{
    DataKey, Profile, RegistryContract, RegistryContractClient, Role, VerificationStatus,
    TTL_EXTEND_TO, TTL_THRESHOLD,
};
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use soroban_sdk::{
    map,
    testutils::{
        storage::{Instance as _, Persistent as _},
        Address as _, Events as _, Ledger,
    },
    vec, Address, Env, IntoVal, String, Symbol, TryIntoVal, Val, Vec,
};

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

    // Assert the contract_initialized event was emitted
    let all_events = env.events().all();
    assert_eq!(all_events.len(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_get_admin_before_initialize_panics_with_not_initialized() {
    let (_env, client) = setup();
    // get_admin should panic with NotInitialized (#4) instead of NotFound (#3)
    client.get_admin();
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
    assert_eq!(profile.role(), crate::Role::Issuer);
    assert!(!profile.verified());
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
    assert_eq!(profile.role(), crate::Role::Buyer);
    assert!(!profile.verified());
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_register_issuer_before_initialize_panics() {
    let (env, client) = setup();
    client.register_issuer(&Address::generate(&env), &map![&env]);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_register_buyer_before_initialize_panics() {
    let (env, client) = setup();
    client.register_buyer(&Address::generate(&env), &map![&env]);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_batch_register_issuers_before_initialize_panics() {
    let (env, client) = setup();
    client.batch_register_issuers(&vec![&env]);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_revoke_before_initialize_panics() {
    let (env, client) = setup();
    client.revoke(&Address::generate(&env));
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_reinstate_before_initialize_panics() {
    let (env, client) = setup();
    client.reinstate(&Address::generate(&env));
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_verify_profile_before_initialize_panics() {
    let (env, client) = setup();
    client.verify_profile(&Address::generate(&env), &true);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_transfer_ownership_before_initialize_panics() {
    let (env, client) = setup();
    client.transfer_ownership(&Address::generate(&env));
}

#[test]
fn test_is_verified_returns_false_for_registered_but_unverified() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert!(!client.is_verified(&issuer));
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
    assert!(!client.is_verified(&issuer));
    client.verify_profile(&issuer, &true);
    assert!(client.is_verified(&issuer));
    let result = client.revoke(&issuer);
    assert!(result);
    assert!(!client.is_verified(&issuer));
}

#[test]
fn test_revoke_already_revoked_returns_true_no_reemit() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert!(!client.is_verified(&issuer));

    // First revoke — should be a no-op (already unverified) and return true.
    let result = client.revoke(&issuer);
    assert!(result);
    assert!(!client.is_verified(&issuer));
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Pending
    );

    // Second revoke on already-unverified profile — should succeed
    // without panic and without altering state.
    let result2 = client.revoke(&issuer);
    assert!(result2);
    assert!(!client.is_verified(&issuer));
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Pending
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_revoke_unregistered_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    client.revoke(&unknown);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_revoke_wrong_auth_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, RegistryContract);
    let client = RegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let metadata = map![&env];
    let profile = Profile::new(Role::Issuer, true, env.ledger().timestamp(), metadata);

    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::Profile(issuer.clone()), &profile);
        env.storage().persistent().extend_ttl(
            &DataKey::Profile(issuer.clone()),
            TTL_THRESHOLD,
            TTL_EXTEND_TO,
        );
    });

    assert!(client.is_verified(&issuer));
    client.revoke(&issuer);
    assert!(client.is_verified(&issuer));
    assert!(env.events().all().is_empty());
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_re_register_revoked_issuer_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert!(!client.is_verified(&issuer));
    client.verify_profile(&issuer, &true);
    assert!(client.is_verified(&issuer));
    client.revoke(&issuer);
    assert!(!client.is_verified(&issuer));
    // A revoked address still has a profile in storage, so re-registering
    // must panic with AlreadyRegistered (#2).
    client.register_issuer(&issuer, &map![&env]);
}

#[test]
fn test_reinstate_revoked_issuer_restores_verification() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert!(!client.is_verified(&issuer));
    client.verify_profile(&issuer, &true);
    assert!(client.is_verified(&issuer));

    client.revoke(&issuer);
    assert!(!client.is_verified(&issuer));

    // Reinstate the revoked issuer via admin.verify_profile.
    client.verify_profile(&issuer, &true);
    assert!(client.is_verified(&issuer));
}

// ============== REINSTATE TESTS ==============

#[test]
fn test_reinstate_restores_verified_and_emits_event() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert!(!client.is_verified(&issuer));
    client.verify_profile(&issuer, &true);
    assert!(client.is_verified(&issuer));

    client.revoke(&issuer);
    assert!(!client.is_verified(&issuer));

    let result = client.reinstate(&issuer);
    assert!(result);
    assert!(client.is_verified(&issuer));

    assert_eq!(
        env.events().all(),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "contract_initialized"), admin.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "issuer_registered"), issuer.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "profile_verified"), issuer.clone()).into_val(&env),
                true.into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "address_revoked"), issuer.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "address_reinstated"), issuer.clone()).into_val(&env),
                ().into_val(&env),
            ),
        ]
    );
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_reinstate_wrong_auth_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, RegistryContract);
    let client = RegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let metadata = map![&env];
    let profile = Profile::new(Role::Issuer, false, env.ledger().timestamp(), metadata);

    env.as_contract(&contract_id, || {
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::Profile(issuer.clone()), &profile);
        env.storage().persistent().extend_ttl(
            &DataKey::Profile(issuer.clone()),
            TTL_THRESHOLD,
            TTL_EXTEND_TO,
        );
    });

    // The issuer is not the admin and env.mock_all_auths() was not called,
    // so calling reinstate should panic with an auth error.
    client.reinstate(&issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_reinstate_unregistered_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    client.reinstate(&unknown);
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
#[should_panic(expected = "Error(Contract, #7)")]
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
    let profile = Profile::new(Role::Issuer, true, env.ledger().timestamp(), metadata);

    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::Profile(issuer.clone()), &profile);
        env.storage().persistent().extend_ttl(
            &DataKey::Profile(issuer.clone()),
            TTL_THRESHOLD,
            TTL_EXTEND_TO,
        );
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
fn test_update_profile_happy_path() {
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
        ),
        (
            String::from_str(&env, "tax_id"),
            String::from_str(&env, "12-3456789"),
        ),
    ];
    let result = client.update_profile(&issuer, &updated_metadata);
    assert!(result);

    let profile = client.get_profile(&issuer);
    assert_eq!(profile.metadata, updated_metadata);
    assert_eq!(profile.role(), crate::Role::Issuer);
    assert!(!profile.verified());

    assert_eq!(
        env.events().all(),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "contract_initialized"), admin.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "issuer_registered"), issuer.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "profile_updated"), issuer.clone()).into_val(&env),
                ().into_val(&env),
            ),
        ]
    );
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_update_profile_wrong_auth_panics() {
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
    let profile = Profile::new(Role::Issuer, true, env.ledger().timestamp(), metadata);

    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .set(&DataKey::Profile(issuer.clone()), &profile);
        env.storage().persistent().extend_ttl(
            &DataKey::Profile(issuer.clone()),
            TTL_THRESHOLD,
            TTL_EXTEND_TO,
        );
    });

    let updated_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Bad Actor"),
        )
    ];
    client.update_profile(&issuer, &updated_metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_update_profile_unregistered_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    let metadata = map![&env];
    client.update_profile(&unknown, &metadata);
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

// ============== CROSS-ROLE REGISTRATION GUARD (Issue #189) ==============

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_register_issuer_then_buyer_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, RegistryContract);
    let client = RegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let metadata = map![&env];

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "register_issuer",
            args: (issuer.clone(), metadata.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.register_issuer(&issuer, &metadata);

    assert!(!client.is_verified(&issuer));
    assert_eq!(client.get_profile(&issuer).role(), Role::Issuer);

    assert_eq!(
        env.events().all(),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "contract_initialized"), admin.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "issuer_registered"), issuer.clone()).into_val(&env),
                ().into_val(&env),
            ),
        ]
    );

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "register_buyer",
            args: (issuer.clone(), metadata.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.register_buyer(&issuer, &metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_register_buyer_then_issuer_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, RegistryContract);
    let client = RegistryContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let buyer = Address::generate(&env);
    let metadata = map![&env];

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &buyer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "register_buyer",
            args: (buyer.clone(), metadata.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.register_buyer(&buyer, &metadata);

    assert!(!client.is_verified(&buyer));
    assert_eq!(client.get_profile(&buyer).role(), Role::Buyer);

    assert_eq!(
        env.events().all(),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "contract_initialized"), admin.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "buyer_registered"), buyer.clone()).into_val(&env),
                ().into_val(&env),
            ),
        ]
    );

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &buyer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "register_issuer",
            args: (buyer.clone(), metadata.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.register_issuer(&buyer, &metadata);
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

    assert!(!client.is_verified(&issuer1));
    assert!(!client.is_verified(&issuer2));
    assert!(!client.is_verified(&issuer3));

    assert_eq!(client.get_profile(&issuer1).role(), crate::Role::Issuer);
    assert_eq!(client.get_profile(&issuer2).role(), crate::Role::Issuer);
    assert_eq!(client.get_profile(&issuer3).role(), crate::Role::Issuer);
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

    assert!(!client.is_verified(&issuer1));
    assert!(!client.is_verified(&issuer2));
    assert!(!client.is_verified(&issuer3));
}

// ============== ISSUE #446: PRE-VALIDATION ==============

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_batch_register_issuers_invalid_metadata_rejects_all() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env); // would be valid
    let issuer2 = Address::generate(&env); // would be valid
    let issuer3 = Address::generate(&env); // has invalid metadata (oversized)

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Good Issuer")
        )
    ];

    let mut oversized_metadata = map![&env];
    for i in 0..21 {
        let key = String::from_str(&env, &std::format!("key_{}", i));
        let value = String::from_str(&env, &std::format!("value_{}", i));
        oversized_metadata.set(key, value);
    }

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata.clone()),
        (issuer2.clone(), valid_metadata.clone()),
        (issuer3.clone(), oversized_metadata),
    ];

    // This should panic because issuer3 has invalid metadata.
    // With pre-validation, issuer1 and issuer2 are NOT persisted.
    client.batch_register_issuers(&entries);
}

#[test]
fn test_batch_register_issuers_invalid_metadata_leaves_state_clean() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env); // would be valid
    let issuer2 = Address::generate(&env); // has invalid metadata (empty key)

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Good Issuer")
        )
    ];

    let bad_metadata = map![
        &env,
        (String::from_str(&env, ""), String::from_str(&env, "value"))
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata),
        (issuer2.clone(), bad_metadata),
    ];

    // Panic expected because issuer2 has invalid metadata.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_register_issuers(&entries);
    }));
    assert!(
        result.is_err(),
        "batch_register_issuers should panic on invalid metadata"
    );

    // Verify that issuer1 was NOT registered — the entire batch was rejected.
    assert!(!client.is_verified(&issuer1));
    assert_eq!(
        client.get_verification_status(&issuer1),
        VerificationStatus::Unregistered
    );
}

// ============== BATCH REGISTER BUYERS (#448) ==============

#[test]
fn test_batch_register_buyers_empty_vec() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let entries = Vec::new(&env);
    let skipped = client.batch_register_buyers(&entries);
    assert_eq!(skipped.len(), 0);
}

#[test]
fn test_batch_register_buyers_all_new() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);
    let buyer3 = Address::generate(&env);

    let metadata1 = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Buyer 1")
        )
    ];
    let metadata2 = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Buyer 2")
        )
    ];
    let metadata3 = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Buyer 3")
        )
    ];

    let entries = vec![
        &env,
        (buyer1.clone(), metadata1),
        (buyer2.clone(), metadata2),
        (buyer3.clone(), metadata3),
    ];

    let skipped = client.batch_register_buyers(&entries);
    assert_eq!(skipped.len(), 0);

    assert!(!client.is_verified(&buyer1));
    assert!(!client.is_verified(&buyer2));
    assert!(!client.is_verified(&buyer3));

    assert_eq!(client.get_profile(&buyer1).role(), crate::Role::Buyer);
    assert_eq!(client.get_profile(&buyer2).role(), crate::Role::Buyer);
    assert_eq!(client.get_profile(&buyer3).role(), crate::Role::Buyer);
}

#[test]
fn test_batch_register_buyers_all_duplicate() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);

    client.register_buyer(&buyer1, &map![&env]);
    client.register_buyer(&buyer2, &map![&env]);

    let entries = vec![
        &env,
        (buyer1.clone(), map![&env]),
        (buyer2.clone(), map![&env]),
    ];

    let skipped = client.batch_register_buyers(&entries);
    // Both were already registered — both are reported as skipped.
    assert_eq!(skipped.len(), 2);
    assert!(skipped.contains(&buyer1));
    assert!(skipped.contains(&buyer2));
}

#[test]
fn test_batch_register_buyers_mixed() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let buyer1 = Address::generate(&env); // existing
    let buyer2 = Address::generate(&env); // new
    let buyer3 = Address::generate(&env); // new

    client.register_buyer(&buyer1, &map![&env]);

    let entries = vec![
        &env,
        (buyer1.clone(), map![&env]),
        (buyer2.clone(), map![&env]),
        (buyer3.clone(), map![&env]),
    ];

    let skipped = client.batch_register_buyers(&entries);
    // Only buyer1 was already registered.
    assert_eq!(skipped.len(), 1);
    assert!(skipped.contains(&buyer1));

    assert!(!client.is_verified(&buyer1));
    assert!(!client.is_verified(&buyer2));
    assert!(!client.is_verified(&buyer3));

    assert_eq!(client.get_profile(&buyer2).role(), crate::Role::Buyer);
    assert_eq!(client.get_profile(&buyer3).role(), crate::Role::Buyer);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_batch_register_buyers_exceeds_limit() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let mut entries = Vec::new(&env);
    for _ in 0..51 {
        let address = Address::generate(&env);
        entries.push_back((address, map![&env]));
    }
    client.batch_register_buyers(&entries);
}

#[test]
fn test_verify_profile_updates_status() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    assert!(!client.is_verified(&issuer));

    // Verify
    let result = client.verify_profile(&issuer, &true);
    assert!(result);
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
#[should_panic(expected = "Error(Contract, #3)")]
fn test_verify_profile_unknown_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    client.verify_profile(&unknown, &true);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_verify_profile_wrong_auth_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    // Clear all mocked auths — a non-admin caller should be rejected
    env.set_auths(&[]);
    client.verify_profile(&issuer, &true);
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
    client.verify_profile(&issuer, &true);
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
    client.verify_profile(&issuer, &true);
    client.revoke(&issuer);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Revoked
    );
}

#[test]
fn test_get_verification_status_distinguishes_pending_from_unregistered() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let never_registered = Address::generate(&env);
    let pending = Address::generate(&env);

    client.register_issuer(&pending, &map![&env]);

    // is_verified returns false for both — indistinguishable
    assert!(!client.is_verified(&never_registered));
    assert!(!client.is_verified(&pending));

    // get_verification_status tells them apart
    assert_eq!(
        client.get_verification_status(&never_registered),
        VerificationStatus::Unregistered
    );
    assert_eq!(
        client.get_verification_status(&pending),
        VerificationStatus::Pending
    );
}

#[test]
fn test_get_verification_status_revoked_distinct_from_pending() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let pending = Address::generate(&env);
    let revoked = Address::generate(&env);

    client.register_issuer(&pending, &map![&env]);
    client.register_issuer(&revoked, &map![&env]);
    client.verify_profile(&revoked, &true);
    client.revoke(&revoked);

    assert_eq!(
        client.get_verification_status(&pending),
        VerificationStatus::Pending
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
    client.verify_profile(&issuer, &true);
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

// ============== ISSUE #173: TRANSFER ADMIN ==============

#[test]
fn test_transfer_admin_changes_admin() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    client.initialize(&admin);
    client.transfer_admin(&new_admin);
    assert_eq!(client.get_admin(), new_admin);
}

#[test]
fn test_transfer_admin_bypasses_transfer_ownership_dual_auth() {
    let env = Env::default();
    let contract_id = env.register_contract(None, RegistryContract);
    let client = RegistryContractClient::new(&env, &contract_id);
    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "transfer_ownership",
            args: (new_admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_transfer_ownership(&new_admin).is_err());

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "transfer_admin",
            args: (new_admin.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.transfer_admin(&new_admin);
    assert_eq!(client.get_admin(), new_admin);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_transfer_admin_by_non_admin_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    client.initialize(&admin);
    env.set_auths(&[]);
    client.transfer_admin(&new_admin);
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_transfer_admin_before_initialize_panics() {
    let (env, client) = setup();
    let new_admin = Address::generate(&env);
    client.transfer_admin(&new_admin);
}

#[test]
fn test_transfer_admin_emits_event() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    client.initialize(&admin);
    client.transfer_admin(&new_admin);
    assert_eq!(
        env.events().all(),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "contract_initialized"), admin.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "admin_transferred"), admin.clone()).into_val(&env),
                new_admin.clone().into_val(&env),
            ),
        ]
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
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_registry_transfer_ownership_requires_both_auths() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    let new_admin = Address::generate(&env);
    client.initialize(&admin);
    env.set_auths(&[]);
    client.transfer_ownership(&new_admin);
}

// ============== PROPERTY-BASED INVARIANT TESTS ==============

fn build_metadata(
    env: &Env,
    entries: &std::vec::Vec<(std::string::String, std::string::String)>,
) -> soroban_sdk::Map<String, String> {
    let mut metadata = map![env];
    for (k, v) in entries {
        metadata.set(
            String::from_str(env, k.as_str()),
            String::from_str(env, v.as_str()),
        );
    }
    metadata
}

fn metadata_entries(
) -> impl Strategy<Value = std::vec::Vec<(std::string::String, std::string::String)>> {
    prop::collection::vec(("[a-zA-Z_][a-zA-Z0-9_]{0,9}", "[a-zA-Z0-9_]{1,20}"), 0..=5)
}

#[test]
fn prop_is_verified_always_consistent_with_get_verification_status_after_register() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(
            &(any::<bool>(), metadata_entries()),
            |(is_buyer, entries)| {
                let (env, client) = setup();
                let admin = Address::generate(&env);
                client.initialize(&admin);
                let address = Address::generate(&env);
                let metadata = build_metadata(&env, &entries);
                if is_buyer {
                    client.register_buyer(&address, &metadata);
                } else {
                    client.register_issuer(&address, &metadata);
                }
                let verified = client.is_verified(&address);
                let status = client.get_verification_status(&address);
                prop_assert!(!verified);
                prop_assert_eq!(status, VerificationStatus::Pending);
                Ok(())
            },
        )
        .unwrap();
}

#[test]
fn prop_revoke_always_sets_is_verified_false_and_status_revoked() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(
            &(any::<bool>(), metadata_entries(), 1usize..=3),
            |(is_buyer, entries, verify_count)| {
                let (env, client) = setup();
                let admin = Address::generate(&env);
                client.initialize(&admin);
                let address = Address::generate(&env);
                let metadata = build_metadata(&env, &entries);
                if is_buyer {
                    client.register_buyer(&address, &metadata);
                } else {
                    client.register_issuer(&address, &metadata);
                }
                for _ in 0..verify_count {
                    client.verify_profile(&address, &true);
                }
                client.revoke(&address);
                prop_assert!(!client.is_verified(&address));
                prop_assert_eq!(
                    client.get_verification_status(&address),
                    VerificationStatus::Revoked
                );
                Ok(())
            },
        )
        .unwrap();
}

#[test]
fn prop_unregistered_address_never_verified() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(
            &prop::collection::vec((any::<bool>(), metadata_entries()), 1..=5),
            |other_profiles| {
                let (env, client) = setup();
                let admin = Address::generate(&env);
                client.initialize(&admin);
                for (is_buyer, entries) in &other_profiles {
                    let addr = Address::generate(&env);
                    let metadata = build_metadata(&env, entries);
                    if *is_buyer {
                        client.register_buyer(&addr, &metadata);
                    } else {
                        client.register_issuer(&addr, &metadata);
                    }
                }
                let unknown = Address::generate(&env);
                prop_assert!(!client.is_verified(&unknown));
                prop_assert_eq!(
                    client.get_verification_status(&unknown),
                    VerificationStatus::Unregistered
                );
                Ok(())
            },
        )
        .unwrap();
}

#[test]
fn prop_re_verify_after_revoke_restores_verified_state() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(
            &(any::<bool>(), metadata_entries(), 1usize..=3),
            |(is_buyer, entries, cycles)| {
                let (env, client) = setup();
                let admin = Address::generate(&env);
                client.initialize(&admin);
                let address = Address::generate(&env);
                let metadata = build_metadata(&env, &entries);
                if is_buyer {
                    client.register_buyer(&address, &metadata);
                } else {
                    client.register_issuer(&address, &metadata);
                }
                client.verify_profile(&address, &true);
                for _ in 0..cycles {
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
                }
                Ok(())
            },
        )
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

#[test]
fn test_profile_packing_correctness() {
    let env = Env::default();
    let _addr = Address::generate(&env);
    let metadata = map![&env];

    // Issuer, verified = true
    let p1 = Profile::new(Role::Issuer, true, 100, metadata.clone());
    assert_eq!(p1.role(), Role::Issuer);
    assert!(p1.verified());

    // Issuer, verified = false
    let p2 = Profile::new(Role::Issuer, false, 100, metadata.clone());
    assert_eq!(p2.role(), Role::Issuer);
    assert!(!p2.verified());

    // Buyer, verified = true
    let p3 = Profile::new(Role::Buyer, true, 100, metadata.clone());
    assert_eq!(p3.role(), Role::Buyer);
    assert!(p3.verified());

    // Buyer, verified = false
    let p4 = Profile::new(Role::Buyer, false, 100, metadata.clone());
    assert_eq!(p4.role(), Role::Buyer);
    assert!(!p4.verified());
}

// ============== METADATA EDGE CASE TESTS (#190) ==============

#[test]
fn test_metadata_empty_map_accepted() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let metadata = map![&env];
    let result = client.register_issuer(&issuer, &metadata);
    assert!(result);
    let profile = client.get_profile(&issuer);
    assert_eq!(profile.metadata.len(), 0);
}

#[test]
fn test_metadata_max_size_map_accepted() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let mut metadata = map![&env];
    for i in 0..20 {
        let key = String::from_str(&env, &std::format!("key_{}", i));
        let value = String::from_str(&env, &std::format!("value_{}", i));
        metadata.set(key, value);
    }
    assert_eq!(metadata.len(), 20);
    let result = client.register_issuer(&issuer, &metadata);
    assert!(result);
    let profile = client.get_profile(&issuer);
    assert_eq!(profile.metadata.len(), 20);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_metadata_oversize_map_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let mut metadata = map![&env];
    for i in 0..21 {
        let key = String::from_str(&env, &std::format!("key_{}", i));
        let value = String::from_str(&env, &std::format!("value_{}", i));
        metadata.set(key, value);
    }
    client.register_issuer(&issuer, &metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_metadata_oversize_map_via_update_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    let mut oversized = map![&env];
    for i in 0..21 {
        let key = String::from_str(&env, &std::format!("key_{}", i));
        let value = String::from_str(&env, &std::format!("value_{}", i));
        oversized.set(key, value);
    }
    client.update_metadata(&issuer, &oversized);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_metadata_empty_key_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let metadata = map![
        &env,
        (String::from_str(&env, ""), String::from_str(&env, "value"),)
    ];
    client.register_issuer(&issuer, &metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_metadata_empty_value_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let metadata = map![
        &env,
        (String::from_str(&env, "key"), String::from_str(&env, ""),)
    ];
    client.register_issuer(&issuer, &metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_metadata_empty_key_via_update_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    let bad_metadata = map![
        &env,
        (String::from_str(&env, ""), String::from_str(&env, "value"),)
    ];
    client.update_metadata(&issuer, &bad_metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_metadata_empty_value_via_update_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    let bad_metadata = map![
        &env,
        (String::from_str(&env, "key"), String::from_str(&env, ""),)
    ];
    client.update_metadata(&issuer, &bad_metadata);
}

#[test]
fn test_metadata_buyer_empty_map_accepted() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let buyer = Address::generate(&env);
    let result = client.register_buyer(&buyer, &map![&env]);
    assert!(result);
    let profile = client.get_profile(&buyer);
    assert_eq!(profile.metadata.len(), 0);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_register_buyer_rejects_oversized_metadata_key() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let buyer = Address::generate(&env);
    let long_key = "k".repeat((crate::MAX_METADATA_KEY_LEN + 1) as usize);
    let metadata = map![
        &env,
        (
            String::from_str(&env, &long_key),
            String::from_str(&env, "value")
        )
    ];
    client.register_buyer(&buyer, &metadata);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_register_issuer_rejects_oversized_metadata_value() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let long_value = "v".repeat((crate::MAX_METADATA_VALUE_LEN + 1) as usize);
    let metadata = map![
        &env,
        (
            String::from_str(&env, "key"),
            String::from_str(&env, &long_value)
        )
    ];
    client.register_issuer(&issuer, &metadata);
}

#[test]
fn test_register_metadata_at_limits_accepted() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let buyer = Address::generate(&env);
    let key_prefix = "k".repeat((crate::MAX_METADATA_KEY_LEN - 2) as usize);
    let value = "v".repeat(crate::MAX_METADATA_VALUE_LEN as usize);
    let mut metadata = map![&env];
    for i in 0..crate::MAX_METADATA_SIZE {
        let key = std::format!("{key_prefix}{i:02}");
        metadata.set(String::from_str(&env, &key), String::from_str(&env, &value));
    }
    assert_eq!(metadata.len(), crate::MAX_METADATA_SIZE);
    assert!(client.register_buyer(&buyer, &metadata));
    let profile = client.get_profile(&buyer);
    assert_eq!(profile.metadata.len(), crate::MAX_METADATA_SIZE);
}

// ============== EVENT-EMISSION TESTS (#188) ==============

#[test]
fn test_register_issuer_emits_event() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);

    client.register_issuer(&issuer, &map![&env]);

    // State after: the issuer is registered but not yet verified.
    assert!(!client.is_verified(&issuer));

    // Registration emits exactly one `issuer_registered` event carrying the
    // issuer address in the topics and an empty data payload.
    assert_eq!(
        env.events().all(),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "contract_initialized"), admin.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "issuer_registered"), issuer.clone()).into_val(&env),
                ().into_val(&env),
            ),
        ]
    );
}

#[test]
fn test_register_buyer_emits_event() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let buyer = Address::generate(&env);

    client.register_buyer(&buyer, &map![&env]);

    // State after: the buyer is registered but not yet verified.
    assert!(!client.is_verified(&buyer));

    // Registration emits exactly one `buyer_registered` event carrying the
    // buyer address in the topics and an empty data payload.
    assert_eq!(
        env.events().all(),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "contract_initialized"), admin.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "buyer_registered"), buyer.clone()).into_val(&env),
                ().into_val(&env),
            ),
        ]
    );
}

#[test]
fn test_revoke_emits_event() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.verify_profile(&issuer, &true);

    client.revoke(&issuer);

    // State after: verification has been revoked.
    assert!(!client.is_verified(&issuer));

    // The full event stream: registration, admin verification, then revoke.
    assert_eq!(
        env.events().all(),
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "contract_initialized"), admin.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "issuer_registered"), issuer.clone()).into_val(&env),
                ().into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "profile_verified"), issuer.clone()).into_val(&env),
                true.into_val(&env),
            ),
            (
                client.address.clone(),
                (Symbol::new(&env, "address_revoked"), issuer.clone()).into_val(&env),
                ().into_val(&env),
            ),
        ]
    );
}

// ============== ISSUE #179: TTL EXTENSION ON READ ==============

#[test]
fn test_get_profile_extends_ttl() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    // New profiles start unverified (#130) — verify so the read below
    // exercises a fully-registered profile.
    client.verify_profile(&issuer, &true);

    let contract_id = client.address.clone();
    let key = DataKey::Profile(issuer.clone());

    // Record the initial remaining TTL, then advance the ledger so the
    // remaining TTL drops below the write-path threshold.
    let ttl_before_drain: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    // Advance to leave ~50 ledgers remaining.
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + ttl_before_drain - 50);

    let ttl_before_read: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    assert!(
        ttl_before_read < TTL_THRESHOLD,
        "TTL should be below threshold before read, got {ttl_before_read}"
    );

    // Read the profile — this should extend the entry's TTL.
    let profile = client.get_profile(&issuer);
    assert!(profile.verified());

    let ttl_after_read: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));

    assert!(
        ttl_after_read > ttl_before_read,
        "get_profile should extend TTL: before={ttl_before_read}, after={ttl_after_read}"
    );
    assert!(
        ttl_after_read >= 1_999_000,
        "TTL should be extended close to EXTEND_TO (2_000_000), got {ttl_after_read}"
    );
}

#[test]
fn test_is_verified_extends_ttl() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    // New profiles start unverified (#130) — verify so is_verified is true.
    client.verify_profile(&issuer, &true);

    let contract_id = client.address.clone();
    let key = DataKey::Profile(issuer.clone());

    // Drain TTL below the threshold.
    let ttl_before_drain: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + ttl_before_drain - 50);

    let ttl_before_read: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    assert!(
        ttl_before_read < TTL_THRESHOLD,
        "TTL should be below threshold before read, got {ttl_before_read}"
    );

    // Call is_verified — this should extend the entry's TTL.
    assert!(client.is_verified(&issuer));

    let ttl_after_read: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));

    assert!(
        ttl_after_read > ttl_before_read,
        "is_verified should extend TTL: before={ttl_before_read}, after={ttl_after_read}"
    );
    assert!(
        ttl_after_read >= 1_999_000,
        "TTL should be extended close to EXTEND_TO (2_000_000), got {ttl_after_read}"
    );
}

#[test]
fn test_is_verified_does_not_extend_ttl_for_unknown() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    // Calling is_verified on an unknown address must return false and must
    // not panic (no TTL extension attempted for a non-existent entry).
    let unknown = Address::generate(&env);
    assert!(!client.is_verified(&unknown));

    // Also verify the same function still works for a registered issuer.
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    // New profiles start unverified (#130) — verify before asserting.
    client.verify_profile(&issuer, &true);
    assert!(client.is_verified(&issuer));
}

#[test]
fn test_get_admin_extends_instance_ttl() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let contract_id = client.address.clone();

    // Drain instance TTL below the threshold.
    let ttl_before_drain: u32 =
        env.as_contract(&contract_id, || env.storage().instance().get_ttl());
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + ttl_before_drain - 50);

    let ttl_before_read: u32 = env.as_contract(&contract_id, || env.storage().instance().get_ttl());
    assert!(
        ttl_before_read < TTL_THRESHOLD,
        "Instance TTL should be below threshold before read, got {ttl_before_read}"
    );

    // Read the admin — this should extend the instance TTL.
    assert_eq!(client.get_admin(), admin);

    let ttl_after_read: u32 = env.as_contract(&contract_id, || env.storage().instance().get_ttl());

    assert!(
        ttl_after_read > ttl_before_read,
        "get_admin should extend instance TTL: before={ttl_before_read}, after={ttl_after_read}"
    );
    assert!(
        ttl_after_read >= 1_999_000,
        "Instance TTL should be extended close to EXTEND_TO (2_000_000), got {ttl_after_read}"
    );
}

// ============== ISSUE #447: update_metadata extends instance TTL ==============

#[test]
fn test_update_metadata_extends_instance_ttl() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(
        &issuer,
        &map![
            &env,
            (
                String::from_str(&env, "name"),
                String::from_str(&env, "Acme Corp")
            )
        ],
    );

    let contract_id = client.address.clone();

    // Drain instance TTL below the threshold (100).
    let ttl_before_drain: u32 =
        env.as_contract(&contract_id, || env.storage().instance().get_ttl());
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + ttl_before_drain - 50);

    let ttl_before_update: u32 =
        env.as_contract(&contract_id, || env.storage().instance().get_ttl());
    assert!(
        ttl_before_update < TTL_THRESHOLD,
        "Instance TTL should be below threshold before update, got {ttl_before_update}"
    );

    // Update metadata — this should extend the instance TTL.
    let updated_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Acme LLC")
        )
    ];
    let result = client.update_metadata(&issuer, &updated_metadata);
    assert!(result);

    let ttl_after_update: u32 =
        env.as_contract(&contract_id, || env.storage().instance().get_ttl());

    assert!(
        ttl_after_update > ttl_before_update,
        "update_metadata should extend instance TTL: before={ttl_before_update}, after={ttl_after_update}"
    );
    assert!(
        ttl_after_update >= 1_999_000,
        "Instance TTL should be extended close to EXTEND_TO (2_000_000), got {ttl_after_update}"
    );
}

// ============== PROPERTY-BASED TESTS: METADATA BOUNDARY (Issue #449) ==============

/// Strategy that generates metadata entries with valid key/value patterns.
fn valid_metadata_entries(
) -> impl Strategy<Value = std::vec::Vec<(std::string::String, std::string::String)>> {
    prop::collection::vec(("[a-zA-Z_][a-zA-Z0-9_]{0,9}", "[a-zA-Z0-9_]{1,20}"), 0..=5)
}

/// Strategy that generates metadata entries that may contain empty keys or
/// values (approximately 30% chance per entry).
fn metadata_entries_with_empty_field(
) -> impl Strategy<Value = std::vec::Vec<(std::string::String, std::string::String)>> {
    prop::collection::vec(
        (
            prop_oneof![
                Just(std::string::String::new()),
                "[a-zA-Z][a-zA-Z0-9_]{0,9}"
            ],
            prop_oneof![Just(std::string::String::new()), "[a-zA-Z0-9_]{1,20}"],
        ),
        1..=5,
    )
}

/// Proptest: metadata maps with sizes in 0..=20 are accepted, sizes in
/// 21..=25 are rejected with `InvalidMetadata`.
#[test]
fn prop_metadata_size_boundary_accepted_and_rejected() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(
            &(0u32..=25, valid_metadata_entries()),
            |(target_size, entries)| {
                let (env, client) = setup();
                let admin = Address::generate(&env);
                client.initialize(&admin);
                let address = Address::generate(&env);

                // Build a metadata map with exactly `target_size` entries.
                let mut metadata = map![&env];
                for i in 0..target_size {
                    if let Some((k, v)) = entries.get(i as usize) {
                        metadata.set(
                            String::from_str(&env, k.as_str()),
                            String::from_str(&env, v.as_str()),
                        );
                    } else {
                        // Generate a fallback key/value pair when the
                        // strategy provided fewer entries than target_size.
                        metadata.set(
                            String::from_str(&env, &std::format!("key_{i}")),
                            String::from_str(&env, &std::format!("value_{i}")),
                        );
                    }
                }

                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    client.register_issuer(&address, &metadata);
                }));

                if target_size <= 20 {
                    prop_assert!(
                        result.is_ok(),
                        "metadata with {} entries should be accepted, but panicked",
                        target_size
                    );
                    // Verify the profile was registered successfully.
                    prop_assert_eq!(client.get_profile(&address).role(), Role::Issuer);
                } else {
                    prop_assert!(
                        result.is_err(),
                        "metadata with {} entries should be rejected, but succeeded",
                        target_size
                    );
                }
                Ok(())
            },
        )
        .unwrap();
}

/// Proptest: metadata containing at least one empty key or empty value is
/// always rejected with `InvalidMetadata`, regardless of total size.
#[test]
fn prop_metadata_empty_key_or_value_always_rejected() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&metadata_entries_with_empty_field(), |entries| {
            let (env, client) = setup();
            let admin = Address::generate(&env);
            client.initialize(&admin);
            let address = Address::generate(&env);

            let metadata = build_metadata(&env, &entries);

            let has_empty = entries.iter().any(|(k, v)| k.is_empty() || v.is_empty());

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                client.register_issuer(&address, &metadata);
            }));

            if has_empty {
                prop_assert!(
                    result.is_err(),
                    "metadata with empty key/value should be rejected, but succeeded"
                );
            }
            // When all keys/values are non-empty the registration must
            // succeed (the strategy can generate up to 5 entries, well
            // below MAX_METADATA_SIZE).
            if !has_empty {
                prop_assert!(
                    result.is_ok(),
                    "metadata with all non-empty fields should be accepted, but panicked"
                );
            }
            Ok(())
        })
        .unwrap();
}

// ============== ISSUE #851: BATCH REGISTRATION CONSERVATION PROPTESTS ==============

type BatchEntryPlan = (
    usize,
    std::vec::Vec<usize>,
    std::collections::BTreeSet<usize>,
    std::vec::Vec<std::vec::Vec<(std::string::String, std::string::String)>>,
);

/// Plan for a batch registration proptest: a pool of `unique_count` addresses,
/// a vector of indices into that pool forming the batch (with possible
/// duplicates), a subset of pool indices to pre-register, and a valid
/// metadata map for every batch entry.
fn batch_entry_plan() -> impl Strategy<Value = BatchEntryPlan> {
    (1usize..=50).prop_flat_map(|unique_count| {
        prop::collection::vec(0..unique_count, 1..=50).prop_flat_map(move |entry_indices| {
            let entry_count = entry_indices.len();
            (
                Just(unique_count),
                Just(entry_indices),
                prop::collection::btree_set(0..unique_count, 0..=unique_count),
                prop::collection::vec(valid_metadata_entries(), entry_count..=entry_count),
            )
        })
    })
}

/// Asserts that the most recent `batch_registered` event carries exactly
/// `expected_registered` and `expected_skipped` in its data payload.
fn assert_batch_event_counts(env: &Env, expected_registered: u32, expected_skipped: u32) {
    let all_events = env.events().all();
    let mut found = false;
    for i in (0..all_events.len()).rev() {
        let event = all_events.get(i).expect("event index in range");
        let topics: Vec<Val> = match event.1.clone().try_into_val(env) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if topics.len() != 1 {
            continue;
        }
        let topic_symbol: Symbol = match topics.get(0).unwrap().try_into_val(env) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if topic_symbol == Symbol::new(env, "batch_registered") {
            let (registered, skipped): (u32, u32) = event
                .2
                .clone()
                .try_into_val(env)
                .expect("batch event data should be (u32, u32)");
            assert_eq!(registered, expected_registered);
            assert_eq!(skipped, expected_skipped);
            found = true;
            break;
        }
    }
    assert!(found, "batch_registered event not found");
}

/// Runs the batch-registration conservation proptest for either issuers
/// (`is_buyer = false`) or buyers (`is_buyer = true`).
fn run_batch_register_prop_test(is_buyer: bool) {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(20));
    runner
        .run(
            &batch_entry_plan(),
            |(unique_count, entry_indices, preregistered, metadata_per_entry)| {
                let (env, client) = setup();
                let admin = Address::generate(&env);
                client.initialize(&admin);

                // Build the deterministic address pool.
                let addresses: std::vec::Vec<Address> =
                    (0..unique_count).map(|_| Address::generate(&env)).collect();

                // Pre-register the chosen addresses and snapshot their profiles
                // so we can prove skipped entries are unchanged.
                let mut preregistered_profiles = std::vec::Vec::new();
                let preregister_metadata = build_metadata(&env, &metadata_per_entry[0]);
                for idx in &preregistered {
                    let addr = addresses[*idx].clone();
                    if is_buyer {
                        client.register_buyer(&addr, &preregister_metadata);
                    } else {
                        client.register_issuer(&addr, &preregister_metadata);
                    }
                    preregistered_profiles.push((addr, client.get_profile(&addresses[*idx])));
                }

                // Build the batch entries from the generated plan.
                let mut entries = Vec::new(&env);
                for (i, idx) in entry_indices.iter().enumerate() {
                    let addr = addresses[*idx].clone();
                    let metadata = build_metadata(&env, &metadata_per_entry[i]);
                    entries.push_back((addr, metadata));
                }

                // Invoke the batch function under test.
                let skipped = if is_buyer {
                    client.batch_register_buyers(&entries)
                } else {
                    client.batch_register_issuers(&entries)
                };

                // Recompute the expected outcome from the plan.
                let mut seen = std::collections::HashSet::new();
                let mut expected_registered: u32 = 0;
                let mut expected_skipped = Vec::new(&env);
                for idx in &entry_indices {
                    if preregistered.contains(idx) || seen.contains(idx) {
                        expected_skipped.push_back(addresses[*idx].clone());
                    } else {
                        seen.insert(*idx);
                        expected_registered += 1;
                    }
                }

                // Conservation: every entry is either registered or skipped.
                prop_assert_eq!(
                    expected_registered + expected_skipped.len(),
                    entry_indices.len() as u32
                );
                prop_assert_eq!(skipped, expected_skipped.clone());

                // Newly registered addresses are Pending with the right role;
                // pre-registered skipped addresses are untouched.
                for (i, idx) in entry_indices.iter().enumerate() {
                    let addr = addresses[*idx].clone();
                    let already_registered =
                        preregistered.contains(idx) || entry_indices[..i].contains(idx);
                    if already_registered {
                        if let Some((_, before)) =
                            preregistered_profiles.iter().find(|(a, _)| a == &addr)
                        {
                            let after = client.get_profile(&addr);
                            prop_assert_eq!(before.packed_flags, after.packed_flags);
                            prop_assert_eq!(before.registered_at, after.registered_at);
                            prop_assert_eq!(before.metadata.clone(), after.metadata.clone());
                        }
                    } else {
                        let profile = client.get_profile(&addr);
                        prop_assert_eq!(
                            profile.role(),
                            if is_buyer { Role::Buyer } else { Role::Issuer }
                        );
                        prop_assert_eq!(
                            client.get_verification_status(&addr),
                            VerificationStatus::Pending
                        );
                        if is_buyer {
                            let expected_metadata = build_metadata(&env, &metadata_per_entry[i]);
                            prop_assert_eq!(profile.metadata, expected_metadata);
                        } else {
                            prop_assert_eq!(profile.metadata.len(), 0);
                        }
                    }
                }

                // The emitted batch event carries the same counts.
                assert_batch_event_counts(&env, expected_registered, expected_skipped.len());

                Ok(())
            },
        )
        .unwrap();
}

#[test]
fn prop_batch_register_issuers_conserves_entries() {
    run_batch_register_prop_test(false);
}

#[test]
fn prop_batch_register_buyers_conserves_entries() {
    run_batch_register_prop_test(true);
}

#[test]
fn test_batch_register_issuers_exceeds_limit_leaves_state_clean() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let mut entries = Vec::new(&env);
    for _ in 0..51 {
        let address = Address::generate(&env);
        entries.push_back((address, map![&env]));
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_register_issuers(&entries);
    }));
    assert!(result.is_err(), "batch over 50 entries should panic");

    for (address, _) in entries.iter() {
        assert_eq!(
            client.get_verification_status(&address),
            VerificationStatus::Unregistered
        );
    }
}

#[test]
fn test_batch_register_buyers_exceeds_limit_leaves_state_clean() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let mut entries = Vec::new(&env);
    for _ in 0..51 {
        let address = Address::generate(&env);
        entries.push_back((address, map![&env]));
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_register_buyers(&entries);
    }));
    assert!(result.is_err(), "batch over 50 entries should panic");

    for (address, _) in entries.iter() {
        assert_eq!(
            client.get_verification_status(&address),
            VerificationStatus::Unregistered
        );
    }
}

// ============== ISSUE #841: PER-ROLE COUNTERS + PAGINATED ENUMERATION ==============

/// Collects every address indexed for `role` by walking `list_profiles` in
/// pages of `MAX_LIST_LIMIT`, exactly as an indexer or dashboard would.
fn collect_all_profiles(client: &RegistryContractClient, role: &Role) -> std::vec::Vec<Address> {
    let page_size = crate::MAX_LIST_LIMIT;
    let mut all = std::vec::Vec::new();
    let mut start = 0u32;
    loop {
        let page = client.list_profiles(role, &start, &page_size);
        all.extend(page.iter());
        if page.len() < page_size {
            break;
        }
        start += page_size;
    }
    all
}

/// Registers `count` issuers through a single admin batch and returns the
/// addresses in registration order.
fn batch_register_issuers_n(
    env: &Env,
    client: &RegistryContractClient,
    count: u32,
) -> std::vec::Vec<Address> {
    let mut entries = Vec::new(env);
    let mut addresses = std::vec::Vec::new();
    for _ in 0..count {
        let address = Address::generate(env);
        entries.push_back((address.clone(), map![env]));
        addresses.push(address);
    }
    client.batch_register_issuers(&entries);
    addresses
}

/// Registers `count` buyers through a single admin batch and returns the
/// addresses in registration order.
fn batch_register_buyers_n(
    env: &Env,
    client: &RegistryContractClient,
    count: u32,
) -> std::vec::Vec<Address> {
    let mut entries = Vec::new(env);
    let mut addresses = std::vec::Vec::new();
    for _ in 0..count {
        let address = Address::generate(env);
        entries.push_back((address.clone(), map![env]));
        addresses.push(address);
    }
    client.batch_register_buyers(&entries);
    addresses
}

#[test]
fn test_profile_count_is_zero_for_fresh_registry() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    assert_eq!(client.get_profile_count(&Role::Issuer), 0);
    assert_eq!(client.get_profile_count(&Role::Buyer), 0);
    assert_eq!(client.list_profiles(&Role::Issuer, &0, &10).len(), 0);
    assert_eq!(client.list_profiles(&Role::Buyer, &0, &10).len(), 0);
}

#[test]
fn test_profile_count_and_list_track_single_registrations() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);
    let issuer3 = Address::generate(&env);
    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);

    client.register_issuer(&issuer1, &map![&env]);
    client.register_buyer(&buyer1, &map![&env]);
    client.register_issuer(&issuer2, &map![&env]);
    client.register_buyer(&buyer2, &map![&env]);
    client.register_issuer(&issuer3, &map![&env]);

    assert_eq!(client.get_profile_count(&Role::Issuer), 3);
    assert_eq!(client.get_profile_count(&Role::Buyer), 2);

    // The index preserves registration order per role, and the two roles are
    // enumerated independently.
    assert_eq!(
        client.list_profiles(&Role::Issuer, &0, &10),
        vec![&env, issuer1.clone(), issuer2.clone(), issuer3.clone()]
    );
    assert_eq!(
        client.list_profiles(&Role::Buyer, &0, &10),
        vec![&env, buyer1.clone(), buyer2.clone()]
    );
    assert_eq!(collect_all_profiles(&client, &Role::Issuer).len(), 3);
    assert_eq!(collect_all_profiles(&client, &Role::Buyer).len(), 2);
}

#[test]
fn test_list_profiles_only_returns_profiles_with_the_matching_role() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);
    let buyer1 = Address::generate(&env);

    client.register_issuer(&issuer1, &map![&env]);
    client.register_buyer(&buyer1, &map![&env]);
    client.register_issuer(&issuer2, &map![&env]);

    for address in collect_all_profiles(&client, &Role::Issuer) {
        assert_eq!(client.get_profile(&address).role(), Role::Issuer);
    }
    for address in collect_all_profiles(&client, &Role::Buyer) {
        assert_eq!(client.get_profile(&address).role(), Role::Buyer);
    }
}

#[test]
fn test_list_profiles_pages_are_contiguous_and_non_overlapping() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let expected = batch_register_issuers_n(&env, &client, 5);
    assert_eq!(client.get_profile_count(&Role::Issuer), 5);

    let page0 = client.list_profiles(&Role::Issuer, &0, &2);
    let page1 = client.list_profiles(&Role::Issuer, &2, &2);
    let page2 = client.list_profiles(&Role::Issuer, &4, &2);
    assert_eq!(page0, vec![&env, expected[0].clone(), expected[1].clone()]);
    assert_eq!(page1, vec![&env, expected[2].clone(), expected[3].clone()]);
    assert_eq!(page2, vec![&env, expected[4].clone()]);

    let mut walked = std::vec::Vec::new();
    walked.extend(page0.iter());
    walked.extend(page1.iter());
    walked.extend(page2.iter());
    assert_eq!(walked, expected);

    // Paging with a different page size yields the same sequence.
    let mut single_step_pages = std::vec::Vec::new();
    for start in 0..5u32 {
        single_step_pages.extend(client.list_profiles(&Role::Issuer, &start, &1).iter());
    }
    assert_eq!(single_step_pages, expected);
}

#[test]
fn test_list_profiles_clamps_start_and_zero_limit() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let expected = batch_register_issuers_n(&env, &client, 3);
    assert_eq!(client.get_profile_count(&Role::Issuer), 3);

    // A page that starts inside the index but runs past the end is short, not
    // an error.
    assert_eq!(
        client.list_profiles(&Role::Issuer, &1, &50),
        vec![&env, expected[1].clone(), expected[2].clone()]
    );
    // Starting exactly at, or beyond, the count yields an empty page.
    assert_eq!(client.list_profiles(&Role::Issuer, &3, &10).len(), 0);
    assert_eq!(client.list_profiles(&Role::Issuer, &9, &10).len(), 0);
    // u32::MAX saturates instead of overflowing.
    assert_eq!(client.list_profiles(&Role::Issuer, &u32::MAX, &50).len(), 0);
    // A zero-sized page returns nothing without touching the index.
    assert_eq!(client.list_profiles(&Role::Issuer, &0, &0).len(), 0);
    assert_eq!(client.get_profile_count(&Role::Issuer), 3);
}

#[test]
fn test_list_profiles_limit_at_cap_is_accepted() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    // 50 issuers in one batch (the batch cap) plus 5 more in a second batch.
    let mut expected = batch_register_issuers_n(&env, &client, 50);
    expected.extend(batch_register_issuers_n(&env, &client, 5));
    assert_eq!(client.get_profile_count(&Role::Issuer), 55);

    let first_page = client.list_profiles(&Role::Issuer, &0, &crate::MAX_LIST_LIMIT);
    assert_eq!(first_page.len(), 50);
    for address in first_page.iter() {
        assert_eq!(client.get_profile(&address).role(), Role::Issuer);
    }

    let second_page = client.list_profiles(
        &Role::Issuer,
        &crate::MAX_LIST_LIMIT,
        &crate::MAX_LIST_LIMIT,
    );
    assert_eq!(second_page.len(), 5);
    assert_eq!(collect_all_profiles(&client, &Role::Issuer), expected);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_profiles_limit_above_cap_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    batch_register_issuers_n(&env, &client, 1);
    client.list_profiles(&Role::Issuer, &0, &(crate::MAX_LIST_LIMIT + 1));
}

#[test]
fn test_list_profiles_limit_above_cap_panics_for_every_page_size() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    batch_register_issuers_n(&env, &client, 1);

    for limit in [crate::MAX_LIST_LIMIT + 1, 100u32, 1_000, u32::MAX] {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            client.list_profiles(&Role::Issuer, &0, &limit)
        }));
        assert!(
            result.is_err(),
            "limit {limit} is above the cap and must panic with PageSizeExceeded"
        );
    }
    // The rejected reads left the index untouched.
    assert_eq!(client.get_profile_count(&Role::Issuer), 1);
    assert_eq!(client.list_profiles(&Role::Issuer, &0, &1).len(), 1);
}

#[test]
fn test_batch_register_issuers_indexes_only_new_entries() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    // Registered up-front through the single-address path.
    let issuer1 = Address::generate(&env);
    client.register_issuer(&issuer1, &map![&env]);
    assert_eq!(client.get_profile_count(&Role::Issuer), 1);

    let issuer2 = Address::generate(&env);
    let issuer3 = Address::generate(&env);
    // issuer1 is a duplicate, issuer3 appears twice: one registration plus one
    // skip, and neither may be indexed twice.
    let entries = vec![
        &env,
        (issuer1.clone(), map![&env]),
        (issuer2.clone(), map![&env]),
        (issuer3.clone(), map![&env]),
        (issuer3.clone(), map![&env]),
    ];

    let skipped = client.batch_register_issuers(&entries);
    assert_eq!(skipped.len(), 2);
    assert!(skipped.contains(&issuer1));
    assert!(skipped.contains(&issuer3));

    // One pre-existing + two new (issuer3 counted once) => 3 indexed.
    assert_eq!(client.get_profile_count(&Role::Issuer), 3);
    assert_eq!(
        client.list_profiles(&Role::Issuer, &0, &10),
        vec![&env, issuer1, issuer2, issuer3]
    );
}

#[test]
fn test_batch_register_buyers_indexes_only_new_entries() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let buyer1 = Address::generate(&env);
    client.register_buyer(&buyer1, &map![&env]);
    assert_eq!(client.get_profile_count(&Role::Buyer), 1);

    let buyer2 = Address::generate(&env);
    let buyer3 = Address::generate(&env);
    let entries = vec![
        &env,
        (buyer1.clone(), map![&env]),
        (buyer2.clone(), map![&env]),
        (buyer3.clone(), map![&env]),
        (buyer3.clone(), map![&env]),
    ];

    let skipped = client.batch_register_buyers(&entries);
    assert_eq!(skipped.len(), 2);
    assert!(skipped.contains(&buyer1));
    assert!(skipped.contains(&buyer3));

    assert_eq!(client.get_profile_count(&Role::Buyer), 3);
    assert_eq!(
        client.list_profiles(&Role::Buyer, &0, &10),
        vec![&env, buyer1, buyer2, buyer3]
    );
}

#[test]
fn test_batch_register_leaves_the_other_role_index_untouched() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);
    let buyer1 = Address::generate(&env);

    client.batch_register_issuers(&vec![
        &env,
        (issuer1.clone(), map![&env]),
        (issuer2.clone(), map![&env]),
    ]);
    client.batch_register_buyers(&vec![&env, (buyer1.clone(), map![&env])]);

    assert_eq!(client.get_profile_count(&Role::Issuer), 2);
    assert_eq!(client.get_profile_count(&Role::Buyer), 1);
    assert_eq!(
        client.list_profiles(&Role::Issuer, &0, &10),
        vec![&env, issuer1, issuer2]
    );
    assert_eq!(
        client.list_profiles(&Role::Buyer, &0, &10),
        vec![&env, buyer1]
    );
}

#[test]
fn test_repeated_single_registration_does_not_index_twice() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    assert_eq!(client.get_profile_count(&Role::Issuer), 1);

    // A duplicate single registration panics, so the index must be unchanged.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.register_issuer(&issuer, &map![&env]);
    }));
    assert!(result.is_err(), "duplicate registration should panic");
    assert_eq!(client.get_profile_count(&Role::Issuer), 1);
    assert_eq!(
        client.list_profiles(&Role::Issuer, &0, &10),
        vec![&env, issuer.clone()]
    );

    // The cross-role attempt panics too, and must not touch either index.
    let cross = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.register_buyer(&issuer, &map![&env]);
    }));
    assert!(cross.is_err(), "cross-role registration should panic");
    assert_eq!(client.get_profile_count(&Role::Issuer), 1);
    assert_eq!(client.get_profile_count(&Role::Buyer), 0);
    assert_eq!(client.list_profiles(&Role::Buyer, &0, &10).len(), 0);
    assert_eq!(
        client.list_profiles(&Role::Issuer, &0, &10),
        vec![&env, issuer]
    );
}

#[test]
fn test_revoked_profiles_remain_enumerated() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);
    client.register_issuer(&issuer1, &map![&env]);
    client.register_issuer(&issuer2, &map![&env]);
    client.verify_profile(&issuer1, &true);
    client.revoke(&issuer1);

    // Revocation only flips verification — the profile is not deregistered, so
    // the enumeration still reports it and the count is unchanged.
    assert_eq!(client.get_profile_count(&Role::Issuer), 2);
    assert_eq!(
        client.list_profiles(&Role::Issuer, &0, &10),
        vec![&env, issuer1, issuer2]
    );
}

#[test]
fn test_profile_index_and_count_keys_extend_ttl_on_write() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    let contract_id = client.address.clone();
    let index_key = DataKey::ProfileIndex(Role::Issuer, 0);
    let count_key = DataKey::ProfileCount(Role::Issuer);
    let buyer_count_key = DataKey::ProfileCount(Role::Buyer);

    let index_ttl: u32 = env.as_contract(&contract_id, || {
        env.storage().persistent().get_ttl(&index_key)
    });
    let count_ttl: u32 = env.as_contract(&contract_id, || {
        env.storage().persistent().get_ttl(&count_key)
    });
    assert!(
        index_ttl >= 1_999_000,
        "index entry TTL should be extended close to EXTEND_TO, got {index_ttl}"
    );
    assert!(
        count_ttl >= 1_999_000,
        "count TTL should be extended close to EXTEND_TO, got {count_ttl}"
    );

    // The counter key for a role with no registrations is never written.
    let unwritten: bool = env.as_contract(&contract_id, || {
        env.storage().persistent().has(&buyer_count_key)
    });
    assert!(!unwritten);
}

#[test]
fn test_list_profiles_and_count_extend_ttl_on_read() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    let contract_id = client.address.clone();
    let index_key = DataKey::ProfileIndex(Role::Issuer, 0);
    let count_key = DataKey::ProfileCount(Role::Issuer);

    // Drain both entries below the threshold.
    let ttl_before_drain: u32 = env.as_contract(&contract_id, || {
        env.storage().persistent().get_ttl(&index_key)
    });
    assert!(ttl_before_drain > 50);
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + ttl_before_drain - 50);

    let index_before: u32 = env.as_contract(&contract_id, || {
        env.storage().persistent().get_ttl(&index_key)
    });
    let count_before: u32 = env.as_contract(&contract_id, || {
        env.storage().persistent().get_ttl(&count_key)
    });
    assert!(index_before < TTL_THRESHOLD);
    assert!(count_before < TTL_THRESHOLD);

    assert_eq!(client.get_profile_count(&Role::Issuer), 1);
    assert_eq!(
        client.list_profiles(&Role::Issuer, &0, &10),
        vec![&env, issuer]
    );

    let index_after: u32 = env.as_contract(&contract_id, || {
        env.storage().persistent().get_ttl(&index_key)
    });
    let count_after: u32 = env.as_contract(&contract_id, || {
        env.storage().persistent().get_ttl(&count_key)
    });
    assert!(
        index_after > index_before && index_after >= 1_999_000,
        "list_profiles should extend the index TTL: before={index_before}, after={index_after}"
    );
    assert!(
        count_after > count_before && count_after >= 1_999_000,
        "get_profile_count should extend the count TTL: before={count_before}, after={count_after}"
    );
}

#[test]
fn test_list_profiles_skips_index_slots_that_expired() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);
    let issuer3 = Address::generate(&env);
    client.batch_register_issuers(&vec![
        &env,
        (issuer1.clone(), map![&env]),
        (issuer2.clone(), map![&env]),
        (issuer3.clone(), map![&env]),
    ]);

    // Simulate an index entry that aged out while the profile survived: the
    // page must return the surviving addresses rather than fail.
    let contract_id = client.address.clone();
    env.as_contract(&contract_id, || {
        env.storage()
            .persistent()
            .remove(&DataKey::ProfileIndex(Role::Issuer, 1));
    });

    assert_eq!(
        client.list_profiles(&Role::Issuer, &0, &10),
        vec![&env, issuer1, issuer3]
    );
}

/// A registration step: which pool address to register, in which role, and
/// through which entry point.
type IndexStep = (usize, bool, bool);

fn index_step_plan() -> impl Strategy<Value = std::vec::Vec<IndexStep>> {
    prop::collection::vec((0usize..6, any::<bool>(), any::<bool>()), 1..=14)
}

/// Applies `steps` and asserts that, for both roles, the counter and the
/// concatenated `list_profiles` pages describe exactly the addresses that were
/// registered for that role, in registration order and without duplicates.
fn run_profile_index_prop_test(
    steps: std::vec::Vec<IndexStep>,
) -> Result<(), proptest::test_runner::TestCaseError> {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let pool: std::vec::Vec<Address> = (0..6).map(|_| Address::generate(&env)).collect();

    // First-registration order per role, and the role each address ended up
    // with (an address can only ever hold one profile).
    let mut expected: std::collections::HashMap<bool, std::vec::Vec<Address>> =
        std::collections::HashMap::new();
    let mut assigned: std::collections::HashMap<usize, bool> = std::collections::HashMap::new();

    for (idx, is_buyer, via_batch) in steps {
        let address = pool[idx].clone();
        // `true` once this pool address holds a profile, whatever the role.
        let already_registered = assigned.contains_key(&idx);
        if via_batch {
            let entries = vec![&env, (address.clone(), map![&env])];
            let skipped = if is_buyer {
                client.batch_register_buyers(&entries)
            } else {
                client.batch_register_issuers(&entries)
            };
            if already_registered {
                // Already registered under some role: the batch must skip it.
                prop_assert_eq!(skipped.len(), 1);
                prop_assert!(skipped.contains(&address));
            } else {
                prop_assert_eq!(skipped.len(), 0);
                assigned.insert(idx, is_buyer);
                expected.entry(is_buyer).or_default().push(address);
            }
        } else if already_registered {
            // Already registered: the single-address path must panic rather
            // than index the address a second time.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if is_buyer {
                    client.register_buyer(&address, &map![&env]);
                } else {
                    client.register_issuer(&address, &map![&env]);
                }
            }));
            prop_assert!(result.is_err(), "duplicate registration should panic");
        } else {
            if is_buyer {
                client.register_buyer(&address, &map![&env]);
            } else {
                client.register_issuer(&address, &map![&env]);
            }
            assigned.insert(idx, is_buyer);
            expected.entry(is_buyer).or_default().push(address);
        }
    }

    for (is_buyer, role) in [(true, Role::Buyer), (false, Role::Issuer)] {
        let want = expected.get(&is_buyer).cloned().unwrap_or_default();

        // The counter matches the number of registered profiles for the role.
        prop_assert_eq!(client.get_profile_count(&role), want.len() as u32);

        // Walking the index in pages reconstructs the registration order
        // exactly, with no duplicates and no cross-role leakage.
        let walked = collect_all_profiles(&client, &role);
        prop_assert_eq!(walked.clone(), want.clone());
        let mut unique = walked.clone();
        unique.sort();
        unique.dedup();
        prop_assert_eq!(unique.len(), walked.len(), "index contains duplicates");

        // Any page size the caller picks yields the same sequence.
        for page_size in [1u32, 2, 3, 7] {
            let mut paged = std::vec::Vec::new();
            let mut start = 0u32;
            while start < want.len() as u32 {
                paged.extend(client.list_profiles(&role, &start, &page_size).iter());
                start += page_size;
            }
            prop_assert_eq!(paged, want.clone());
        }

        // Every enumerated address really holds a profile with this role.
        for address in walked {
            prop_assert_eq!(client.get_profile(&address).role(), role.clone());
        }
    }
    Ok(())
}

#[test]
fn prop_profile_count_and_index_match_registrations() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(20));
    runner
        .run(&index_step_plan(), |steps| {
            run_profile_index_prop_test(steps)
        })
        .unwrap();
}

#[test]
fn test_profile_index_survives_pagination_across_a_mixed_registry() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    // 30 issuers over three batches, interleaved with 20 buyers: 10 through a
    // batch and 10 registered singly.
    let issuers = batch_register_issuers_n(&env, &client, 20);
    let batch_buyers = batch_register_buyers_n(&env, &client, 10);
    // Re-send two of the issuers: both must be skipped, neither re-indexed.
    let repeat = vec![
        &env,
        (issuers[0].clone(), map![&env]),
        (issuers[1].clone(), map![&env]),
    ];
    let skipped = client.batch_register_issuers(&repeat);
    assert_eq!(skipped.len(), 2);

    let more_issuers = batch_register_issuers_n(&env, &client, 10);

    let mut single_buyers = std::vec::Vec::new();
    for _ in 0..10 {
        let buyer = Address::generate(&env);
        client.register_buyer(&buyer, &map![&env]);
        single_buyers.push(buyer);
    }

    assert_eq!(client.get_profile_count(&Role::Issuer), 30);
    assert_eq!(client.get_profile_count(&Role::Buyer), 20);

    // Issuer order: first batch, then the second (the repeat batch was all
    // skips, so it appended nothing).
    let mut expected_issuers = issuers;
    expected_issuers.extend(more_issuers);
    assert_eq!(
        collect_all_profiles(&client, &Role::Issuer),
        expected_issuers
    );

    // Buyer order: the batch ran first, then the single registrations.
    let mut expected_buyers = batch_buyers;
    expected_buyers.extend(single_buyers);
    assert_eq!(collect_all_profiles(&client, &Role::Buyer), expected_buyers);
}
