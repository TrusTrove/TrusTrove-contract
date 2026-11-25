//! Tests for profile update sequence numbers (#1128), the emergency guardian
//! freeze (#1125), the delayed admin transfer (#1124) and the metadata key
//! policy (#1120).

extern crate std;

use crate::{
    DataKey, MetadataKeyPolicy, PendingAdminTransfer, RegistryContract, RegistryContractClient,
    RegistryError, METADATA_KEYS_V1, MIN_ADMIN_TRANSFER_DELAY,
};
use soroban_sdk::{
    map,
    testutils::{Address as _, Events as _, Ledger},
    vec, Address, BytesN, Env, Map, String, Symbol, TryIntoVal, Val, Vec,
};

fn setup() -> (Env, Address, RegistryContractClient<'static>) {
    let env = Env::default();
    env.mock_all_auths();
    let admin = Address::generate(&env);
    let contract_id = env.register(RegistryContract, (admin.clone(),));
    let client = RegistryContractClient::new(&env, &contract_id);
    (env, admin, client)
}

fn meta(env: &Env, key: &str, value: &str) -> Map<String, String> {
    map![
        env,
        (String::from_str(env, key), String::from_str(env, value))
    ]
}

/// Number of events from the last top-level call whose first topic is `name`.
fn count_events(env: &Env, name: &str) -> u32 {
    let expected = Symbol::new(env, name);
    let mut count = 0;
    for event in env.events().all().iter() {
        let topics: Vec<Val> = event.1.clone();
        if topics.is_empty() {
            continue;
        }
        let symbol: Result<Symbol, _> = topics.get(0).unwrap().try_into_val(env);
        if symbol.map(|s| s == expected).unwrap_or(false) {
            count += 1;
        }
    }
    count
}

/// Data of the last `profile_version` event from the last top-level call.
fn last_version_event(env: &Env) -> Option<u64> {
    let expected = Symbol::new(env, "profile_version");
    let mut found = None;
    for event in env.events().all().iter() {
        let symbol: Result<Symbol, _> = event.1.get(0).unwrap().try_into_val(env);
        if symbol.map(|s| s == expected).unwrap_or(false) {
            found = Some(event.2.try_into_val(env).unwrap());
        }
    }
    found
}

// ---------------------------------------------------------------------------
// #1128 profile update sequence numbers
// ---------------------------------------------------------------------------

#[test]
fn version_is_zero_for_unregistered_address() {
    let (env, _admin, client) = setup();
    assert_eq!(client.get_profile_version(&Address::generate(&env)), 0);
}

#[test]
fn registration_sets_version_one_and_emits_event() {
    let (env, _admin, client) = setup();
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &meta(&env, "name", "Acme"));
    assert_eq!(last_version_event(&env), Some(1));
    assert_eq!(client.get_profile_version(&issuer), 1);

    let buyer = Address::generate(&env);
    client.register_buyer(&buyer, &map![&env]);
    assert_eq!(last_version_event(&env), Some(1));
    assert_eq!(client.get_profile_version(&buyer), 1);
}

#[test]
fn batch_registration_versions_only_new_profiles() {
    let (env, _admin, client) = setup();
    let existing = Address::generate(&env);
    client.register_issuer(&existing, &map![&env]);
    let fresh = Address::generate(&env);
    let entries = vec![
        &env,
        (existing.clone(), map![&env]),
        (fresh.clone(), map![&env]),
        (fresh.clone(), map![&env]),
    ];
    client.batch_register_issuers(&entries);
    assert_eq!(count_events(&env, "profile_version"), 1);
    assert_eq!(client.get_profile_version(&existing), 1);
    assert_eq!(client.get_profile_version(&fresh), 1);

    let buyer = Address::generate(&env);
    client.batch_register_buyers(&vec![&env, (buyer.clone(), map![&env])]);
    assert_eq!(client.get_profile_version(&buyer), 1);
}

#[test]
fn metadata_change_bumps_version_but_identical_metadata_does_not() {
    let (env, _admin, client) = setup();
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &meta(&env, "name", "Acme"));

    client.update_profile(&issuer, &meta(&env, "name", "Acme 2"));
    assert_eq!(last_version_event(&env), Some(2));
    client.update_metadata(&issuer, &meta(&env, "name", "Acme 3"));
    assert_eq!(client.get_profile_version(&issuer), 3);

    // Idempotent: same metadata again leaves the version (and emits none).
    client.update_metadata(&issuer, &meta(&env, "name", "Acme 3"));
    assert_eq!(count_events(&env, "profile_version"), 0);
    client.update_profile(&issuer, &meta(&env, "name", "Acme 3"));
    assert_eq!(client.get_profile_version(&issuer), 3);
}

#[test]
fn verification_state_changes_bump_version_and_noops_do_not() {
    let (env, admin, client) = setup();
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);

    client.verify_profile(&admin, &issuer, &true); // Pending -> Verified
    assert_eq!(client.get_profile_version(&issuer), 2);
    client.verify_profile(&admin, &issuer, &true); // no change
    assert_eq!(count_events(&env, "profile_version"), 0);
    assert_eq!(client.get_profile_version(&issuer), 2);

    client.revoke(&issuer); // Verified -> Revoked
    assert_eq!(client.get_profile_version(&issuer), 3);
    client.revoke(&issuer); // idempotent
    assert_eq!(client.get_profile_version(&issuer), 3);

    client.reinstate(&issuer); // Revoked -> Verified
    assert_eq!(client.get_profile_version(&issuer), 4);
    client.reinstate(&issuer); // idempotent
    assert_eq!(client.get_profile_version(&issuer), 4);
}

#[test]
fn legacy_profile_without_version_starts_counting_from_first_change() {
    let (env, _admin, client) = setup();
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    // Simulate a profile registered before #1128 shipped.
    env.as_contract(&client.address, || {
        env.storage()
            .persistent()
            .remove(&DataKey::ProfileVersion(issuer.clone()));
    });
    assert_eq!(client.get_profile_version(&issuer), 0);
    // The stored Profile still decodes as before.
    assert!(client.get_profile(&issuer).metadata.is_empty());
    client.update_profile(&issuer, &meta(&env, "name", "Acme"));
    assert_eq!(client.get_profile_version(&issuer), 1);
}

#[test]
fn versions_are_tracked_per_profile() {
    let (env, _admin, client) = setup();
    let a = Address::generate(&env);
    let b = Address::generate(&env);
    client.register_issuer(&a, &map![&env]);
    client.register_issuer(&b, &map![&env]);
    client.update_profile(&a, &meta(&env, "name", "A"));
    assert_eq!(client.get_profile_version(&a), 2);
    assert_eq!(client.get_profile_version(&b), 1);
}

// ---------------------------------------------------------------------------
// #1124 delayed admin transfer
// ---------------------------------------------------------------------------

#[test]
fn delayed_transfer_keeps_old_admin_until_accepted() {
    let (env, admin, client) = setup();
    env.ledger().set_timestamp(1_000);
    let new_admin = Address::generate(&env);

    let activates_at = client.propose_admin(&new_admin, &MIN_ADMIN_TRANSFER_DELAY);
    assert_eq!(activates_at, 1_000 + MIN_ADMIN_TRANSFER_DELAY);
    assert_eq!(count_events(&env, "admin_transfer_proposed"), 1);
    assert_eq!(
        client.get_pending_admin_transfer(),
        Some(PendingAdminTransfer {
            new_admin: new_admin.clone(),
            activates_at,
        })
    );
    // Old admin still in charge during the delay.
    assert_eq!(client.get_admin(), admin);
    client.set_verifier(&Address::generate(&env));

    // One second early is rejected; exactly at activates_at succeeds.
    env.ledger().set_timestamp(activates_at - 1);
    assert_eq!(
        client.try_accept_admin(),
        Err(Ok(RegistryError::AdminTransferNotReady.into()))
    );
    env.ledger().set_timestamp(activates_at);
    client.accept_admin();
    assert_eq!(count_events(&env, "admin_transfer_accepted"), 1);
    assert_eq!(client.get_admin(), new_admin);
    assert_eq!(client.get_pending_admin_transfer(), None);
}

#[test]
fn accept_admin_requires_the_proposed_admins_auth() {
    let (env, _admin, client) = setup();
    let new_admin = Address::generate(&env);
    client.propose_admin(&new_admin, &MIN_ADMIN_TRANSFER_DELAY);
    env.ledger().set_timestamp(MIN_ADMIN_TRANSFER_DELAY);
    client.accept_admin();
    let auths = env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, new_admin);
}

#[test]
fn propose_admin_rejects_short_delay_self_and_overflow() {
    let (env, admin, client) = setup();
    let new_admin = Address::generate(&env);
    assert_eq!(
        client.try_propose_admin(&new_admin, &(MIN_ADMIN_TRANSFER_DELAY - 1)),
        Err(Ok(RegistryError::InvalidAdminTransfer.into()))
    );
    assert_eq!(
        client.try_propose_admin(&admin, &MIN_ADMIN_TRANSFER_DELAY),
        Err(Ok(RegistryError::InvalidAdminTransfer.into()))
    );
    env.ledger().set_timestamp(10);
    assert_eq!(
        client.try_propose_admin(&new_admin, &u64::MAX),
        Err(Ok(RegistryError::InvalidAdminTransfer.into()))
    );
    assert_eq!(client.get_pending_admin_transfer(), None);
}

#[test]
fn accept_and_cancel_without_proposal_fail() {
    let (env, admin, client) = setup();
    assert_eq!(
        client.try_accept_admin(),
        Err(Ok(RegistryError::NoPendingAdminTransfer.into()))
    );
    assert_eq!(
        client.try_cancel_admin_transfer(&admin),
        Err(Ok(RegistryError::NoPendingAdminTransfer.into()))
    );
    let _ = env;
}

#[test]
fn admin_can_cancel_and_strangers_cannot() {
    let (env, admin, client) = setup();
    let new_admin = Address::generate(&env);
    client.propose_admin(&new_admin, &MIN_ADMIN_TRANSFER_DELAY);
    assert_eq!(
        client.try_cancel_admin_transfer(&new_admin),
        Err(Ok(RegistryError::Unauthorized.into()))
    );
    client.cancel_admin_transfer(&admin);
    assert_eq!(count_events(&env, "admin_transfer_cancelled"), 1);
    assert_eq!(client.get_pending_admin_transfer(), None);
    env.ledger().set_timestamp(MIN_ADMIN_TRANSFER_DELAY);
    assert_eq!(
        client.try_accept_admin(),
        Err(Ok(RegistryError::NoPendingAdminTransfer.into()))
    );
}

#[test]
fn new_proposal_replaces_pending_one() {
    let (env, _admin, client) = setup();
    let first = Address::generate(&env);
    let second = Address::generate(&env);
    client.propose_admin(&first, &MIN_ADMIN_TRANSFER_DELAY);
    client.propose_admin(&second, &(MIN_ADMIN_TRANSFER_DELAY * 2));
    let pending = client.get_pending_admin_transfer().unwrap();
    assert_eq!(pending.new_admin, second);
    assert_eq!(pending.activates_at, MIN_ADMIN_TRANSFER_DELAY * 2);
}

#[test]
fn immediate_transfer_clears_pending_proposal() {
    let (env, _admin, client) = setup();
    client.propose_admin(&Address::generate(&env), &MIN_ADMIN_TRANSFER_DELAY);
    client.transfer_admin(&Address::generate(&env));
    assert_eq!(client.get_pending_admin_transfer(), None);

    client.propose_admin(&Address::generate(&env), &MIN_ADMIN_TRANSFER_DELAY);
    client.transfer_ownership(&Address::generate(&env));
    assert_eq!(client.get_pending_admin_transfer(), None);
}

// ---------------------------------------------------------------------------
// #1125 emergency guardian freeze
// ---------------------------------------------------------------------------

fn setup_with_guardian() -> (Env, Address, Address, RegistryContractClient<'static>) {
    let (env, admin, client) = setup();
    let guardian = Address::generate(&env);
    client.set_guardian(&guardian);
    (env, admin, guardian, client)
}

#[test]
fn guardian_configuration() {
    let (env, admin, client) = setup();
    assert_eq!(client.get_guardian(), None);
    assert_eq!(
        client.try_freeze_admin(),
        Err(Ok(RegistryError::GuardianNotSet.into()))
    );
    assert_eq!(
        client.try_set_guardian(&admin),
        Err(Ok(RegistryError::InvalidGuardian.into()))
    );
    let guardian = Address::generate(&env);
    client.set_guardian(&guardian);
    assert_eq!(count_events(&env, "guardian_set"), 1);
    assert_eq!(client.get_guardian(), Some(guardian));
    client.remove_guardian();
    assert_eq!(client.get_guardian(), None);
}

#[test]
fn freeze_requires_guardian_auth_and_is_idempotent() {
    let (env, _admin, guardian, client) = setup_with_guardian();
    assert!(!client.is_admin_frozen());
    client.freeze_admin();
    let auths = env.auths();
    assert_eq!(auths.len(), 1);
    assert_eq!(auths[0].0, guardian);
    assert_eq!(count_events(&env, "admin_frozen"), 1);
    assert!(client.is_admin_frozen());

    client.freeze_admin();
    assert_eq!(count_events(&env, "admin_frozen"), 0);
    assert!(client.is_admin_frozen());
}

#[test]
fn freeze_blocks_every_admin_write() {
    let (env, _admin, _guardian, client) = setup_with_guardian();
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    let verifier = Address::generate(&env);
    client.set_verifier(&verifier);
    client.freeze_admin();

    let other = Address::generate(&env);
    assert_eq!(
        client.try_revoke(&issuer),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_reinstate(&issuer),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_verify_profile(&verifier, &issuer, &true),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_batch_register_issuers(&vec![&env, (other.clone(), map![&env])]),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_batch_register_buyers(&vec![&env, (other.clone(), map![&env])]),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_transfer_admin(&other),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_transfer_ownership(&other),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_propose_admin(&other, &MIN_ADMIN_TRANSFER_DELAY),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_set_verifier(&other),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_remove_verifier(),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_set_guardian(&other),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_remove_guardian(),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_set_metadata_key_policy(&MetadataKeyPolicy::AllowlistV1),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_pause(),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_unpause(),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_upgrade(&BytesN::from_array(&env, &[0u8; 32])),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
}

#[test]
fn freeze_keeps_reads_results_and_self_service_writes() {
    let (env, admin, _guardian, client) = setup_with_guardian();
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &map![&env]);
    client.verify_profile(&admin, &issuer, &true);
    client.freeze_admin();

    // Existing verification results and reads are unchanged.
    assert!(client.is_verified(&issuer));
    assert_eq!(client.get_admin(), admin);
    assert_eq!(client.get_profile_version(&issuer), 2);
    // Users can still manage their own profiles.
    client.update_profile(&issuer, &meta(&env, "name", "Acme"));
    let buyer = Address::generate(&env);
    client.register_buyer(&buyer, &map![&env]);
    assert!(client.get_role(&buyer).is_some());
}

#[test]
fn only_guardian_unfreezes_and_admin_writes_resume() {
    let (env, _admin, guardian, client) = setup_with_guardian();
    client.freeze_admin();
    client.unfreeze_admin();
    let auths = env.auths();
    assert_eq!(auths[0].0, guardian);
    assert_eq!(count_events(&env, "admin_unfrozen"), 1);
    assert!(!client.is_admin_frozen());
    client.set_verifier(&Address::generate(&env));

    // Unfreezing a live registry is a no-op.
    client.unfreeze_admin();
    assert_eq!(count_events(&env, "admin_unfrozen"), 0);
}

#[test]
fn freeze_works_while_paused() {
    let (_env, _admin, _guardian, client) = setup_with_guardian();
    client.pause();
    client.freeze_admin();
    assert!(client.is_admin_frozen());
    assert_eq!(
        client.try_unpause(),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    client.unfreeze_admin();
    client.unpause();
}

#[test]
fn guardian_can_cancel_pending_transfer_while_frozen() {
    let (env, admin, guardian, client) = setup_with_guardian();
    let attacker = Address::generate(&env);
    client.propose_admin(&attacker, &MIN_ADMIN_TRANSFER_DELAY);
    client.freeze_admin();

    env.ledger().set_timestamp(MIN_ADMIN_TRANSFER_DELAY);
    assert_eq!(
        client.try_accept_admin(),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    assert_eq!(
        client.try_cancel_admin_transfer(&admin),
        Err(Ok(RegistryError::AdminFrozen.into()))
    );
    client.cancel_admin_transfer(&guardian);
    assert_eq!(client.get_pending_admin_transfer(), None);
    assert_eq!(client.get_admin(), admin);
}

// ---------------------------------------------------------------------------
// #1120 metadata key policy
// ---------------------------------------------------------------------------

#[test]
fn default_policy_is_unrestricted() {
    let (env, _admin, client) = setup();
    assert_eq!(
        client.get_metadata_key_policy(),
        MetadataKeyPolicy::Unrestricted
    );
    assert_eq!(client.get_allowed_metadata_keys(), None);
    client.register_issuer(&Address::generate(&env), &meta(&env, "anything", "x"));
}

#[test]
fn allowlist_v1_exposes_documented_keys() {
    let (env, admin, client) = setup();
    client.set_metadata_key_policy(&MetadataKeyPolicy::AllowlistV1);
    assert_eq!(count_events(&env, "metadata_key_policy_set"), 1);
    assert_eq!(env.auths()[0].0, admin);
    assert_eq!(
        client.get_metadata_key_policy(),
        MetadataKeyPolicy::AllowlistV1
    );
    let keys = client.get_allowed_metadata_keys().unwrap();
    assert_eq!(keys.len(), METADATA_KEYS_V1.len() as u32);
    for (i, key) in METADATA_KEYS_V1.iter().enumerate() {
        assert_eq!(keys.get(i as u32).unwrap(), String::from_str(&env, key));
    }
}

#[test]
fn allowlist_v1_accepts_every_allowed_key() {
    let (env, _admin, client) = setup();
    client.set_metadata_key_policy(&MetadataKeyPolicy::AllowlistV1);
    let mut metadata = Map::new(&env);
    for key in METADATA_KEYS_V1.iter() {
        metadata.set(String::from_str(&env, key), String::from_str(&env, "v"));
    }
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &metadata);
    client.update_metadata(&issuer, &meta(&env, "tax_id", "12-3456789"));
    client.batch_register_buyers(&vec![&env, (Address::generate(&env), metadata)]);
}

#[test]
fn allowlist_v1_rejects_unknown_keys_on_every_write_path() {
    let (env, _admin, client) = setup();
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &meta(&env, "name", "Acme"));
    client.set_metadata_key_policy(&MetadataKeyPolicy::AllowlistV1);

    let bad = meta(&env, "favourite_colour", "blue");
    let other = Address::generate(&env);
    assert_eq!(
        client.try_register_issuer(&other, &bad),
        Err(Ok(RegistryError::UnknownMetadataKey.into()))
    );
    assert_eq!(
        client.try_register_buyer(&other, &bad),
        Err(Ok(RegistryError::UnknownMetadataKey.into()))
    );
    assert_eq!(
        client.try_update_profile(&issuer, &bad),
        Err(Ok(RegistryError::UnknownMetadataKey.into()))
    );
    assert_eq!(
        client.try_update_metadata(&issuer, &bad),
        Err(Ok(RegistryError::UnknownMetadataKey.into()))
    );

    // Batch is atomic: a single unknown key rejects the whole batch.
    let entries = vec![
        &env,
        (other.clone(), meta(&env, "name", "ok")),
        (Address::generate(&env), bad.clone()),
    ];
    assert_eq!(
        client.try_batch_register_issuers(&entries),
        Err(Ok(RegistryError::UnknownMetadataKey.into()))
    );
    assert_eq!(
        client.try_batch_register_buyers(&entries),
        Err(Ok(RegistryError::UnknownMetadataKey.into()))
    );
    assert!(client.get_role(&other).is_none());
    // Key match is exact (case-sensitive).
    assert_eq!(
        client.try_register_issuer(&other, &meta(&env, "Name", "Acme")),
        Err(Ok(RegistryError::UnknownMetadataKey.into()))
    );
}

#[test]
fn switching_policy_keeps_existing_metadata_readable() {
    let (env, _admin, client) = setup();
    let issuer = Address::generate(&env);
    client.register_issuer(&issuer, &meta(&env, "legacy_key", "x"));
    client.set_metadata_key_policy(&MetadataKeyPolicy::AllowlistV1);
    let profile = client.get_profile(&issuer);
    assert_eq!(
        profile.metadata.get(String::from_str(&env, "legacy_key")),
        Some(String::from_str(&env, "x"))
    );
    // Switching back restores the legacy behaviour.
    client.set_metadata_key_policy(&MetadataKeyPolicy::Unrestricted);
    client.update_profile(&issuer, &meta(&env, "another_key", "y"));
}
