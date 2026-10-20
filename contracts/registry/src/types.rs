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
}
