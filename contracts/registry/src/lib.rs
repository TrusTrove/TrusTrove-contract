#![no_std]

use soroban_sdk::{contract, contractimpl, panic_with_error, Address, Env, Map, String, Vec};

mod errors;
mod events;
mod test;
mod types;

pub use errors::*;
pub use types::*;

#[contract]
pub struct RegistryContract;

#[contractimpl]
impl RegistryContract {
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, RegistryError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        Self::extend_instance_ttl(&env);
    }

    pub fn register_issuer(env: Env, address: Address, metadata: Map<String, String>) -> bool {
        address.require_auth();
        if env
            .storage()
            .persistent()
            .has(&DataKey::Profile(address.clone()))
        {
            panic_with_error!(&env, RegistryError::AlreadyRegistered);
        }
        let profile = Profile {
            address: address.clone(),
            role: Role::Issuer,
            verified: true,
            registered_at: env.ledger().timestamp(),
            metadata,
        };
        let key = DataKey::Profile(address.clone());
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
        events::issuer_registered(&env, &address);
        Self::extend_instance_ttl(&env);
        true
    }

    pub fn batch_register_issuers(env: Env, entries: Vec<(Address, Map<String, String>)>) -> u32 {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        admin.require_auth();

        let mut count: u32 = 0;
        for entry in entries.iter() {
            let (address, metadata) = entry;
            let key = DataKey::Profile(address.clone());
            if env.storage().persistent().has(&key) {
                continue;
            }

            let profile = Profile {
                address: address.clone(),
                role: Role::Issuer,
                verified: true,
                registered_at: env.ledger().timestamp(),
                metadata,
            };

            env.storage().persistent().set(&key, &profile);
            env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
            events::issuer_registered(&env, &address);
            count += 1;
        }

        if count > 0 {
            Self::extend_instance_ttl(&env);
        }
        count
    }

    pub fn register_buyer(env: Env, address: Address, metadata: Map<String, String>) -> bool {
        address.require_auth();
        if env
            .storage()
            .persistent()
            .has(&DataKey::Profile(address.clone()))
        {
            panic_with_error!(&env, RegistryError::AlreadyRegistered);
        }
        let profile = Profile {
            address: address.clone(),
            role: Role::Buyer,
            verified: true,
            registered_at: env.ledger().timestamp(),
            metadata,
        };
        let key = DataKey::Profile(address.clone());
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
        events::buyer_registered(&env, &address);
        Self::extend_instance_ttl(&env);
        true
    }

    pub fn get_profile(env: Env, address: Address) -> Profile {
        env.storage()
            .persistent()
            .get(&DataKey::Profile(address.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound))
    }

    pub fn is_verified(env: Env, address: Address) -> bool {
        env.storage()
            .persistent()
            .get::<_, Profile>(&DataKey::Profile(address))
            .map(|p| p.verified)
            .unwrap_or(false)
    }

    pub fn revoke(env: Env, address: Address) -> bool {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        admin.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        profile.verified = false;
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
        events::address_revoked(&env, &address);
        Self::extend_instance_ttl(&env);
        true
    }

    pub fn verify_profile(env: Env, address: Address, verify: bool) -> bool {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        admin.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        profile.verified = verify;
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
        events::profile_verified(&env, &address, verify);
        Self::extend_instance_ttl(&env);
        true
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound))
    }

    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }
}
