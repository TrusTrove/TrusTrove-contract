use soroban_sdk::contracterror;

#[contracterror]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolFactoryError {
    /// No longer reachable from the contract: state setup moved to the
    /// protocol constructor (`__constructor`, #827), which the ledger invokes
    /// exactly once at deploy time. The code stays reserved so error numbers
    /// never get re-assigned to a different meaning.
    AlreadyInitialized = 1,
    NotInitialized = 2,
    AssetAlreadyRegistered = 3,
    /// `register_asset` could not read the registry contract from the invoice
    /// contract it was handed, because that invoice contract has not been
    /// initialized (or is not an invoice contract at all).
    InvoiceNotInitialized = 4,
    /// `register_existing_pool` was given an address that does not expose a
    /// pool initialized for the requested asset.
    InvalidPool = 5,
}
