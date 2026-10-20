use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryError {
    AlreadyInitialized = 1,
    AlreadyRegistered = 2,
    NotFound = 3,
    NotInitialized = 4,
    BatchSizeExceeded = 5,
    InvalidMetadata = 6,
    NotRegistered = 7,
    /// The requested `list_profiles` page size is above
    /// `MAX_LIST_LIMIT` (50), mirroring `BatchSizeExceeded` for the batch
    /// registration entry points. Pagination must be requested in bounded
    /// pages so a single call can never walk the whole index.
    PageSizeExceeded = 8,
    /// The `caller` passed to `verify_profile` is neither the stored admin
    /// nor the stored verifier (#732).
    Unauthorized = 9,
    /// The requested profile state transition is not permitted (#982).
    /// `reinstate` only accepts profiles that are currently revoked
    /// (`Revoked -> Verified`); calling it on a `Pending` or already
    /// `Verified` profile fails with this error instead of silently
    /// approving a profile that never completed verification.
    InvalidProfileState = 10,
}
