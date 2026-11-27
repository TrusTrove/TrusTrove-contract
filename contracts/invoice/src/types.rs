use soroban_sdk::{contracttype, Address, BytesN, String, Symbol, Vec};

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
    Cancelled,
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
    /// Cumulative amount repaid so far by the buyer, in USDC stroops.
    pub repaid_amount: u128,
    /// Remaining balance left to be repaid, in USDC stroops.
    pub remaining_balance: u128,
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
    SupportedAsset(Address),
    SupportedAssetCount,
    // EscrowContract intentionally last to avoid changing enum discriminants for
    // already-deployed contract storage keys. New variants must keep being
    // appended after it, in the same spirit, rather than inserted earlier.
    EscrowContract,
    // Address of the agent-registry contract (deployed separately from the
    // underwrite-contract repo) that `submit_attestation` consults to check
    // an attesting agent's signing key.
    AgentRegistryContract,
    // Attestation recorded against a given invoice, keyed by invoice id.
    Attestation(BytesN<32>),
    // O(1) status-membership marker: `(status, short invoice id)` -> the
    // `u32` position of the invoice's row in the `StatusIndexEntry` array
    // while it belongs to that status. Written when an invoice enters a
    // status and removed when it leaves, replacing the full
    // `StatusIndexEntry` scans previously used for duplicate, idempotency
    // and removal-position checks (issues #831 / #835). Appended after every
    // existing variant so already-deployed key discriminants stay stable.
    StatusMembership(InvoiceStatus, u64),
    // O(1) issuer-index membership marker: `(issuer, short invoice id)` ->
    // `true` while the invoice is in that issuer's index. Written by
    // `extend_issuer_index` (issue #831). The issuer index is append-only,
    // so a presence bit is sufficient — no stored position is needed.
    IssuerMembership(Address, u64),
    // O(1) buyer-index membership marker, mirroring `IssuerMembership`.
    // Written by `extend_buyer_index` (issue #831). Appended after existing
    // variants so already-deployed key discriminants stay stable.
    BuyerMembership(Address, u64),
    DefaultGraceSeconds,
    SupportedAssetList,
    AssetMetadata(Address),
    MaxInvoiceLifetime,
    CreatedAtIndexEntry(u32),
    // Ledger timestamp at which the issuer's delivery confirmation was
    // recorded (issue #1160). Written at most once per party by
    // `confirm_delivery` and read by `get_lifecycle_timestamps`. Kept as a
    // separate entry rather than an `Invoice` field so the existing `Invoice`
    // encoding — and every already-deployed record — stays byte-for-byte
    // unchanged. Appended after every existing variant so already-deployed
    // key discriminants stay stable.
    IssuerConfirmedAt(BytesN<32>),
    // Ledger timestamp at which the buyer's delivery confirmation was
    // recorded (issue #1160). Mirrors `IssuerConfirmedAt`.
    BuyerConfirmedAt(BytesN<32>),
    // Optional buyer purchase-order hash, stored separately so existing
    // Invoice records keep their serialized shape.
    PurchaseOrderHash(BytesN<32>),
    // O(1) buyer-index position marker. Added separately from the legacy
    // boolean membership marker to preserve its stored value type.
    BuyerIndexPosition(Address, u64),
    // 32-byte proof-of-delivery evidence hash attached by the buyer or issuer
    // after shipment, keyed by invoice id (issue #1162). Only the hash is
    // stored; the underlying documents stay off-chain. Appended after every
    // existing variant so already-deployed key discriminants stay stable.
    DeliveryEvidence(BytesN<32>),
}

/// Metadata describing a supported asset (optional asset code and decimals).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetMetadata {
    pub code: Option<String>,
    pub decimals: Option<u32>,
}

/// A risk attestation recorded against an invoice by a registered
/// Underwrite agent, gating `list_for_financing`.
///
/// Written once by [`InvoiceContract::submit_attestation`] and never
/// updated — the presence check itself is the replay guard, so an invoice
/// can only ever carry a single attestation.
#[contracttype]
#[derive(Clone, Debug)]
pub struct Attestation {
    /// Identifier of the attesting agent, as registered in the
    /// agent-registry contract.
    pub agent_id: Symbol,
    /// Risk score in basis points, bounded to `0..=10_000`. Never a float —
    /// same fixed-point convention as [`Invoice::discount_bps`].
    pub risk_score: u32,
    /// Hash of the off-chain evidence backing this attestation (e.g. the
    /// underwriting report), for later off-chain verification.
    pub evidence_hash: BytesN<32>,
    /// Unix timestamp (seconds) at which the attestation was submitted.
    pub submitted_at: u64,
    /// Unix timestamp (seconds, ledger timestamp) after which this
    /// attestation no longer unlocks listing: `list_for_financing` panics
    /// with [`crate::InvoiceError::AttestationExpired`] once
    /// `env.ledger().timestamp() > valid_until`. Equality
    /// (`timestamp == valid_until`) is still valid — the expiry check is
    /// strict `>`. Copied from the signed [`AttestationPayload::valid_until`]
    /// (issue #823).
    pub valid_until: u64,
}

/// The signed payload an Underwrite agent produces off-chain and submits
/// via [`InvoiceContract::submit_attestation`].
///
/// Decoded from the raw `payload: Bytes` argument via
/// [`soroban_sdk::xdr::FromXdr`], and the *same* raw bytes are what get
/// keccak256-hashed and checked against `signature`. The field order here
/// therefore doubles as the wire format agents must sign over.
///
/// Layout v2 (issues #822 / #823): `contract_address`, `network_id`, and
/// `valid_until` are new. The first two bind a signature to one deployed
/// invoice contract on one network, so a payload signed for one deployment
/// can never be replayed against another deployment that trusts the same
/// agent key; the third bounds how long the risk assessment stays valid.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AttestationPayload {
    /// Domain separator binding this signature to TrusTrove attestations
    /// specifically (v2 layout), so a signature can't be replayed against
    /// an unrelated contract or message scheme.
    pub domain_separator: BytesN<32>,
    /// The invoice this attestation is for. Must match the `invoice_id`
    /// argument passed to `submit_attestation`.
    pub invoice_id: BytesN<32>,
    /// Address of the invoice contract this signature is bound to (issue
    /// #822). `submit_attestation` rejects a payload whose
    /// `contract_address` differs from `env.current_contract_address()`
    /// with [`crate::InvoiceError::PayloadBindingMismatch`].
    pub contract_address: Address,
    /// Network this signature is bound to (issue #822), carried exactly as
    /// `env.ledger().network_id()` returns it (a 32-byte network identifier
    /// in this SDK — distinct values for testnet vs mainnet). A payload
    /// whose `network_id` doesn't match the executing ledger is rejected
    /// with [`crate::InvoiceError::PayloadBindingMismatch`].
    pub network_id: BytesN<32>,
    /// Risk score in basis points. Values above 10,000 are rejected on-chain.
    pub risk_score: u32,
    pub evidence_hash: BytesN<32>,
    pub agent_id: Symbol,
    /// Caller-chosen nonce. Not separately tracked on-chain: the
    /// one-attestation-per-invoice storage check is the replay guard.
    pub nonce: u64,
    /// Unix timestamp (seconds, ledger timestamp) until which this
    /// attestation may unlock `list_for_financing` (issue #823).
    /// `submit_attestation` rejects an already-expired payload and
    /// `list_for_financing` panics with
    /// [`crate::InvoiceError::AttestationExpired`] once
    /// `env.ledger().timestamp() > valid_until`. Equality
    /// (`timestamp == valid_until`) is still valid.
    pub valid_until: u64,
}

/// Local mirror of the `Agent` record exposed by the agent-registry
/// contract (from the separate `underwrite-contract` repo). This contract
/// has no crate dependency on that repo, so this shape must be kept in
/// sync with its `get_agent` return type by hand.
///
/// See `docs/AGENT_REGISTRY_INTERFACE.md` for the normative spec: the
/// registry's `Agent` must have exactly these two fields (extra fields break
/// decoding), and `pubkey` is a 65-byte uncompressed SEC1 secp256k1 key.
#[contracttype]
#[derive(Clone, Debug)]
pub struct Agent {
    pub active: bool,
    pub pubkey: BytesN<65>,
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
            InvoiceStatus::Cancelled => "Cancelled",
        }
    }
}

/// One page of invoice IDs for a single status, returned by
/// [`InvoiceContract::get_invoice_ids_by_status`](crate::InvoiceContract::get_invoice_ids_by_status)
/// (issue #1152).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceIdPage {
    /// Invoice IDs on this page. Each one existed and had the requested
    /// status when the page was read. Can be shorter than the requested
    /// `page_size` (even empty) while `next` is still `Some`, when stale
    /// legacy index rows were filtered out.
    pub ids: Vec<BytesN<32>>,
    /// Cursor to pass as `cursor` to fetch the following page, or `None`
    /// when this page reached the start of the index and iteration is
    /// complete. An exactly full last page also returns `None`, so no extra
    /// empty request is needed.
    pub next: Option<u32>,
}

/// Age of an invoice's attestation, returned by
/// [`InvoiceContract::get_attestation_age`](crate::InvoiceContract::get_attestation_age)
/// (issue #1158).
///
/// Lets funding and risk clients judge how stale an underwriting assessment
/// is without loading and decoding the whole [`Attestation`].
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct AttestationAge {
    /// Ledger timestamp (seconds) at which the attestation was submitted,
    /// copied verbatim from [`Attestation::submitted_at`].
    pub submitted_at: u64,
    /// Ledger time elapsed since submission: `now - submitted_at` at the
    /// moment of the read, saturating at `0` if the ledger timestamp ever
    /// reported an earlier time than the stored submission timestamp.
    pub age_seconds: u64,
}

/// Every lifecycle timestamp recorded for an invoice, returned by
/// [`InvoiceContract::get_lifecycle_timestamps`](crate::InvoiceContract::get_lifecycle_timestamps)
/// (issue #1160).
///
/// Each field is `None` when the contract has not recorded that transition;
/// see the method documentation for the per-field meaning of absence.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct LifecycleTimestamps {
    /// When the invoice was created. `Some` for every stored invoice;
    /// optional so clients can treat all seven fields uniformly.
    pub created_at: Option<u64>,
    /// When the invoice was listed for financing; `None` until
    /// `list_for_financing` succeeds.
    pub listed_at: Option<u64>,
    /// When the invoice was funded; `None` until `mark_funded` succeeds.
    pub funded_at: Option<u64>,
    /// When the goods were reported shipped; `None` until `mark_shipped`
    /// succeeds.
    pub shipped_at: Option<u64>,
    /// When repayment settled the invoice; `None` while the invoice is
    /// outstanding and for invoices that were defaulted, cancelled or
    /// expired rather than repaid.
    pub repaid_at: Option<u64>,
    /// Earlier of the issuer's and buyer's delivery-confirmation
    /// timestamps; `None` until at least one party confirms, and for
    /// invoices confirmed before confirmation timestamps were tracked.
    pub first_confirmation_at: Option<u64>,
    /// Later of the issuer's and buyer's delivery-confirmation timestamps;
    /// `None` until both parties confirm. Equal to `first_confirmation_at`
    /// when both confirmations landed in the same ledger.
    pub second_confirmation_at: Option<u64>,
}

/// Compact financing terms for an invoice, returned by
/// [`InvoiceContract::get_financing_terms`](crate::InvoiceContract::get_financing_terms)
/// (issue #1138).
///
/// Everything a pool or wallet client needs to evaluate an invoice before
/// funding, collected in one storage read.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct FinancingTerms {
    /// Full face value in USDC stroops, same as [`Invoice::face_value`].
    pub face_value: u128,
    /// Discount in basis points, same as [`Invoice::discount_bps`].
    pub discount_bps: u32,
    /// Funding asset (token contract), same as [`Invoice::funding_asset`].
    pub funding_asset: Address,
    /// Unix timestamp (seconds) at which repayment is due, same as
    /// [`Invoice::due_date`].
    pub due_date: u64,
    /// Issuer address, same as [`Invoice::issuer`].
    pub issuer: Address,
    /// Buyer address, same as [`Invoice::buyer`].
    pub buyer: Address,
    /// Current lifecycle status, same as [`Invoice::status`].
    pub status: InvoiceStatus,
}

/// Settlement summary for reconciling invoice economics.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct SettlementSummary {
    pub status: InvoiceStatus,
    pub face_value: u128,
    pub funded_amount: u128,
    pub discount_bps: u32,
    pub discount: u128,
    pub funded_at: Option<u64>,
    pub due_date: u64,
    pub repaid_at: Option<u64>,
    pub term: Option<u64>,
    pub elapsed: Option<u64>,
    pub earned_by_pool: Option<u128>,
    pub refund_to_buyer: Option<u128>,
    pub repaid_amount: u128,
    pub remaining_balance: u128,
}

/// Listing expiry information returned by
/// [`InvoiceContract::get_listing_expiry_preview`](crate::InvoiceContract::get_listing_expiry_preview).
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListingExpiryPreview {
    /// Timestamp at which the listing becomes eligible for expiration.
    /// `expire_listing` succeeds at this timestamp.
    pub expiry_timestamp: u64,
    /// Whether the current ledger timestamp is at or past `expiry_timestamp`.
    pub is_expired: bool,
}

/// A reason an invoice cannot currently be funded.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvoiceFundingRejectionReason {
    InvoiceNotListed,
    ListingExpired,
    AttestationMissing,
    AttestationExpired,
    IssuerNotVerified,
    BuyerNotVerified,
    UnsupportedAsset,
    InvalidDiscount,
}

/// Read-only snapshot of an invoice's funding eligibility.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceFundingEligibility {
    /// Current lifecycle status of the invoice.
    pub invoice_status: InvoiceStatus,
    /// Listing expiry timestamp, or `None` when the invoice is not listed.
    pub listing_expiry_timestamp: Option<u64>,
    /// Attestation validity timestamp, or `None` when no attestation exists.
    pub attestation_valid_until: Option<u64>,
    /// Whether the issuer is currently verified in the registry.
    pub issuer_verified: bool,
    /// Whether the buyer is currently verified in the registry.
    pub buyer_verified: bool,
    /// Whether the invoice funding asset is currently allow-listed.
    pub asset_supported: bool,
    /// Whether the discount is within the invoice listing range `1..=5000`.
    pub discount_bps_valid: bool,
    /// True when no invoice funding rejection reasons apply.
    pub is_eligible: bool,
    /// Every currently applicable invoice-level rejection reason.
    pub rejection_reasons: Vec<InvoiceFundingRejectionReason>,
}

/// Repayment operation identified by the repayment eligibility view.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepaymentAction {
    /// No repayment action is currently available.
    Unavailable,
    /// Repay the full remaining balance using `InvoiceContract::repay`.
    Repay,
}

/// A reason a caller cannot currently repay an invoice.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RepaymentRejectionReason {
    ContractPaused,
    CallerNotBuyer,
    InvalidStatus,
    NoAmountDue,
    SettlementNotConfigured,
}

/// Read-only snapshot of whether a caller can repay an invoice in full.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct RepaymentEligibility {
    /// The full remaining balance that `repay` would request.
    pub amount_due: u128,
    /// The repayment action when it is currently valid.
    pub action: RepaymentAction,
    /// Whether the caller can currently invoke `repay`.
    pub is_eligible: bool,
    /// Every currently applicable invoice-level rejection reason.
    pub rejection_reasons: Vec<RepaymentRejectionReason>,
}

/// Why an invoice currently can or cannot be defaulted, as returned by
/// [`InvoiceContract::get_default_eligibility`](crate::InvoiceContract::get_default_eligibility)
/// (issue #1149).
#[contracttype]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DefaultEligibilityReason {
    /// Default is permitted: the invoice is in a defaultable status and the
    /// configured grace period after its due date has fully elapsed.
    Eligible,
    /// Default is blocked because the invoice is not `Funded`, `Active`, or
    /// `Confirmed` — it has not been funded yet or is already terminal.
    InvalidStatus,
    /// Default is blocked because
    /// `now <= due_date + default_grace_seconds`, exactly the boundary
    /// `trigger_default` rejects with `DueDateNotPassed`.
    DueDateNotPassed,
}

/// Result of the [`InvoiceContract::get_default_eligibility`](crate::InvoiceContract::get_default_eligibility)
/// view: whether `trigger_default` would succeed right now, plus the reason
/// when it would not (issue #1149).
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct DefaultEligibility {
    pub eligible: bool,
    pub reason: DefaultEligibilityReason,
}
