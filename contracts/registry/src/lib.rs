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
    /// Initializes the registry contract and stores the admin address.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `admin` - The address that will be authorized as contract admin.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Panics
    /// * `AlreadyInitialized` if the contract has already been initialized.
    ///
    /// # Example
    /// ```ignore
    /// client.initialize(&admin);
    /// ```
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, RegistryError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        Self::extend_instance_ttl(&env);
    }

    /// Registers a new issuer profile with initial metadata.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The issuer address to register.
    /// * `metadata` - Profile metadata for the issuer.
    ///
    /// # Returns
    /// * `bool` - `true` when registration succeeds.
    ///
    /// # Panics
    /// * `AlreadyRegistered` if the address is already registered.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.register_issuer(&issuer, &metadata);
    /// ```
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

    /// Batch registers multiple issuer profiles (admin only).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `entries` - A list of (address, metadata) pairs for each issuer.
    ///
    /// # Returns
    /// * `u32` - The number of newly registered issuers.
    ///
    /// # Panics
    /// * `NotFound` if the admin address is not set.
    ///
    /// # Example
    /// ```ignore
    /// let count = client.batch_register_issuers(&entries);
    /// ```
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

    /// Registers a new buyer profile with initial metadata.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The buyer address to register.
    /// * `metadata` - Profile metadata for the buyer.
    ///
    /// # Returns
    /// * `bool` - `true` when registration succeeds.
    ///
    /// # Panics
    /// * `AlreadyRegistered` if the address is already registered.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.register_buyer(&buyer, &metadata);
    /// ```
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

    /// Updates the metadata for an existing registered profile.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The address whose metadata will be updated.
    /// * `metadata` - The new metadata map for the profile.
    ///
    /// # Returns
    /// * `bool` - `true` when metadata is updated successfully.
    ///
    /// # Panics
    /// * `NotFound` if the address is not registered.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.update_metadata(&issuer, &new_metadata);
    /// ```
    pub fn update_metadata(env: Env, address: Address, metadata: Map<String, String>) -> bool {
        address.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        profile.metadata = metadata;
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
        events::metadata_updated(&env, &address);
        true
    }

    /// Retrieves a registered profile by address.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The address of the profile to retrieve.
    ///
    /// # Returns
    /// * `Profile` - The stored profile for the address.
    ///
    /// # Panics
    /// * `NotFound` if the address is not registered.
    ///
    /// # Example
    /// ```ignore
    /// let profile = client.get_profile(&issuer);
    /// ```
    pub fn get_profile(env: Env, address: Address) -> Profile {
        env.storage()
            .persistent()
            .get(&DataKey::Profile(address.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound))
    }

    /// Checks whether a registered profile is verified.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The address to check.
    ///
    /// # Returns
    /// * `bool` - `true` if the address is registered and verified.
    ///
    /// # Example
    /// ```ignore
    /// let verified = client.is_verified(&issuer);
    /// ```
    pub fn is_verified(env: Env, address: Address) -> bool {
        env.storage()
            .persistent()
            .get::<_, Profile>(&DataKey::Profile(address))
            .map(|p| p.verified)
            .unwrap_or(false)
    }

    /// Revokes verification for a registered profile.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The address to revoke.
    ///
    /// # Returns
    /// * `bool` - `true` when revocation succeeds.
    ///
    /// # Panics
    /// * `NotFound` if the admin or the profile address is missing.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.revoke(&address);
    /// ```
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

    /// Verifies or unverifies a profile (admin only).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The address whose profile to verify.
    /// * `verify` - `true` to verify, `false` to unverify.
    ///
    /// # Returns
    /// * `bool` - `true` when the profile is updated.
    ///
    /// # Panics
    /// * `NotFound` if the profile does not exist.
    /// * `NotAuthorized` if caller is not the admin.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.verify_profile(&address, true);
    /// ```
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

    /// Returns the stored contract admin address.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Returns
    /// * `Address` - The stored admin address.
    ///
    /// # Panics
    /// * `NotFound` if the admin address is not set.
    ///
    /// # Example
    /// ```ignore
    /// let admin = client.get_admin();
    /// ```
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
