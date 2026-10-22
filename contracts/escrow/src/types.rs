use soroban_sdk::{contracttype, BytesN};

/// Record of an escrow lock stored for a given invoice.
///
/// # Invariants
///
/// - `amount` must be greater than zero — guaranteed by [`EscrowContract::lock`].
/// - `locked_at` is set to `env.ledger().timestamp()` at lock time and is never
///   updated for the lifetime of the record.
/// - A partial `release_to_pool` repayment reduces `amount` in place while
///   preserving `locked_at`; the record is only removed when the residual is
///   fully repaid (or on a full release / default); after removal
///   `get_locked` / `get_locked_at` return `0`.
#[contracttype]
#[derive(Clone, Debug)]
pub struct EscrowRecord {
    /// 32-byte invoice identifier scoped to this escrow lock.
    /// Used as the key suffix in [`DataKey::Locked`].
    pub invoice_id: BytesN<32>,
    /// Amount of USDC (in stroops, 7 decimals) held in escrow.
    /// Set from the `amount` argument of [`EscrowContract::lock`].
    pub amount: u128,
    /// Unix timestamp (seconds) recorded when the lock was created.
    /// Used to enforce the minimum lock period (grace period) before
    /// a [`default`](EscrowAction::DefaultHandled) can be triggered.
    pub locked_at: u64,
    /// The invoice issuer address that is authorized to receive released funds.
    /// Set from the `issuer` argument of [`EscrowContract::lock`] and validated
    /// in [`EscrowContract::release_to_issuer`].
    pub issuer: soroban_sdk::Address,
}

/// Actions that can be recorded in the escrow event history.
///
/// Each variant corresponds to a state transition of an escrow lock.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EscrowAction {
    /// Funds were locked in escrow against an invoice.
    Locked,
    /// Locked funds were released to the invoice issuer.
    ReleasedToIssuer,
    /// Locked funds were returned to the pool (partial or full repayment).
    ReleasedToPool,
    /// Lock was resolved as a default and funds returned to the pool.
    DefaultHandled,
    /// Stale lock returned to the pool via the permissionless `release_after_timeout`.
    TimeoutReleased,
}

/// An entry in the escrow event history for a given invoice.
///
/// History is stored under [`DataKey::History`] as a `Vec<EscrowEvent>`
/// and is append-only — see [`EscrowContract::append_history`].
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EscrowEvent {
    /// 32-byte invoice identifier this event relates to.
    pub invoice_id: BytesN<32>,
    /// The action that produced this event. See [`EscrowAction`].
    pub action: EscrowAction,
    /// Amount of USDC (in stroops) involved in this action.
    pub amount: u128,
    /// Unix timestamp (seconds) when this event was recorded.
    pub timestamp: u64,
    /// The caller address that triggered this event (for DefaultHandled events).
    pub caller: Option<soroban_sdk::Address>,
}

/// Storage keys used by the escrow contract.
///
/// Instance keys (no parameter) are stored with
/// [`env.storage().instance().set()`](soroban_sdk::storage::InstanceAccessor::set)
/// and are managed under the instance TTL. Persistent keys (`Locked`, `History`)
/// are stored with
/// [`env.storage().persistent().set()`](soroban_sdk::storage::PersistentAccessor::set)
/// and their TTL is explicitly extended on every write.
#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    /// Admin address authorised to trigger default resolution.
    Admin,
    /// Address of the pool contract that can lock / release / default escrow.
    PoolContract,
    /// Address of the USDC token contract held in escrow.
    UsdcAsset,
    /// Active escrow lock for a given invoice. The inner `BytesN<32>` is the
    /// invoice ID. Value type: [`EscrowRecord`].
    Locked(BytesN<32>),
    /// Event history for a given invoice. The inner `BytesN<32>` is the
    /// invoice ID. Value type: `Vec<EscrowEvent>`.
    History(BytesN<32>),
    /// Seconds after `locked_at` before `release_after_timeout` may return a
    /// still-locked record to the pool. Instance key; value type: `u64`.
    TimeoutWindow,
    /// Evidence that the locked principal for a given invoice was released to
    /// the issuer at funding time (issue #819). The inner `BytesN<32>` is the
    /// invoice ID. Value type: `u128` (the amount released to the issuer).
    ///
    /// Written by [`EscrowContract::release_to_issuer`] when it removes the
    /// [`DataKey::Locked`] record, and consumed by exactly one subsequent
    /// [`EscrowContract::release_to_pool`] (repayment) call. The pool reads it
    /// via [`EscrowContract::was_released_to_issuer`] in
    /// `PoolContract::handle_default` to tell "principal already paid to the
    /// issuer at funding" (normal) apart from an out-of-band lock removal
    /// (#21 guard).
    ///
    /// **Storage-key layout:** this enum is unnumbered, so each variant's
    /// discriminant is its positional index and that index *is* the on-chain
    /// storage-key encoding. New variants must therefore be **appended at the
    /// end** (this one takes index 6) — inserting one mid-enum would silently
    /// renumber every later variant and orphan the keys already written by
    /// deployed ledgers.
    IssuerReleased(BytesN<32>),
    /// Invoice contract reference pinned by the admin as defense-in-depth for
    /// `release_to_pool` (#997). Instance key; value type: `Address`.
    ///
    /// Written by [`EscrowContract::set_invoice_contract`] (admin-authenticated).
    /// `release_to_pool` still resolves the invoice contract it authorizes
    /// from the pool's `get_invoice_contract` view (#820), but when a pin is
    /// stored here that resolved address must equal the pin or the call is
    /// rejected with [`EscrowError::InvalidConfig`](crate::EscrowError::InvalidConfig) —
    /// so a pool whose invoice-contract configuration drifted from the
    /// escrow admin's pin cannot silently redirect repayments.
    ///
    /// **Storage-key layout:** appended at the end of the unnumbered enum
    /// (index 7) — see the note on [`DataKey::IssuerReleased`].
    InvoiceContract,
}
