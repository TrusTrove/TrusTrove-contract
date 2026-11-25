// soroban-sdk >= 22 deprecates `Events::publish` in favour of the
// `#[contractevent]` macro. The topic tuples below are the documented
// on-wire event format (see docs/EVENTS.md) and consumers index them
// positionally; migrating them is a schema change, not an SDK repair,
// so it is deliberately out of scope for the #826 upgrade.
#![allow(deprecated)]
use soroban_sdk::{Address, BytesN, Env, Symbol};

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

pub fn profile_updated(env: &Env, address: &Address) {
    env.events()
        .publish((Symbol::new(env, "profile_updated"), address.clone()), ());
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

/// Emitted whenever a profile's update sequence number changes (#1128),
/// including `1` on registration. Topic: `("profile_version", address)`;
/// data: the new `u64` version. Consumers that see a jump of more than one
/// between consecutive events for an address have missed an update.
pub fn profile_version(env: &Env, address: &Address, version: u64) {
    env.events().publish(
        (Symbol::new(env, "profile_version"), address.clone()),
        version,
    );
}

/// Emitted by `set_guardian`. Topic: `("guardian_set", admin)`; data: guardian.
pub fn guardian_set(env: &Env, admin: &Address, guardian: &Address) {
    env.events().publish(
        (Symbol::new(env, "guardian_set"), admin.clone()),
        guardian.clone(),
    );
}

/// Emitted by `remove_guardian`. Topic: `("guardian_removed", admin)`.
pub fn guardian_removed(env: &Env, admin: &Address) {
    env.events()
        .publish((Symbol::new(env, "guardian_removed"), admin.clone()), ());
}

/// Emitted by `freeze_admin` when admin-only writes become frozen.
pub fn admin_frozen(env: &Env, guardian: &Address) {
    env.events()
        .publish((Symbol::new(env, "admin_frozen"), guardian.clone()), ());
}

/// Emitted by `unfreeze_admin` when admin-only writes are restored.
pub fn admin_unfrozen(env: &Env, guardian: &Address) {
    env.events()
        .publish((Symbol::new(env, "admin_unfrozen"), guardian.clone()), ());
}

/// Emitted by `propose_admin`. Topic: `("admin_transfer_proposed", admin)`;
/// data: `(new_admin, activates_at)`.
pub fn admin_transfer_proposed(env: &Env, admin: &Address, new_admin: &Address, activates_at: u64) {
    env.events().publish(
        (Symbol::new(env, "admin_transfer_proposed"), admin.clone()),
        (new_admin.clone(), activates_at),
    );
}

/// Emitted by `cancel_admin_transfer`. Topic:
/// `("admin_transfer_cancelled", caller)`; data: the cancelled `new_admin`.
pub fn admin_transfer_cancelled(env: &Env, caller: &Address, new_admin: &Address) {
    env.events().publish(
        (Symbol::new(env, "admin_transfer_cancelled"), caller.clone()),
        new_admin.clone(),
    );
}

/// Emitted by `accept_admin`. Topic: `("admin_transfer_accepted", old_admin)`;
/// data: the new admin.
pub fn admin_transfer_accepted(env: &Env, old_admin: &Address, new_admin: &Address) {
    env.events().publish(
        (
            Symbol::new(env, "admin_transfer_accepted"),
            old_admin.clone(),
        ),
        new_admin.clone(),
    );
}

/// Emitted by `set_metadata_key_policy`. Topic:
/// `("metadata_key_policy_set", admin)`; data: the new policy.
pub fn metadata_key_policy_set(env: &Env, admin: &Address, policy: crate::MetadataKeyPolicy) {
    env.events().publish(
        (Symbol::new(env, "metadata_key_policy_set"), admin.clone()),
        policy,
    );
}
