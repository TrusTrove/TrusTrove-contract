#![cfg(test)]

use crate::{RegistryContract, RegistryContractClient};
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
    let count = client.batch_register_issuers(&entries);
    assert_eq!(count, 0);
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

    let count = client.batch_register_issuers(&entries);
    assert_eq!(count, 3);

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

    let count = client.batch_register_issuers(&entries);
    assert_eq!(count, 0);
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

    let count = client.batch_register_issuers(&entries);
    assert_eq!(count, 2);

    assert!(client.is_verified(&issuer1));
    assert!(client.is_verified(&issuer2));
    assert!(client.is_verified(&issuer3));
}
