//! Shared TTL (time-to-live) constants for all storage entries in this contract.
//!
//! `THRESHOLD` is the minimum remaining ledger count before an entry is
//! eligible for extension; `EXTEND_TO` is the ledger count the entry's TTL is
//! extended to when that threshold is crossed. Every `extend_ttl` call in the
//! pool contract should go through these constants so the bump policy stays
//! consistent across instance and persistent storage.

pub const THRESHOLD: u32 = 100;
pub const EXTEND_TO: u32 = 2_000_000;
