#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, xdr::ToXdr, Address, BytesN, Env, String, Symbol, Vec,
};

mod constants;
mod errors;
mod events;
mod types;

#[cfg(test)]
mod test;

pub use constants::*;
pub use errors::*;
pub use types::*;

/// Function name of the invoice contract's registry getter, used by
/// `register_asset` to resolve the registry a new pool instance must verify
/// issuer/buyer profiles against.
const INVOICE_GET_REGISTRY: &str = "get_registry_contract";

#[contract]
pub struct PoolFactoryContract;

#[contractimpl]
impl PoolFactoryContract {
    /// Constructs the pool factory with an admin address.
    ///
    /// Runs atomically at deploy time (Soroban protocol constructor, #827):
    /// there is no window in which the factory exists without an admin, so no
    /// one can front-run initialization.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `admin` - The admin address for this contract.
    ///
    /// # Auth
    /// Requires authorization from `admin`.
    ///
    /// # Panics
    /// A panic aborts the deployment transaction, so an unconfigured factory
    /// is never created. `AlreadyInitialized` is unreachable from this entry
    /// point (the protocol invokes the constructor exactly once) and is kept
    /// only for error-code stability.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Example
    /// ```ignore
    /// // Deploy-and-construct in a single operation:
    /// // env.register(PoolFactoryContract, (admin,))
    /// ```
    pub fn __constructor(env: Env, admin: Address) {
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::AssetCount, &0u32);
        events::contract_initialized(&env, &admin);
    }

    /// Deploys a new pool instance for an asset and records it as that asset's
    /// pool.
    ///
    /// This is the factory's core function: it deploys a fresh instance of the
    /// pool Wasm identified by `pool_wasm_hash`, constructing it for `asset`
    /// in the same ledger operation (#827), and records the asset -> pool
    /// mapping so `get_pool_for_asset` (and the invoice contract, the
    /// frontend, and any other integrator) can find the instance later. The
    /// pool's `__constructor` runs as part of the create-contract call, so
    /// there is never an on-chain window in which a factory-deployed pool
    /// exists without its admin and wiring.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `asset` - The asset the new pool instance custodies. Passed to the
    ///   pool as its `funding_asset`, the single asset that instance supports.
    /// * `pool_wasm_hash` - Hash of the uploaded pool Wasm to deploy. Must
    ///   already be present in the ledger.
    /// * `invoice_contract` - The invoice contract the new pool funds invoices
    ///   from. Must be constructed.
    /// * `escrow_contract` - The escrow contract the new pool locks and settles
    ///   invoice funds through. Must already be constructed with the same
    ///   `asset`, since `pool::__constructor` cross-checks the escrow's
    ///   configured asset against its own.
    ///
    /// # Auth
    /// Requires authorization from the admin stored at construction. The
    /// deployment itself needs no authorization of its own: the new instance's
    /// deployer is this factory, which the caller has already authorized by
    /// invoking `register_asset`. The pool constructor's `admin.require_auth()`
    /// is satisfied by the same signature that authorized this call.
    ///
    /// # Instance identity
    /// The instance is deployed with this factory as the deploying contract and
    /// a salt derived from `asset`, so an asset's pool address is deterministic
    /// and independent of the deployment order. Combined with the
    /// already-registered guard, a second `register_asset` call for the same
    /// asset can never orphan the first instance.
    ///
    /// # Registry wiring
    /// The new pool's registry contract is not a parameter: it is read back from
    /// `invoice_contract` (`get_registry_contract`). The pool re-verifies
    /// issuer/buyer profiles in `fund_invoice` against the same registry the
    /// invoice contract verified them against, so deriving it keeps the two
    /// contracts from disagreeing and removes a whole class of misconfiguration
    /// (a pool pointed at a registry that is not the one its invoices were
    /// checked against would reject invoices the invoice contract had already
    /// accepted). The factory's admin becomes the new pool's admin and its
    /// initial treasury, which `pool::__constructor` explicitly allows.
    ///
    /// # Panics
    /// * `NotInitialized` if the factory has not been initialized.
    /// * `AssetAlreadyRegistered` if the asset already has a pool. Register an
    ///   asset once only; there is no replace semantics.
    /// * `InvoiceNotInitialized` if `invoice_contract` does not report a
    ///   registry contract.
    /// * `PoolError::InvalidConfiguration` if the pool rejects the resulting
    ///   address set (for example if the factory admin is also one of the
    ///   invoice/escrow/asset/registry addresses).
    /// * `PoolError::EscrowAssetMismatch` if `escrow_contract` was constructed
    ///   with a different asset than `asset`.
    ///
    /// A failure of the constructor aborts the create-contract operation, so
    /// no half-initialized instance is recorded.
    ///
    /// # Returns
    /// * `Address` - The address of the newly deployed pool instance.
    ///
    /// # Example
    /// ```ignore
    /// client.register_asset(&asset, &pool_wasm_hash, &invoice, &escrow);
    /// ```
    pub fn register_asset(
        env: Env,
        asset: Address,
        pool_wasm_hash: BytesN<32>,
        invoice_contract: Address,
        escrow_contract: Address,
    ) -> Address {
        let admin = Self::admin(&env);
        admin.require_auth();
        Self::assert_unregistered(&env, &asset);

        let registry_contract = Self::registry_of(&env, &invoice_contract);

        // Construct-and-deploy in one step (#827): the pool's __constructor
        // runs inside the create-contract ledger operation, so the instance
        // either exists fully wired or not at all. The deployer address plus
        // the salt fix the instance address, so it is known to the caller
        // before the transaction is even built.
        let pool_address = env
            .deployer()
            .with_current_contract(Self::asset_salt(&env, &asset))
            .deploy_v2(
                pool_wasm_hash,
                (
                    admin.clone(),
                    invoice_contract.clone(),
                    escrow_contract.clone(),
                    asset.clone(),
                    registry_contract,
                    admin.clone(),
                    DEFAULT_MIN_INITIAL_DEPOSIT,
                    String::from_str(&env, DEFAULT_SHARE_NAME),
                    String::from_str(&env, DEFAULT_SHARE_SYMBOL),
                    DEFAULT_SHARE_DECIMALS,
                ),
            );

        // Recorded after deployment, so a lookup can never observe a pool
        // that failed its constructor.
        Self::record_pool(&env, &asset, &pool_address);
        events::pool_instance_created(&env, &asset, &pool_address);
        pool_address
    }

    /// Registers an existing pool contract for a given asset.
    ///
    /// This function is used for migration scenarios where a pool contract
    /// already exists and needs to be registered under the factory without
    /// deploying a new instance.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `asset` - The asset address to register.
    /// * `pool_address` - The existing pool contract address.
    ///
    /// # Auth
    /// Requires authorization from the admin.
    ///
    /// # Panics
    /// * `NotInitialized` if the factory has not been initialized.
    /// * `AssetAlreadyRegistered` if the asset is already registered.
    /// * `InvalidPool` if `pool_address` does not expose a pool constructed
    ///   for `asset`.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Example
    /// ```ignore
    /// client.register_existing_pool(&usdc, &existing_pool);
    /// ```
    pub fn register_existing_pool(env: Env, asset: Address, pool_address: Address) {
        let admin = Self::admin(&env);
        admin.require_auth();
        Self::assert_unregistered(&env, &asset);

        let pool_funding_asset = env.try_invoke_contract::<Address, soroban_sdk::Error>(
            &pool_address,
            &Symbol::new(&env, "get_funding_asset"),
            Vec::new(&env),
        );
        if !matches!(pool_funding_asset, Ok(Ok(ref funding_asset)) if funding_asset == &asset) {
            panic_with_error!(&env, PoolFactoryError::InvalidPool);
        }

        Self::record_pool(&env, &asset, &pool_address);
        events::existing_pool_registered(&env, &asset, &pool_address);
    }

    /// Returns the pool instance the factory manages for an asset.
    ///
    /// This is the read-only lookup that callers (the invoice contract, the
    /// frontend, other integrators) use to find an asset's pool without
    /// re-deploying it or guessing addresses. It is the one lookup that covers
    /// both registration paths, since `register_asset` and
    /// `register_existing_pool` write the same key.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `asset` - The asset to look up.
    ///
    /// # Auth
    /// No authorization is required (read-only view).
    ///
    /// # Panics
    /// Does not panic: an unregistered asset simply has no pool.
    ///
    /// # Returns
    /// * `Option<Address>` - The asset's pool instance, or `None` if the asset
    ///   has not been registered.
    ///
    /// # Example
    /// ```ignore
    /// let pool = client.get_pool_for_asset(&usdc);
    /// ```
    pub fn get_pool_for_asset(env: Env, asset: Address) -> Option<Address> {
        env.storage().instance().get(&DataKey::PoolForAsset(asset))
    }

    /// Returns aggregated statistics across all registered pool instances.
    ///
    /// Iterates through all registered assets, queries each pool instance's
    /// `get_stats`, and returns a vector pairing each responsive pool address
    /// with its current `PoolStats`. Pools whose call fails are skipped so one
    /// unavailable or invalid pool cannot block stats for all other assets.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// No authorization is required (read-only view).
    ///
    /// # Returns
    /// * `Vec<(Address, PoolStats)>` - A list of (Pool Address, PoolStats) tuples.
    ///
    /// # Cost
    /// Resource consumption scales linearly with the number of registered assets,
    /// as a cross-contract call is made for each instance.
    pub fn get_aggregate_stats(env: Env) -> Vec<(Address, PoolStats)> {
        let assets = Self::list_assets(env.clone());
        let mut aggregate = Vec::new(&env);
        for asset in assets {
            if let Some(pool_address) = Self::get_pool_for_asset(env.clone(), asset) {
                let result = env.try_invoke_contract::<PoolStats, soroban_sdk::Error>(
                    &pool_address,
                    &Symbol::new(&env, "get_stats"),
                    Vec::new(&env),
                );
                if let Ok(Ok(stats)) = result {
                    aggregate.push_back((pool_address, stats));
                }
            }
        }
        aggregate
    }

    /// Lists all registered assets.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// No authorization is required (read-only view).
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `Vec<Address>` - A vector of all registered asset addresses.
    ///
    /// # Example
    /// ```ignore
    /// let assets = client.list_assets();
    /// ```
    pub fn list_assets(env: Env) -> Vec<Address> {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::AssetCount)
            .unwrap_or(0);

        let mut assets = Vec::new(&env);
        for i in 0..count {
            if let Some(asset) = env.storage().instance().get(&DataKey::AssetIndex(i)) {
                assets.push_back(asset);
            }
        }
        assets
    }

    /// Returns the stored admin, panicking if the factory is not initialized.
    fn admin(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, PoolFactoryError::NotInitialized))
    }

    /// Panics if `asset` already has a pool registered.
    ///
    /// Guards both registration paths so neither can silently repoint an
    /// asset at a different pool and orphan the instance (and any LP position)
    /// behind the previous mapping.
    fn assert_unregistered(env: &Env, asset: &Address) {
        if env
            .storage()
            .instance()
            .has(&DataKey::PoolForAsset(asset.clone()))
        {
            panic_with_error!(env, PoolFactoryError::AssetAlreadyRegistered);
        }
    }

    /// Records `pool_address` as `asset`'s pool and appends `asset` to the
    /// enumeration index used by `list_assets`.
    fn record_pool(env: &Env, asset: &Address, pool_address: &Address) {
        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::AssetCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::AssetCount, &(count + 1));
        env.storage()
            .instance()
            .set(&DataKey::AssetIndex(count), asset);
        env.storage()
            .instance()
            .set(&DataKey::PoolForAsset(asset.clone()), pool_address);
    }

    /// Derives the per-asset deploy salt.
    ///
    /// Hashing the asset's XDR encoding keeps the salt deterministic (so the
    /// pool address for an asset is stable and predictable off-chain) while
    /// staying distinct for every asset, including account-addressed assets
    /// that do not fit in a 32-byte contract address.
    fn asset_salt(env: &Env, asset: &Address) -> BytesN<32> {
        let digest = env.crypto().sha256(&asset.clone().to_xdr(env));
        BytesN::from_array(env, &digest.to_array())
    }

    /// Reads back the registry contract that `invoice_contract` was wired to.
    ///
    /// The factory derives the new pool's registry from the invoice contract
    /// rather than taking it as a parameter, so the pool and the invoice
    /// contract can never disagree about which profile registry they check
    /// against (see the "Registry wiring" section on `register_asset`).
    fn registry_of(env: &Env, invoice_contract: &Address) -> Address {
        let registry_contract: Option<Address> = env.invoke_contract(
            invoice_contract,
            &Symbol::new(env, INVOICE_GET_REGISTRY),
            Vec::new(env),
        );
        registry_contract
            .unwrap_or_else(|| panic_with_error!(env, PoolFactoryError::InvoiceNotInitialized))
    }
}
