use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolError {
    /// No longer reachable from the contract: state setup moved to the
    /// protocol constructor (`__constructor`, #827), which the ledger invokes
    /// exactly once at deploy time. The code stays reserved so error numbers
    /// never get re-assigned to a different meaning.
    AlreadyInitialized = 1,
    NotInitialized = 2,
    InvalidAmount = 4,
    InsufficientLiquidity = 5,
    NoShares = 6,
    InsufficientShares = 7,
    InvoiceNotListed = 8,
    InvoiceNotFound = 10,
    AssetMismatch = 11,
    UtilizationCapExceeded = 12,
    Overflow = 13,
    MinimumDeposit = 14,
    InvalidConfiguration = 15,
    AlreadyFunded = 16,
    ActiveCountUnderflow = 17,
    IssuerNotVerified = 18,
    BuyerNotVerified = 19,
    EscrowAssetMismatch = 20,
    EscrowDefaultNotReleased = 21,
    /// Protocol fee basis points exceed `MAX_PROTOCOL_FEE_BPS` (2000 bps = 20%).
    FeeTooHigh = 22,
    /// Transfer amount exceeds sender's share balance.
    InsufficientBalance = 23,
    /// `transfer_from` amount exceeds the remaining allowance (or the allowance
    /// has expired, which reads back as `0`).
    InsufficientAllowance = 24,
    /// `approve` was given an `expiration_ledger` at or before the current
    /// ledger sequence while also granting a non-zero amount.
    InvalidExpiration = 25,
    /// A batch entry point was given more entries than `MAX_BATCH_SIZE`. The
    /// cap bounds the worst-case cost of a single batch so a caller cannot
    /// exceed the per-transaction budget by batching.
    BatchSizeExceeded = 26,
    /// `receive_repayment_with_refund` was given a refund destination that
    /// does not match the buyer recorded on the configured invoice contract
    /// (`get_buyer`). The refund is invoice-computed data, so its destination
    /// must be the invoice's canonical buyer (#969).
    BuyerMismatch = 27,
    /// The invoice's `discount_bps` is below the admin-configured minimum
    /// discount floor (`DataKey::MinDiscountBps`, set via `set_min_discount`),
    /// so funding it would deploy LP capital at less than the pool's yield
    /// floor. Never triggers while the floor is at its default of `0`
    /// (issue #829).
    DiscountBelowMinimum = 28,
    /// `handle_default` asked the invoice contract to persist the `Defaulted`
    /// status and it reported that the transition did not happen. The pool
    /// books no loss in that case, so an invoice that is still active can
    /// never leave the pool reporting a realised loss (#971).
    InvoiceDefaultNotRecorded = 29,
}
