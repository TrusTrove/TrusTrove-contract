use soroban_sdk::{contracttype, Address, Map, String};

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum Role {
    Issuer,
    Buyer,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum VerificationStatus {
    Unregistered,
    Pending,
    Verified,
    Revoked,
}

/// Registered profile, storage-packed via the optimization introduced in #142.
///
/// `role`, `verified` and `revoked` are encoded in `packed_flags`. The bit
/// layout is an internal implementation detail and is NOT a stable external
/// contract; it may change without a corresponding change to this doc.
/// Off-chain consumers must not decode it directly — use the decoded contract
/// views `get_role`, `is_verified` and `get_verification_status` (#609).
#[contracttype]
#[derive(Clone, Debug)]
pub struct Profile {
    pub packed_flags: u32,
    pub registered_at: u64,
    pub metadata: Map<String, String>,
}

impl Profile {
    pub fn new(
        role: Role,
        verified: bool,
        registered_at: u64,
        metadata: Map<String, String>,
    ) -> Self {
        let mut packed_flags = 0u32;
        if role == Role::Buyer {
            packed_flags |= 1;
        }
        if verified {
            packed_flags |= 2;
        }
        Profile {
            packed_flags,
            registered_at,
            metadata,
        }
    }

    pub fn role(&self) -> Role {
        if (self.packed_flags & 1) != 0 {
            Role::Buyer
        } else {
            Role::Issuer
        }
    }

    /// Changes only the role bit, leaving verification/revocation flags and
    /// registration time untouched (#1129).
    pub fn set_role(&mut self, role: Role) {
        if role == Role::Buyer {
            self.packed_flags |= 1;
        } else {
            self.packed_flags &= !1;
        }
    }

    pub fn verified(&self) -> bool {
        (self.packed_flags & 2) != 0
    }

    pub fn set_verified(&mut self, verified: bool) {
        if verified {
            self.packed_flags |= 2;
        } else {
            self.packed_flags &= !2;
        }
    }

    pub fn revoked(&self) -> bool {
        (self.packed_flags & 4) != 0
    }

    pub fn set_revoked(&mut self, revoked: bool) {
        if revoked {
            self.packed_flags |= 4;
        } else {
            self.packed_flags &= !4;
        }
    }
}

#[contracttype]
pub enum DataKey {
    Admin,
    /// Optional operational role allowed to call `verify_profile` alongside
    /// the admin (#732). Set via `set_verifier`, cleared via
    /// `remove_verifier`. Holding this role grants no admin powers: it cannot
    /// transfer admin/ownership, pause, revoke, reinstate or change itself.
    Verifier,
    Profile(Address),
    /// The `index`-th registered address for `role`, in registration order.
    ///
    /// Written once per successful registration by every registration path
    /// (`register_issuer`, `register_buyer`, `batch_register_issuers` and
    /// `batch_register_buyers`) so the registry can be enumerated without an
    /// off-chain replay of the `issuer_registered` / `buyer_registered` event
    /// stream. Entries are append-only: the registry has no deregistration
    /// path, so an index slot is never rewritten or freed.
    ProfileIndex(Role, u32),
    /// Number of populated `ProfileIndex` slots for `role`.
    ///
    /// Backs [`RegistryContract::get_profile_count`](crate::RegistryContract::get_profile_count)
    /// and doubles as the exclusive upper bound (the total number of indexed
    /// addresses) for `list_profiles`, so a caller can size the last page
    /// without first enumerating.
    ProfileCount(Role),
    /// Ledger timestamp when the address most recently became verified. Cleared
    /// on revocation and set again on reinstatement or verification.
    VerifiedSince(Address),
    /// Per-profile update sequence number (#1128), stored beside — not inside
    /// — `Profile` so existing `Profile` entries keep decoding unchanged.
    ///
    /// Set to `1` when the profile is registered and incremented by one every
    /// time a write actually changes the stored profile (metadata or
    /// role/verification flags). Calls that leave the profile unchanged
    /// (idempotent `revoke` / `reinstate`, re-verifying with the same value,
    /// re-submitting identical metadata) do not bump it. Profiles registered
    /// before this key existed have no entry and report `0` until their first
    /// change.
    ProfileVersion(Address),
    /// Optional emergency guardian (#1125). The only address that may
    /// freeze / unfreeze admin-only writes. Set via `set_guardian`, cleared
    /// via `remove_guardian`; it holds no other power.
    Guardian,
    /// `true` while the guardian has frozen admin-only writes (#1125).
    /// Absent / `false` means admin writes are live.
    AdminFrozen,
    /// Pending delayed admin transfer (#1124); see
    /// [`PendingAdminTransfer`]. Absent when no transfer is proposed.
    PendingAdmin,
    /// Active metadata key policy (#1120); see [`MetadataKeyPolicy`]. Absent
    /// means [`MetadataKeyPolicy::Unrestricted`].
    MetadataKeyPolicy,
}

/// A proposed admin change waiting out its delay (#1124).
///
/// The current admin keeps full control until `new_admin` calls
/// `accept_admin` at or after `activates_at` (ledger timestamp, seconds).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingAdminTransfer {
    pub new_admin: Address,
    pub activates_at: u64,
}

/// Versioned policy for which metadata keys registration and update paths
/// accept (#1120).
///
/// * `Unrestricted` — legacy behaviour and the default: any key that passes
///   the length / count bounds is accepted.
/// * `AllowlistV1` — only the keys in `METADATA_KEYS_V1` are accepted; any
///   other key is rejected with `RegistryError::UnknownMetadataKey`.
///
/// New allowlists are added as new variants (`AllowlistV2`, ...) rather than by
/// editing an existing one, so a given policy version always means the same
/// key set.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataKeyPolicy {
    Unrestricted,
    AllowlistV1,
}
