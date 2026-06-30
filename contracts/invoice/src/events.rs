use soroban_sdk::{Address, BytesN, Env, Symbol};

pub fn invoice_created(
    env: &Env,
    invoice_id: &BytesN<32>,
    issuer: &Address,
    buyer: &Address,
    face_value: u128,
    funding_asset: &Address,
) {
    env.events().publish(
        (
            Symbol::new(env, "invoice_created"),
            invoice_id.clone(),
            issuer.clone(),
            buyer.clone(),
            funding_asset.clone(),
        ),
        face_value,
    );
}

pub fn invoice_listed(env: &Env, invoice_id: &BytesN<32>, discount_bps: u32) {
    env.events().publish(
        (Symbol::new(env, "invoice_listed"), invoice_id.clone()),
        discount_bps,
    );
}

pub fn invoice_funded(env: &Env, invoice_id: &BytesN<32>, funded_amount: u128) {
    env.events().publish(
        (Symbol::new(env, "invoice_funded"), invoice_id.clone()),
        funded_amount,
    );
}

pub fn invoice_shipped(env: &Env, invoice_id: &BytesN<32>) {
    env.events().publish(
        (Symbol::new(env, "invoice_shipped"), invoice_id.clone()),
        (),
    );
}

pub fn delivery_confirmed(env: &Env, invoice_id: &BytesN<32>, confirmer: &Address) {
    env.events().publish(
        (
            Symbol::new(env, "delivery_confirmed"),
            invoice_id.clone(),
            confirmer.clone(),
        ),
        (),
    );
}

pub fn both_confirmed(env: &Env, invoice_id: &BytesN<32>) {
    env.events()
        .publish((Symbol::new(env, "both_confirmed"), invoice_id.clone()), ());
}

pub fn invoice_repaid(env: &Env, invoice_id: &BytesN<32>, amount: u128) {
    env.events().publish(
        (Symbol::new(env, "invoice_repaid"), invoice_id.clone()),
        amount,
    );
}

pub fn invoice_defaulted(env: &Env, invoice_id: &BytesN<32>) {
    env.events().publish(
        (Symbol::new(env, "invoice_defaulted"), invoice_id.clone()),
        (),
    );
}

pub fn invoice_expired(env: &Env, invoice_id: &BytesN<32>) {
    env.events().publish(
        (Symbol::new(env, "invoice_expired"), invoice_id.clone()),
        (),
    );
}

pub fn pool_contract_set(env: &Env, new_pool: &Address) {
    env.events().publish(
        (Symbol::new(env, "pool_contract_set"), new_pool.clone()),
        (),
    );
}

pub fn expiry_window_set(env: &Env, window: u64) {
    env.events()
        .publish((Symbol::new(env, "expiry_window_set"),), window);
}

#[allow(dead_code)]
pub fn delivery_unconfirmed(env: &Env, invoice_id: &BytesN<32>) {
    env.events().publish(
        (Symbol::new(env, "delivery_unconfirmed"), invoice_id.clone()),
        (),
    );
}

pub fn ownership_transferred(env: &Env, old_admin: &Address, new_admin: &Address) {
    env.events().publish(
        (Symbol::new(env, "ownership_transferred"), old_admin.clone()),
        new_admin.clone(),
    );
}
