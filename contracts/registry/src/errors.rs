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
    /// A registration or verification-status batch exceeds its 50-entry cap.
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
    /// The guardian has frozen admin-only writes (#1125). Only the guardian
    /// can lift the freeze with `unfreeze_admin`.
    AdminFrozen = 10,
    /// A guardian-only entry point was called but no guardian is configured.
    GuardianNotSet = 11,
    /// `set_guardian` was given the current admin; the guardian must be a
    /// separate key to be useful against an admin key compromise.
    InvalidGuardian = 12,
    /// `accept_admin` / `cancel_admin_transfer` was called with no pending
    /// delayed admin transfer.
    NoPendingAdminTransfer = 13,
    /// `accept_admin` was called before the proposal's `activates_at`.
    AdminTransferNotReady = 14,
    /// `propose_admin` was given a delay below `MIN_ADMIN_TRANSFER_DELAY`, a
    /// delay that overflows the ledger timestamp, or the current admin as the
    /// new admin.
    InvalidAdminTransfer = 15,
    /// A metadata key is not allowed by the active `MetadataKeyPolicy`
    /// (#1120).
    UnknownMetadataKey = 16,
}
