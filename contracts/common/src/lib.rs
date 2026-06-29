#![no_std]

use soroban_sdk::{Env, IntoVal, Val};

pub const DEFAULT_MIN_TTL: u32 = 100;
pub const DEFAULT_MAX_TTL: u32 = 2_000_000;

pub fn persistent_set<K: IntoVal<Env, Val>, V: IntoVal<Env, Val>>(env: &Env, key: &K, val: &V) {
    env.storage().persistent().set(key, val);
    env.storage()
        .persistent()
        .extend_ttl(key, DEFAULT_MIN_TTL, DEFAULT_MAX_TTL);
}
