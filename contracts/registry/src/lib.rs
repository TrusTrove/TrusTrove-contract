#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, Address, BytesN, Env, Map, String, Vec,
};
use trusttrove_pause::{require_not_paused, set_paused};

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
/// Maximum number of addresses a single `list_profiles` call may return.
///
/// Mirrors the 50-entry cap enforced on `batch_register_issuers` /
/// `batch_register_buyers`, so enumeration stays bounded per transaction no
/// matter how many profiles a role accumulates. Callers page through the index
/// with `start` / `limit` instead of asking for everything at once.
const MAX_LIST_LIMIT: u32 = 50;

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
        require_not_paused(&env);
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, RegistryError::AlreadyInitialized);
        }
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
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
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
        Self::extend_instance_ttl(&env);
        true
    }

    // Returns the list of addresses that were skipped (already registered) so
    // the caller knows exactly which entries were not processed (#66).
    //
    // Only freshly created profiles are appended to the issuer enumeration
    // index; entries skipped as already-registered are never indexed twice
    // (#841).
    pub fn batch_register_issuers(
        env: Env,
        entries: Vec<(Address, Map<String, String>)>,
    ) -> Vec<Address> {
        require_not_paused(&env);
        if entries.len() > 50 {
            panic_with_error!(&env, RegistryError::BatchSizeExceeded);
        }

        let admin = Self::require_admin(&env);
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

        let admin = Self::require_admin(&env);
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
        require_not_paused(&env);
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
        profile.metadata = metadata;
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::profile_updated(&env, &address);
        Self::extend_instance_ttl(&env);
        true
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
        let admin = Self::require_admin(&env);
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
            Self::extend_instance_ttl(&env);
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
        let admin = Self::require_admin(&env);
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
        env.storage().persistent().set(&key, &profile);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        events::address_reinstated(&env, &address);
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
        Self::require_admin_or_verifier(&env, &caller);
        caller.require_auth();
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
        let admin = Self::require_admin(&env);
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
        let admin = Self::require_admin(&env);
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
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
        let admin = Self::require_admin(&env);
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
        let admin = Self::require_admin(&env);
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
        let admin = Self::require_admin(&env);
        admin.require_auth();
        set_paused(&env, true);
        events::paused(&env, &admin);
        Self::extend_instance_ttl(&env);
    }

    /// Disengages the emergency circuit breaker, restoring state-changing calls.
    ///
    /// Deliberately *not* guarded by `require_not_paused`: otherwise a paused
    /// registry could never be resumed. Emits `unpaused`.
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
        let admin = Self::require_admin(&env);
        admin.require_auth();
        set_paused(&env, false);
        events::unpaused(&env, &admin);
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
