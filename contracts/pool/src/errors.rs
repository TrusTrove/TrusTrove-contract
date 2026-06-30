use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolError {
    AlreadyInitialized = 1,
    NotFound = 2,
    NotAuthorized = 3,
    InvalidAmount = 4,
    InsufficientLiquidity = 5,
    NoShares = 6,
    InsufficientShares = 7,
    InvoiceNotListed = 8,
    AlreadyFunded = 9,
    InvoiceNotFound = 10,
    AssetMismatch = 11,
    UtilizationCapExceeded = 12,
    Overflow = 13,
    NotInitialized = 14,
}
