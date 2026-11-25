// soroban-sdk >= 22 deprecates `Events::publish` in favour of the
// `#[contractevent]` macro. The topic tuples below are the documented
// on-wire event format (see docs/EVENTS.md) and consumers index them
// positionally; migrating them is a schema change, not an SDK repair,
// so it is deliberately out of scope for the #826 upgrade.
#![allow(deprecated)]
use crate::Role;
use soroban_sdk::{Address, BytesN, Env, String, Symbol};

pub fn upgraded(env: &Env, wasm_hash: &BytesN<32>) {
    env.events()
        .publish((Symbol::new(env, "upgraded"), wasm_hash.clone()), ());
}

pub fn contract_initialized(env: &Env, admin: &Address) {
    env.events().publish(
        (Symbol::new(env, "contract_initialized"), admin.clone()),
        (),
    );
}

pub fn issuer_registered(env: &Env, address: &Address) {
    env.events()
        .publish((Symbol::new(env, "issuer_registered"), address.clone()), ());
}

pub fn buyer_registered(env: &Env, address: &Address) {
    env.events()
        .publish((Symbol::new(env, "buyer_registered"), address.clone()), ());
}

pub fn address_revoked(env: &Env, address: &Address) {
    env.events()
        .publish((Symbol::new(env, "address_revoked"), address.clone()), ());
}

pub fn batch_registered(env: &Env, registered: u32, skipped: u32) {
    env.events().publish(
        (Symbol::new(env, "batch_registered"),),
        (registered, skipped),
    );
}

pub fn profile_verified(env: &Env, address: &Address, status: bool) {
    env.events().publish(
        (Symbol::new(env, "profile_verified"), address.clone()),
        status,
    );
}

/// Emitted only for an actual role transition, never for an idempotent
/// `change_role` call (#1129). The per-address `version` disambiguates repeated
/// transitions for off-chain consumers.
pub fn role_changed(
    env: &Env,
    address: &Address,
    previous_role: &Role,
    new_role: &Role,
    version: u32,
) {
    env.events().publish(
        (Symbol::new(env, "role_changed"), address.clone()),
        (previous_role.clone(), new_role.clone(), version),
    );
}

pub fn profile_updated(env: &Env, address: &Address) {
    env.events()
        .publish((Symbol::new(env, "profile_updated"), address.clone()), ());
}

/// Emitted when the profile owner removes one metadata key.
pub fn metadata_key_removed(env: &Env, address: &Address, key: &String) {
    env.events().publish(
        (Symbol::new(env, "metadata_key_removed"), address.clone()),
        key.clone(),
    );
}

pub fn address_reinstated(env: &Env, address: &Address) {
    env.events().publish(
        (Symbol::new(env, "address_reinstated"), address.clone()),
        (),
    );
}

pub fn ownership_transferred(env: &Env, old_admin: &Address, new_admin: &Address) {
    env.events().publish(
        (Symbol::new(env, "ownership_transferred"), old_admin.clone()),
        new_admin.clone(),
    );
}

pub fn admin_transferred(env: &Env, old_admin: &Address, new_admin: &Address) {
    env.events().publish(
        (Symbol::new(env, "admin_transferred"), old_admin.clone()),
        new_admin.clone(),
    );
}

/// Emitted by `pause` when the emergency circuit breaker is engaged. `admin` is
/// the address that authorized the pause, indexed so indexers can track who
/// flipped the breaker.
pub fn paused(env: &Env, admin: &Address) {
    env.events()
        .publish((Symbol::new(env, "paused"), admin.clone()), ());
}

/// Emitted by `unpause` when the emergency circuit breaker is disengaged.
pub fn unpaused(env: &Env, admin: &Address) {
    env.events()
        .publish((Symbol::new(env, "unpaused"), admin.clone()), ());
}

/// Emitted by `set_verifier` when the admin assigns (or replaces) the
/// verifier role. `admin` is indexed; the data is the new verifier address.
pub fn verifier_set(env: &Env, admin: &Address, verifier: &Address) {
    env.events().publish(
        (Symbol::new(env, "verifier_set"), admin.clone()),
        verifier.clone(),
    );
}

/// Emitted by `remove_verifier` when the admin clears the verifier role.
pub fn verifier_removed(env: &Env, admin: &Address) {
    env.events()
        .publish((Symbol::new(env, "verifier_removed"), admin.clone()), ());
}
