use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolFactoryError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    AssetAlreadyRegistered = 3,
    /// `register_asset` could not read the registry contract from the invoice
    /// contract it was handed, because that invoice contract has not been
    /// initialized (or is not an invoice contract at all).
    InvoiceNotInitialized = 4,
}
