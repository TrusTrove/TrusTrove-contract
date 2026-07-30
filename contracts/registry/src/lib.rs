#![no_std]

use soroban_sdk::{contract, contractimpl, panic_with_error, Address, Env, Map, String, Vec};

mod errors;
mod events;
mod test;
mod ttl;
mod types;

use ttl::{EXTEND_TO, THRESHOLD};

pub use errors::*;
pub use types::*;

/// Maximum number of entries allowed in a metadata map.
const MAX_METADATA_SIZE: u32 = 20;

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
    /// # Auth
    /// * Requires `admin.require_auth()` — the incoming admin must sign the
    ///   initialization call so ownership cannot be assigned to an address
    ///   the caller does not control.
    ///
    /// # Panics
    /// * `RegistryError::AlreadyInitialized` if the contract has already been
    ///   initialized (an admin is already stored under `DataKey::Admin`).
    ///
    /// # Returns
    /// * `()` - No value is returned.
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
        events::contract_initialized(&env, &admin);
        Self::extend_instance_ttl(&env);
    }

    /// Registers a new issuer profile with initial metadata.
    ///
    /// The profile is stored under `DataKey::Profile(address)` in persistent
    /// storage with its TTL extended, and an `issuer_registered` event is
    /// emitted on success.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The issuer address to register.
    /// * `metadata` - Profile metadata for the issuer.
    ///
    /// # Auth
    /// * Requires `address.require_auth()` — the issuer being registered
    ///   must sign the call, so accounts cannot be enrolled without consent.
    ///
    /// # Panics
    /// * `RegistryError::NotInitialized` if the contract has not been initialized.
    /// * `RegistryError::InvalidMetadata` if `metadata` exceeds `MAX_METADATA_SIZE`
    ///   entries or contains an empty key or value.
    /// * `RegistryError::AlreadyRegistered` if a profile is already stored
    ///   for `address`.
    ///
    /// # Returns
    /// * `bool` - `true` when registration succeeds.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.register_issuer(&issuer, &metadata);
    /// ```
    pub fn register_issuer(env: Env, address: Address, metadata: Map<String, String>) -> bool {
        Self::require_initialized(&env);
        Self::validate_metadata(&env, &metadata);
        address.require_auth();
        if env
            .storage()
            .persistent()
            .has(&DataKey::Profile(address.clone()))
        {
            panic_with_error!(&env, RegistryError::AlreadyRegistered);
        }
        let profile = Profile::new(
            address.clone(),
            Role::Issuer,
            true,
            env.ledger().timestamp(),
            metadata,
        );
        let key = DataKey::Profile(address.clone());
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, THRESHOLD, EXTEND_TO);
        events::issuer_registered(&env, &address);
        Self::extend_instance_ttl(&env);
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
            Self::validate_metadata(&env, &metadata);
            let key = DataKey::Profile(address.clone());
            if env.storage().persistent().has(&key) {
                skipped.push_back(address.clone());
                continue;
            }

            let profile = Profile::new(
                address.clone(),
                Role::Issuer,
                true,
                env.ledger().timestamp(),
                metadata,
            );

            env.storage().persistent().set(&key, &profile);
            env.storage().persistent().extend_ttl(&key, THRESHOLD, EXTEND_TO);
            events::issuer_registered(&env, &address);
            registered += 1;
        }

        events::batch_registered(&env, registered, skipped.len());

        if registered > 0 {
            Self::extend_instance_ttl(&env);
        }
        skipped
    }

    /// Registers a new buyer profile with initial metadata.
    ///
    /// The profile is stored under `DataKey::Profile(address)` in persistent
    /// storage with its TTL extended, and a `buyer_registered` event is
    /// emitted on success.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The buyer address to register.
    /// * `metadata` - Profile metadata for the buyer.
    ///
    /// # Auth
    /// * Requires `address.require_auth()` — the buyer being registered must
    ///   sign the call, so accounts cannot be enrolled without consent.
    ///
    /// # Panics
    /// * `RegistryError::NotInitialized` if the contract has not been initialized.
    /// * `RegistryError::InvalidMetadata` if `metadata` exceeds `MAX_METADATA_SIZE`
    ///   entries or contains an empty key or value.
    /// * `RegistryError::AlreadyRegistered` if a profile is already stored
    ///   for `address`.
    ///
    /// # Returns
    /// * `bool` - `true` when registration succeeds.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.register_buyer(&buyer, &metadata);
    /// ```
    pub fn register_buyer(env: Env, address: Address, metadata: Map<String, String>) -> bool {
        Self::require_initialized(&env);
        Self::validate_metadata(&env, &metadata);
        address.require_auth();
        if env
            .storage()
            .persistent()
            .has(&DataKey::Profile(address.clone()))
        {
            panic_with_error!(&env, RegistryError::AlreadyRegistered);
        }
        let profile = Profile::new(
            address.clone(),
            Role::Buyer,
            true,
            env.ledger().timestamp(),
            metadata,
        );
        let key = DataKey::Profile(address.clone());
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, THRESHOLD, EXTEND_TO);
        events::buyer_registered(&env, &address);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Updates the metadata for an existing registered profile owned by
    /// `address`.
    ///
    /// Both issuer and buyer profiles can be updated through this single
    /// function — they share the same storage key (`DataKey::Profile`).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The profile owner whose metadata should be replaced.
    /// * `metadata` - The new metadata map for the profile.
    ///
    /// # Auth
    /// * Requires `address.require_auth()` — only the profile owner may
    ///   update their own metadata.
    ///
    /// # Panics
    /// * `RegistryError::InvalidMetadata` if `metadata` exceeds
    ///   `MAX_METADATA_SIZE` entries or contains an empty key or value.
    /// * `RegistryError::NotRegistered` if no profile exists for `address`.
    ///
    /// # Returns
    /// * `bool` - `true` when the metadata is successfully updated.
    ///
    /// # Example
    /// ```ignore
    /// let ok = client.update_profile(&issuer, &new_metadata);
    /// ```
    pub fn update_profile(env: Env, address: Address, metadata: Map<String, String>) -> bool {
        Self::validate_metadata(&env, &metadata);
        address.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotRegistered));
        profile.metadata = metadata;
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, THRESHOLD, EXTEND_TO);
        events::profile_updated(&env, &address);
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
    /// * `RegistryError::InvalidMetadata` if `metadata` exceeds `MAX_METADATA_SIZE`
    ///   entries or contains an empty key or value.
    /// * `RegistryError::NotFound` if the address is not registered.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.update_metadata(&issuer, &new_metadata);
    /// ```
    pub fn update_metadata(env: Env, address: Address, metadata: Map<String, String>) -> bool {
        Self::validate_metadata(&env, &metadata);
        address.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        profile.metadata = metadata;
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, THRESHOLD, EXTEND_TO);
        events::metadata_updated(&env, &address);
        true
    }

    /// Retrieves a registered profile by address.
    ///
    /// Reads the profile entry from persistent storage, extends its TTL using
    /// the same threshold and target duration as the write path, and returns
    /// the stored value.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The address of the profile to retrieve.
    ///
    /// # Auth
    /// * No `require_auth()` call is made — this is a read-only view.
    ///
    /// # Panics
    /// * `RegistryError::NotFound` if no profile is stored for `address`.
    ///
    /// # Returns
    /// * `Profile` - The stored profile for the address.
    ///
    /// # Example
    /// ```ignore
    /// let profile = client.get_profile(&issuer);
    /// ```
    pub fn get_profile(env: Env, address: Address) -> Profile {
        let key = DataKey::Profile(address.clone());
        let profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        env.storage().persistent().extend_ttl(&key, THRESHOLD, EXTEND_TO);
        profile
    }

    /// Checks whether a registered profile is verified.
    ///
    /// Returns `false` for addresses that have never been registered as well
    /// as for addresses whose profile has had verification revoked. When the
    /// entry exists, its TTL is extended using the same threshold and target
    /// duration as the write path.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The address to check.
    ///
    /// # Auth
    /// * No `require_auth()` call is made — this is a read-only view.
    ///
    /// # Panics
    /// * This function does not panic; missing profiles are reported as
    ///   `false` rather than as an error.
    ///
    /// # Returns
    /// * `bool` - `true` if the address is registered and verified.
    ///
    /// # Example
    /// ```ignore
    /// let verified = client.is_verified(&issuer);
    /// ```
    pub fn is_verified(env: Env, address: Address) -> bool {
        let key = DataKey::Profile(address);
        match env.storage().persistent().get::<_, Profile>(&key) {
            Some(profile) => {
                let verified = profile.verified();
                env.storage().persistent().extend_ttl(&key, THRESHOLD, EXTEND_TO);
                verified
            }
            None => false,
        }
    }

    pub fn get_verification_status(env: Env, address: Address) -> VerificationStatus {
        match env
            .storage()
            .persistent()
            .get::<_, Profile>(&DataKey::Profile(address))
        {
            None => VerificationStatus::Unregistered,
            Some(p) if p.verified() => VerificationStatus::Verified,
            Some(_) => VerificationStatus::Revoked,
        }
    }

    /// Revokes a registered profile by setting its verification status to `false`.
    ///
    /// This function is idempotent: calling it on an already-revoked profile
    /// returns `true` without re-emitting the `address_revoked` event or
    /// rewriting storage.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The address of the profile to revoke.
    ///
    /// # Auth
    /// * Requires admin authorization (the stored admin from `DataKey::Admin`).
    ///
    /// # Returns
    /// * `true` if the profile is now in revoked state (including if it was
    ///   already revoked).
    ///
    /// # Panics
    /// * `NotFound` if the address is not registered.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.revoke(&issuer);
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
        if !profile.verified() {
            return true;
        }
        profile.set_verified(false);
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, THRESHOLD, EXTEND_TO);
        events::address_revoked(&env, &address);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Reinstates verification for a previously revoked profile.
    ///
    /// This is the admin-only inverse of `revoke`: it flips the profile's
    /// `verified` flag back to `true` and emits a dedicated
    /// `address_reinstated` event so integrators can distinguish
    /// reinstatement from a generic verification toggle.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The registered address whose verification should be
    ///   reinstated.
    ///
    /// # Auth
    /// * Requires `admin.require_auth()` — only the stored contract admin
    ///   (read from `DataKey::Admin`) may reinstate a profile.
    ///
    /// # Panics
    /// * `RegistryError::NotFound` if the contract admin is not set (contract
    ///   was never initialized).
    /// * `RegistryError::NotFound` if no profile is stored for `address`.
    ///
    /// # Returns
    /// * `bool` - `true` when the profile is successfully reinstated.
    ///
    /// # Example
    /// ```ignore
    /// let ok = client.reinstate(&issuer);
    /// ```
    pub fn reinstate(env: Env, address: Address) -> bool {
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
        profile.set_verified(true);
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, THRESHOLD, EXTEND_TO);
        events::address_reinstated(&env, &address);
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
        profile.set_verified(verify);
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, THRESHOLD, EXTEND_TO);
        events::profile_verified(&env, &address, verify);
        Self::extend_instance_ttl(&env);
        true
    }

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

    /// Returns the stored contract admin address.
    ///
    /// Reads the admin entry from instance storage, extends its TTL using
    /// the same threshold and target duration as the write path, and returns
    /// the stored value.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// * No `require_auth()` call is made — this is a read-only view.
    ///
    /// # Panics
    /// * `RegistryError::NotFound` if the admin address is not set (contract
    ///   was never initialized).
    ///
    /// # Returns
    /// * `Address` - The stored admin address.
    ///
    /// # Example
    /// ```ignore
    /// let admin = client.get_admin();
    /// ```
    pub fn get_admin(env: Env) -> Address {
        let admin = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        env.storage().instance().extend_ttl(THRESHOLD, EXTEND_TO);
        admin
    }
}

impl RegistryContract {
    fn require_initialized(env: &Env) {
        if !env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, RegistryError::NotInitialized);
        }
    }

    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(THRESHOLD, EXTEND_TO);
    }

    fn validate_metadata(env: &Env, metadata: &Map<String, String>) {
        if metadata.len() > MAX_METADATA_SIZE {
            panic_with_error!(env, RegistryError::InvalidMetadata);
        }
        for key in metadata.keys().iter() {
            if key.is_empty() {
                panic_with_error!(env, RegistryError::InvalidMetadata);
            }
            if let Some(value) = metadata.get(key) {
                if value.is_empty() {
                    panic_with_error!(env, RegistryError::InvalidMetadata);
                }
            }
        }
    }
}
