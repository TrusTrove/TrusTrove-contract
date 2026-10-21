//! Resource-budget benchmark harness for the protocol's hot paths (#734).
//!
//! Measures `invoice::create`, `pool::deposit`, `pool::fund_invoice` and
//! `invoice::repay` against the real registry/invoice/escrow/pool contracts
//! (via `trusttrove_test_utils::setup_full_protocol`) at several protocol
//! volumes, i.e. with N invoices already funded and outstanding, so index and
//! state growth shows up in the numbers.
//!
//! For each `(operation, volume)` it records:
//! * `cpu_insns`        — CPU instructions charged by the Soroban test host.
//! * `mem_bytes`        — memory bytes charged by the Soroban test host.
//! * `entries_written`  — ledger entries created, modified or deleted.
//! * `bytes_written`    — XDR size of every written entry (post-call state).
//! * `ttl_bumps`        — entries whose TTL changed without a data change.
//!
//! Ledger *reads* are not counted separately (the SDK 21 test host does not
//! expose a per-call footprint); their cost is included in `cpu_insns`.
//!
//! Output is a stable, sorted CSV (one row per measurement, no timestamps) so
//! two runs can be compared with a plain `diff`. It is printed to stdout and
//! written to `$BENCH_OUT` if set, otherwise `target/budget-bench.csv` at the
//! workspace root. Run with:
//!
//! ```text
//! cargo test -p trusttrove-pool bench_ -- --nocapture
//! BENCH_OUT=docs/benchmarks/budget.csv cargo test -p trusttrove-pool bench_
//! ```
//!
//! See `docs/LIMITATIONS.md` (Gas & Budget) for how the output is used.
#![cfg(test)]

extern crate std;

use std::collections::BTreeMap;
use std::format;
use std::string::String as StdString;
use std::vec::Vec as StdVec;

use soroban_sdk::xdr::{Limits, ToXdr, WriteXdr};
use soroban_sdk::{BytesN, Env};
use trusttrove_test_utils::{
    setup_full_protocol, test_agent_id, test_agent_signing_key, FullProtocol,
};

/// Protocol volumes to benchmark at: number of invoices already funded and
/// outstanding in the pool when the measured calls run.
const VOLUMES: [u32; 3] = [0, 10, 50];

const FACE_VALUE: u128 = 10_000_000_000;
const DISCOUNT_BPS: u32 = 200;
const TERM_SECS: u64 = 30 * 86_400;
/// Seed liquidity: comfortably covers `max(VOLUMES) + 1` funded invoices.
const SEED_DEPOSIT: u128 = 10_000_000_000_000;
const BENCH_DEPOSIT: u128 = 1_000_000_000;

/// Per-transaction network limits (Stellar mainnet soroban settings). Any hot
/// path exceeding these cannot be submitted at all, so the harness fails hard.
const TX_MAX_CPU_INSNS: u64 = 100_000_000;
const TX_MAX_MEM_BYTES: u64 = 41_943_040;

#[derive(Clone, Debug)]
struct Measurement {
    op: &'static str,
    volume: u32,
    cpu_insns: u64,
    mem_bytes: u64,
    entries_written: u32,
    bytes_written: u64,
    ttl_bumps: u32,
}

/// Ledger state keyed by XDR-encoded `LedgerKey` → (XDR `LedgerEntry`, TTL).
type LedgerState = BTreeMap<StdVec<u8>, (StdVec<u8>, Option<u32>)>;

fn ledger_state(env: &Env) -> LedgerState {
    env.to_snapshot()
        .ledger
        .ledger_entries
        .iter()
        .map(|(key, (entry, ttl))| {
            (
                WriteXdr::to_xdr(&**key, Limits::none()).unwrap(),
                (WriteXdr::to_xdr(&**entry, Limits::none()).unwrap(), *ttl),
            )
        })
        .collect()
}

/// Runs `f` with a freshly reset budget and returns its resource usage.
fn measure<T>(env: &Env, op: &'static str, volume: u32, f: impl FnOnce() -> T) -> (T, Measurement) {
    let before = ledger_state(env);
    // `reset_unlimited` zeroes the counters without imposing the default
    // test-host limit, so setup work never leaks into a measurement and a
    // measured call is only bounded by the explicit network limits below.
    env.budget().reset_unlimited();
    let out = f();
    let cpu_insns = env.budget().cpu_instruction_cost();
    let mem_bytes = env.budget().memory_bytes_cost();
    let after = ledger_state(env);

    let mut entries_written = 0u32;
    let mut bytes_written = 0u64;
    let mut ttl_bumps = 0u32;
    for (key, (entry, ttl)) in after.iter() {
        match before.get(key) {
            Some((old_entry, _)) if old_entry != entry => {
                entries_written += 1;
                bytes_written += entry.len() as u64;
            }
            Some((_, old_ttl)) if old_ttl != ttl => ttl_bumps += 1,
            Some(_) => {}
            None => {
                entries_written += 1;
                bytes_written += entry.len() as u64;
            }
        }
    }
    // Deleted entries are writes too (they carry no post-call bytes).
    entries_written += before.keys().filter(|k| !after.contains_key(*k)).count() as u32;

    (
        out,
        Measurement {
            op,
            volume,
            cpu_insns,
            mem_bytes,
            entries_written,
            bytes_written,
            ttl_bumps,
        },
    )
}

/// Full protocol with issuer/buyer verified and the pool seeded with
/// liquidity. Budget is unlimited for setup; `measure` resets it per call.
fn bench_protocol() -> FullProtocol {
    let p = setup_full_protocol();
    p.env.budget().reset_unlimited();
    p.registry.verify_profile(&p.admin, &p.issuer, &true);
    p.registry.verify_profile(&p.admin, &p.buyer, &true);
    p.pool.deposit(&p.lp, &SEED_DEPOSIT);
    p
}

fn attest(p: &FullProtocol, invoice_id: &BytesN<32>) {
    let env = &p.env;
    let payload = trusttrove_invoice::AttestationPayload {
        domain_separator: BytesN::from_array(
            env,
            &trusttrove_invoice::ATTESTATION_DOMAIN_SEPARATOR,
        ),
        invoice_id: invoice_id.clone(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(env, &[9u8; 32]),
        agent_id: test_agent_id(env),
        nonce: 1,
    };
    let payload_bytes = ToXdr::to_xdr(payload, env);
    let digest = env.crypto().keccak256(&payload_bytes).to_array();
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(env, &sig_bytes);
    p.invoice
        .submit_attestation(invoice_id, &payload_bytes, &signature);
}

fn create_invoice(p: &FullProtocol) -> BytesN<32> {
    let due_date = p.env.ledger().timestamp() + TERM_SECS;
    p.invoice
        .create(&p.issuer, &p.buyer, &FACE_VALUE, &due_date, &p.usdc_id)
}

fn attest_and_list(p: &FullProtocol, invoice_id: &BytesN<32>) {
    attest(p, invoice_id);
    p.invoice.list_for_financing(invoice_id, &DISCOUNT_BPS);
}

fn ship_and_confirm(p: &FullProtocol, invoice_id: &BytesN<32>) {
    p.invoice.mark_shipped(invoice_id);
    p.invoice.confirm_delivery(invoice_id, &p.issuer);
    p.invoice.confirm_delivery(invoice_id, &p.buyer);
}

/// Measures every hot path once at `volume` outstanding invoices.
fn run_at_volume(volume: u32) -> StdVec<Measurement> {
    let p = bench_protocol();

    // Pre-fill: `volume` invoices created, attested, listed and funded, left
    // outstanding so indexes, pool accounting and escrow all carry real load.
    for _ in 0..volume {
        let id = create_invoice(&p);
        attest_and_list(&p, &id);
        p.pool.fund_invoice(&id);
    }

    let mut rows = StdVec::new();

    let (_, m) = measure(&p.env, "pool::deposit", volume, || {
        p.pool.deposit(&p.lp, &BENCH_DEPOSIT)
    });
    rows.push(m);

    let (invoice_id, m) = measure(&p.env, "invoice::create", volume, || create_invoice(&p));
    rows.push(m);

    attest_and_list(&p, &invoice_id);
    let (funded, m) = measure(&p.env, "pool::fund_invoice", volume, || {
        p.pool.fund_invoice(&invoice_id)
    });
    assert!(funded);
    rows.push(m);

    ship_and_confirm(&p, &invoice_id);
    let (repaid, m) = measure(&p.env, "invoice::repay", volume, || {
        p.invoice.repay(&invoice_id)
    });
    assert!(repaid);
    rows.push(m);

    rows
}

fn to_csv(rows: &[Measurement]) -> StdString {
    let mut out =
        StdString::from("op,volume,cpu_insns,mem_bytes,entries_written,bytes_written,ttl_bumps\n");
    for r in rows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{}\n",
            r.op,
            r.volume,
            r.cpu_insns,
            r.mem_bytes,
            r.entries_written,
            r.bytes_written,
            r.ttl_bumps
        ));
    }
    out
}

fn write_report(csv: &str) {
    // `cargo test` runs with the package directory as cwd, so resolve a
    // relative `BENCH_OUT` against the workspace root instead.
    let workspace_root = format!("{}/../..", env!("CARGO_MANIFEST_DIR"));
    let path = match std::env::var("BENCH_OUT") {
        Ok(p) if std::path::Path::new(&p).is_absolute() => p,
        Ok(p) => format!("{workspace_root}/{p}"),
        Err(_) => format!("{workspace_root}/target/budget-bench.csv"),
    };
    if let Some(parent) = std::path::Path::new(&path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // Best-effort: a read-only checkout must not fail the benchmark itself.
    if let Err(e) = std::fs::write(&path, csv) {
        std::println!("bench: could not write {path}: {e}");
    } else {
        std::println!("bench: wrote {path}");
    }
}

#[test]
fn bench_core_flows_budget() {
    let mut rows: StdVec<Measurement> = VOLUMES.iter().flat_map(|v| run_at_volume(*v)).collect();
    // Deterministic order so runs diff cleanly regardless of execution order.
    rows.sort_by(|a, b| a.op.cmp(b.op).then(a.volume.cmp(&b.volume)));

    let csv = to_csv(&rows);
    std::println!("\n== BUDGET BENCHMARK (#734) ==\n{csv}=============================");
    write_report(&csv);

    for r in &rows {
        assert!(r.cpu_insns > 0, "{} @ {}: no CPU recorded", r.op, r.volume);
        assert!(
            r.mem_bytes > 0,
            "{} @ {}: no memory recorded",
            r.op,
            r.volume
        );
        assert!(
            r.entries_written > 0,
            "{} @ {}: expected ledger writes",
            r.op,
            r.volume
        );
        assert!(
            r.cpu_insns <= TX_MAX_CPU_INSNS,
            "{} @ volume {} uses {} CPU instructions, over the {} per-tx network limit",
            r.op,
            r.volume,
            r.cpu_insns,
            TX_MAX_CPU_INSNS
        );
        assert!(
            r.mem_bytes <= TX_MAX_MEM_BYTES,
            "{} @ volume {} uses {} memory bytes, over the {} per-tx network limit",
            r.op,
            r.volume,
            r.mem_bytes,
            TX_MAX_MEM_BYTES
        );
    }
    assert_eq!(rows.len(), VOLUMES.len() * 4);
}
