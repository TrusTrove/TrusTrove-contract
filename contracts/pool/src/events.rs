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

pub fn pool_initialized(
    env: &Env,
    admin: &Address,
    invoice_contract: &Address,
    escrow_contract: &Address,
    usdc_asset: &Address,
) {
    env.events().publish(
        (
            Symbol::new(env, "pool_initialized"),
            admin.clone(),
            invoice_contract.clone(),
            escrow_contract.clone(),
            usdc_asset.clone(),
        ),
        (),
    );
}

pub fn lp_deposited(env: &Env, lp: &Address, usdc_amount: u128, shares_issued: u128) {
    env.events().publish(
        (Symbol::new(env, "lp_deposited"), lp.clone()),
        (usdc_amount, shares_issued),
    );
}

pub fn lp_withdrawn(env: &Env, lp: &Address, usdc_amount: u128, shares_burned: u128) {
    env.events().publish(
        (Symbol::new(env, "lp_withdrawn"), lp.clone()),
        (usdc_amount, shares_burned),
    );
}

/// Emitted by both SEP-41 share-movement entry points — `transfer` and
/// `transfer_from` — using the standard `transfer(from, to, amount)` data
/// shape, so generic token indexers can watch share movements the same way
/// they watch any SEP-41 token.
pub fn shares_transferred(env: &Env, from: &Address, to: &Address, amount: i128) {
    env.events().publish(
        (Symbol::new(env, "transfer"), from.clone()),
        (to.clone(), amount),
    );
}

pub fn invoice_funded(env: &Env, invoice_id: &BytesN<32>, funded_amount: u128) {
    env.events().publish(
        (Symbol::new(env, "invoice_funded"), invoice_id.clone()),
        funded_amount,
    );
}

pub fn repayment_received(
    env: &Env,
    invoice_id: &BytesN<32>,
    amount: u128,
    lp_yield: u128,
    protocol_cut: u128,
) {
    env.events().publish(
        (Symbol::new(env, "repayment_received"), invoice_id.clone()),
        (amount, lp_yield, protocol_cut),
    );
}

pub fn invoice_defaulted(env: &Env, invoice_id: &BytesN<32>, loss_amount: u128) {
    env.events().publish(
        (Symbol::new(env, "invoice_defaulted"), invoice_id.clone()),
        loss_amount,
    );
}

pub fn max_utilization_updated(env: &Env, old_cap_bps: u32, new_cap_bps: u32) {
    env.events().publish(
        (Symbol::new(env, "max_utilization_updated"),),
        (old_cap_bps, new_cap_bps),
    );
}

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

pub fn protocol_fee_updated(env: &Env, old_fee_bps: u32, new_fee_bps: u32, treasury: &Address) {
    env.events().publish(
        (Symbol::new(env, "protocol_fee_updated"),),
        (old_fee_bps, new_fee_bps, treasury.clone()),
    );
}

/// Emitted by `approve` whenever an LP's spending grant changes, so a spender
/// (and any indexer watching LP positions) can observe pre-authorized share
/// movement without polling `allowance`. `amount` is the new total grant, not a
/// delta, matching SEP-41's `approve` semantics.
pub fn allowance_approved(
    env: &Env,
    from: &Address,
    spender: &Address,
    amount: i128,
    expiration_ledger: u32,
) {
    env.events().publish(
        (Symbol::new(env, "allowance_approved"), from.clone()),
        (spender.clone(), amount, expiration_ledger),
    );
}

/// Emitted by `set_min_discount` when the admin changes the pool's minimum
/// discount floor (issue #829), mirroring `protocol_fee_updated`: the old
/// value is included so an indexer can report what changed rather than only
/// what it changed to. A rejected change emits nothing.
pub fn min_discount_updated(env: &Env, old_min_bps: u32, new_min_bps: u32) {
    env.events().publish(
        (Symbol::new(env, "min_discount_updated"),),
        (old_min_bps, new_min_bps),
    );
}
