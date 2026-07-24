use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EscrowError {
    AlreadyInitialized = 1,
    NotFound = 2,
    AlreadyLocked = 4,
    InvalidAmount = 5,
    NotInitialized = 6,
}
