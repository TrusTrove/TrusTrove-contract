// soroban-sdk >= 22 deprecates `Events::publish` in favour of the
// `#[contractevent]` macro. The topic tuples below are the documented
// on-wire event format (see docs/EVENTS.md) and consumers index them
// positionally; migrating them is a schema change, not an SDK repair,
// so it is deliberately out of scope for the #826 upgrade.
#![allow(deprecated)]
use soroban_sdk::{Address, BytesN, Env, Symbol};

/// Emitted by `__constructor` when the escrow is first configured (issue
/// #827 moved initialization into the deploy transaction), so indexers
/// following escrow deployments can see the admin, pool and funding asset
/// from the event alone. Matches the `contract_initialized` convention used
/// by the invoice and registry contracts (#867).
pub fn contract_initialized(
    env: &Env,
    admin: &Address,
    pool_contract: &Address,
    usdc_asset: &Address,
) {
    env.events().publish(
        (
            Symbol::new(env, "contract_initialized"),
            admin.clone(),
            pool_contract.clone(),
            usdc_asset.clone(),
        ),
        (),
    );
}

pub fn upgraded(env: &Env, wasm_hash: &BytesN<32>) {
    env.events()
        .publish((Symbol::new(env, "upgraded"), wasm_hash.clone()), ());
}

pub fn funds_locked(env: &Env, invoice_id: &BytesN<32>, issuer: &Address, amount: u128) {
    env.events().publish(
        (
            Symbol::new(env, "funds_locked"),
            invoice_id.clone(),
            issuer.clone(),
        ),
        amount,
    );
}

pub fn released_to_issuer(env: &Env, invoice_id: &BytesN<32>, issuer: &Address, amount: u128) {
    env.events().publish(
        (
            Symbol::new(env, "released_to_issuer"),
            invoice_id.clone(),
            issuer.clone(),
        ),
        amount,
    );
}

pub fn released_to_pool(env: &Env, invoice_id: &BytesN<32>, pool: &Address, amount: u128) {
    env.events().publish(
        (
            Symbol::new(env, "released_to_pool"),
            invoice_id.clone(),
            pool.clone(),
        ),
        amount,
    );
}

pub fn default_resolved(
    env: &Env,
    invoice_id: &BytesN<32>,
    pool: &Address,
    caller: &Address,
    amount: u128,
) {
    env.events().publish(
        (
            Symbol::new(env, "default_resolved"),
            invoice_id.clone(),
            pool.clone(),
            caller.clone(),
        ),
        amount,
    );
}

/// Emitted by `set_timeout_window` when the admin updates the timeout window.
pub fn timeout_window_set(env: &Env, window: u64) {
    env.events()
        .publish((Symbol::new(env, "timeout_window_set"),), window);
}

/// Emitted by `set_invoice_contract` when the admin pins the invoice
/// contract reference that `release_to_pool` validates against (#997).
/// `admin` is indexed; the data is the pinned invoice contract address.
pub fn invoice_contract_set(env: &Env, admin: &Address, invoice_contract: &Address) {
    env.events().publish(
        (Symbol::new(env, "invoice_contract_set"), admin.clone()),
        invoice_contract.clone(),
    );
}

/// Emitted by `release_after_timeout` when a stale lock is returned to the pool.
pub fn timeout_released(env: &Env, invoice_id: &BytesN<32>, pool: &Address, amount: u128) {
    env.events().publish(
        (
            Symbol::new(env, "timeout_released"),
            invoice_id.clone(),
            pool.clone(),
        ),
        amount,
    );
}

#[allow(dead_code)]
pub fn ownership_transferred(env: &Env, old_admin: &Address, new_admin: &Address) {
    env.events().publish(
        (Symbol::new(env, "ownership_transferred"), old_admin.clone()),
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
