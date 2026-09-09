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
    NotRevoked = 8,
}
