#![cfg(test)]

extern crate std;

use crate::{
    DataKey, Profile, RegistryContract, RegistryContractClient, Role, VerificationStatus,
    MAX_METADATA_KEY_LEN, MAX_METADATA_SIZE, MAX_METADATA_VALUE_LEN, TTL_EXTEND_TO, TTL_THRESHOLD,
};
use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use soroban_sdk::{
    map,
    testutils::{
        storage::{Instance as _, Persistent as _},
        Address as _, Events as _, Ledger,
    },
    vec, Address, BytesN, Env, IntoVal, String, Symbol, TryFromVal, TryIntoVal, Val, Vec,
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
    client.verify_profile(&Address::generate(&env), &Address::generate(&env), &true);
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
    client.verify_profile(&admin, &issuer, &true);
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

    // First revoke transitions Pending to Revoked.
    let result = client.revoke(&issuer);
    assert!(result);
    assert!(!client.is_verified(&issuer));
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Revoked
    );

    // Second revoke on already-unverified profile — should succeed
    // without panic and without altering state.
    let result2 = client.revoke(&issuer);
    assert!(result2);
    assert!(!client.is_verified(&issuer));
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Revoked
    );
    assert_eq!(count_events_named(&env, "address_revoked"), 1);
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
    client.verify_profile(&admin, &issuer, &true);
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
    client.verify_profile(&admin, &issuer, &true);
    assert!(client.is_verified(&issuer));

    client.revoke(&issuer);
    assert!(!client.is_verified(&issuer));

    // Reinstate the revoked issuer via admin.verify_profile.
    client.verify_profile(&admin, &issuer, &true);
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
    client.verify_profile(&admin, &issuer, &true);
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
fn test_reinstate_on_pending_profile_does_not_verify() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    // The profile is Pending: never verified, never revoked. `reinstate` is
    // the inverse of `revoke` and must not act as an undocumented alternate
    // path to verify a Pending profile (#606).
    let events_before = env.events().all().len();
    let result = client.reinstate(&issuer);
    assert!(result);
    assert!(!client.is_verified(&issuer));
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Pending
    );
    // No `address_reinstated` event and no other emission: the call is a no-op.
    assert_eq!(env.events().all().len(), events_before);
}

#[test]
fn test_reinstate_on_verified_profile_is_idempotent_noop() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.verify_profile(&admin, &issuer, &true);

    // Already Verified (never revoked): reinstate must not rewrite storage or
    // re-emit `address_reinstated`, mirroring `revoke`'s idempotency guard (#606).
    let events_before = env.events().all().len();
    assert!(client.reinstate(&issuer));
    assert!(client.is_verified(&issuer));
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Verified
    );
    assert_eq!(env.events().all().len(), events_before);
}

// ============== REINSTATE STATE CONSTRAINTS AND EVENT BEHAVIOR (#983) ==============

/// Asserts that two profiles carry the same observable state.
///
/// `Profile` deliberately does not implement `PartialEq`: its `packed_flags`
/// bit layout is a private implementation detail that must not become a
/// stable external contract (#609). The decoded views and the remaining
/// fields are compared individually instead, which is exactly the state an
/// off-chain consumer can observe.
#[track_caller]
fn assert_profiles_equivalent(before: &Profile, after: &Profile) {
    assert_eq!(after.role(), before.role());
    assert_eq!(after.verified(), before.verified());
    assert_eq!(after.revoked(), before.revoked());
    assert_eq!(after.registered_at, before.registered_at);
    assert_eq!(after.metadata, before.metadata);
}

/// Counts the events whose first topic symbol is `name`.
///
/// Used to assert how many times a specific event was published, which the
/// "no event" and "exactly one event" assertions below need. The topic is
/// decoded back into a `Symbol` because `Val` itself is not comparable.
fn count_events_named(env: &Env, name: &str) -> u32 {
    let expected = Symbol::new(env, name);
    let all_events = env.events().all();
    let mut count = 0u32;
    for i in 0..all_events.len() {
        let event = all_events.get(i).expect("event index in range");
        let topics: Vec<Val> = match event.1.clone().try_into_val(env) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if topics.is_empty() {
            continue;
        }
        let topic_symbol: Symbol = match topics.get(0).unwrap().try_into_val(env) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if topic_symbol == expected {
            count += 1;
        }
    }
    count
}

/// A `reinstate` call against a `Pending` profile is rejected as a state
/// transition: the stored profile is left exactly as it was and no event is
/// emitted (#983).
///
/// `Pending` is the state of a freshly registered profile. `reinstate` is the
/// inverse of `revoke`, so it must never double as an alternate path to
/// verify a profile that was never verified — `verify_profile` is the only
/// way to move a profile out of `Pending`.
#[test]
fn test_reinstate_on_pending_profile_leaves_profile_unchanged() {
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
    client.register_issuer(&issuer, &metadata);

    let profile_before = client.get_profile(&issuer);
    assert!(!profile_before.verified());
    assert!(!profile_before.revoked());
    let events_before = env.events().all();

    // The no-op path returns before any storage work, so the profile entry's
    // remaining TTL must not be renewed either. Read the TTL around the
    // call — `get_profile` above extends it, so it cannot be sampled after
    // the call.
    let contract_id = client.address.clone();
    let key = DataKey::Profile(issuer.clone());
    let ttl_before_reinstate: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));

    assert!(client.reinstate(&issuer));

    let ttl_after_reinstate: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    assert_eq!(
        ttl_after_reinstate, ttl_before_reinstate,
        "a no-op reinstate must not write to the profile entry"
    );

    // The whole profile record survives the call untouched: role, both
    // flags, the registration timestamp and the metadata all stay the same.
    let profile_after = client.get_profile(&issuer);
    assert_profiles_equivalent(&profile_before, &profile_after);
    assert!(!profile_after.verified());
    assert!(!profile_after.revoked());
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Pending
    );

    // No event at all — in particular no `address_reinstated`.
    assert_eq!(env.events().all(), events_before);
    assert_eq!(count_events_named(&env, "address_reinstated"), 0);
}

/// A `reinstate` call against an already `Verified` profile is rejected as a
/// state transition: the stored profile is left exactly as it was and no
/// event is emitted (#983).
///
/// This is the idempotency guard introduced with `revoke` (#606). Without it
/// a second `reinstate` would re-emit `address_reinstated` and off-chain
/// indexers would read it as a fresh un-revocation.
#[test]
fn test_reinstate_on_verified_profile_leaves_profile_unchanged() {
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
    client.register_issuer(&issuer, &metadata);
    client.verify_profile(&admin, &issuer, &true);

    let profile_before = client.get_profile(&issuer);
    assert!(profile_before.verified());
    assert!(!profile_before.revoked());
    let events_before = env.events().all();

    // The no-op path returns before any storage work, so the profile entry's
    // remaining TTL must not be renewed either. Read the TTL around the
    // call — `get_profile` above extends it, so it cannot be sampled after
    // the call.
    let contract_id = client.address.clone();
    let key = DataKey::Profile(issuer.clone());
    let ttl_before_reinstate: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));

    assert!(client.reinstate(&issuer));

    let ttl_after_reinstate: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    assert_eq!(
        ttl_after_reinstate, ttl_before_reinstate,
        "a no-op reinstate must not write to the profile entry"
    );

    // The whole profile record survives the call untouched.
    let profile_after = client.get_profile(&issuer);
    assert_profiles_equivalent(&profile_before, &profile_after);
    assert!(profile_after.verified());
    assert!(!profile_after.revoked());
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Verified
    );

    // No event at all — in particular no `address_reinstated`.
    assert_eq!(env.events().all(), events_before);
    assert_eq!(count_events_named(&env, "address_reinstated"), 0);
}

/// Reinstating a `Revoked` profile clears the `revoked` flag and sets
/// `verified`, and carries every other profile field over untouched (#983).
///
/// The cleared `revoked` flag is asserted directly on the profile rather
/// than through `get_verification_status`: the status view checks `verified`
/// first, so it reports `Verified` even if the `revoked` flag were still
/// set. Reading the flag off the profile is the only way to pin it.
#[test]
fn test_reinstate_clears_revoked_flag_and_preserves_profile_fields() {
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
    client.register_issuer(&issuer, &metadata);
    client.verify_profile(&admin, &issuer, &true);
    client.revoke(&issuer);

    let profile_before = client.get_profile(&issuer);
    assert!(!profile_before.verified());
    assert!(profile_before.revoked());
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Revoked
    );

    assert!(client.reinstate(&issuer));

    let profile_after = client.get_profile(&issuer);
    // `verified` is set and the `revoked` flag is cleared.
    assert!(profile_after.verified());
    assert!(!profile_after.revoked());
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Verified
    );

    // The transition flips the two flags and nothing else: role, registration
    // timestamp and metadata are carried over untouched.
    assert_eq!(profile_after.role(), profile_before.role());
    assert_eq!(profile_after.registered_at, profile_before.registered_at);
    assert_eq!(profile_after.metadata, profile_before.metadata);
}

/// Reinstating a `Revoked` profile extends the profile entry's TTL (#983).
///
/// A reinstatement is the moment an administrator brings a profile back
/// into service, so the entry must be kept alive on the same schedule as
/// every other write path in the registry.
#[test]
fn test_reinstate_extends_profile_ttl() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.verify_profile(&admin, &issuer, &true);
    client.revoke(&issuer);

    let contract_id = client.address.clone();
    let key = DataKey::Profile(issuer.clone());

    // Record the initial remaining TTL, then advance the ledger so the
    // remaining TTL drops below the write-path threshold. The reinstatement
    // is then the call that has to renew it.
    let ttl_before_drain: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + ttl_before_drain - 50);

    let ttl_before_reinstate: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    assert!(
        ttl_before_reinstate < TTL_THRESHOLD,
        "TTL should be below threshold before reinstate, got {ttl_before_reinstate}"
    );

    assert!(client.reinstate(&issuer));

    let ttl_after_reinstate: u32 =
        env.as_contract(&contract_id, || env.storage().persistent().get_ttl(&key));
    assert!(
        ttl_after_reinstate > ttl_before_reinstate,
        "reinstate should extend TTL: before={ttl_before_reinstate}, after={ttl_after_reinstate}"
    );
    assert!(
        ttl_after_reinstate >= 1_999_000,
        "TTL should be extended close to EXTEND_TO (2_000_000), got {ttl_after_reinstate}"
    );
}

/// A reinstatement emits exactly one `address_reinstated` event, and a
/// follow-up call against the now-`Verified` profile emits none (#983).
///
/// `address_reinstated` is the only signal an indexer gets for an
/// un-revocation, so it must be published exactly once per real transition.
#[test]
fn test_reinstate_emits_exactly_one_address_reinstated_event() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.verify_profile(&admin, &issuer, &true);
    client.revoke(&issuer);

    // Revoking does not reinstate anything.
    assert_eq!(count_events_named(&env, "address_reinstated"), 0);

    assert!(client.reinstate(&issuer));
    assert_eq!(count_events_named(&env, "address_reinstated"), 1);

    // The profile is `Verified` now, so the second call takes the no-op path
    // and must not publish a second event.
    assert!(client.reinstate(&issuer));
    assert_eq!(count_events_named(&env, "address_reinstated"), 1);
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

// ============== ISSUE #992: update_metadata PRESERVES PACKED FLAGS ==============

#[test]
fn test_update_metadata_preserves_flags_on_verified_profile() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.verify_profile(&admin, &issuer, &true);

    let before = client.get_profile(&issuer);
    assert!(before.verified());
    assert!(!before.revoked());
    assert_eq!(before.role(), Role::Issuer);
    assert_eq!(before.packed_flags, 0b010); // Issuer | verified

    let updated_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Updated Corp"),
        )
    ];
    let result = client.update_metadata(&issuer, &updated_metadata);
    assert!(result);

    let after = client.get_profile(&issuer);
    assert_eq!(after.metadata, updated_metadata);
    assert_eq!(after.packed_flags, before.packed_flags);
    assert_eq!(after.role(), before.role());
    assert_eq!(after.verified(), before.verified());
    assert_eq!(after.revoked(), before.revoked());
    assert_eq!(after.registered_at, before.registered_at);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Verified
    );
}

#[test]
fn test_update_metadata_preserves_flags_on_revoked_profile() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.verify_profile(&admin, &issuer, &true);
    client.revoke(&issuer);

    let before = client.get_profile(&issuer);
    assert!(!before.verified());
    assert!(before.revoked());
    assert_eq!(before.role(), Role::Issuer);
    assert_eq!(before.packed_flags, 0b100); // Issuer | revoked

    let updated_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Revoked Corp"),
        )
    ];
    let result = client.update_metadata(&issuer, &updated_metadata);
    assert!(result);

    let after = client.get_profile(&issuer);
    assert_eq!(after.metadata, updated_metadata);
    assert_eq!(after.packed_flags, before.packed_flags);
    assert_eq!(after.role(), before.role());
    assert_eq!(after.verified(), before.verified());
    assert_eq!(after.revoked(), before.revoked());
    assert_eq!(after.registered_at, before.registered_at);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Revoked
    );
}

#[test]
fn test_update_metadata_preserves_flags_on_pending_profile() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    let before = client.get_profile(&issuer);
    assert!(!before.verified());
    assert!(!before.revoked());
    assert_eq!(before.role(), Role::Issuer);
    assert_eq!(before.packed_flags, 0b000); // Issuer | pending

    let updated_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Pending Corp"),
        )
    ];
    let result = client.update_metadata(&issuer, &updated_metadata);
    assert!(result);

    let after = client.get_profile(&issuer);
    assert_eq!(after.metadata, updated_metadata);
    assert_eq!(after.packed_flags, before.packed_flags);
    assert_eq!(after.role(), before.role());
    assert_eq!(after.verified(), before.verified());
    assert_eq!(after.revoked(), before.revoked());
    assert_eq!(after.registered_at, before.registered_at);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Pending
    );
}

#[test]
fn test_update_metadata_preserves_flags_on_buyer_roles() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    // Buyer: Verified
    let buyer_verified = Address::generate(&env);
    client.register_buyer(&buyer_verified, &map![&env]);
    client.verify_profile(&admin, &buyer_verified, &true);
    let before_v = client.get_profile(&buyer_verified);
    assert_eq!(before_v.packed_flags, 0b011); // Buyer | verified

    // Buyer: Revoked
    let buyer_revoked = Address::generate(&env);
    client.register_buyer(&buyer_revoked, &map![&env]);
    client.verify_profile(&admin, &buyer_revoked, &true);
    client.revoke(&buyer_revoked);
    let before_r = client.get_profile(&buyer_revoked);
    assert_eq!(before_r.packed_flags, 0b101); // Buyer | revoked

    // Buyer: Pending
    let buyer_pending = Address::generate(&env);
    client.register_buyer(&buyer_pending, &map![&env]);
    let before_p = client.get_profile(&buyer_pending);
    assert_eq!(before_p.packed_flags, 0b001); // Buyer | pending

    let updated_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Buyer Updated"),
        )
    ];

    for who in [&buyer_verified, &buyer_revoked, &buyer_pending] {
        let before = client.get_profile(who);
        let result = client.update_metadata(who, &updated_metadata);
        assert!(result);
        let after = client.get_profile(who);
        assert_eq!(after.metadata, updated_metadata);
        assert_eq!(after.packed_flags, before.packed_flags);
        assert_eq!(after.role(), before.role());
        assert_eq!(after.verified(), before.verified());
        assert_eq!(after.revoked(), before.revoked());
        assert_eq!(after.registered_at, before.registered_at);
    }
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

// ============== ISSUE #990: BATCH METADATA BOUNDS COVERAGE ==============

#[test]
fn test_batch_register_issuers_accepts_max_metadata_limits() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);
    let issuer3 = Address::generate(&env);

    // Max entries (20)
    let mut max_entries = map![&env];
    for i in 0..MAX_METADATA_SIZE {
        let key = String::from_str(&env, &std::format!("key_{}", i));
        let value = String::from_str(&env, &std::format!("value_{}", i));
        max_entries.set(key, value);
    }
    assert_eq!(max_entries.len(), 20);

    // Max key length (64)
    let max_key = "k".repeat(MAX_METADATA_KEY_LEN as usize);
    let max_key_metadata = map![
        &env,
        (
            String::from_str(&env, &max_key),
            String::from_str(&env, "value")
        )
    ];

    // Max value length (512)
    let max_value = "v".repeat(MAX_METADATA_VALUE_LEN as usize);
    let max_value_metadata = map![
        &env,
        (
            String::from_str(&env, "key"),
            String::from_str(&env, &max_value)
        )
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), max_entries),
        (issuer2.clone(), max_key_metadata),
        (issuer3.clone(), max_value_metadata),
    ];

    let skipped = client.batch_register_issuers(&entries);
    assert_eq!(skipped.len(), 0);

    assert!(!client.is_verified(&issuer1));
    assert!(!client.is_verified(&issuer2));
    assert!(!client.is_verified(&issuer3));

    assert_eq!(client.get_profile(&issuer1).metadata.len(), 20);
    assert_eq!(
        client
            .get_profile(&issuer2)
            .metadata
            .get(String::from_str(&env, &max_key))
            .unwrap(),
        String::from_str(&env, "value")
    );
    assert_eq!(
        client
            .get_profile(&issuer3)
            .metadata
            .get(String::from_str(&env, "key"))
            .unwrap()
            .len(),
        MAX_METADATA_VALUE_LEN
    );
}

#[test]
fn test_batch_register_buyers_accepts_max_metadata_limits() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);
    let buyer3 = Address::generate(&env);

    // Max entries (20)
    let mut max_entries = map![&env];
    for i in 0..MAX_METADATA_SIZE {
        let key = String::from_str(&env, &std::format!("key_{}", i));
        let value = String::from_str(&env, &std::format!("value_{}", i));
        max_entries.set(key, value);
    }

    // Max key length (64)
    let max_key = "k".repeat(MAX_METADATA_KEY_LEN as usize);
    let max_key_metadata = map![
        &env,
        (
            String::from_str(&env, &max_key),
            String::from_str(&env, "value")
        )
    ];

    // Max value length (512)
    let max_value = "v".repeat(MAX_METADATA_VALUE_LEN as usize);
    let max_value_metadata = map![
        &env,
        (
            String::from_str(&env, "key"),
            String::from_str(&env, &max_value)
        )
    ];

    let entries = vec![
        &env,
        (buyer1.clone(), max_entries),
        (buyer2.clone(), max_key_metadata),
        (buyer3.clone(), max_value_metadata),
    ];

    let skipped = client.batch_register_buyers(&entries);
    assert_eq!(skipped.len(), 0);

    assert!(!client.is_verified(&buyer1));
    assert!(!client.is_verified(&buyer2));
    assert!(!client.is_verified(&buyer3));

    assert_eq!(client.get_profile(&buyer1).metadata.len(), 20);
    assert_eq!(
        client
            .get_profile(&buyer2)
            .metadata
            .get(String::from_str(&env, &max_key))
            .unwrap(),
        String::from_str(&env, "value")
    );
    assert_eq!(
        client
            .get_profile(&buyer3)
            .metadata
            .get(String::from_str(&env, "key"))
            .unwrap()
            .len(),
        MAX_METADATA_VALUE_LEN
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_batch_register_issuers_rejects_oversized_entry_count() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);
    let issuer3 = Address::generate(&env);

    // Valid metadata for first two
    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    // Third entry has 21 entries (exceeds MAX_METADATA_SIZE=20)
    let mut oversized = map![&env];
    for i in 0..21 {
        let key = String::from_str(&env, &std::format!("key_{}", i));
        let value = String::from_str(&env, &std::format!("value_{}", i));
        oversized.set(key, value);
    }

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata.clone()),
        (issuer2.clone(), valid_metadata),
        (issuer3.clone(), oversized),
    ];

    // Should panic and reject entire batch
    client.batch_register_issuers(&entries);
}

#[test]
fn test_batch_register_issuers_rejects_oversized_entry_count_leaves_state_clean() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);
    let issuer3 = Address::generate(&env);

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    let mut oversized = map![&env];
    for i in 0..21 {
        let key = String::from_str(&env, &std::format!("key_{}", i));
        let value = String::from_str(&env, &std::format!("value_{}", i));
        oversized.set(key, value);
    }

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata.clone()),
        (issuer2.clone(), valid_metadata),
        (issuer3.clone(), oversized),
    ];

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_register_issuers(&entries);
    }));
    assert!(
        result.is_err(),
        "batch with oversized metadata should panic"
    );

    // Verify NO profiles were persisted (atomic rejection)
    assert_eq!(
        client.get_verification_status(&issuer1),
        VerificationStatus::Unregistered
    );
    assert_eq!(
        client.get_verification_status(&issuer2),
        VerificationStatus::Unregistered
    );
    assert_eq!(
        client.get_verification_status(&issuer3),
        VerificationStatus::Unregistered
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_batch_register_issuers_rejects_oversized_key() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    let long_key = "k".repeat((MAX_METADATA_KEY_LEN + 1) as usize);
    let oversized_key_metadata = map![
        &env,
        (
            String::from_str(&env, &long_key),
            String::from_str(&env, "value")
        )
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata),
        (issuer2.clone(), oversized_key_metadata),
    ];

    client.batch_register_issuers(&entries);
}

#[test]
fn test_batch_register_issuers_rejects_oversized_key_leaves_state_clean() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    let long_key = "k".repeat((MAX_METADATA_KEY_LEN + 1) as usize);
    let oversized_key_metadata = map![
        &env,
        (
            String::from_str(&env, &long_key),
            String::from_str(&env, "value")
        )
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata),
        (issuer2.clone(), oversized_key_metadata),
    ];

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_register_issuers(&entries);
    }));
    assert!(result.is_err());

    assert_eq!(
        client.get_verification_status(&issuer1),
        VerificationStatus::Unregistered
    );
    assert_eq!(
        client.get_verification_status(&issuer2),
        VerificationStatus::Unregistered
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_batch_register_issuers_rejects_oversized_value() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    let long_value = "v".repeat((MAX_METADATA_VALUE_LEN + 1) as usize);
    let oversized_value_metadata = map![
        &env,
        (
            String::from_str(&env, "key"),
            String::from_str(&env, &long_value)
        )
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata),
        (issuer2.clone(), oversized_value_metadata),
    ];

    client.batch_register_issuers(&entries);
}

#[test]
fn test_batch_register_issuers_rejects_oversized_value_leaves_state_clean() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    let long_value = "v".repeat((MAX_METADATA_VALUE_LEN + 1) as usize);
    let oversized_value_metadata = map![
        &env,
        (
            String::from_str(&env, "key"),
            String::from_str(&env, &long_value)
        )
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata),
        (issuer2.clone(), oversized_value_metadata),
    ];

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_register_issuers(&entries);
    }));
    assert!(result.is_err());

    assert_eq!(
        client.get_verification_status(&issuer1),
        VerificationStatus::Unregistered
    );
    assert_eq!(
        client.get_verification_status(&issuer2),
        VerificationStatus::Unregistered
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_batch_register_issuers_rejects_empty_key() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    let empty_key_metadata = map![
        &env,
        (String::from_str(&env, ""), String::from_str(&env, "value"))
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata),
        (issuer2.clone(), empty_key_metadata),
    ];

    client.batch_register_issuers(&entries);
}

#[test]
fn test_batch_register_issuers_rejects_empty_key_leaves_state_clean() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    let empty_key_metadata = map![
        &env,
        (String::from_str(&env, ""), String::from_str(&env, "value"))
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata),
        (issuer2.clone(), empty_key_metadata),
    ];

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_register_issuers(&entries);
    }));
    assert!(result.is_err());

    assert_eq!(
        client.get_verification_status(&issuer1),
        VerificationStatus::Unregistered
    );
    assert_eq!(
        client.get_verification_status(&issuer2),
        VerificationStatus::Unregistered
    );
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_batch_register_issuers_rejects_empty_value() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    let empty_value_metadata = map![
        &env,
        (String::from_str(&env, "key"), String::from_str(&env, ""))
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata),
        (issuer2.clone(), empty_value_metadata),
    ];

    client.batch_register_issuers(&entries);
}

#[test]
fn test_batch_register_issuers_rejects_empty_value_leaves_state_clean() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let issuer1 = Address::generate(&env);
    let issuer2 = Address::generate(&env);

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    let empty_value_metadata = map![
        &env,
        (String::from_str(&env, "key"), String::from_str(&env, ""))
    ];

    let entries = vec![
        &env,
        (issuer1.clone(), valid_metadata),
        (issuer2.clone(), empty_value_metadata),
    ];

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_register_issuers(&entries);
    }));
    assert!(result.is_err());

    assert_eq!(
        client.get_verification_status(&issuer1),
        VerificationStatus::Unregistered
    );
    assert_eq!(
        client.get_verification_status(&issuer2),
        VerificationStatus::Unregistered
    )
}

#[test]
fn test_batch_register_buyers_metadata_bounds_mirror_issuers() {
    // Buyers use the same validate_metadata path; spot-check the same boundaries.
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);

    let valid_metadata = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "Valid")
        )
    ];

    // Oversized entry count
    let mut oversized = map![&env];
    for i in 0..21 {
        let key = String::from_str(&env, &std::format!("key_{}", i));
        let value = String::from_str(&env, &std::format!("value_{}", i));
        oversized.set(key, value);
    }

    let entries = vec![
        &env,
        (buyer1.clone(), valid_metadata),
        (buyer2.clone(), oversized),
    ];

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.batch_register_buyers(&entries);
    }));
    assert!(result.is_err());

    assert_eq!(
        client.get_verification_status(&buyer1),
        VerificationStatus::Unregistered
    );
    assert_eq!(
        client.get_verification_status(&buyer2),
        VerificationStatus::Unregistered
    );
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
    let result = client.verify_profile(&admin, &issuer, &true);
    assert!(result);
    assert!(client.is_verified(&issuer));

    // Revoke
    client.revoke(&issuer);
    assert!(!client.is_verified(&issuer));

    // Re-verify
    let result = client.verify_profile(&admin, &issuer, &true);
    assert!(result);
    assert!(client.is_verified(&issuer));

    // Un-verify again
    let result2 = client.verify_profile(&admin, &issuer, &false);
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
    client.verify_profile(&admin, &unknown, &true);
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
    client.verify_profile(&admin, &issuer, &true);
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
    client.verify_profile(&admin, &issuer, &true);
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
    client.verify_profile(&admin, &issuer, &true);
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

// The state-transition table in docs/THREAT_MODEL.md ("Registry Profile
// States and Transitions") is the documented contract for how a profile
// moves between Pending, Verified and Revoked. This test walks every
// documented transition — including the two documented no-ops — so the
// table cannot silently drift from the code (#995).
#[test]
fn test_documented_verification_state_transitions() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    let metadata = map![&env];

    // Unregistered -> Pending on self-authenticated registration.
    client.register_issuer(&issuer, &metadata);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Pending
    );

    // Pending -> Verified via verify_profile(_, true) (admin or verifier).
    client.verify_profile(&admin, &issuer, &true);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Verified
    );

    // Verified -> Revoked via the admin-only revoke.
    client.revoke(&issuer);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Revoked
    );

    // Revoked -> Verified via the admin-only reinstate.
    client.reinstate(&issuer);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Verified
    );

    // Admin revoke transitions a never-verified (Pending) profile to Revoked.
    let pending = Address::generate(&env);
    client.register_buyer(&pending, &metadata);
    client.revoke(&pending);
    assert_eq!(
        client.get_verification_status(&pending),
        VerificationStatus::Revoked
    );

    // Revoked remains Revoked via verify_profile(_, false) (admin or verifier).
    client.verify_profile(&admin, &pending, &false);
    assert_eq!(
        client.get_verification_status(&pending),
        VerificationStatus::Revoked
    );
}

// ============== GET_ROLE VIEW TESTS (#609) ==============

#[test]
fn test_get_role_returns_issuer_role() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    // Decoded on-chain: no need to reverse-engineer packed_flags (#609).
    assert_eq!(client.get_role(&issuer), Some(Role::Issuer));
}

#[test]
fn test_get_role_returns_buyer_and_survives_verification_changes() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let buyer = Address::generate(&env);
    client.register_buyer(&buyer, &map![&env]);
    client.verify_profile(&admin, &buyer, &true);
    assert_eq!(client.get_role(&buyer), Some(Role::Buyer));
    client.revoke(&buyer);
    // The role is stable across verification state changes.
    assert_eq!(client.get_role(&buyer), Some(Role::Buyer));
}

#[test]
fn test_get_role_unregistered_returns_none() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let unknown = Address::generate(&env);
    assert_eq!(client.get_role(&unknown), None);
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
    client.verify_profile(&admin, &revoked, &true);
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
    client.verify_profile(&admin, &issuer, &true);
    client.revoke(&issuer);
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Revoked
    );
    client.verify_profile(&admin, &issuer, &true);
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
                    client.verify_profile(&admin, &address, &true);
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
                client.verify_profile(&admin, &address, &true);
                for _ in 0..cycles {
                    client.revoke(&address);
                    prop_assert_eq!(
                        client.get_verification_status(&address),
                        VerificationStatus::Revoked
                    );
                    client.verify_profile(&admin, &address, &true);
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
    client.verify_profile(&admin, &issuer, &true);

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
    client.verify_profile(&admin, &issuer, &true);

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
    client.verify_profile(&admin, &issuer, &true);

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
    client.verify_profile(&admin, &issuer, &true);
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
                        // Both issuers and buyers now preserve metadata in batch registration
                        let expected_metadata = build_metadata(&env, &metadata_per_entry[i]);
                        prop_assert_eq!(profile.metadata, expected_metadata);
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
// ============== ISSUE #715: EMERGENCY PAUSE / UNPAUSE ==============

#[test]
fn test_pause_blocks_state_changes_and_unpause_restores_them() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    // Live: registration works. New profiles start `Pending` until an admin
    // verifies them, so `is_verified` is false and the status is `Pending`.
    let issuer = Address::generate(&env);
    assert!(client.register_issuer(&issuer, &map![&env]));
    let flags_before_pause = client.get_profile(&issuer).packed_flags;

    client.pause();

    // State-changing entry points are rejected while paused...
    let new_issuer = Address::generate(&env);
    assert!(client
        .try_register_issuer(&new_issuer, &map![&env])
        .is_err());
    assert!(client.try_update_metadata(&issuer, &map![&env]).is_err());
    assert!(client.try_revoke(&issuer).is_err());
    assert!(client.try_transfer_admin(&Address::generate(&env)).is_err());
    assert!(client
        .try_batch_register_issuers(&vec![&env, (Address::generate(&env), map![&env])])
        .is_err());

    // ...while read-only views keep working.
    assert_eq!(client.get_admin(), admin);
    assert!(!client.is_verified(&issuer));
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Pending
    );
    // A view that deserializes a full record stays live too, and the rejected
    // writes above left the record untouched.
    assert_eq!(client.get_profile(&issuer).packed_flags, flags_before_pause);

    client.unpause();

    // Unpaused: state changes flow again.
    assert!(client.register_issuer(&new_issuer, &map![&env]));
    assert!(client.update_metadata(&issuer, &map![&env]));
}

/// Pins the exact breaker error: `PauseError::ContractPaused` from the shared
/// crate surfaces as `Error(Contract, #1)`. `RegistryError::AlreadyInitialized`
/// is the only other #1 and is unreachable from `register_issuer`.
#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_register_issuer_reverts_while_paused_with_contract_paused_error() {
    let (env, client) = setup();
    client.initialize(&Address::generate(&env));
    client.pause();
    client.register_issuer(&Address::generate(&env), &map![&env]);
}

/// Negative auth: only a non-admin signed `pause`, so the stored admin's
/// `require_auth()` must reject it — a non-admin cannot engage the breaker.
#[test]
fn test_pause_requires_admin_authorization() {
    let (env, client) = setup();
    client.initialize(&Address::generate(&env));
    let non_admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &non_admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &client.address,
            fn_name: "pause",
            args: soroban_sdk::Vec::new(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_pause().is_err());
}

/// Negative auth for disengaging: a non-admin cannot unpause either, so a
/// paused registry cannot be resumed by anyone but the admin.
#[test]
fn test_unpause_requires_admin_authorization() {
    let (env, client) = setup();
    client.initialize(&Address::generate(&env));
    client.pause();
    let non_admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &non_admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &client.address,
            fn_name: "unpause",
            args: soroban_sdk::Vec::new(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_unpause().is_err());
}

#[test]
fn test_pause_and_unpause_emit_events() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    client.pause();
    let events = env.events().all();
    let (contract, topics, _) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, client.address);
    assert_eq!(topics.len(), 2);
    assert_eq!(
        Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&env, "paused")
    );
    assert_eq!(
        Address::try_from_val(&env, &topics.get(1).unwrap()).unwrap(),
        admin
    );

    client.unpause();
    let events = env.events().all();
    let (contract, topics, _) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, client.address);
    assert_eq!(
        Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&env, "unpaused")
    );
    assert_eq!(
        Address::try_from_val(&env, &topics.get(1).unwrap()).unwrap(),
        admin
    );
}

/// Admin ownership transfer is a state change, so the breaker gates it too:
/// while paused it reverts and the admin is unchanged.
#[test]
fn test_transfer_ownership_blocked_while_paused() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let new_admin = Address::generate(&env);

    client.pause();
    assert!(client.try_transfer_ownership(&new_admin).is_err());
    assert_eq!(client.get_admin(), admin);
}

/// The breaker is storage-local: pausing one registry must not affect a second,
/// independently deployed instance.
#[test]
fn test_pause_is_scoped_to_one_instance() {
    let env = Env::default();
    env.mock_all_auths();
    let id_a = env.register_contract(None, RegistryContract);
    let id_b = env.register_contract(None, RegistryContract);
    let admin = Address::generate(&env);
    let a = RegistryContractClient::new(&env, &id_a);
    let b = RegistryContractClient::new(&env, &id_b);
    a.initialize(&admin);
    b.initialize(&admin);

    a.pause();

    let issuer = Address::generate(&env);
    assert!(a.try_register_issuer(&issuer, &map![&env]).is_err());
    assert!(b.register_issuer(&issuer, &map![&env]));
}

// ---------------------------------------------------------------------------
// #732: secondary Verifier role for verify_profile
// ---------------------------------------------------------------------------

/// Registers a fresh registry with an admin, a verifier and one unverified
/// issuer, using `mock_all_auths` for the setup calls only.
fn setup_with_verifier() -> (
    Env,
    Address,
    RegistryContractClient<'static>,
    Address,
    Address,
    Address,
) {
    let (env, client) = setup();
    let contract_id = client.address.clone();
    let admin = Address::generate(&env);
    let verifier = Address::generate(&env);
    let issuer = Address::generate(&env);
    client.initialize(&admin);
    client.set_verifier(&verifier);
    client.register_issuer(&issuer, &map![&env]);
    (env, contract_id, client, admin, verifier, issuer)
}

#[test]
fn test_set_verifier_stores_and_emits_event() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    assert_eq!(client.get_verifier(), None);

    let verifier = Address::generate(&env);
    client.set_verifier(&verifier);
    assert_eq!(client.get_verifier(), Some(verifier.clone()));

    let last = env.events().all().last().unwrap();
    assert_eq!(
        vec![&env, last],
        vec![
            &env,
            (
                client.address.clone(),
                (Symbol::new(&env, "verifier_set"), admin.clone()).into_val(&env),
                verifier.into_val(&env),
            ),
        ]
    );
}

#[test]
fn test_verifier_can_verify_and_unverify_profile() {
    let (env, contract_id, client, _admin, verifier, issuer) = setup_with_verifier();

    // Only the verifier signs — the admin's auth is never provided.
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &verifier,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "verify_profile",
            args: (verifier.clone(), issuer.clone(), true).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.verify_profile(&verifier, &issuer, &true));
    assert!(client.is_verified(&issuer));

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &verifier,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "verify_profile",
            args: (verifier.clone(), issuer.clone(), false).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.verify_profile(&verifier, &issuer, &false));
    assert!(!client.is_verified(&issuer));
}

#[test]
fn test_admin_can_still_verify_when_verifier_set() {
    let (_env, _contract_id, client, admin, _verifier, issuer) = setup_with_verifier();
    assert!(client.verify_profile(&admin, &issuer, &true));
    assert!(client.is_verified(&issuer));
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_verify_profile_rejects_caller_that_is_neither_admin_nor_verifier() {
    let (env, _contract_id, client, _admin, _verifier, issuer) = setup_with_verifier();
    let stranger = Address::generate(&env);
    // Even with all auths mocked, an unrecognised caller is rejected.
    client.verify_profile(&stranger, &issuer, &true);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_verify_profile_rejects_non_admin_when_no_verifier_set() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.verify_profile(&Address::generate(&env), &issuer, &true);
}

#[test]
fn test_verify_profile_as_verifier_requires_verifier_signature() {
    let (env, contract_id, client, admin, verifier, issuer) = setup_with_verifier();

    // Naming the verifier as caller while only the admin signs must fail:
    // the role check passes but `caller.require_auth()` does not.
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "verify_profile",
            args: (verifier.clone(), issuer.clone(), true).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client
        .try_verify_profile(&verifier, &issuer, &true)
        .is_err());
    assert!(!client.is_verified(&issuer));
}

#[test]
fn test_verifier_cannot_call_transfer_admin() {
    let (env, contract_id, client, admin, verifier, _issuer) = setup_with_verifier();

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &verifier,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "transfer_admin",
            args: (verifier.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_transfer_admin(&verifier).is_err());
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_verifier_cannot_call_transfer_ownership() {
    let (env, contract_id, client, admin, verifier, _issuer) = setup_with_verifier();

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &verifier,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "transfer_ownership",
            args: (verifier.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_transfer_ownership(&verifier).is_err());
    assert_eq!(client.get_admin(), admin);
}

#[test]
fn test_verifier_cannot_reassign_or_remove_verifier_role() {
    let (env, contract_id, client, _admin, verifier, _issuer) = setup_with_verifier();
    let other = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &verifier,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_verifier",
            args: (other.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_set_verifier(&other).is_err());

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &verifier,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "remove_verifier",
            args: Vec::<Val>::new(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_remove_verifier().is_err());
    assert_eq!(client.get_verifier(), Some(verifier));
}

#[test]
fn test_verifier_cannot_revoke_or_reinstate() {
    let (env, contract_id, client, admin, verifier, issuer) = setup_with_verifier();
    client.verify_profile(&admin, &issuer, &true);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &verifier,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "revoke",
            args: (issuer.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_revoke(&issuer).is_err());
    assert!(client.is_verified(&issuer));

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &verifier,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "reinstate",
            args: (issuer.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    assert!(client.try_reinstate(&issuer).is_err());
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_set_verifier_by_non_admin_panics() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    env.set_auths(&[]);
    client.set_verifier(&Address::generate(&env));
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_set_verifier_before_initialize_panics() {
    let (env, client) = setup();
    client.set_verifier(&Address::generate(&env));
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_removed_verifier_can_no_longer_verify() {
    let (_env, _contract_id, client, _admin, verifier, issuer) = setup_with_verifier();
    client.remove_verifier();
    assert_eq!(client.get_verifier(), None);
    client.verify_profile(&verifier, &issuer, &true);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_replaced_verifier_loses_role() {
    let (env, _contract_id, client, _admin, old_verifier, issuer) = setup_with_verifier();
    let new_verifier = Address::generate(&env);
    client.set_verifier(&new_verifier);
    assert!(client.verify_profile(&new_verifier, &issuer, &true));
    client.verify_profile(&old_verifier, &issuer, &true);
}

#[test]
fn test_verifier_survives_admin_transfer() {
    let (env, _contract_id, client, _admin, verifier, issuer) = setup_with_verifier();
    let new_admin = Address::generate(&env);
    client.transfer_admin(&new_admin);
    // The role is independent of who the admin is.
    assert_eq!(client.get_verifier(), Some(verifier.clone()));
    assert!(client.verify_profile(&verifier, &issuer, &true));
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_upgrade_requires_admin_auth() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    env.set_auths(&[]);
    client.upgrade(&BytesN::from_array(&env, &[0; 32]));
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_upgrade_before_initialize_panics() {
    let (env, client) = setup();
    client.upgrade(&BytesN::from_array(&env, &[0; 32]));
}

// ============== ISSUES #986, #987, #989, #991: REGISTRY INVARIANTS ==============

/// Data payloads of every `profile_verified` event emitted for `address`, in
/// emission order.
fn profile_verified_payloads(env: &Env, address: &Address) -> std::vec::Vec<bool> {
    let mut out = std::vec::Vec::new();
    for event in env.events().all().iter() {
        let topics: Vec<Val> = match event.1.clone().try_into_val(env) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if topics.len() != 2 {
            continue;
        }
        let name: Symbol = match topics.get(0).unwrap().try_into_val(env) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let who: Address = match topics.get(1).unwrap().try_into_val(env) {
            Ok(a) => a,
            Err(_) => continue,
        };
        if name == Symbol::new(env, "profile_verified") && who == *address {
            out.push(event.2.clone().try_into_val(env).expect("bool payload"));
        }
    }
    out
}

// #986: verify_profile maintains BOTH flags in each direction.
#[test]
fn test_verify_profile_maintains_verified_and_revoked_flags() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    // Pending: neither flag set.
    let pending = client.get_profile(&issuer);
    assert!(!pending.verified());
    assert!(!pending.revoked());
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Pending
    );

    // verify=true -> Verified, revoked cleared.
    client.verify_profile(&admin, &issuer, &true);
    let verified = client.get_profile(&issuer);
    assert!(verified.verified());
    assert!(!verified.revoked());
    assert_eq!(verified.role(), Role::Issuer);
    assert_eq!(verified.packed_flags, 0b010); // Issuer | verified
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Verified
    );
    assert_eq!(profile_verified_payloads(&env, &issuer), [true]);

    // verify=false -> Revoked, verified cleared.
    client.verify_profile(&admin, &issuer, &false);
    let revoked = client.get_profile(&issuer);
    assert!(!revoked.verified());
    assert!(revoked.revoked());
    assert_eq!(revoked.role(), Role::Issuer);
    assert_eq!(revoked.packed_flags, 0b100); // Issuer | revoked
    assert_eq!(
        client.get_verification_status(&issuer),
        VerificationStatus::Revoked
    );
    assert_eq!(profile_verified_payloads(&env, &issuer), [true, false]);
}

// #987: repeated verify_profile calls are INTENTIONAL AUDIT EVENTS, not
// idempotent no-ops. Storage ends in the same state; every call emits a
// `profile_verified` event carrying the requested value, so an indexer can
// count administrative decisions rather than only state changes.
#[test]
fn test_verify_profile_repeated_call_is_an_audit_event() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    client.verify_profile(&admin, &issuer, &true);
    let after_first = client.get_profile(&issuer);
    assert!(client.verify_profile(&admin, &issuer, &true));
    let after_second = client.get_profile(&issuer);

    // Storage: unchanged by the repeat.
    assert_eq!(after_first.packed_flags, after_second.packed_flags);
    assert_eq!(after_first.registered_at, after_second.registered_at);
    assert_eq!(after_first.metadata, after_second.metadata);
    // Events: one per call, not de-duplicated.
    assert_eq!(profile_verified_payloads(&env, &issuer), [true, true]);

    // Same rule in the un-verify direction.
    client.verify_profile(&admin, &issuer, &false);
    let revoked_first = client.get_profile(&issuer);
    client.verify_profile(&admin, &issuer, &false);
    assert_eq!(
        client.get_profile(&issuer).packed_flags,
        revoked_first.packed_flags
    );
    assert_eq!(
        profile_verified_payloads(&env, &issuer),
        [true, true, false, false]
    );
}

// #989: a batch mixing an already-stored address, an in-batch duplicate and a
// unique address is accounted for deterministically.
#[test]
fn test_batch_register_issuers_duplicate_accounting_and_event_counts() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let existing = Address::generate(&env);
    let repeated = Address::generate(&env);
    let unique = Address::generate(&env);
    client.register_issuer(&existing, &map![&env]);
    assert_eq!(client.get_profile_count(&Role::Issuer), 1);

    let entries = vec![
        &env,
        (existing.clone(), map![&env]),
        (repeated.clone(), map![&env]),
        (repeated.clone(), map![&env]),
        (unique.clone(), map![&env]),
    ];
    let skipped = client.batch_register_issuers(&entries);

    // The stored address is skipped first; the repeated address registers on
    // its first occurrence and is skipped on its second, so order follows the
    // input order.
    assert_eq!(skipped, vec![&env, existing.clone(), repeated.clone()]);
    assert_batch_event_counts(&env, 2, 2);
    assert_eq!(client.get_profile_count(&Role::Issuer), 3);
    assert_eq!(
        client.list_profiles(&Role::Issuer, &0, &10),
        vec![&env, existing, repeated, unique]
    );
}

// #991: update_profile replaces metadata only; every packed flag and
// registered_at survive, for differing roles and verification states.
#[test]
fn test_update_profile_preserves_role_flags_and_registered_at() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);

    let old_meta = |env: &Env| {
        map![
            env,
            (
                String::from_str(env, "name"),
                String::from_str(env, "before")
            )
        ]
    };
    let new_meta = map![
        &env,
        (
            String::from_str(&env, "name"),
            String::from_str(&env, "after")
        ),
        (
            String::from_str(&env, "site"),
            String::from_str(&env, "example.com")
        )
    ];

    env.ledger().with_mut(|l| l.timestamp = 1_000);
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &old_meta(&env));
    client.verify_profile(&admin, &issuer, &true); // Issuer, Verified

    env.ledger().with_mut(|l| l.timestamp = 2_000);
    let buyer = Address::generate(&env);
    client.register_buyer(&buyer, &old_meta(&env)); // Buyer, Pending

    env.ledger().with_mut(|l| l.timestamp = 3_000);
    let revoked_buyer = Address::generate(&env);
    client.register_buyer(&revoked_buyer, &old_meta(&env));
    client.verify_profile(&admin, &revoked_buyer, &false); // Buyer, Revoked

    for who in [&issuer, &buyer, &revoked_buyer] {
        let before = client.get_profile(who);
        let role_before = client.get_role(who);
        let status_before = client.get_verification_status(who);

        assert!(client.update_profile(who, &new_meta));

        let after = client.get_profile(who);
        assert_eq!(after.metadata, new_meta);
        assert_ne!(after.metadata, before.metadata);
        assert_eq!(after.packed_flags, before.packed_flags);
        assert_eq!(after.role(), before.role());
        assert_eq!(after.verified(), before.verified());
        assert_eq!(after.revoked(), before.revoked());
        assert_eq!(after.registered_at, before.registered_at);
        assert_eq!(client.get_role(who), role_before);
        assert_eq!(client.get_verification_status(who), status_before);
    }

    assert_eq!(client.get_profile(&issuer).registered_at, 1_000);
    assert_eq!(client.get_profile(&buyer).registered_at, 2_000);
    assert_eq!(client.get_profile(&revoked_buyer).registered_at, 3_000);
}

#[test]
fn test_revoke_pending_verified_and_revoked_transitions() {
    let (env, client) = setup();
    let admin = Address::generate(&env);
    client.initialize(&admin);
    let pending = Address::generate(&env);
    let verified = Address::generate(&env);
    client.register_issuer(&pending, &map![&env]);
    client.register_issuer(&verified, &map![&env]);
    client.verify_profile(&admin, &verified, &true);

    assert_eq!(
        client.get_verification_status(&pending),
        VerificationStatus::Pending
    );
    assert_eq!(
        client.get_verification_status(&verified),
        VerificationStatus::Verified
    );
    assert_eq!(count_events_named(&env, "address_revoked"), 0);

    assert!(client.revoke(&pending));
    assert_eq!(
        client.get_verification_status(&pending),
        VerificationStatus::Revoked
    );
    let pending_profile = client.get_profile(&pending);
    assert!(!pending_profile.verified());
    assert!(pending_profile.revoked());
    assert!(!client.is_verified(&pending));
    assert_eq!(count_events_named(&env, "address_revoked"), 1);

    assert!(client.revoke(&verified));
    assert_eq!(
        client.get_verification_status(&verified),
        VerificationStatus::Revoked
    );
    let verified_profile = client.get_profile(&verified);
    assert!(!verified_profile.verified());
    assert!(verified_profile.revoked());
    assert!(!client.is_verified(&verified));
    assert_eq!(count_events_named(&env, "address_revoked"), 2);

    assert!(client.revoke(&pending));
    assert!(client.revoke(&verified));
    assert_eq!(
        client.get_verification_status(&pending),
        VerificationStatus::Revoked
    );
    assert_eq!(
        client.get_verification_status(&verified),
        VerificationStatus::Revoked
    );
    assert_eq!(count_events_named(&env, "address_revoked"), 2);
}
