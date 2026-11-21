// soroban-sdk >= 22 deprecates `Events::publish` in favour of the
// `#[contractevent]` macro. The topic tuples below are the documented
// on-wire event format (see docs/EVENTS.md) and consumers index them
// positionally; migrating them is a schema change, not an SDK repair,
// so it is deliberately out of scope for the #826 upgrade.
#![allow(deprecated)]
use soroban_sdk::{Address, BytesN, Env, Symbol};

/// Emitted by `upgrade` when the pool contract WASM bytecode is updated.
pub fn upgraded(env: &Env, wasm_hash: &BytesN<32>) {
    env.events()
        .publish((Symbol::new(env, "upgraded"), wasm_hash.clone()), ());
}

/// Emitted by `__constructor` when the pool contract is deployed and initialized.
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

/// Emitted by `deposit` when an LP deposits USDC and receives pool shares.
pub fn lp_deposited(env: &Env, lp: &Address, usdc_amount: u128, shares_issued: u128) {
    env.events().publish(
        (Symbol::new(env, "lp_deposited"), lp.clone()),
        (usdc_amount, shares_issued),
    );
}

/// Emitted by `withdraw` when an LP redeems pool shares for USDC.
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

/// Emitted by `fund_invoice` when the pool funds an invoice.
pub fn invoice_funded(env: &Env, invoice_id: &BytesN<32>, funded_amount: u128) {
    env.events().publish(
        (Symbol::new(env, "invoice_funded"), invoice_id.clone()),
        funded_amount,
    );
}

/// Emitted by `settle_repayment` during repayment processing (`receive_repayment`
/// and `receive_repayment_with_refund`). Topics index the event name and the
/// repaid `invoice_id`. Data payload is a 3-tuple `(amount, lp_yield, protocol_cut)`:
/// - `amount`: Total repaid face value / payment amount in USDC stroops.
/// - `lp_yield`: Yield distributed to liquidity providers (`TotalDeposits` and `TotalYieldDistributed`).
/// - `protocol_cut`: Treasury cut routed to the protocol fee recipient (0 if fee is disabled).
///
/// Note: Defaults do NOT emit this event; `handle_default` emits `invoice_defaulted` instead.
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

/// Emitted by `handle_default` when an invoice defaults. Topics index the event name
/// and the defaulted `invoice_id`. Data payload is `loss_amount`: the defaulted
/// funded principal in USDC stroops.
///
/// Note that this payload represents the invoice's defaulted funded principal and
/// is not necessarily an LP capital loss:
/// - **Unrecoverable Loss Path (`escrow_released == false`):** When the principal
///   had already been released to the issuer at funding time, escrow recovers nothing
///   on default. `TotalDeposits` shrinks by `loss_amount` so LPs absorb the loss as a
///   lower share price.
/// - **Recovered Principal Path (`escrow_released == true`):** When escrow still held
///   and returned the principal (e.g. pre-#819 locks or unreleased escrow), `TotalDeposits`
///   is deliberately left unchanged to preserve LP capital and share value (issue #439),
///   while the event still emits the defaulted funded principal.
pub fn invoice_defaulted(env: &Env, invoice_id: &BytesN<32>, loss_amount: u128) {
    env.events().publish(
        (Symbol::new(env, "invoice_defaulted"), invoice_id.clone()),
        loss_amount,
    );
}

/// Emitted by `set_max_utilization` when the admin updates the pool utilization cap.
///
/// Topic: `max_utilization_updated`; data: `(old_cap_bps, new_cap_bps)`.
pub fn max_utilization_updated(env: &Env, old_cap_bps: u32, new_cap_bps: u32) {
    env.events().publish(
        (Symbol::new(env, "max_utilization_updated"),),
        (old_cap_bps, new_cap_bps),
    );
}

/// Emitted by `transfer_ownership` when the pool admin role is transferred.
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

/// Emitted by `set_protocol_fee` when the admin updates the protocol fee rate or treasury.
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
