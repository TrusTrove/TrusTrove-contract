use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvoiceError {
    /// No longer reachable from the contract: state setup moved to the
    /// protocol constructor (`__constructor`, #827), which the ledger invokes
    /// exactly once at deploy time. The code stays reserved so error numbers
    /// never get re-assigned to a different meaning.
    AlreadyInitialized = 1,
    NotFound = 2,
    NotAuthorized = 3,
    IssuerNotVerified = 4,
    BuyerNotVerified = 5,
    InvalidFaceValue = 6,
    InvalidDueDate = 7,
    InvalidStatusTransition = 8,
    DiscountTooHigh = 9,
    AlreadyConfirmed = 10,
    DueDateNotPassed = 11,
    UnsupportedAsset = 13,
    ListingNotExpired = 14,
    MathOverflow = 15,
    InvalidAmount = 16,
    InvalidDiscount = 12,
    CounterOverflow = 17,
    InvalidExpiryWindow = 18,
    InvalidParticipants = 19,
    NotInitialized = 20,
    UntrustedSigner = 21,
    AlreadyAttested = 22,
    VerificationRequired = 23,
    CrossContractCallFailed = 24,
    RepaymentExceedsBalance = 25,
    InvalidConfiguration = 26,
    /// A paginated `get_by_*` query was given a `page_size` larger than
    /// [`MAX_PAGE_SIZE`](crate::MAX_PAGE_SIZE), which would defeat the
    /// per-call gas bound pagination exists to provide.
    InvalidPageSize = 27,
    /// A batch entry point was given more entries than
    /// [`MAX_BATCH_SIZE`](crate::MAX_BATCH_SIZE), which would defeat the
    /// per-call cost bound batching exists to provide.
    BatchSizeExceeded = 28,
    /// A signed attestation payload was bound to a different invoice
    /// contract address or network than the one executing
    /// `submit_attestation` (issue #822). Prevents signatures from
    /// replaying across deployments (testnet vs mainnet, redeploys).
    PayloadBindingMismatch = 29,
    /// An attestation payload was already past its `valid_until`
    /// timestamp at submission, or the recorded attestation expired
    /// before `list_for_financing` ran (issue #823). Staleness check on
    /// the ledger timestamp, strict `>` semantics.
    AttestationExpired = 30,
    /// An attestation payload contains a risk score above 10,000 basis points.
    InvalidRiskScore = 31,
    /// The signature recovery byte is outside Soroban's supported range.
    InvalidRecoveryId = 32,
}
