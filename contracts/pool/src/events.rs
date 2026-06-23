use soroban_sdk::{Address, BytesN, Env, Symbol};

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

pub fn invoice_funded(env: &Env, invoice_id: &BytesN<32>, funded_amount: u128) {
    env.events().publish(
        (Symbol::new(env, "invoice_funded"), invoice_id.clone()),
        funded_amount,
    );
}

pub fn repayment_received(env: &Env, invoice_id: &BytesN<32>, amount: u128, yield_amount: u128) {
    env.events().publish(
        (Symbol::new(env, "repayment_received"), invoice_id.clone()),
        (amount, yield_amount),
    );
}

pub fn invoice_defaulted(env: &Env, invoice_id: &BytesN<32>, loss_amount: u128) {
    env.events().publish(
        (Symbol::new(env, "invoice_defaulted"), invoice_id.clone()),
        loss_amount,
    );
}

pub fn yield_distributed(
    env: &Env,
    lp: &Address,
    invoice_id: &BytesN<32>,
    yield_amount: u128,
    share_bps: u32,
) {
    env.events().publish(
        (
            Symbol::new(env, "yield_distributed"),
            lp.clone(),
            invoice_id.clone(),
        ),
        (yield_amount, share_bps),
    );
}
