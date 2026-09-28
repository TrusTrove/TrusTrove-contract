//! Shared TTL constants re-exported from the workspace-level `trusttrove-ttl`
//! crate so the bump policy stays consistent across all contracts.
//!
//! `TTL_THRESHOLD` is the minimum number of ledgers an entry must have
//! remaining before it is extended (25% of `TTL_EXTEND_TO`), and
//! `TTL_EXTEND_TO` is the number of ledgers the entry is extended to.

pub use trusttrove_ttl::EXTEND_TO as TTL_EXTEND_TO;
pub use trusttrove_ttl::THRESHOLD as TTL_THRESHOLD;

/// Default minimum initial deposit floor, used when `initialize` is not given
/// an explicit `min_initial_deposit` and as the fallback for pool instances
/// that predate the admin-configurable minimum. Prevents share-price griefing
/// by requiring the initial deposit in an empty pool to be at least this
/// floor.
///
/// This value assumes 7-decimal stroops (1 unit = 10_000_000 stroops), which
/// only holds for the pool's originally supported asset. Under the factory
/// model each instance funds a different asset, so a deploy for an asset with
/// different decimals should pass its own `min_initial_deposit` to
/// `initialize` rather than rely on this default.
pub const DEFAULT_MIN_INITIAL_DEPOSIT: u128 = 10_000_000;

/// Maximum protocol fee in basis points (2000 bps = 20%).
/// Prevents excessive fee extraction by capping the protocol cut at 20% of yield spread,
/// mirroring the bounds-check pattern used by `list_for_financing`'s discount cap.
pub const MAX_PROTOCOL_FEE_BPS: u32 = 2000;

/// Default decimal places reported by an LP share token's SEP-41 `decimals()`,
/// used as the fallback for pool instances that predate `initialize` taking a
/// `share_decimals` argument.
///
/// Shares are issued against the funding asset's base units — 1 USDC is
/// `DEFAULT_MIN_INITIAL_DEPOSIT` = 10_000_000 stroops — so the fallback matches
/// the 7 decimals of the USDC pools that existed before the factory model. Like
/// `DEFAULT_MIN_INITIAL_DEPOSIT`, this only holds for that originally supported
/// asset: a pool funding something with different decimals must pass its own
/// `share_decimals`, otherwise wallets render its share balances at the wrong
/// precision.
pub const DEFAULT_SHARE_DECIMALS: u32 = 7;
