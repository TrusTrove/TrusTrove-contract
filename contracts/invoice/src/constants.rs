//! Shared TTL constants re-exported from the workspace-level `trusttrove-ttl`
//! crate so the bump policy stays consistent across all contracts.
//!
//! `TTL_THRESHOLD` is the minimum number of ledgers an entry must have
//! remaining before it is extended (25% of `TTL_EXTEND_TO`), and
//! `TTL_EXTEND_TO` is the number of ledgers the entry is extended to.

pub use trusttrove_ttl::EXTEND_TO as TTL_EXTEND_TO;
pub use trusttrove_ttl::THRESHOLD as TTL_THRESHOLD;

/// Lower bound on `Invoice::face_value`, in USDC stroops (7 decimals).
///
/// Dust face values are rejected because they can never be funded: the
/// pool computes `funded_amount = face_value * (10_000 - discount_bps) /
/// 10_000` and rejects a zero result (`PoolContract::fund_invoice`), so
/// any value that rounds to `0` at the maximum 5000 bps discount creates
/// an invoice that is permanently unfundable yet still occupies the
/// issuer/buyer/status indexes (issue #824).
///
/// `100_000` stroops is 0.01 USDC:
/// - correctness floor: at the maximum 5000 bps discount a face value of
///   `2` stroops is the arithmetic minimum that keeps
///   `funded_amount >= 1`; `100_000` clears that by five orders of
///   magnitude (`funded_amount = 50_000`);
/// - economic floor: one cent of USDC is the smallest amount worth the
///   on-chain index writes an invoice costs, so listing dust invoices no
///   longer buys view-bloat with the caller's own negligible fees.
pub const MIN_FACE_VALUE: u128 = 100_000;
