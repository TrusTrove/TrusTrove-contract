#![no_std]

use soroban_sdk::{contract, contractimpl, map, panic_with_error, Address, Env, Map, String, Vec};

mod constants;
mod errors;
mod events;
mod test;
mod types;

pub use constants::*;
pub use errors::*;
pub use types::*;

/// Maximum number of entries allowed in a metadata map.
const MAX_METADATA_SIZE: u32 = 20;
/// Maximum length of a single metadata key.
const MAX_METADATA_KEY_LEN: u32 = 64;
/// Maximum length of a single metadata value.
const MAX_METADATA_VALUE_LEN: u32 = 512;

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
    /// * `RegistryError::InvalidMetadata` if `metadata` exceeds
    ///   `MAX_METADATA_SIZE` entries, contains an empty key or value, or has a
    ///   key longer than `MAX_METADATA_KEY_LEN` or a value longer than
    ///   `MAX_METADATA_VALUE_LEN`.
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
        // #130: new profiles start unverified; admin must verify via verify_profile.
        let profile = Profile::new(Role::Issuer, false, env.ledger().timestamp(), metadata);
        let key = DataKey::Profile(address.clone());
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
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

        // Pre-validate ALL entries' metadata before processing any of them.
        // This ensures atomicity: if any entry has invalid metadata, the
        // entire batch is rejected and no entries are persisted (#446).
        for entry in entries.iter() {
            let (_address, metadata) = entry;
            Self::validate_metadata(&env, &metadata);
        }

        let mut skipped: Vec<Address> = Vec::new(&env);
        let mut registered: u32 = 0;
        for entry in entries.iter() {
            let (address, _metadata) = entry;
            let key = DataKey::Profile(address.clone());
            if env.storage().persistent().has(&key) {
                skipped.push_back(address.clone());
                continue;
            }

            // #130: new profiles start unverified; admin must verify via verify_profile.
            let profile = Profile::new(Role::Issuer, false, env.ledger().timestamp(), map![&env]);

            env.storage().persistent().set(&key, &profile);
            env.storage()
                .persistent()
                .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
            events::issuer_registered(&env, &address);
            registered += 1;
        }

        events::batch_registered(&env, registered, skipped.len());

        if registered > 0 {
            Self::extend_instance_ttl(&env);
        }
        skipped
    }

    /// Batch-registers buyer profiles.
    ///
    /// Mirrors [`batch_register_issuers`](Self::batch_register_issuers) but
    /// creates `Role::Buyer` profiles. The admin must be authorized and the
    /// batch size must not exceed 50 entries.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `entries` - A vector of `(Address, Map<String, String>)` pairs, each
    ///   representing a buyer address and its metadata.
    ///
    /// # Auth
    /// * Requires `admin.require_auth()` — only the stored contract admin may
    ///   batch-register buyers.
    ///
    /// # Panics
    /// * `RegistryError::BatchSizeExceeded` if `entries.len() > 50`.
    /// * `RegistryError::NotFound` if the contract admin is not set.
    ///
    /// # Returns
    /// * `Vec<Address>` - The list of addresses that were skipped (already
    ///   registered).
    pub fn batch_register_buyers(
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

            // #130: new profiles start unverified; admin must verify via verify_profile.
            let profile = Profile::new(Role::Buyer, false, env.ledger().timestamp(), metadata);

            env.storage().persistent().set(&key, &profile);
            env.storage()
                .persistent()
                .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
            events::buyer_registered(&env, &address);
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
    /// * `RegistryError::InvalidMetadata` if `metadata` exceeds
    ///   `MAX_METADATA_SIZE` entries, contains an empty key or value, or has a
    ///   key longer than `MAX_METADATA_KEY_LEN` or a value longer than
    ///   `MAX_METADATA_VALUE_LEN`.
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
        // #130: new profiles start unverified; admin must verify via verify_profile.
        let profile = Profile::new(Role::Buyer, false, env.ledger().timestamp(), metadata);
        let key = DataKey::Profile(address.clone());
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
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
    ///   `MAX_METADATA_SIZE` entries, contains an empty key or value, or has a
    ///   key longer than `MAX_METADATA_KEY_LEN` or a value longer than
    ///   `MAX_METADATA_VALUE_LEN`.
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
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
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
    /// * `RegistryError::InvalidMetadata` if `metadata` exceeds
    ///   `MAX_METADATA_SIZE` entries, contains an empty key or value, or has a
    ///   key longer than `MAX_METADATA_KEY_LEN` or a value longer than
    ///   `MAX_METADATA_VALUE_LEN`.
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
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::metadata_updated(&env, &address);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Retrieves a registered profile by address.
    ///
    /// Reads the profile entry from persistent storage, extends its TTL using
    /// the same threshold and target duration as the write path, and returns
    /// a decoded view of the profile with `role`, `verified`, and `revoked`
    /// fields instead of the raw `packed_flags` bit representation.
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
    /// * `ProfileView` - The decoded profile view for the address.
    ///
    /// # Example
    /// ```ignore
    /// let profile = client.get_profile(&issuer);
    /// ```
    pub fn get_profile(env: Env, address: Address) -> ProfileView {
        let key = DataKey::Profile(address.clone());
        let profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        ProfileView::from_profile(&profile)
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
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
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
            Some(p) if p.revoked() => VerificationStatus::Revoked,
            Some(_) => VerificationStatus::Pending,
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
        profile.set_revoked(true);
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
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
    /// * `RegistryError::NotRevoked` if the profile has not been revoked.
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
        if !profile.revoked() {
            panic_with_error!(&env, RegistryError::NotRevoked);
        }
        profile.set_verified(true);
        profile.set_revoked(false);
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
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
        if verify {
            profile.set_revoked(false);
        } else {
            profile.set_revoked(true);
        }
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
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

    /// Transfers contract admin to a new address.
    ///
    /// Unlike `transfer_ownership`, this function only requires auth from the
    /// current admin — the new admin does not need to sign. This is useful
    /// for key rotation scenarios where the current admin key may be
    /// compromised or needs to be rotated without the new key holder's
    /// involvement.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `new_admin` - The address that will become the new contract admin.
    ///
    /// # Auth
    /// * Requires `current_admin.require_auth()` — only the current stored
    ///   contract admin may call this function.
    ///
    /// # Panics
    /// * `RegistryError::NotFound` if the contract has not been initialized
    ///   (no admin is stored under `DataKey::Admin`).
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Example
    /// ```ignore
    /// client.transfer_admin(&new_admin);
    /// ```
    pub fn transfer_admin(env: Env, new_admin: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        events::admin_transferred(&env, &admin, &new_admin);
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
    /// * `RegistryError::NotInitialized` if the admin address is not set
    ///   (contract was never initialized).
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
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotInitialized));
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
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
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    fn validate_metadata(env: &Env, metadata: &Map<String, String>) {
        if metadata.len() > MAX_METADATA_SIZE {
            panic_with_error!(env, RegistryError::InvalidMetadata);
        }
        for (key, value) in metadata.iter() {
            if key.is_empty() || value.is_empty() {
                panic_with_error!(env, RegistryError::InvalidMetadata);
            }
            if key.len() > MAX_METADATA_KEY_LEN || value.len() > MAX_METADATA_VALUE_LEN {
                panic_with_error!(env, RegistryError::InvalidMetadata);
            }
        }
    }
}
