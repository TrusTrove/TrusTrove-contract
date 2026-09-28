use soroban_sdk::{Address, Env, Symbol};

/// Published after `register_asset` deploys and initializes a new pool
/// instance, so off-chain indexers (e.g. TrusTrove-app) learn about the new
/// instance the same way they learn about other state-changing calls via
/// `contracts/pool/src/events.rs`.
pub fn pool_instance_created(env: &Env, asset: &Address, pool_address: &Address) {
    env.events().publish(
        (Symbol::new(env, "pool_instance_created"), asset.clone()),
        pool_address.clone(),
    );
}
