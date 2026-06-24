use soroban_sdk::{contracttype, Address, BytesN};

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum InvoiceStatus {
    Created,
    Listed,
    Funded,
    Active,
    Confirmed,
    Repaid,
    Defaulted,
    Expired,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Invoice {
    pub id: BytesN<32>,
    pub issuer: Address,
    pub buyer: Address,
    pub face_value: u128,
    pub discount_bps: u32,
    pub funded_amount: u128,
    pub due_date: u64,
    pub status: InvoiceStatus,
    pub created_at: u64,
    pub listed_at: Option<u64>,
    pub funded_at: Option<u64>,
    pub shipped_at: Option<u64>,
    pub issuer_confirmed: bool,
    pub buyer_confirmed: bool,
    pub repaid_at: Option<u64>,
    pub funding_asset: Address,
    pub funding_pool: Option<Address>,
}

#[contracttype]
pub enum DataKey {
    Admin,
    RegistryContract,
    PoolContract,
    Counter,
    Invoice(BytesN<32>),
    InvoicesByIssuer(Address),
    InvoicesByBuyer(Address),
    InvoicesByStatus(u32),
    ExpiryWindow,
}
