use soroban_sdk::{contracttype, Address};

#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    Admin,
    AssetCount,
    AssetIndex(u32),
    /// The pool instance the factory manages for a given asset.
    ///
    /// Written by both `register_asset` (freshly deployed instance) and
    /// `register_existing_pool` (pre-existing instance adopted during
    /// migration) so `get_pool_for_asset` reads one key regardless of how the
    /// instance came to exist.
    PoolForAsset(Address),
}
