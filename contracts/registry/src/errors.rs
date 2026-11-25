use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryError {
    /// No longer reachable from the contract: state setup moved to the
    /// protocol constructor (`__constructor`, #827), which the ledger invokes
    /// exactly once at deploy time. The code stays reserved so error numbers
    /// never get re-assigned to a different meaning.
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
    /// No pending challenge exists for the address, or the supplied nonce does
    /// not match the stored challenge (#1130).
    InvalidNonce = 10,
    /// The submitted/ supplied nonce is not valid at the current ledger
    /// timestamp (#1130).
    NonceExpired = 11,
    /// This nonce was already consumed for the address and cannot be replayed
    /// (#1130).
    NonceAlreadyUsed = 12,
    /// A verification was attempted without a nonce after the address opted
    /// into the challenge flow (#1130).
    NonceRequired = 13,
}
