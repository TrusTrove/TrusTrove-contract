extern crate std;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use soroban_sdk::{
    testutils::{Address as _, Events as _},
    Address, Bytes, BytesN, Env, Symbol, TryFromVal,
};
use trusttrove_escrow::EscrowContractClient;
use trusttrove_invoice::InvoiceContractClient;
use trusttrove_pool::PoolContractClient;

use crate::{DataKey, PoolFactoryContract, PoolFactoryContractClient};

/// Everything a test needs to drive the factory, already wired together: an
/// initialized factory plus the invoice/escrow/registry contracts and the pool
/// Wasm hash that `register_asset` deploys.
struct TestEnv {
    env: Env,
    factory: PoolFactoryContractClient<'static>,
    factory_id: Address,
    admin: Address,
    asset: Address,
    invoice_id: Address,
    escrow_id: Address,
    registry_id: Address,
    pool_wasm_hash: BytesN<32>,
}

fn setup() -> TestEnv {
    let env = Env::default();
    // `register_asset` requires the factory admin's authorization at the root
    // invocation *and* the freshly deployed pool requires that same admin's
    // authorization from a nested invocation, which only the non-root-capable
    // mock authorizes.
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let asset = Address::generate(&env);
    // The pool only consults the registry in `fund_invoice`, so an address is
    // enough here; `register_asset` reads it back off the invoice contract
    // rather than taking it as a parameter.
    let registry_id = Address::generate(&env);

    // Real invoice and escrow contracts: the pool's `initialize` cross-checks
    // the escrow's configured asset and the factory reads the invoice's
    // registry, so both have to be real contracts for the round trip to work.
    let invoice_id = env.register_contract(None, trusttrove_invoice::InvoiceContract);
    let invoice = InvoiceContractClient::new(&env, &invoice_id);
    invoice.initialize(&admin, &registry_id);

    let escrow_id = new_escrow(&env, &admin, &asset);

    let factory_id = env.register_contract(None, PoolFactoryContract);
    let factory = PoolFactoryContractClient::new(&env, &factory_id);
    factory.initialize(&admin);

    let pool_wasm_hash = env
        .deployer()
        .upload_contract_wasm(Bytes::from_slice(&env, pool_wasm()));

    TestEnv {
        env,
        factory,
        factory_id,
        admin,
        asset,
        invoice_id,
        escrow_id,
        registry_id,
        pool_wasm_hash,
    }
}

/// Deploys and initializes an escrow contract for `asset`.
///
/// The escrow's stored pool address is irrelevant to the factory flow (the
/// factory only deploys and wires pools, it never moves funds), so a generated
/// address stands in for it.
fn new_escrow(env: &Env, admin: &Address, asset: &Address) -> Address {
    let escrow_id = env.register_contract(None, trusttrove_escrow::EscrowContract);
    let escrow = EscrowContractClient::new(env, &escrow_id);
    escrow.initialize(admin, &Address::generate(env), asset);
    escrow_id
}

fn register_asset(te: &TestEnv, asset: &Address) -> Address {
    let escrow_id = if *asset == te.asset {
        te.escrow_id.clone()
    } else {
        new_escrow(&te.env, &te.admin, asset)
    };
    te.factory
        .register_asset(asset, &te.pool_wasm_hash, &te.invoice_id, &escrow_id)
}

/// The `trusttrove_pool.wasm` artifact that `register_asset` deploys.
///
/// `register_asset` deploys through `Env::deployer()` and then invokes
/// `initialize` on the fresh instance. The SDK 21 test host can only dispatch
/// that call when the instance was created from real Wasm: an instance created
/// from a placeholder Wasm has no exports, so the `initialize` call traps with
/// `Error(Context, InvalidAction)`. The deploy-and-initialize tests therefore
/// upload the actual pool artifact rather than a stub.
///
/// It is resolved on first use instead of being committed to the repository
/// (a ~50 KiB binary that would also go stale whenever the pool's ABI
/// changes). Resolution order:
///
/// 1. reuse the workspace release artifact when it is already there, which is
///    what `cargo build --workspace --release --target wasm32v1-none`
///    (and therefore CI) produces, otherwise
/// 2. build it into `target/pool-factory-test-fixtures`, a dedicated target
///    directory so the slow (`lto = true`) Wasm build is cached across
///    `cargo test` runs. It cannot share the outer `target/`, because the outer
///    cargo holds an exclusive lock on that directory while this test runs.
fn pool_wasm() -> &'static [u8] {
    static WASM: OnceLock<std::vec::Vec<u8>> = OnceLock::new();
    WASM.get_or_init(|| {
        const WASM_FILE: &str = "trusttrove_pool.wasm";

        let manifest_dir =
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
        let workspace_dir = manifest_dir
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");

        let release_artifact = workspace_dir
            .join("target/wasm32v1-none/release")
            .join(WASM_FILE);
        let path = if release_artifact.exists() {
            release_artifact
        } else {
            let fixture_dir = workspace_dir.join("target/pool-factory-test-fixtures");
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let status = Command::new(cargo)
                .current_dir(workspace_dir)
                .args([
                    "build",
                    "--package",
                    "trusttrove-pool",
                    "--release",
                    "--target",
                    "wasm32v1-none",
                    "--target-dir",
                ])
                .arg(&fixture_dir)
                // The fixture is a plain Wasm build and must not inherit the
                // outer build's rustflags: under `cargo tarpaulin` (the
                // `coverage` CI job) they carry `-C instrument-coverage` plus
                // an `--extern profiler_builtins` that only exists for the host
                // target, which fails the Wasm build outright.
                .env_remove("RUSTFLAGS")
                .env_remove("CARGO_ENCODED_RUSTFLAGS")
                .env_remove("RUSTDOCFLAGS")
                .status()
                .expect("failed to run cargo to build the trusttrove-pool Wasm");
            assert!(
                status.success(),
                "building the trusttrove-pool Wasm failed ({status}); the \
                 wasm32v1-none target is required, install it with \
                 `rustup target add wasm32v1-none`"
            );
            fixture_dir.join("wasm32v1-none/release").join(WASM_FILE)
        };

        std::fs::read(&path).unwrap_or_else(|e| panic!("reading pool Wasm {}: {e}", path.display()))
    })
}

#[test]
fn test_initialize() {
    let env = Env::default();
    env.mock_all_auths();
    let factory_id = env.register_contract(None, PoolFactoryContract);
    let factory_client = PoolFactoryContractClient::new(&env, &factory_id);

    let admin = Address::generate(&env);
    factory_client.initialize(&admin);

    // Verify admin is stored
    env.as_contract(&factory_id, || {
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        assert_eq!(stored_admin, admin);
    });
}

#[test]
#[should_panic(expected = "Error(Contract, #1)")]
fn test_initialize_twice_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let factory_id = env.register_contract(None, PoolFactoryContract);
    let factory_client = PoolFactoryContractClient::new(&env, &factory_id);

    let admin = Address::generate(&env);
    factory_client.initialize(&admin);
    factory_client.initialize(&admin);
}

#[test]
fn test_register_asset_deploys_and_initializes_pool() {
    let te = setup();

    let pool_address = register_asset(&te, &te.asset.clone());

    // The factory recorded the mapping and the enumeration index.
    assert_eq!(
        te.factory.get_pool_for_asset(&te.asset),
        Some(pool_address.clone())
    );
    let assets = te.factory.list_assets();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets.get(0), Some(te.asset.clone()));

    // The deployed instance is a live pool contract, initialized for this asset
    // and wired to the invoice/escrow the caller passed in.
    let pool = PoolContractClient::new(&te.env, &pool_address);
    assert_eq!(pool.get_usdc_asset(), te.asset);
    assert_eq!(pool.get_invoice_contract(), te.invoice_id);
    assert_eq!(pool.get_escrow_contract(), te.escrow_id);
    assert_eq!(pool.get_admin(), te.admin);
    assert_eq!(pool.get_treasury(), te.admin);

    // The registry the new pool verifies issuer/buyer profiles against is the
    // one the invoice contract itself uses, read back off the pool's storage
    // because the pool exposes no public registry getter.
    let registry: Address = te
        .env
        .as_contract(&pool_address, || {
            te.env
                .storage()
                .instance()
                .get(&trusttrove_pool::DataKey::RegistryContract)
        })
        .expect("pool instance should record a registry contract");
    assert_eq!(registry, te.registry_id);
}

#[test]
fn test_register_asset_emits_pool_instance_created_event() {
    let te = setup();

    let pool_address = register_asset(&te, &te.asset.clone());

    let events = te.env.events().all();
    let (contract, topics, data) = events.get(events.len() - 1).unwrap();
    assert_eq!(contract, te.factory_id);
    assert_eq!(topics.len(), 2);
    assert_eq!(
        Symbol::try_from_val(&te.env, &topics.get(0).unwrap()).unwrap(),
        Symbol::new(&te.env, "pool_instance_created")
    );
    assert_eq!(
        Address::try_from_val(&te.env, &topics.get(1).unwrap()).unwrap(),
        te.asset
    );
    assert_eq!(Address::try_from_val(&te.env, &data).unwrap(), pool_address);
}

#[test]
fn test_register_asset_pool_address_is_deterministic() {
    let te = setup();

    // The instance address is fixed by (deploying contract, salt), and the salt
    // is derived from the asset alone, so a caller can predict the pool address
    // for an asset before building the transaction.
    let salt = PoolFactoryContract::asset_salt(&te.env, &te.asset);
    let predicted = te
        .env
        .deployer()
        .with_address(te.factory_id.clone(), salt)
        .deployed_address();

    let pool_address = register_asset(&te, &te.asset.clone());

    assert_eq!(pool_address, predicted);
}

#[test]
fn test_register_asset_gives_each_asset_its_own_pool() {
    let te = setup();
    let other_asset = Address::generate(&te.env);

    let pool = register_asset(&te, &te.asset.clone());
    let other_pool = register_asset(&te, &other_asset);

    // Instances never share state, and each asset resolves to its own pool.
    assert_ne!(pool, other_pool);
    assert_eq!(te.factory.get_pool_for_asset(&te.asset), Some(pool));
    assert_eq!(
        te.factory.get_pool_for_asset(&other_asset),
        Some(other_pool.clone())
    );
    assert_eq!(
        PoolContractClient::new(&te.env, &other_pool).get_usdc_asset(),
        other_asset
    );

    let assets = te.factory.list_assets();
    assert_eq!(assets.len(), 2);
    assert_eq!(assets.get(0), Some(te.asset.clone()));
    assert_eq!(assets.get(1), Some(other_asset));
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_register_asset_without_admin_auth_panics() {
    let te = setup();
    // Drop every mocked authorization: only the factory's stored admin may
    // register an asset and have the factory deploy a pool on its behalf.
    te.env.set_auths(&[]);
    register_asset(&te, &te.asset.clone());
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_register_asset_duplicate_panics() {
    let te = setup();
    register_asset(&te, &te.asset.clone());
    // Re-registering would silently repoint the asset at a second instance and
    // orphan the first one, so the second call is rejected.
    register_asset(&te, &te.asset.clone());
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_register_asset_with_uninitialized_invoice_panics() {
    let te = setup();
    // An invoice contract that was never initialized reports no registry, so
    // the pool cannot be wired and registration is rejected rather than
    // deploying a pool pointed at nothing.
    let uninitialized_invoice = te
        .env
        .register_contract(None, trusttrove_invoice::InvoiceContract);

    te.factory.register_asset(
        &te.asset,
        &te.pool_wasm_hash,
        &uninitialized_invoice,
        &te.escrow_id,
    );
}

#[test]
fn test_register_existing_pool() {
    let env = Env::default();
    env.mock_all_auths();
    let factory_id = env.register_contract(None, PoolFactoryContract);
    let factory_client = PoolFactoryContractClient::new(&env, &factory_id);

    let admin = Address::generate(&env);
    factory_client.initialize(&admin);

    let asset = Address::generate(&env);
    let pool_address = Address::generate(&env);

    factory_client.register_existing_pool(&asset, &pool_address);

    // Verify registration
    env.as_contract(&factory_id, || {
        let stored_pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolForAsset(asset.clone()))
            .unwrap();
        assert_eq!(stored_pool, pool_address);

        let count: u32 = env.storage().instance().get(&DataKey::AssetCount).unwrap();
        assert_eq!(count, 1);

        let indexed_asset: Address = env
            .storage()
            .instance()
            .get(&DataKey::AssetIndex(0))
            .unwrap();
        assert_eq!(indexed_asset, asset);
    });
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_register_existing_pool_duplicate_panics() {
    let env = Env::default();
    env.mock_all_auths();
    let factory_id = env.register_contract(None, PoolFactoryContract);
    let factory_client = PoolFactoryContractClient::new(&env, &factory_id);

    let admin = Address::generate(&env);
    factory_client.initialize(&admin);

    let asset = Address::generate(&env);
    let pool_address = Address::generate(&env);

    factory_client.register_existing_pool(&asset, &pool_address);
    factory_client.register_existing_pool(&asset, &pool_address);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_register_existing_pool_after_register_asset_panics() {
    let te = setup();
    // The two registration paths share one mapping, so an asset deployed by
    // `register_asset` cannot be silently repointed at another pool.
    register_asset(&te, &te.asset.clone());
    te.factory
        .register_existing_pool(&te.asset, &Address::generate(&te.env));
}

#[test]
fn test_get_pool_for_asset_returns_deployed_pool() {
    let te = setup();
    let pool_address = register_asset(&te, &te.asset.clone());

    assert_eq!(te.factory.get_pool_for_asset(&te.asset), Some(pool_address));
}

#[test]
fn test_get_pool_for_asset_returns_migrated_pool() {
    let te = setup();
    let existing_pool = Address::generate(&te.env);
    te.factory.register_existing_pool(&te.asset, &existing_pool);

    // `register_existing_pool` writes the same key, so an asset adopted during
    // the migration resolves through the same lookup.
    assert_eq!(
        te.factory.get_pool_for_asset(&te.asset),
        Some(existing_pool)
    );
}

#[test]
fn test_get_pool_for_asset_unregistered_returns_none() {
    let te = setup();
    assert_eq!(te.factory.get_pool_for_asset(&te.asset), None);
    assert_eq!(
        te.factory.get_pool_for_asset(&Address::generate(&te.env)),
        None
    );
}

#[test]
fn test_get_pool_for_asset_requires_no_auth() {
    let te = setup();
    let pool_address = register_asset(&te, &te.asset.clone());
    te.env.set_auths(&[]);

    // Read-only view: no authorization is required, even with every mocked
    // authorization dropped.
    assert_eq!(te.factory.get_pool_for_asset(&te.asset), Some(pool_address));
}

#[test]
fn test_list_assets_empty() {
    let env = Env::default();
    let factory_id = env.register_contract(None, PoolFactoryContract);
    let factory_client = PoolFactoryContractClient::new(&env, &factory_id);

    let assets = factory_client.list_assets();
    assert_eq!(assets.len(), 0);
}

#[test]
fn test_gas_benchmark_register_existing_pool() {
    extern crate std;
    let env = Env::default();
    env.mock_all_auths();
    let factory_id = env.register_contract(None, PoolFactoryContract);
    let factory_client = PoolFactoryContractClient::new(&env, &factory_id);

    let admin = Address::generate(&env);
    factory_client.initialize(&admin);

    let asset = Address::generate(&env);
    let pool_address = Address::generate(&env);

    // Measure register_existing_pool resource cost
    env.budget().reset_default();
    let cpu_before = env.budget().cpu_instruction_cost();
    let mem_before = env.budget().memory_bytes_cost();

    factory_client.register_existing_pool(&asset, &pool_address);

    let cpu_after = env.budget().cpu_instruction_cost();
    let mem_after = env.budget().memory_bytes_cost();

    let cpu_delta = cpu_after - cpu_before;
    let mem_delta = mem_after - mem_before;

    // Log for manual inspection
    std::eprintln!("register_existing_pool CPU instructions: {}", cpu_delta);
    std::eprintln!("register_existing_pool Memory bytes: {}", mem_delta);
}

#[test]
fn test_list_assets_single() {
    let env = Env::default();
    let factory_id = env.register_contract(None, PoolFactoryContract);
    let factory_client = PoolFactoryContractClient::new(&env, &factory_id);

    // Simulate asset registration by setting the index directly
    let asset = Address::generate(&env);
    env.as_contract(&factory_id, || {
        env.storage().instance().set(&DataKey::AssetCount, &1u32);
        env.storage()
            .instance()
            .set(&DataKey::AssetIndex(0), &asset);
    });

    let assets = factory_client.list_assets();
    assert_eq!(assets.len(), 1);
    assert_eq!(assets.get(0), Some(asset));
}

#[test]
fn test_list_assets_multiple() {
    let env = Env::default();
    let factory_id = env.register_contract(None, PoolFactoryContract);
    let factory_client = PoolFactoryContractClient::new(&env, &factory_id);

    // Simulate multiple asset registrations
    let asset1 = Address::generate(&env);
    let asset2 = Address::generate(&env);
    let asset3 = Address::generate(&env);

    env.as_contract(&factory_id, || {
        env.storage().instance().set(&DataKey::AssetCount, &3u32);
        env.storage()
            .instance()
            .set(&DataKey::AssetIndex(0), &asset1);
        env.storage()
            .instance()
            .set(&DataKey::AssetIndex(1), &asset2);
        env.storage()
            .instance()
            .set(&DataKey::AssetIndex(2), &asset3);
    });

    let assets = factory_client.list_assets();
    assert_eq!(assets.len(), 3);
    assert_eq!(assets.get(0), Some(asset1));
    assert_eq!(assets.get(1), Some(asset2));
    assert_eq!(assets.get(2), Some(asset3));
}

// --------------- Mock Token for Integration Tests ---------------

use soroban_sdk::{contract, contractimpl, contracttype};

#[contract]
pub struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let from_key = TKey(from.clone());
        let to_key = TKey(to.clone());
        let from_bal: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        let to_bal: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&from_key, &(from_bal - amount));
        env.storage().persistent().set(&to_key, &(to_bal + amount));
    }

    pub fn balance(env: Env, addr: Address) -> i128 {
        env.storage().persistent().get(&TKey(addr)).unwrap_or(0)
    }
}

#[contracttype]
pub struct TKey(Address);

// --------------- New Tests ---------------

#[test]
fn test_pool_instances_do_not_cross_contaminate() {
    let te = setup();

    // Register two actual mock tokens
    let asset1 = te.env.register_contract(None, MockToken);
    let asset2 = te.env.register_contract(None, MockToken);

    let pool1 = register_asset(&te, &asset1);
    let pool2 = register_asset(&te, &asset2);

    let lp1 = Address::generate(&te.env);
    let lp2 = Address::generate(&te.env);

    // Give LP1 and LP2 some mock tokens
    let lp1_bal_key = TKey(lp1.clone());
    let lp2_bal_key = TKey(lp2.clone());
    te.env.as_contract(&asset1, || {
        te.env
            .storage()
            .persistent()
            .set(&lp1_bal_key, &1_000_000_000_i128);
    });
    te.env.as_contract(&asset2, || {
        te.env
            .storage()
            .persistent()
            .set(&lp2_bal_key, &2_000_000_000_i128);
    });

    let pool1_client = PoolContractClient::new(&te.env, &pool1);
    let pool2_client = PoolContractClient::new(&te.env, &pool2);

    // LP1 deposits 20_000_000 into pool1
    pool1_client.deposit(&lp1, &20_000_000);

    // LP2 deposits 30_000_000 into pool2
    pool2_client.deposit(&lp2, &30_000_000);

    let stats1 = pool1_client.get_stats();
    let stats2 = pool2_client.get_stats();

    // Verify no cross contamination
    assert_eq!(stats1.total_deposits, 20_000_000);
    assert_eq!(stats1.total_shares, 20_000_000);

    assert_eq!(stats2.total_deposits, 30_000_000);
    assert_eq!(stats2.total_shares, 30_000_000);
}

#[test]
fn test_get_aggregate_stats() {
    let te = setup();

    let asset1 = te.env.register_contract(None, MockToken);
    let asset2 = te.env.register_contract(None, MockToken);

    let pool1 = register_asset(&te, &asset1);
    let pool2 = register_asset(&te, &asset2);

    let lp1 = Address::generate(&te.env);
    let lp2 = Address::generate(&te.env);

    let lp1_bal_key = TKey(lp1.clone());
    let lp2_bal_key = TKey(lp2.clone());
    te.env.as_contract(&asset1, || {
        te.env
            .storage()
            .persistent()
            .set(&lp1_bal_key, &1_000_000_000_i128);
    });
    te.env.as_contract(&asset2, || {
        te.env
            .storage()
            .persistent()
            .set(&lp2_bal_key, &2_000_000_000_i128);
    });

    let pool1_client = PoolContractClient::new(&te.env, &pool1);
    let pool2_client = PoolContractClient::new(&te.env, &pool2);

    pool1_client.deposit(&lp1, &20_000_000);
    pool2_client.deposit(&lp2, &30_000_000);

    let aggregate = te.factory.get_aggregate_stats();
    assert_eq!(aggregate.len(), 2);

    // The order depends on listing order, which is insertion order.
    // pool1 was inserted first.
    let (addr1, stat1) = aggregate.get(0).unwrap();
    let (addr2, stat2) = aggregate.get(1).unwrap();

    assert_eq!(addr1, pool1);
    assert_eq!(stat1.total_deposits, 20_000_000);

    assert_eq!(addr2, pool2);
    assert_eq!(stat2.total_deposits, 30_000_000);
}
