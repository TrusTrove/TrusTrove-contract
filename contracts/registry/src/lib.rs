#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, Address, BytesN, Env, Map, String, Vec,
};
use trusttrove_pause::{require_not_paused, set_paused};

mod constants;
mod errors;
mod events;
mod test;
#[cfg(test)]
#[path = "tests/governance.rs"]
mod test_governance;
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
/// Maximum number of addresses a single `list_profiles` call may return.
///
/// Mirrors the 50-entry cap enforced on `batch_register_issuers` /
/// `batch_register_buyers`, so enumeration stays bounded per transaction no
/// matter how many profiles a role accumulates. Callers page through the index
/// with `start` / `limit` instead of asking for everything at once.
const MAX_LIST_LIMIT: u32 = 50;
/// Minimum delay, in seconds of ledger time, between `propose_admin` and the
/// earliest `accept_admin` (#1124). One day gives operators time to notice a
/// proposal made with a compromised key and cancel it (or have the guardian
/// freeze admin writes) before it can take effect.
pub const MIN_ADMIN_TRANSFER_DELAY: u64 = 86_400;
/// Metadata keys accepted under [`MetadataKeyPolicy::AllowlistV1`] (#1120).
///
/// | Key | Intended content |
/// |---|---|
/// | `name` | Display / trading name |
/// | `legal_name` | Registered legal entity name |
/// | `email` | Business contact email |
/// | `phone` | Business contact phone number |
/// | `website` | Public website URL |
/// | `country` | ISO 3166-1 alpha-2 country code |
/// | `jurisdiction` | Jurisdiction of incorporation |
/// | `registration_number` | Company registration number |
/// | `tax_id` | Tax identifier |
/// | `industry` | Industry / sector |
/// | `description` | Free-text business description |
/// | `logo_url` | Logo image URL |
///
/// This list is frozen for V1. Supporting another key means adding a new
/// policy version, never editing this array.
pub const METADATA_KEYS_V1: [&str; 12] = [
    "name",
    "legal_name",
    "email",
    "phone",
    "website",
    "country",
    "jurisdiction",
    "registration_number",
    "tax_id",
    "industry",
    "description",
    "logo_url",
];

#[contract]
pub struct RegistryContract;

#[contractimpl]
impl RegistryContract {
    /// Constructs the registry contract and stores the admin address.
    ///
    /// Runs atomically at deploy time (Soroban protocol constructor, #827):
    /// there is no window in which the instance exists without a stored
    /// admin, so no one can front-run initialization and claim ownership.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `admin` - The address that will be authorized as contract admin.
    ///
    /// # Auth
    /// * Requires `admin.require_auth()` — the incoming admin must sign the
    ///   deployment so ownership cannot be assigned to an address
    ///   the caller does not control.
    ///
    /// # Panics
    /// * Any panic aborts the deployment transaction atomically, so a
    ///   registry that ends up on-chain is always fully constructed.
    ///   (`RegistryError::AlreadyInitialized` is unreachable from this
    ///   entry point and kept only for error-code stability.)
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Example
    /// ```ignore
    /// // Deploy-and-construct in a single operation:
    /// // env.register(RegistryContract, (admin,))
    /// ```
    pub fn __constructor(env: Env, admin: Address) {
        require_not_paused(&env);
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        events::contract_initialized(&env, &admin);
        Self::extend_instance_ttl(&env);
    }

    /// Replaces this contract's Wasm with an installed Wasm using the stored admin.
    ///
    /// The contract address and storage are preserved. Emits an `upgraded`
    /// event carrying the new Wasm hash.
    ///
    /// # Panics
    /// * `RegistryError::NotInitialized` if no admin is stored.
    /// * `RegistryError::AdminFrozen` while the guardian has frozen admin
    ///   writes (#1125).
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        Self::require_not_frozen(&env);
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotInitialized));
        admin.require_auth();

        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());
        Self::extend_instance_ttl(&env);
        events::upgraded(&env, &new_wasm_hash);
    }

    /// Registers a new issuer profile with initial metadata.
    ///
    /// The profile is stored under `DataKey::Profile(address)` in persistent
    /// storage with its TTL extended, the address is appended to the issuer
    /// enumeration index (`DataKey::ProfileIndex(Role::Issuer, n)` /
    /// `DataKey::ProfileCount(Role::Issuer)`), and an `issuer_registered` event
    /// is emitted on success.
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
    /// * `RegistryError::UnknownMetadataKey` if a key is not allowed by the
    ///   active `MetadataKeyPolicy` (#1120).
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
        require_not_paused(&env);
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
        Self::index_profile(&env, &Role::Issuer, &address);
        events::issuer_registered(&env, &address);
        Self::bump_profile_version(&env, &address);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Batch-registers issuer profiles.
    ///
    /// Each input is an `(Address, Map<String, String>)` pair. At most 50
    /// entries are accepted per call; an empty batch is valid. Only the stored
    /// admin may call this method, and the admin must authorize the call.
    ///
    /// Every metadata map is validated before any profile is created. A map
    /// may contain at most 20 entries; keys and values must be nonempty, keys
    /// may be at most 64 characters, and values may be at most 512 characters.
    /// Validated metadata maps are stored on newly created profiles.
    ///
    /// Addresses already registered before the call are skipped. If an
    /// address occurs more than once in the batch, its first occurrence is
    /// registered and later occurrences are skipped. The returned vector
    /// contains skipped addresses in input encounter order.
    ///
    /// # Panics
    /// * `RegistryError::BatchSizeExceeded` if `entries.len() > 50`.
    /// * `RegistryError::NotInitialized` if the contract has not been
    ///   initialized.
    /// * `RegistryError::InvalidMetadata` if any supplied metadata map is
    ///   invalid. Validation happens before registration, so no profiles are
    ///   created when any map is invalid.
    ///
    /// # Returns
    /// * `Vec<Address>` - Addresses skipped because they were already
    ///   registered, in input encounter order.
    ///
    /// # Example
    /// ```ignore
    /// let entries = vec![
    ///     &env,
    ///     (existing.clone(), map![&env]),
    ///     (new_issuer.clone(), map![&env]),
    ///     (new_issuer.clone(), map![&env]),
    /// ];
    /// let skipped: Vec<Address> = client.batch_register_issuers(&entries);
    /// assert_eq!(skipped, vec![&env, existing, new_issuer]);
    /// ```
    ///
    /// Only newly created profiles are appended to the issuer enumeration
    /// index; skipped addresses are never indexed twice.
    pub fn batch_register_issuers(
        env: Env,
        entries: Vec<(Address, Map<String, String>)>,
    ) -> Vec<Address> {
        require_not_paused(&env);
        if entries.len() > 50 {
            panic_with_error!(&env, RegistryError::BatchSizeExceeded);
        }

        let admin = Self::require_admin_unfrozen(&env);
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
            let (address, metadata) = entry;
            let key = DataKey::Profile(address.clone());
            if env.storage().persistent().has(&key) {
                skipped.push_back(address.clone());
                continue;
            }

            // #130: new profiles start unverified; admin must verify via verify_profile.
            let profile = Profile::new(Role::Issuer, false, env.ledger().timestamp(), metadata);

            env.storage().persistent().set(&key, &profile);
            env.storage()
                .persistent()
                .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
            Self::index_profile(&env, &Role::Issuer, &address);
            events::issuer_registered(&env, &address);
            Self::bump_profile_version(&env, &address);
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
    /// * `RegistryError::NotInitialized` if the contract admin is not set.
    ///
    /// # Returns
    /// * `Vec<Address>` - The list of addresses that were skipped (already
    ///   registered).
    ///
    /// # Indexing
    /// Only freshly created profiles are appended to the buyer enumeration
    /// index; entries skipped as already-registered are never indexed twice
    /// (#841).
    pub fn batch_register_buyers(
        env: Env,
        entries: Vec<(Address, Map<String, String>)>,
    ) -> Vec<Address> {
        require_not_paused(&env);
        if entries.len() > 50 {
            panic_with_error!(&env, RegistryError::BatchSizeExceeded);
        }

        let admin = Self::require_admin_unfrozen(&env);
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
            Self::index_profile(&env, &Role::Buyer, &address);
            events::buyer_registered(&env, &address);
            Self::bump_profile_version(&env, &address);
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
    /// storage with its TTL extended, the address is appended to the buyer
    /// enumeration index (`DataKey::ProfileIndex(Role::Buyer, n)` /
    /// `DataKey::ProfileCount(Role::Buyer)`), and a `buyer_registered` event is
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
    /// * `RegistryError::UnknownMetadataKey` if a key is not allowed by the
    ///   active `MetadataKeyPolicy` (#1120).
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
        require_not_paused(&env);
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
        Self::index_profile(&env, &Role::Buyer, &address);
        events::buyer_registered(&env, &address);
        Self::bump_profile_version(&env, &address);
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
    /// * `RegistryError::UnknownMetadataKey` if a key is not allowed by the
    ///   active `MetadataKeyPolicy` (#1120).
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
        require_not_paused(&env);
        Self::validate_metadata(&env, &metadata);
        address.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotRegistered));
        let changed = profile.metadata != metadata;
        profile.metadata = metadata;
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::profile_updated(&env, &address);
        if changed {
            Self::bump_profile_version(&env, &address);
        }
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
    /// * `RegistryError::InvalidMetadata` if `metadata` exceeds
    ///   `MAX_METADATA_SIZE` entries, contains an empty key or value, or has a
    ///   key longer than `MAX_METADATA_KEY_LEN` or a value longer than
    ///   `MAX_METADATA_VALUE_LEN`.
    /// * `RegistryError::UnknownMetadataKey` if a key is not allowed by the
    ///   active `MetadataKeyPolicy` (#1120).
    /// * `RegistryError::NotRegistered` if the address is not registered.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.update_metadata(&issuer, &new_metadata);
    /// ```
    pub fn update_metadata(env: Env, address: Address, metadata: Map<String, String>) -> bool {
        require_not_paused(&env);
        Self::validate_metadata(&env, &metadata);
        address.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotRegistered));
        let changed = profile.metadata != metadata;
        profile.metadata = metadata;
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::profile_updated(&env, &address);
        if changed {
            Self::bump_profile_version(&env, &address);
        }
        Self::extend_instance_ttl(&env);
        true
    }

    /// Removes one metadata key from an existing profile.
    ///
    /// # Auth
    /// * Requires `address.require_auth()`.
    ///
    /// # Panics
    /// * `RegistryError::NotRegistered` if no profile exists for `address`.
    /// * `RegistryError::InvalidMetadata` if `metadata_key` is not present.
    pub fn remove_metadata_key(env: Env, address: Address, metadata_key: String) -> bool {
        require_not_paused(&env);
        address.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotRegistered));
        if !profile.metadata.contains_key(metadata_key.clone()) {
            panic_with_error!(&env, RegistryError::InvalidMetadata);
        }
        profile.metadata.remove(metadata_key.clone());
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::metadata_key_removed(&env, &address, &metadata_key);
        Self::bump_profile_version(&env, &address);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Returns the number of metadata keys stored on `address`'s profile.
    ///
    /// # Panics
    /// * `RegistryError::NotFound` if no profile exists for `address`.
    pub fn get_metadata_key_count(env: Env, address: Address) -> u32 {
        Self::require_initialized(&env);
        let key = DataKey::Profile(address.clone());
        let profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        Self::extend_instance_ttl(&env);
        profile.metadata.len()
    }

    /// Retrieves a registered profile by address.
    ///
    /// Reads the profile entry from persistent storage, extends its TTL using
    /// the same threshold and target duration as the write path, and returns
    /// the stored value.
    ///
    /// Note: the returned `Profile` encodes `role`/`verified`/`revoked` in a
    /// single internal `packed_flags` bitmask. That bit layout is a private
    /// implementation detail, not a stable external contract (#609). Clients
    /// that only have RPC access should read the decoded views
    /// [`get_role`](Self::get_role), [`is_verified`](Self::is_verified) and
    /// [`get_verification_status`](Self::get_verification_status) instead of
    /// reverse-engineering `packed_flags`.
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
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
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
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                verified
            }
            None => false,
        }
    }

    /// Returns the ledger timestamp when `address` became currently verified.
    ///
    /// Returns `Some(timestamp)` only while the profile is in `Verified` state.
    /// The timestamp is set by `verify_profile(..., true)` and `reinstate`, and
    /// cleared on revocation or `verify_profile(..., false)`.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `RegistryError::NotFound` if no profile is stored for `address`.
    pub fn get_verified_since(env: Env, address: Address) -> Option<u64> {
        Self::require_initialized(&env);
        let profile_key = DataKey::Profile(address.clone());
        let profile: Profile = env
            .storage()
            .persistent()
            .get(&profile_key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        env.storage()
            .persistent()
            .extend_ttl(&profile_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        let since_key = DataKey::VerifiedSince(address);
        let since: Option<u64> = env.storage().persistent().get(&since_key);
        if since.is_some() {
            env.storage()
                .persistent()
                .extend_ttl(&since_key, TTL_THRESHOLD, TTL_EXTEND_TO);
        }
        Self::extend_instance_ttl(&env);

        if profile.verified() && !profile.revoked() {
            since
        } else {
            None
        }
    }

    pub fn get_verification_status(env: Env, address: Address) -> VerificationStatus {
        let key = DataKey::Profile(address);
        match env.storage().persistent().get::<_, Profile>(&key) {
            None => VerificationStatus::Unregistered,
            Some(profile) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                if profile.verified() {
                    VerificationStatus::Verified
                } else if profile.revoked() {
                    VerificationStatus::Revoked
                } else {
                    VerificationStatus::Pending
                }
            }
        }
    }

    /// Returns the decoded [`Role`] of a registered address, if any.
    ///
    /// Lightweight decoded accessor that sits alongside
    /// [`is_verified`](Self::is_verified) and
    /// [`get_verification_status`](Self::get_verification_status) (#609).
    /// [`get_profile`](Self::get_profile) returns the raw `Profile` struct,
    /// whose `role`/`verified`/`revoked` state is stored as an internal
    /// `packed_flags` bitmask; this view decodes the role on-chain so
    /// off-chain consumers (indexers, explorers, RPC clients) never have to
    /// reverse-engineer that private layout to learn whether an address is
    /// an `Issuer` or a `Buyer`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The address to look up.
    ///
    /// # Auth
    /// * No `require_auth()` call is made — this is a read-only view.
    ///
    /// # Panics
    /// * Does not panic — unregistered addresses are reported as `None`,
    ///   mirroring how `get_verification_status` reports `Unregistered`.
    ///
    /// # Returns
    /// * `Option<Role>` - `Some(role)` when a profile is stored for
    ///   `address`, `None` otherwise. When the entry exists, its TTL is
    ///   extended using the same threshold and target duration as the write
    ///   path.
    ///
    /// # Example
    /// ```ignore
    /// let role = client.get_role(&issuer); // Some(Role::Issuer)
    /// ```
    pub fn get_role(env: Env, address: Address) -> Option<Role> {
        let key = DataKey::Profile(address);
        match env.storage().persistent().get::<_, Profile>(&key) {
            Some(profile) => {
                let role = profile.role();
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                Some(role)
            }
            None => None,
        }
    }

    /// Returns how many profiles are registered for `role`.
    ///
    /// This is the O(1) companion to the other registry views: it answers
    /// "how many issuers/buyers exist?" without the caller having to know an
    /// address up front and without replaying the registration event stream.
    /// The count is derived from the same `DataKey::ProfileCount(role)` entry
    /// the enumeration index is appended under, so it always matches the number
    /// of addresses reachable through
    /// [`list_profiles`](Self::list_profiles) for `role`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `role` - The role to count (`Role::Issuer` or `Role::Buyer`).
    ///
    /// # Auth
    /// * No `require_auth()` call is made — this is a read-only view.
    ///
    /// # Panics
    /// * Does not panic — a role with no registrations (or an uninitialized
    ///   contract) reports `0`.
    ///
    /// # Returns
    /// * `u32` - The number of profiles registered for `role`, counting
    ///   revoked profiles as well (revocation only flips verification; it does
    ///   not deregister the profile or remove its index slot).
    ///
    /// # Example
    /// ```ignore
    /// let issuer_count = client.get_profile_count(&Role::Issuer);
    /// ```
    pub fn get_profile_count(env: Env, role: Role) -> u32 {
        let count_key = DataKey::ProfileCount(role);
        match env.storage().persistent().get::<_, u32>(&count_key) {
            Some(count) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&count_key, TTL_THRESHOLD, TTL_EXTEND_TO);
                count
            }
            // A missing counter means "no registrations yet" — there is no
            // entry to keep alive, and nothing to extend.
            None => 0,
        }
    }

    /// Returns a page of registered addresses for `role`, in registration
    /// order.
    ///
    /// Together with [`get_profile_count`](Self::get_profile_count) this
    /// replaces "replay `issuer_registered` / `buyer_registered` from genesis"
    /// with a bounded on-chain read, which is what an indexer or an admin
    /// dashboard needs to page through the registry.
    ///
    /// Pages are contiguous and non-overlapping: page `n` is
    /// `list_profiles(role, n * limit, limit)`, and each address appears in
    /// exactly one page, so walking `start = 0, limit, 2*limit, ...` until a
    /// page comes back shorter than `limit` enumerates every registered
    /// address for `role` exactly once.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `role` - The role to enumerate (`Role::Issuer` or `Role::Buyer`).
    /// * `start` - Zero-based index of the first address to return. Values at
    ///   or beyond `get_profile_count(role)` yield an empty page.
    /// * `limit` - Maximum number of addresses to return. Must be
    ///   `<= MAX_LIST_LIMIT` (50); `0` yields an empty page.
    ///
    /// # Auth
    /// * No `require_auth()` call is made — this is a read-only view.
    ///
    /// # Panics
    /// * `RegistryError::PageSizeExceeded` if `limit > MAX_LIST_LIMIT`.
    ///
    /// # Returns
    /// * `Vec<Address>` - Up to `limit` registered addresses for `role`,
    ///   starting at index `start`, in registration order.
    ///
    /// # Cost
    /// Bounded by `limit`: at most `limit` storage reads plus TTL extensions,
    /// never the whole index. A `start` past the end of the index reads nothing
    /// beyond the role's counter.
    ///
    /// # Example
    /// ```ignore
    /// let page = client.list_profiles(&Role::Issuer, &0, &25);
    /// ```
    pub fn list_profiles(env: Env, role: Role, start: u32, limit: u32) -> Vec<Address> {
        if limit > MAX_LIST_LIMIT {
            panic_with_error!(&env, RegistryError::PageSizeExceeded);
        }

        let count_key = DataKey::ProfileCount(role.clone());
        let count: u32 = match env.storage().persistent().get::<_, u32>(&count_key) {
            Some(count) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&count_key, TTL_THRESHOLD, TTL_EXTEND_TO);
                count
            }
            None => 0,
        };

        // Clamp the window to the indexed range so a page that runs off the end
        // returns a short (possibly empty) page instead of panicking, and use
        // a saturating bound so a `start` near `u32::MAX` cannot overflow.
        let end = start.saturating_add(limit).min(count);

        let mut addresses: Vec<Address> = Vec::new(&env);
        let mut index = start;
        while index < end {
            let index_key = DataKey::ProfileIndex(role.clone(), index);
            // A slot can be missing if its entry aged out while the profile
            // itself survived, so skip gaps instead of failing the whole page.
            if let Some(address) = env.storage().persistent().get::<_, Address>(&index_key) {
                env.storage()
                    .persistent()
                    .extend_ttl(&index_key, TTL_THRESHOLD, TTL_EXTEND_TO);
                addresses.push_back(address);
            }
            index += 1;
        }
        addresses
    }

    /// Revokes a registered profile by setting its verification status to `false`.
    ///
    /// This function is idempotent: calling it on an already-unverified profile
    /// returns `true` without re-emitting the `address_revoked` event or
    /// rewriting storage. The instance and profile TTLs are still extended so
    /// repeated administrative reads cannot let the registry singleton age out.
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
        require_not_paused(&env);
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        if profile.revoked() {
            // Only an already revoked profile is idempotent; Pending must transition.
            // Extend TTLs so repeated admin reads don't let storage age out.
            env.storage()
                .persistent()
                .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
            let verified_since_key = DataKey::VerifiedSince(address.clone());
            if env.storage().persistent().has(&verified_since_key) {
                env.storage().persistent().extend_ttl(
                    &verified_since_key,
                    TTL_THRESHOLD,
                    TTL_EXTEND_TO,
                );
            }
            Self::extend_instance_ttl(&env);
            return true;
        }
        profile.set_verified(false);
        profile.set_revoked(true);
        let verified_since_key = DataKey::VerifiedSince(address.clone());
        env.storage().persistent().remove(&verified_since_key);
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::address_revoked(&env, &address);
        Self::bump_profile_version(&env, &address);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Revokes `address` and records an operator-supplied `reason_hash` (#1118).
    ///
    /// Preserves all existing `revoke` verification semantics and storage
    /// layout: `Profile` is unchanged, while `reason_hash` (e.g. a SHA-256
    /// digest of off-chain evidence) is stored under
    /// `DataKey::RevocationReasonHash(address)` so no personal information is
    /// placed on chain.
    ///
    /// Emits `address_revoked(address)` (when transitioning from non-revoked)
    /// and `revocation_reason_recorded(address, reason_hash)`.
    ///
    /// # Auth
    /// * Requires admin authorization (`DataKey::Admin`) and unfrozen admin.
    ///
    /// # Panics
    /// * `RegistryError::NotFound` if `address` is not registered.
    pub fn revoke_with_reason(env: Env, address: Address, reason_hash: BytesN<32>) -> bool {
        Self::revoke(env.clone(), address.clone());
        let reason_key = DataKey::RevocationReasonHash(address.clone());
        env.storage().persistent().set(&reason_key, &reason_hash);
        env.storage()
            .persistent()
            .extend_ttl(&reason_key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::revocation_reason_recorded(&env, &address, &reason_hash);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Returns the operator-supplied revocation reason hash for `address`, if
    /// one was recorded via `revoke_with_reason`.
    ///
    /// # Panics
    /// * `RegistryError::NotInitialized` if the contract has not been
    ///   initialized.
    pub fn get_revocation_reason_hash(env: Env, address: Address) -> Option<BytesN<32>> {
        Self::require_initialized(&env);
        let reason_key = DataKey::RevocationReasonHash(address);
        let result: Option<BytesN<32>> = env.storage().persistent().get(&reason_key);
        if result.is_some() {
            env.storage()
                .persistent()
                .extend_ttl(&reason_key, TTL_THRESHOLD, TTL_EXTEND_TO);
        }
        Self::extend_instance_ttl(&env);
        result
    }

    /// Reinstates verification for a previously revoked profile.
    ///
    /// This is the admin-only inverse of `revoke`: it flips the profile's
    /// `verified` flag back to `true` and emits a dedicated
    /// `address_reinstated` event so integrators can distinguish
    /// reinstatement from a generic verification toggle.
    ///
    /// Like `revoke`, it is guarded by an idempotency check (#606): it only
    /// operates on profiles whose `revoked` flag is set. Calling it on a
    /// profile that was never revoked — a `Pending` profile or one that is
    /// already `Verified` — is a no-op that returns `true` without writing
    /// storage or emitting `address_reinstated`. This keeps `reinstate` from
    /// being used as an undocumented alternate path to verify a `Pending`
    /// profile (use `verify_profile` for that), and keeps the
    /// `address_reinstated` event honest for off-chain indexers: it is only
    /// ever emitted for an actual un-revocation.
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
    /// * `RegistryError::NotInitialized` if the contract admin is not set
    ///   (contract was never initialized).
    /// * `RegistryError::NotFound` if no profile is stored for `address`.
    ///
    /// # Returns
    /// * `bool` - `true` when the profile is verified or the call required no
    ///   action: a successful reinstatement, an already-verified profile, or
    ///   a never-revoked (`Pending`) profile left untouched.
    ///
    /// # Example
    /// ```ignore
    /// let ok = client.reinstate(&issuer);
    /// ```
    pub fn reinstate(env: Env, address: Address) -> bool {
        require_not_paused(&env);
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        if !profile.revoked() {
            return true;
        }
        profile.set_verified(true);
        profile.set_revoked(false);
        let verified_since_key = DataKey::VerifiedSince(address.clone());
        env.storage()
            .persistent()
            .set(&verified_since_key, &env.ledger().timestamp());
        env.storage()
            .persistent()
            .extend_ttl(&verified_since_key, TTL_THRESHOLD, TTL_EXTEND_TO);
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::address_reinstated(&env, &address);
        Self::bump_profile_version(&env, &address);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Sets (or clears) the verification flag on a registered profile.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `caller` - The address authorizing the call. Must be either the
    ///   stored admin (`DataKey::Admin`) or the stored verifier
    ///   (`DataKey::Verifier`, see [`set_verifier`](Self::set_verifier)).
    /// * `address` - The registered address whose verification changes.
    /// * `verify` - `true` to verify (also clears the revoked flag), `false`
    ///   to un-verify (also marks the profile revoked).
    ///
    /// # Repeated calls
    /// Calling with the value the profile already holds is NOT an idempotent
    /// no-op: storage is rewritten to the same state and a `profile_verified`
    /// event is emitted on every call. Each call is an intentional audit
    /// event, so indexers can count administrative decisions and must not
    /// treat the event stream as a list of state changes (#987).
    ///
    /// # Auth
    /// * Requires `caller.require_auth()`, where `caller` is the admin or the
    ///   verifier (#732). This lets day-to-day KYC decisions be delegated to an
    ///   operational key without handing out admin control.
    ///
    /// # Panics
    /// * `RegistryError::NotInitialized` if the contract has not been
    ///   initialized.
    /// * `RegistryError::Unauthorized` if `caller` is neither the admin nor
    ///   the verifier.
    /// * `RegistryError::NotFound` if no profile is stored for `address`.
    ///
    /// # Returns
    /// * `bool` - `true` when the profile is updated.
    ///
    /// # Example
    /// ```ignore
    /// client.verify_profile(&verifier, &issuer, &true);
    /// ```
    pub fn verify_profile(env: Env, caller: Address, address: Address, verify: bool) -> bool {
        require_not_paused(&env);
        Self::require_not_frozen(&env);
        Self::require_admin_or_verifier(&env, &caller);
        caller.require_auth();
        let key = DataKey::Profile(address.clone());
        let mut profile: Profile = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NotFound));
        let flags_before = profile.packed_flags;
        let verified_since_key = DataKey::VerifiedSince(address.clone());
        profile.set_verified(verify);
        if verify {
            profile.set_revoked(false);
            env.storage()
                .persistent()
                .set(&verified_since_key, &env.ledger().timestamp());
            env.storage().persistent().extend_ttl(
                &verified_since_key,
                TTL_THRESHOLD,
                TTL_EXTEND_TO,
            );
        } else {
            profile.set_revoked(true);
            env.storage().persistent().remove(&verified_since_key);
        }
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::profile_verified(&env, &address, verify);
        if profile.packed_flags != flags_before {
            Self::bump_profile_version(&env, &address);
        }
        Self::extend_instance_ttl(&env);
        true
    }

    /// Transfers admin ownership to a new address.
    ///
    /// Requires authentication from both the current admin and the incoming
    /// new admin, preventing accidental transfers to a wrong address. Callers
    /// that intentionally need unilateral key rotation may use
    /// [`transfer_admin`](Self::transfer_admin), but it does not provide this
    /// dual-authorization safety guarantee.
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `new_admin` - The address that will become the new admin.
    ///
    /// # Panics
    /// * `RegistryError::NotInitialized` if the contract has not been initialized.
    pub fn transfer_ownership(env: Env, new_admin: Address) {
        require_not_paused(&env);
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        new_admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        // A proposal made by the outgoing admin must not outlive it.
        env.storage().instance().remove(&DataKey::PendingAdmin);
        events::ownership_transferred(&env, &admin, &new_admin);
        Self::extend_instance_ttl(&env);
    }

    /// Transfers contract admin to a new address.
    ///
    /// Unlike `transfer_ownership`, this function only requires auth from the
    /// current admin — the new admin does not need to sign. This is useful
    /// for key rotation scenarios where the new key holder's involvement is
    /// unavailable. This is an intentional lower-security escape hatch: the
    /// current admin can use it to bypass `transfer_ownership`'s dual-auth
    /// guarantee, including transferring to an address that never authorized
    /// the transfer. Integrators requiring that safety property must use
    /// `transfer_ownership` and ensure both signatures are collected.
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
    /// * `RegistryError::NotInitialized` if the contract has not been
    ///   initialized.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Example
    /// ```ignore
    /// client.transfer_admin(&new_admin);
    /// ```
    pub fn transfer_admin(env: Env, new_admin: Address) {
        require_not_paused(&env);
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        // A proposal made by the outgoing admin must not outlive it.
        env.storage().instance().remove(&DataKey::PendingAdmin);
        events::admin_transferred(&env, &admin, &new_admin);
        Self::extend_instance_ttl(&env);
    }

    /// Assigns the verifier role to `verifier`, replacing any previous one.
    ///
    /// The verifier may call [`verify_profile`](Self::verify_profile) in
    /// addition to the admin. It is deliberately a narrow, operational role
    /// (#732): it cannot call `transfer_admin`, `transfer_ownership`, `pause`,
    /// `unpause`, `revoke`, `reinstate`, `batch_register_*`, `set_verifier`
    /// or `remove_verifier` — all of those stay admin-only.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `verifier` - The address to grant the verifier role.
    ///
    /// # Auth
    /// * Requires `admin.require_auth()` — only the stored admin may assign
    ///   the verifier.
    ///
    /// # Panics
    /// * `RegistryError::NotInitialized` if the contract has not been
    ///   initialized.
    ///
    /// # Example
    /// ```ignore
    /// client.set_verifier(&verifier);
    /// ```
    pub fn set_verifier(env: Env, verifier: Address) {
        require_not_paused(&env);
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        env.storage().instance().set(&DataKey::Verifier, &verifier);
        events::verifier_set(&env, &admin, &verifier);
        Self::extend_instance_ttl(&env);
    }

    /// Clears the verifier role so only the admin can call `verify_profile`.
    ///
    /// Idempotent: calling it when no verifier is set is a no-op apart from
    /// the emitted `verifier_removed` event.
    ///
    /// # Auth
    /// * Requires `admin.require_auth()`.
    ///
    /// # Panics
    /// * `RegistryError::NotInitialized` if the contract has not been
    ///   initialized.
    pub fn remove_verifier(env: Env) {
        require_not_paused(&env);
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        env.storage().instance().remove(&DataKey::Verifier);
        events::verifier_removed(&env, &admin);
        Self::extend_instance_ttl(&env);
    }

    /// Returns the current verifier, or `None` if the role is unassigned.
    ///
    /// # Auth
    /// * No `require_auth()` call is made — this is a read-only view.
    pub fn get_verifier(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Verifier)
    }

    /// Engages the emergency circuit breaker.
    ///
    /// While paused every state-changing entry point (registration, profile
    /// updates, revocation/reinstatement, verification, batch operations and
    /// admin transfers) reverts with `ContractPaused`, while read-only views
    /// (`get_profile`, `is_verified`, `get_verification_status`, `get_role`,
    /// `get_admin`)
    /// stay callable. Only the stored admin may pause, and [`Self::unpause`] is
    /// intentionally never guarded so a paused registry can always be
    /// resumed. Emits `paused`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// Requires authorization from the stored `admin`.
    ///
    /// # Panics
    /// * `RegistryError::NotInitialized` if the contract has not been
    ///   initialized.
    ///
    /// # Example
    /// ```ignore
    /// client.pause();
    /// ```
    pub fn pause(env: Env) {
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        set_paused(&env, true);
        events::paused(&env, &admin);
        Self::extend_instance_ttl(&env);
    }

    /// Disengages the emergency circuit breaker, restoring state-changing calls.
    ///
    /// Deliberately *not* guarded by `require_not_paused`: otherwise a paused
    /// registry could never be resumed. It is still rejected with
    /// `AdminFrozen` while the guardian has frozen admin writes (#1125).
    /// Emits `unpaused`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// Requires authorization from the stored `admin`.
    ///
    /// # Panics
    /// * `RegistryError::NotInitialized` if the contract has not been
    ///   initialized.
    ///
    /// # Example
    /// ```ignore
    /// client.unpause();
    /// ```
    pub fn unpause(env: Env) {
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        set_paused(&env, false);
        events::unpaused(&env, &admin);
        Self::extend_instance_ttl(&env);
    }

    // ------------------------------------------------------------------
    // Delayed admin transfer (#1124)
    // ------------------------------------------------------------------

    /// Proposes `new_admin` as the next admin, effective no earlier than
    /// `delay` seconds from now.
    ///
    /// This is the optional delayed alternative to the immediate
    /// `transfer_ownership` /
    /// `transfer_admin` paths. The current admin stays
    /// fully in control until `new_admin` calls
    /// `accept_admin` at or after
    /// `activates_at = now + delay`, which leaves a window to spot a proposal
    /// made with a compromised key and cancel it, or have the guardian freeze
    /// admin writes. A new proposal replaces any pending one.
    ///
    /// # Auth
    /// * Requires the current admin's authorization.
    ///
    /// # Panics
    /// * `ContractPaused` while paused.
    /// * `RegistryError::AdminFrozen` while admin writes are frozen.
    /// * `RegistryError::InvalidAdminTransfer` if `delay` is below
    ///   `MIN_ADMIN_TRANSFER_DELAY`, `now + delay` overflows, or `new_admin`
    ///   is already the admin.
    ///
    /// # Returns
    /// * `u64` - The `activates_at` ledger timestamp.
    ///
    /// Emits `admin_transfer_proposed(admin) -> (new_admin, activates_at)`.
    pub fn propose_admin(env: Env, new_admin: Address, delay: u64) -> u64 {
        require_not_paused(&env);
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        if delay < MIN_ADMIN_TRANSFER_DELAY || new_admin == admin {
            panic_with_error!(&env, RegistryError::InvalidAdminTransfer);
        }
        let activates_at = env
            .ledger()
            .timestamp()
            .checked_add(delay)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::InvalidAdminTransfer));
        let pending = PendingAdminTransfer {
            new_admin: new_admin.clone(),
            activates_at,
        };
        env.storage()
            .instance()
            .set(&DataKey::PendingAdmin, &pending);
        events::admin_transfer_proposed(&env, &admin, &new_admin, activates_at);
        Self::extend_instance_ttl(&env);
        activates_at
    }

    /// Completes a pending delayed admin transfer.
    ///
    /// # Auth
    /// * Requires authorization from the proposed `new_admin`, proving the
    ///   incoming key is controlled.
    ///
    /// # Panics
    /// * `ContractPaused` while paused.
    /// * `RegistryError::AdminFrozen` while admin writes are frozen.
    /// * `RegistryError::NoPendingAdminTransfer` if nothing is proposed.
    /// * `RegistryError::AdminTransferNotReady` before `activates_at`.
    ///
    /// Emits `admin_transfer_accepted(old_admin) -> new_admin`.
    pub fn accept_admin(env: Env) {
        require_not_paused(&env);
        let old_admin = Self::require_admin_unfrozen(&env);
        let pending: PendingAdminTransfer = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NoPendingAdminTransfer));
        if env.ledger().timestamp() < pending.activates_at {
            panic_with_error!(&env, RegistryError::AdminTransferNotReady);
        }
        pending.new_admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::Admin, &pending.new_admin);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        events::admin_transfer_accepted(&env, &old_admin, &pending.new_admin);
        Self::extend_instance_ttl(&env);
    }

    /// Cancels the pending delayed admin transfer.
    ///
    /// `caller` may be the admin or the guardian. The guardian path keeps
    /// working while admin writes are frozen, so a proposal made with a
    /// compromised admin key can be withdrawn during the freeze.
    ///
    /// # Auth
    /// * Requires `caller.require_auth()`.
    ///
    /// # Panics
    /// * `RegistryError::Unauthorized` if `caller` is neither admin nor
    ///   guardian.
    /// * `RegistryError::AdminFrozen` if the admin calls while frozen.
    /// * `RegistryError::NoPendingAdminTransfer` if nothing is proposed.
    ///
    /// Emits `admin_transfer_cancelled(caller) -> new_admin`.
    pub fn cancel_admin_transfer(env: Env, caller: Address) {
        let admin = Self::require_admin(&env);
        let guardian: Option<Address> = env.storage().instance().get(&DataKey::Guardian);
        if guardian.as_ref() != Some(&caller) {
            if caller != admin {
                panic_with_error!(&env, RegistryError::Unauthorized);
            }
            Self::require_not_frozen(&env);
        }
        caller.require_auth();
        let pending: PendingAdminTransfer = env
            .storage()
            .instance()
            .get(&DataKey::PendingAdmin)
            .unwrap_or_else(|| panic_with_error!(&env, RegistryError::NoPendingAdminTransfer));
        env.storage().instance().remove(&DataKey::PendingAdmin);
        events::admin_transfer_cancelled(&env, &caller, &pending.new_admin);
        Self::extend_instance_ttl(&env);
    }

    /// Returns the pending delayed admin transfer, if any.
    pub fn get_pending_admin_transfer(env: Env) -> Option<PendingAdminTransfer> {
        env.storage().instance().get(&DataKey::PendingAdmin)
    }

    // ------------------------------------------------------------------
    // Emergency guardian freeze (#1125)
    // ------------------------------------------------------------------

    /// Configures the emergency guardian, replacing any previous one.
    ///
    /// The guardian is a second, preconfigured key whose only powers are
    /// `freeze_admin`,
    /// `unfreeze_admin` and cancelling a pending
    /// delayed admin transfer. It must be set before an incident: once
    /// frozen, the guardian cannot be changed until it unfreezes.
    ///
    /// # Auth
    /// * Requires the admin's authorization.
    ///
    /// # Panics
    /// * `ContractPaused` while paused.
    /// * `RegistryError::AdminFrozen` while admin writes are frozen.
    /// * `RegistryError::InvalidGuardian` if `guardian` is the admin.
    pub fn set_guardian(env: Env, guardian: Address) {
        require_not_paused(&env);
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        if guardian == admin {
            panic_with_error!(&env, RegistryError::InvalidGuardian);
        }
        env.storage().instance().set(&DataKey::Guardian, &guardian);
        events::guardian_set(&env, &admin, &guardian);
        Self::extend_instance_ttl(&env);
    }

    /// Removes the guardian. Admin-only; rejected while frozen.
    pub fn remove_guardian(env: Env) {
        require_not_paused(&env);
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        env.storage().instance().remove(&DataKey::Guardian);
        events::guardian_removed(&env, &admin);
        Self::extend_instance_ttl(&env);
    }

    /// Returns the configured guardian, if any.
    pub fn get_guardian(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Guardian)
    }

    /// Freezes every admin-only write (guardian only).
    ///
    /// While frozen, `upgrade`, `batch_register_*`, `revoke`, `reinstate`,
    /// `verify_profile` (admin and verifier), `transfer_ownership`,
    /// `transfer_admin`, `propose_admin`, `accept_admin`, `set_verifier`,
    /// `remove_verifier`, `set_guardian`, `remove_guardian`,
    /// `set_metadata_key_policy`, `pause` and `unpause` revert with
    /// `AdminFrozen`, and an admin-initiated `cancel_admin_transfer` does too.
    /// Existing verification results are untouched, every read stays
    /// available, and users can still register and update their own
    /// profiles.
    ///
    /// Unfreeze rules: only the guardian can lift the freeze, via
    /// `unfreeze_admin`. The admin cannot, because the
    /// freeze exists for when the admin key is suspected compromised. The
    /// freeze works even while paused.
    ///
    /// Idempotent: freezing an already-frozen registry emits nothing.
    ///
    /// # Panics
    /// * `RegistryError::GuardianNotSet` if no guardian is configured.
    pub fn freeze_admin(env: Env) {
        let guardian = Self::require_guardian(&env);
        guardian.require_auth();
        if !Self::is_admin_frozen(env.clone()) {
            env.storage().instance().set(&DataKey::AdminFrozen, &true);
            events::admin_frozen(&env, &guardian);
        }
        Self::extend_instance_ttl(&env);
    }

    /// Lifts the admin freeze (guardian only). Idempotent: unfreezing a
    /// registry that is not frozen emits nothing.
    ///
    /// # Panics
    /// * `RegistryError::GuardianNotSet` if no guardian is configured.
    pub fn unfreeze_admin(env: Env) {
        let guardian = Self::require_guardian(&env);
        guardian.require_auth();
        if Self::is_admin_frozen(env.clone()) {
            env.storage().instance().remove(&DataKey::AdminFrozen);
            events::admin_unfrozen(&env, &guardian);
        }
        Self::extend_instance_ttl(&env);
    }

    /// Returns `true` while admin-only writes are frozen.
    pub fn is_admin_frozen(env: Env) -> bool {
        env.storage()
            .instance()
            .get(&DataKey::AdminFrozen)
            .unwrap_or(false)
    }

    // ------------------------------------------------------------------
    // Metadata key policy (#1120)
    // ------------------------------------------------------------------

    /// Selects which metadata keys the registration and update paths accept.
    ///
    /// The policy applies to every later `register_*`, `batch_register_*`,
    /// `update_profile` and `update_metadata` call. Metadata already stored
    /// is never rewritten or rejected on read. A profile holding keys the new
    /// policy does not allow keeps them until its owner submits an update,
    /// which must then use allowed keys only.
    ///
    /// # Auth
    /// * Requires the admin's authorization.
    ///
    /// # Panics
    /// * `ContractPaused` while paused.
    /// * `RegistryError::AdminFrozen` while admin writes are frozen.
    ///
    /// Emits `metadata_key_policy_set(admin) -> policy`.
    pub fn set_metadata_key_policy(env: Env, policy: MetadataKeyPolicy) {
        require_not_paused(&env);
        let admin = Self::require_admin_unfrozen(&env);
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::MetadataKeyPolicy, &policy);
        events::metadata_key_policy_set(&env, &admin, policy);
        Self::extend_instance_ttl(&env);
    }

    /// Returns the active metadata key policy (`Unrestricted` by default).
    pub fn get_metadata_key_policy(env: Env) -> MetadataKeyPolicy {
        Self::metadata_key_policy(&env)
    }

    /// Returns the keys the active policy accepts, or `None` when any key
    /// within the length and count bounds is accepted.
    pub fn get_allowed_metadata_keys(env: Env) -> Option<Vec<String>> {
        Self::allowed_keys(&env, Self::metadata_key_policy(&env))
    }

    // ------------------------------------------------------------------
    // Profile update sequence numbers (#1128)
    // ------------------------------------------------------------------

    /// Returns `address`'s update sequence number.
    ///
    /// `1` right after registration, then incremented by exactly one for
    /// every write that changes the stored profile: a metadata change through
    /// `update_profile` / `update_metadata`, or a role/verification flag
    /// change through `verify_profile`, `revoke` or `reinstate`. No-op calls
    /// (re-submitting identical metadata, revoking an already revoked
    /// profile, reinstating a profile that is not revoked, or `verify_profile`
    /// with the value already held) leave it unchanged.
    ///
    /// Each bump also emits `profile_version(address) -> version`, so an event
    /// consumer that sees a gap knows it missed an update and should re-read
    /// the profile.
    ///
    /// Returns `0` for unregistered addresses and for profiles registered
    /// before this feature that have not changed since.
    pub fn get_profile_version(env: Env, address: Address) -> u64 {
        let key = DataKey::ProfileVersion(address);
        match env.storage().persistent().get::<_, u64>(&key) {
            Some(version) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                version
            }
            None => 0,
        }
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
    fn require_admin(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, RegistryError::NotInitialized))
    }

    /// Panics with `Unauthorized` unless `caller` is the stored admin or the
    /// stored verifier. Does not call `require_auth` — the caller does that.
    fn require_admin_or_verifier(env: &Env, caller: &Address) {
        let admin = Self::require_admin(env);
        if *caller == admin {
            return;
        }
        let verifier: Option<Address> = env.storage().instance().get(&DataKey::Verifier);
        if verifier.as_ref() != Some(caller) {
            panic_with_error!(env, RegistryError::Unauthorized);
        }
    }

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
        if let Some(allowed) = Self::allowed_keys(env, Self::metadata_key_policy(env)) {
            for key in metadata.keys().iter() {
                if !allowed.contains(&key) {
                    panic_with_error!(env, RegistryError::UnknownMetadataKey);
                }
            }
        }
    }

    fn metadata_key_policy(env: &Env) -> MetadataKeyPolicy {
        env.storage()
            .instance()
            .get(&DataKey::MetadataKeyPolicy)
            .unwrap_or(MetadataKeyPolicy::Unrestricted)
    }

    /// Keys accepted by `policy`, or `None` when the policy accepts any key.
    fn allowed_keys(env: &Env, policy: MetadataKeyPolicy) -> Option<Vec<String>> {
        match policy {
            MetadataKeyPolicy::Unrestricted => None,
            MetadataKeyPolicy::AllowlistV1 => {
                let mut keys = Vec::new(env);
                for key in METADATA_KEYS_V1.iter() {
                    keys.push_back(String::from_str(env, key));
                }
                Some(keys)
            }
        }
    }

    /// Panics with `AdminFrozen` while the guardian has frozen admin writes.
    fn require_not_frozen(env: &Env) {
        if env
            .storage()
            .instance()
            .get::<_, bool>(&DataKey::AdminFrozen)
            .unwrap_or(false)
        {
            panic_with_error!(env, RegistryError::AdminFrozen);
        }
    }

    /// `require_admin` for admin-only writes: also rejects while frozen.
    fn require_admin_unfrozen(env: &Env) -> Address {
        Self::require_not_frozen(env);
        Self::require_admin(env)
    }

    fn require_guardian(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Guardian)
            .unwrap_or_else(|| panic_with_error!(env, RegistryError::GuardianNotSet))
    }

    /// Increments `address`'s update sequence number, extends its TTL and
    /// emits `profile_version`. Callers invoke it only when the stored
    /// profile actually changed (or was just created).
    fn bump_profile_version(env: &Env, address: &Address) -> u64 {
        let key = DataKey::ProfileVersion(address.clone());
        let version = env
            .storage()
            .persistent()
            .get::<_, u64>(&key)
            .unwrap_or(0)
            .saturating_add(1);
        env.storage().persistent().set(&key, &version);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::profile_version(env, address, version);
        version
    }

    /// Appends `address` to the enumeration index for `role` and bumps that
    /// role's profile counter.
    ///
    /// Every registration path funnels through here, so the counter and the
    /// index can never drift apart: the count is exactly the number of index
    /// slots written. Callers must only reach this for a profile that was just
    /// created — entries skipped as already-registered are not indexed a
    /// second time.
    ///
    /// Both keys are persistent and get the same TTL bump as a profile entry,
    /// so a registry that is still being used keeps its enumeration alive
    /// alongside the profiles it points at.
    fn index_profile(env: &Env, role: &Role, address: &Address) {
        let count_key = DataKey::ProfileCount(role.clone());
        let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);

        let index_key = DataKey::ProfileIndex(role.clone(), count);
        env.storage().persistent().set(&index_key, address);
        env.storage()
            .persistent()
            .extend_ttl(&index_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        // `saturating_add` rather than `+ 1`: a count this large is not
        // reachable, and saturating keeps the write from panicking in a
        // release build with overflow checks on.
        env.storage()
            .persistent()
            .set(&count_key, &count.saturating_add(1));
        env.storage()
            .persistent()
            .extend_ttl(&count_key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }
}
