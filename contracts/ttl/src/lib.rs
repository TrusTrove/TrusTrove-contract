#![no_std]

//! Shared TTL (time-to-live) constants for all storage entries across the
//! TrusTrove contract workspace.
//!
//! `THRESHOLD` is the minimum remaining ledger count before an entry is
//! eligible for extension; `EXTEND_TO` is the ledger count the entry's TTL is
//! extended to when that threshold is crossed.
//!
//! With `EXTEND_TO = 2_000_000`, a `THRESHOLD` of `500_000` (25% of
//! `EXTEND_TO`) ensures renewals happen well before expiry rather than at the
//! last second.
//!
//! Every `extend_ttl` call across all contracts should go through these
//! constants so the bump policy stays consistent.

/// Threshold (minimum remaining ledgers before extension triggers): 25% of
/// `EXTEND_TO`, so renewals fire with plenty of headroom.
pub const THRESHOLD: u32 = 500_000;
/// Ledger count the entry is extended to when `THRESHOLD` is crossed.
pub const EXTEND_TO: u32 = 2_000_000;
