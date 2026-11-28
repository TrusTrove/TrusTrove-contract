//! Storage-migration fixture (#1207): old-Wasm state must survive an upgrade.
//!
//! Inputs come from two directories of compiled contracts, named
//! `trusttrove_{registry,invoice,escrow,pool}.wasm`:
//!
//! * `TRUSTTROVE_V1_WASM_DIR`: the baseline build (what is deployed today).
//! * `TRUSTTROVE_CURRENT_WASM_DIR`: the candidate build (the upgrade target).
//!
//! `scripts/migration_fixture.sh` builds both and sets the variables. When they
//! are unset the tests skip with a message so a plain `cargo test --workspace`
//! stays green on machines without a wasm toolchain; setting
//! `TRUSTTROVE_REQUIRE_MIGRATION` (as CI does) turns a missing input into a
//! failure instead.
//!
//! The write phase talks to the baseline contracts through the current typed
//! clients, so it only uses entry points whose signatures are unchanged
//! between baseline and candidate. If a change intentionally alters one of
//! them, update this fixture in the same pull request and say so.

use std::{env, fs, path::PathBuf};

use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, MockAuth, MockAuthInvoke},
    Address, BytesN, Env, IntoVal, Map, String as SorobanString,
};
use trusttrove_escrow::EscrowContractClient;
use trusttrove_invoice::InvoiceContractClient;
use trusttrove_pool::{PoolContractClient, DEFAULT_MIN_INITIAL_DEPOSIT, DEFAULT_SHARE_DECIMALS};
use trusttrove_registry::{RegistryContractClient, Role};

const V1_DIR_VAR: &str = "TRUSTTROVE_V1_WASM_DIR";
const CURRENT_DIR_VAR: &str = "TRUSTTROVE_CURRENT_WASM_DIR";
const REQUIRE_VAR: &str = "TRUSTTROVE_REQUIRE_MIGRATION";

const LOCK_AMOUNT: u128 = 1_000_000_000;
const FACE_VALUE: u128 = 10_000_000_000;
const LP_DEPOSIT: u128 = 50_000_000;

// --------------- Mock USDC token ---------------

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

// --------------- Wasm inputs ---------------

struct WasmSet {
    registry: Vec<u8>,
    invoice: Vec<u8>,
    escrow: Vec<u8>,
    pool: Vec<u8>,
}

fn load_set(var: &str) -> Result<WasmSet, String> {
    let dir = env::var(var).map_err(|_| format!("{var} is not set"))?;
    let read = |name: &str| {
        let path = PathBuf::from(&dir).join(format!("trusttrove_{name}.wasm"));
        fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))
    };
    Ok(WasmSet {
        registry: read("registry")?,
        invoice: read("invoice")?,
        escrow: read("escrow")?,
        pool: read("pool")?,
    })
}

/// Returns the baseline and candidate Wasm, or `None` to skip the test.
fn fixture_inputs() -> Option<(WasmSet, WasmSet)> {
    match (load_set(V1_DIR_VAR), load_set(CURRENT_DIR_VAR)) {
        (Ok(v1), Ok(current)) => Some((v1, current)),
        (v1, current) => {
            let reasons: Vec<String> = [v1.err(), current.err()].into_iter().flatten().collect();
            let reason = reasons.join("; ");
            assert!(
                env::var(REQUIRE_VAR).is_err(),
                "storage-migration inputs missing: {reason}"
            );
            eprintln!("skipping storage-migration fixture: {reason}");
            None
        }
    }
}

// --------------- Deployment of the baseline (v1) contracts ---------------

struct Deployment {
    env: Env,
    admin: Address,
    usdc: Address,
    registry: Address,
    invoice: Address,
    escrow: Address,
    pool: Address,
    issuer: Address,
    buyer: Address,
    lp: Address,
    invoice_id: BytesN<32>,
}

fn unlimited(env: &Env) {
    env.cost_estimate().budget().reset_unlimited();
}

fn seed_balance(env: &Env, token: &Address, holder: &Address, amount: i128) {
    env.as_contract(token, || {
        env.storage()
            .persistent()
            .set(&TKey(holder.clone()), &amount);
    });
}

/// Deploys the baseline Wasm and writes representative state through it.
fn deploy_v1(v1: &WasmSet) -> Deployment {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();
    unlimited(&env);

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);
    let usdc = env.register(MockToken, ());

    let registry = env.register(v1.registry.as_slice(), (admin.clone(),));
    let pool = Address::generate(&env);
    let invoice = env.register(v1.invoice.as_slice(), (admin.clone(), registry.clone()));
    let escrow = env.register(
        v1.escrow.as_slice(),
        (admin.clone(), pool.clone(), usdc.clone()),
    );
    env.register_at(
        &pool,
        v1.pool.as_slice(),
        (
            admin.clone(),
            invoice.clone(),
            escrow.clone(),
            usdc.clone(),
            registry.clone(),
            admin.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            SorobanString::from_str(&env, "TrusTrove USDC Pool Shares"),
            SorobanString::from_str(&env, "TT-USDC"),
            DEFAULT_SHARE_DECIMALS,
        ),
    );
    seed_balance(&env, &usdc, &lp, 100_000_000_000);
    seed_balance(&env, &usdc, &pool, 100_000_000_000);

    // Registry: two verified profiles.
    let registry_client = RegistryContractClient::new(&env, &registry);
    let metadata: Map<SorobanString, SorobanString> = Map::new(&env);
    registry_client.register_issuer(&issuer, &metadata);
    registry_client.register_buyer(&buyer, &metadata);
    registry_client.verify_profile(&admin, &issuer, &true);
    registry_client.verify_profile(&admin, &buyer, &true);

    // Invoice: wiring plus one invoice and its per-party index entries.
    let invoice_client = InvoiceContractClient::new(&env, &invoice);
    invoice_client.add_supported_asset(&usdc);
    invoice_client.set_pool_contract(&pool);
    invoice_client.set_escrow_contract(&escrow);
    let due_date = env.ledger().timestamp() + 30 * 86_400;
    let invoice_id = invoice_client.create(&issuer, &buyer, &FACE_VALUE, &due_date, &usdc);

    // Escrow: a pinned invoice reference and one active lock with history.
    let escrow_client = EscrowContractClient::new(&env, &escrow);
    escrow_client.set_invoice_contract(&invoice);
    escrow_client.lock(&invoice_id, &LOCK_AMOUNT, &issuer);

    // Pool: one LP position.
    PoolContractClient::new(&env, &pool).deposit(&lp, &LP_DEPOSIT);

    Deployment {
        env,
        admin,
        usdc,
        registry,
        invoice,
        escrow,
        pool,
        issuer,
        buyer,
        lp,
        invoice_id,
    }
}

// --------------- Reads ---------------

/// Debug renderings of representative reads, one string per contract. The
/// contract types do not all derive `PartialEq`, but their `Debug` output is
/// deterministic, so equal strings mean equal decoded values.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    registry: String,
    invoice: String,
    escrow: String,
    pool: String,
}

fn snapshot(d: &Deployment) -> Snapshot {
    unlimited(&d.env);
    let registry = RegistryContractClient::new(&d.env, &d.registry);
    let invoice = InvoiceContractClient::new(&d.env, &d.invoice);
    let escrow = EscrowContractClient::new(&d.env, &d.escrow);
    let pool = PoolContractClient::new(&d.env, &d.pool);

    Snapshot {
        registry: format!(
            "{:?} | {:?} | {:?} | {:?} | {:?} | issuers={} buyers={}",
            registry.get_profile(&d.issuer),
            registry.get_profile(&d.buyer),
            registry.get_verification_status(&d.issuer),
            registry.get_role(&d.buyer),
            registry.is_verified(&d.buyer),
            registry.get_profile_count(&Role::Issuer),
            registry.get_profile_count(&Role::Buyer),
        ),
        invoice: format!(
            "{:?} | status={} face={} | by_issuer={:?} | count={} | asset={} | {:?} | {:?}",
            invoice.get(&d.invoice_id),
            invoice.get_status(&d.invoice_id),
            invoice.get_face_value(&d.invoice_id),
            invoice.get_by_issuer(&d.issuer, &0u32, &10u32),
            invoice.get_invoice_count_by_issuer(&d.issuer),
            invoice.is_supported_asset(&d.usdc),
            invoice.get_pool_contract(),
            invoice.get_escrow_contract(),
        ),
        escrow: format!(
            "{:?} | locked={} at={} ledger={} | {:?} | {:?} | {:?}",
            escrow.get_lock_record(&d.invoice_id),
            escrow.get_locked(&d.invoice_id),
            escrow.get_locked_at(&d.invoice_id),
            escrow.get_locked_ledger(&d.invoice_id),
            escrow.get_history(&d.invoice_id),
            escrow.get_invoice_contract(),
            escrow.get_usdc_asset(),
        ),
        pool: format!(
            "{:?} | {:?} | supply={} lp_shares={} price={}",
            pool.get_lp_position(&d.lp),
            pool.get_stats(),
            pool.total_supply(),
            pool.balance(&d.lp),
            pool.get_share_price(),
        ),
    }
}

// --------------- Upgrade ---------------

fn allow_all_auths(env: &Env) {
    env.mock_all_auths_allowing_non_root_auth();
}

/// Uploads the candidate Wasm and returns its hashes as
/// `[registry, invoice, escrow, pool]`.
fn upload_current(d: &Deployment, current: &WasmSet) -> [BytesN<32>; 4] {
    unlimited(&d.env);
    let deployer = d.env.deployer();
    [
        deployer.upload_contract_wasm(current.registry.as_slice()),
        deployer.upload_contract_wasm(current.invoice.as_slice()),
        deployer.upload_contract_wasm(current.escrow.as_slice()),
        deployer.upload_contract_wasm(current.pool.as_slice()),
    ]
}

fn upgrade_all(d: &Deployment, hashes: &[BytesN<32>; 4]) {
    unlimited(&d.env);
    RegistryContractClient::new(&d.env, &d.registry).upgrade(&hashes[0]);
    InvoiceContractClient::new(&d.env, &d.invoice).upgrade(&hashes[1]);
    EscrowContractClient::new(&d.env, &d.escrow).upgrade(&hashes[2]);
    PoolContractClient::new(&d.env, &d.pool).upgrade(&hashes[3]);
}

// --------------- Tests ---------------

/// Success path: every baseline entry decodes identically after the upgrade,
/// and the upgraded contracts accept new writes next to the old state.
#[test]
fn v1_state_survives_wasm_upgrade_and_stays_writable() {
    let Some((v1, current)) = fixture_inputs() else {
        return;
    };
    let d = deploy_v1(&v1);
    let before = snapshot(&d);

    let hashes = upload_current(&d, &current);
    upgrade_all(&d, &hashes);

    assert_eq!(snapshot(&d), before, "upgrade changed decoded v1 state");

    // New writes land next to the old entries without disturbing them.
    unlimited(&d.env);
    let registry = RegistryContractClient::new(&d.env, &d.registry);
    let invoice = InvoiceContractClient::new(&d.env, &d.invoice);
    let escrow = EscrowContractClient::new(&d.env, &d.escrow);
    let pool = PoolContractClient::new(&d.env, &d.pool);

    let metadata: Map<SorobanString, SorobanString> = Map::new(&d.env);
    let second_issuer = Address::generate(&d.env);
    registry.register_issuer(&second_issuer, &metadata);
    assert_eq!(registry.get_profile_count(&Role::Issuer), 2);

    let due_date = d.env.ledger().timestamp() + 30 * 86_400;
    let second_invoice = invoice.create(&d.issuer, &d.buyer, &(FACE_VALUE + 1), &due_date, &d.usdc);
    assert_ne!(second_invoice, d.invoice_id);
    assert_eq!(invoice.get_invoice_count_by_issuer(&d.issuer), 2);

    escrow.lock(&second_invoice, &LOCK_AMOUNT, &d.issuer);
    assert_eq!(escrow.get_locked(&second_invoice), LOCK_AMOUNT);
    assert_eq!(escrow.get_locked(&d.invoice_id), LOCK_AMOUNT);

    // Versioned history (#1295): pre-upgrade v1 history decodes with
    // ledger_sequence == 0, while post-upgrade writes record the current ledger.
    let v1_history = escrow.get_history_v2(&d.invoice_id);
    assert_eq!(v1_history.len(), 1);
    assert_eq!(v1_history.get(0).unwrap().ledger_sequence, 0);
    let v2_history = escrow.get_history_v2(&second_invoice);
    assert_eq!(v2_history.len(), 1);
    assert_eq!(
        v2_history.get(0).unwrap().ledger_sequence,
        d.env.ledger().sequence()
    );

    let shares_before = pool.balance(&d.lp);
    pool.deposit(&d.lp, &LP_DEPOSIT);
    assert!(pool.balance(&d.lp) > shares_before);
}

/// Authorization and invalid input: a non-admin signer and an unknown Wasm
/// hash are both rejected for every contract, and nothing changes.
#[test]
fn upgrade_rejects_non_admin_and_unknown_wasm() {
    let Some((v1, current)) = fixture_inputs() else {
        return;
    };
    let d = deploy_v1(&v1);
    let before = snapshot(&d);
    let hashes = upload_current(&d, &current);
    let unknown = BytesN::from_array(&d.env, &[0xAB; 32]);
    let non_admin = Address::generate(&d.env);

    let registry = RegistryContractClient::new(&d.env, &d.registry);
    let invoice = InvoiceContractClient::new(&d.env, &d.invoice);
    let escrow = EscrowContractClient::new(&d.env, &d.escrow);
    let pool = PoolContractClient::new(&d.env, &d.pool);

    let targets = [&d.registry, &d.invoice, &d.escrow, &d.pool];
    for (index, target) in targets.into_iter().enumerate() {
        d.env.mock_auths(&[MockAuth {
            address: &non_admin,
            invoke: &MockAuthInvoke {
                contract: target,
                fn_name: "upgrade",
                args: (hashes[index].clone(),).into_val(&d.env),
                sub_invokes: &[],
            },
        }]);
        let rejected = match index {
            0 => registry.try_upgrade(&hashes[0]).is_err(),
            1 => invoice.try_upgrade(&hashes[1]).is_err(),
            2 => escrow.try_upgrade(&hashes[2]).is_err(),
            _ => pool.try_upgrade(&hashes[3]).is_err(),
        };
        assert!(rejected, "non-admin upgrade accepted by contract {index}");
    }

    allow_all_auths(&d.env);
    assert!(registry.try_upgrade(&unknown).is_err());
    assert!(invoice.try_upgrade(&unknown).is_err());
    assert!(escrow.try_upgrade(&unknown).is_err());
    assert!(pool.try_upgrade(&unknown).is_err());

    assert_eq!(snapshot(&d), before, "a rejected upgrade changed state");
    assert_eq!(d.admin, registry.get_admin());
}

/// Boundary and idempotency: upgrading to the same Wasm twice, and upgrading
/// to the Wasm already running, leaves every read unchanged.
#[test]
fn repeated_upgrade_with_same_wasm_is_idempotent() {
    let Some((v1, current)) = fixture_inputs() else {
        return;
    };
    let d = deploy_v1(&v1);
    let before = snapshot(&d);
    let hashes = upload_current(&d, &current);

    upgrade_all(&d, &hashes);
    let after_first = snapshot(&d);
    upgrade_all(&d, &hashes);
    let after_second = snapshot(&d);

    assert_eq!(after_first, before);
    assert_eq!(after_second, after_first);
}
