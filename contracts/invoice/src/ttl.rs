//! Shared TTL (time-to-live) constants for all storage entries in this contract.
//!
//! `THRESHOLD` is the minimum remaining ledger count before an entry is
//! eligible for extension; `EXTEND_TO` is the ledger count the entry's TTL is
//! extended to when that threshold is crossed. Every `extend_ttl` call in the
//! invoice contract should go through these constants so the bump policy stays
//! consistent across instance and persistent storage.
//!
//! With `EXTEND_TO = 2_000_000` and `THRESHOLD = 500_000`, entries are renewed
//! when less than 25% of the full lifetime remains (~29 days on a ~5s ledger),
//! preventing last-second renewals that risk expiry during network congestion.

pub const THRESHOLD: u32 = 500_000;
pub const EXTEND_TO: u32 = 2_000_000;
