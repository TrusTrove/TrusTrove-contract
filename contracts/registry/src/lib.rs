#![no_std]

use soroban_sdk::{contract, contractimpl, panic_with_error, Address, Env, Map, String, Vec};
use trusttrove_common::persistent_set;

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
        persistent_set(&env, &key, &profile);
        events::issuer_registered(&env, &address);
        true
    }

    // Returns the list of addresses that were skipped (already registered) so
    // the caller knows exactly which entries were not processed (#66).
    pub fn batch_register_issuers(
        env: Env,
        entries: Vec<(Address, Map<String, String>)>,
    ) -> Vec<Address> {
        if entries.len() > 50 {
            panic_with_error!(&env, RegistryError::BatchSizeExceeded);
        }

        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        admin.require_auth();

        let mut skipped: Vec<Address> = Vec::new(&env);
        let mut registered: u32 = 0;
        for entry in entries.iter() {
            let (address, metadata) = entry;
            let key = DataKey::Profile(address.clone());
            if env.storage().persistent().has(&key) {
                skipped.push_back(address.clone());
                continue;
            }

            let profile = Profile {
                address: address.clone(),
                role: Role::Issuer,
                verified: true,
                registered_at: env.ledger().timestamp(),
                metadata,
            };

            persistent_set(&env, &key, &profile);
            events::issuer_registered(&env, &address);
            registered += 1;
        }

        if registered > 0 {
            Self::extend_instance_ttl(&env);
        }
        skipped
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
        persistent_set(&env, &key, &profile);
        events::buyer_registered(&env, &address);
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
        persistent_set(&env, &key, &profile);
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

    pub fn get_verification_status(env: Env, address: Address) -> VerificationStatus {
        match env
            .storage()
            .persistent()
            .get::<_, Profile>(&DataKey::Profile(address))
        {
            None => VerificationStatus::Unregistered,
            Some(p) if p.verified => VerificationStatus::Verified,
            Some(_) => VerificationStatus::Revoked,
        }
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
        persistent_set(&env, &key, &profile);
        events::address_revoked(&env, &address);
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
        persistent_set(&env, &key, &profile);
        events::profile_verified(&env, &address, verify);
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
    pub fn transfer_ownership(env: Env, new_admin: Address) {
        // Transfers admin ownership to a new address.
        //
        // Requires authentication from BOTH the current admin and the incoming
        // new admin, preventing accidental transfers to wrong addresses.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `new_admin` - The address that will become the new admin.
        //
        // # Panics
        // * `NotFound` if the admin is not set.
        //
        // # Example
        // ```ignore
        // client.transfer_ownership(&new_admin);
        // ```
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        admin.require_auth();
        new_admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        events::ownership_transferred(&env, &admin, &new_admin);
        Self::extend_instance_ttl(&env);
    }

    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound))
    }
}

impl RegistryContract {
    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }
}
