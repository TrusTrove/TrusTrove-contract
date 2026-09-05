//! Shared TTL constants re-exported from the workspace-level `trusttrove-ttl`
//! crate so the bump policy stays consistent across all contracts.
//!
//! `TTL_THRESHOLD` is the minimum number of ledgers an entry must have
//! remaining before it is extended (25% of `TTL_EXTEND_TO`), and
//! `TTL_EXTEND_TO` is the number of ledgers the entry is extended to.

pub use trusttrove_ttl::EXTEND_TO as TTL_EXTEND_TO;
pub use trusttrove_ttl::THRESHOLD as TTL_THRESHOLD;

/// Minimum initial deposit floor (1 USDC = 10_000_000 stroops).
/// Prevents share-price griefing by requiring the initial deposit in an empty pool
/// to be at least this floor.
pub const MIN_INITIAL_DEPOSIT: u128 = 10_000_000;
