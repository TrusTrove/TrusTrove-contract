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
        env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
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
            env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
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
        env.storage()
            .persistent()
            .get(&DataKey::Profile(address.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound))
    }

    /// Checks whether a registered profile is verified.
    ///
    /// Returns `false` for addresses that have never been registered as well
    /// as for addresses whose profile has had verification revoked.
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
        env.storage()
            .persistent()
            .get::<_, Profile>(&DataKey::Profile(address))
            .map(|p| p.verified())
            .unwrap_or(false)
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

    /// Revokes verification for a registered profile.
    ///
    /// Loads the profile, flips its verified flag to `false`, persists the
    /// change (extending the profile's TTL), and emits an `address_revoked`
    /// event.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The registered address whose verification should be
    ///   revoked.
    ///
    /// # Auth
    /// * Requires `admin.require_auth()` — only the stored contract admin
    ///   (read from `DataKey::Admin`) may revoke a profile.
    ///
    /// # Panics
    /// * `RegistryError::NotFound` if the contract admin is not set (contract
    ///   was never initialized).
    /// * `RegistryError::NotFound` if no profile is stored for `address`.
    ///
    /// # Returns
    /// * `bool` - `true` when the profile is successfully marked as revoked.
    ///
    /// # Example
    /// ```ignore
    /// let ok = client.revoke(&issuer);
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
        profile.set_verified(false);
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
        profile.set_verified(verify);
        env.storage().persistent().set(&key, &profile);
        env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
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
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound))
    }
}

impl RegistryContract {
    fn require_initialized(env: &Env) {
        if !env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, RegistryError::NotInitialized);
        }
    }

    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }
}
