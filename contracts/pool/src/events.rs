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

/// Emitted by `deposit` and `withdraw` upon success: LP address, resulting shares, principal, and ledger time (issue #1292).
pub fn lp_position_checkpoint(env: &Env, lp: &Address, resulting_shares: u128, principal: u128, ledger_time: u64) {
    env.events().publish(
        (Symbol::new(env, "lp_position_checkpoint"), lp.clone()),
        (resulting_shares, principal, ledger_time),
    );
}

pub fn lp_withdrawn(env: &Env, lp: &Address, usdc_amount: u128, shares_burned: u128) {
    env.events().publish(
        (Symbol::new(env, "lp_withdrawn"), lp.clone()),
        (usdc_amount, shares_burned),
    );
}

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

pub fn share_price_updated(env: &Env, invoice_id: &BytesN<32>, old_price: u128, new_price: u128) {
    env.events().publish(
        (Symbol::new(env, "share_price_updated"), invoice_id.clone()),
        (old_price, new_price),
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

pub fn max_loss_threshold_updated(env: &Env, old_threshold_bps: Option<u32>, new_threshold_bps: Option<u32>) {
    env.events().publish(
        (Symbol::new(env, "max_loss_threshold_updated"),),
        (old_threshold_bps, new_threshold_bps),
    );
}

pub fn ownership_transferred(env: &Env, old_admin: &Address, new_admin: &Address) {
    env.events().publish(
        (Symbol::new(env, "ownership_transferred"), old_admin.clone()),
        new_admin.clone(),
    );
}

pub fn paused(env: &Env, admin: &Address) {
    env.events()
        .publish((Symbol::new(env, "paused"), admin.clone()), ());
}

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

pub fn min_discount_updated(env: &Env, old_min_bps: u32, new_min_bps: u32) {
    env.events().publish(
        (Symbol::new(env, "min_discount_updated"),),
        (old_min_bps, new_min_bps),
    );
}

pub fn max_maturity_updated(env: &Env, old_max_seconds: u64, new_max_seconds: u64) {
    env.events().publish(
        (Symbol::new(env, "max_maturity_updated"),),
        (old_max_seconds, new_max_seconds),
    );
}
