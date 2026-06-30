use soroban_sdk::{Address, BytesN, Env, Symbol};

pub fn funds_locked(env: &Env, invoice_id: &BytesN<32>, amount: u128) {
    env.events().publish(
        (Symbol::new(env, "funds_locked"), invoice_id.clone()),
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

pub fn default_resolved(env: &Env, invoice_id: &BytesN<32>, pool: &Address, amount: u128) {
    env.events().publish(
        (
            Symbol::new(env, "default_resolved"),
            invoice_id.clone(),
            pool.clone(),
        ),
        amount,
    );
}

pub fn ownership_transferred(env: &Env, old_admin: &Address, new_admin: &Address) {
    env.events().publish(
        (Symbol::new(env, "ownership_transferred"), old_admin.clone()),
        new_admin.clone(),
    );
}
