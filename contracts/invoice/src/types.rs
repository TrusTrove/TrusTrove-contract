use soroban_sdk::{contracttype, Address, BytesN};

#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq)]
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

/// Represents a single invoice tracked by the invoice contract.
///
/// # Invariants
///
/// - `funded_amount <= face_value` — funding must never exceed the face value
///   of the invoice.
/// - `discount_bps` is expressed in basis points (1 bp = 0.01%).
/// - All amounts are denominated in USDC stroops (7 decimals).
#[contracttype]
#[derive(Clone, Debug)]
pub struct Invoice {
    /// Unique 32-byte identifier for this invoice. Used as the primary key
    /// in persistent storage (see [`DataKey::Invoice`]).
    pub id: BytesN<32>,
    /// Address of the party that issued (sold) the invoice and will be paid
    /// the discounted proceeds when funded.
    pub issuer: Address,
    /// Address of the party obligated to repay the invoice at maturity
    /// (typically the buyer of the underlying goods/services).
    pub buyer: Address,
    /// Full face value of the invoice, denominated in USDC stroops
    /// (7 decimals). This is the amount the buyer owes at `due_date`.
    pub face_value: u128,
    /// Discount applied when the invoice is funded, in basis points
    /// (1 bp = 0.01%). Determines the yield to funders.
    pub discount_bps: u32,
    /// Amount that has been funded against this invoice, in USDC stroops.
    ///
    /// Invariant: `funded_amount <= face_value`.
    pub funded_amount: u128,
    /// Unix timestamp (seconds) at which the invoice matures and repayment
    /// is due.
    pub due_date: u64,
    /// Current lifecycle state of the invoice. See [`InvoiceStatus`].
    pub status: InvoiceStatus,
    /// Unix timestamp (seconds) recording when the invoice was created
    /// on-chain.
    pub created_at: u64,
    /// Unix timestamp (seconds) recording when the invoice was listed for
    /// funding. `None` if it has not yet been listed.
    pub listed_at: Option<u64>,
    /// Unix timestamp (seconds) recording when the invoice was funded.
    /// `None` if it has not yet been funded.
    pub funded_at: Option<u64>,
    /// Unix timestamp (seconds) recording when the underlying goods were
    /// shipped by the issuer. `None` until shipment is reported.
    pub shipped_at: Option<u64>,
    /// Dual-confirmation flag: `true` once the issuer has confirmed delivery
    /// / completion. Both `issuer_confirmed` and `buyer_confirmed` must be
    /// `true` to advance the invoice to [`InvoiceStatus::Confirmed`].
    pub issuer_confirmed: bool,
    /// Dual-confirmation flag: `true` once the buyer has confirmed receipt
    /// / acceptance. Both `issuer_confirmed` and `buyer_confirmed` must be
    /// `true` to advance the invoice to [`InvoiceStatus::Confirmed`].
    pub buyer_confirmed: bool,
    /// Unix timestamp (seconds) recording when the invoice was repaid.
    /// `None` until repayment is settled.
    pub repaid_at: Option<u64>,
    /// Address of the asset (token contract) used to fund and repay this
    /// invoice — typically the USDC token contract.
    pub funding_asset: Address,
    /// Address of the pool contract that funded this invoice, when funding
    /// came from a pool rather than a direct funder. `None` for
    /// direct-funded invoices.
    pub funding_pool: Option<Address>,
}

#[contracttype]
pub enum DataKey {
    Admin,
    RegistryContract,
    PoolContract,
    Counter,
    Invoice(BytesN<32>),
    IssuerIndexCount(Address),
    BuyerIndexCount(Address),
    StatusIndexCount(u32),
    StatusCount(u32),
    IssuerIndexEntry(Address, u32),
    BuyerIndexEntry(Address, u32),
    StatusIndexEntry(u32, u32),
    ExpiryWindow,
    DefaultGraceSeconds,
}

impl InvoiceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            InvoiceStatus::Created => "Created",
            InvoiceStatus::Listed => "Listed",
            InvoiceStatus::Funded => "Funded",
            InvoiceStatus::Active => "Active",
            InvoiceStatus::Confirmed => "Confirmed",
            InvoiceStatus::Repaid => "Repaid",
            InvoiceStatus::Defaulted => "Defaulted",
            InvoiceStatus::Expired => "Expired",
        }
    }
}
