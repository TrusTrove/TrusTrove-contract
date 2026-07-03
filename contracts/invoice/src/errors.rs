use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvoiceError {
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
    InsufficientRepayment = 12,
    UnsupportedAsset = 13,
    ListingNotExpired = 14,
}
