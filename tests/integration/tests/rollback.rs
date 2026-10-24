//! Cross-contract failure rollback coverage for the full funding and default
//! flows (issue #1017).
//!
//! The per-contract test suites pin rollback with failure *stubs*
//! (`contracts/pool/src/test.rs`'s `failing_escrow`, `failing_escrow_default`).
//! This file instead wires **real** registry, invoice, escrow and pool
//! instances through the shared `trusttrove_test_utils::setup_full_protocol`
//! fixture, drives the real `pool.fund_invoice` / `pool.handle_default` entry
//! points until a real downstream contract rejects the call mid-flight, and
//! then proves that invoice, pool and escrow all retain their exact pre-call
//! state — pool accounting, invoice record and status index, escrow records
//! and history, and every USDC balance.
//!
//! Failure triggers (no stubs, no storage surgery):
//!
//! * Funding: the invoice's issuer is the pool contract address. Funding then
//!   commits pool state, locks the principal in real escrow and marks the
//!   invoice funded — and only the final `escrow.release_to_issuer` call fails
//!   (`EscrowError::InvalidRecipient`, because escrow refuses to pay the pool
//!   itself). The failure lands at the *last* downstream call, so real state
//!   written by both earlier cross-contract calls must be rolled back.
//! * Default: an out-of-band `escrow.lock` leaves a live lock record whose
//!   `locked_at` is inside escrow's `DEFAULT_MIN_LOCK_SECONDS` (60s) grace
//!   window, so real `escrow.handle_default` fails with
//!   `EscrowError::NotAuthorized` while settling the default. Advancing the
//!   ledger past the grace window and replaying the same call succeeds,
//!   proving the fixture failure was the escrow guard, not a broken setup.
//!
//! Everything is deterministic: fixed amounts, controlled addresses, and
//! explicit ledger timestamps — no randomness, network access, or sleeps.

extern crate std;

use soroban_sdk::{
    testutils::Ledger as _, xdr::ToXdr, Address, BytesN, Map, String as SorobanString,
};

use trusttrove_invoice::InvoiceStatus;
use trusttrove_test_utils::{
    setup_full_protocol, test_agent_id, test_agent_signing_key, FullProtocol, MockTokenClient,
};

// Deterministic fixture amounts. The discount math mirrors the invoice
// contract: funded_amount = face_value * (10000 - discount_bps) / 10000.
const FACE_VALUE: u128 = 10_000_000_000;
const DISCOUNT_BPS: u32 = 200;
const FUNDED_AMOUNT: u128 = FACE_VALUE * (10000 - DISCOUNT_BPS as u128) / 10000;
const DEPOSIT: u128 = 100_000_000_000;

/// Shared fixture for the rollback scenarios: every protocol contract is the
/// real implementation. Registry profiles start unverified (#130), so the
/// issuer and buyer must be verified before `invoice.create` /
/// `list_for_financing` / `pool.fund_invoice` will accept them (same
/// post-setup step `contracts/pool/src/bench.rs` applies).
fn setup() -> FullProtocol {
    let p = setup_full_protocol();
    p.registry.verify_profile(&p.admin, &p.issuer, &true);
    p.registry.verify_profile(&p.admin, &p.buyer, &true);
    p
}

/// Submits a validly signed agent attestation for `invoice_id` so
/// `list_for_financing`'s verification gate passes (same helper shape as
/// `contracts/pool/src/bench.rs` and `tests/integration/tests/lifecycle.rs`).
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
    let payload_bytes = payload.to_xdr(env);
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

/// Creates an invoice with the given issuer (buyer is always the fixture
/// buyer), attests it, and lists it for financing at the fixture discount.
fn create_and_list(p: &FullProtocol, issuer: &Address, term_seconds: u64) -> BytesN<32> {
    let due_date = p.env.ledger().timestamp() + term_seconds;
    let invoice_id = p
        .invoice
        .create(issuer, &p.buyer, &FACE_VALUE, &due_date, &p.usdc_id);
    attest(p, &invoice_id);
    p.invoice.list_for_financing(&invoice_id, &DISCOUNT_BPS);
    invoice_id
}

/// Complete observable state of all three contracts plus every token balance
/// involved, captured through public views only. The rollback assertions
/// compare two snapshots field by field, so any contract that keeps (or
/// loses) state across the failed call is caught.
struct Snapshot {
    // Pool accounting (pool.get_stats / pool views).
    total_deposits: u128,
    total_funded: u128,
    available_liquidity: u128,
    total_yield_distributed: u128,
    total_loss_realised: u128,
    active_invoice_count: u32,
    total_shares: u128,
    max_utilization_bps: u32,
    funded_entry: Option<u128>,
    lp_shares: i128,
    // Invoice record (invoice.get / invoice views).
    invoice_status: u32,
    invoice_funded_amount: u128,
    invoice_funded_at: Option<u64>,
    invoice_funding_pool: Option<Address>,
    invoice_in_listed: bool,
    invoice_in_funded: bool,
    invoice_in_defaulted: bool,
    // Escrow records (escrow views).
    escrow_locked: u128,
    escrow_locked_at: u64,
    escrow_history_len: u32,
    escrow_was_released_to_issuer: bool,
    // USDC balances.
    pool_usdc: i128,
    escrow_usdc: i128,
    issuer_usdc: i128,
    buyer_usdc: i128,
    lp_usdc: i128,
}

impl Snapshot {
    fn take(p: &FullProtocol, invoice_id: &BytesN<32>) -> Self {
        let stats = p.pool.get_stats();
        let record = p.invoice.get(invoice_id);
        let usdc = MockTokenClient::new(&p.env, &p.usdc_id);
        Self {
            total_deposits: stats.total_deposits,
            total_funded: stats.total_funded,
            available_liquidity: stats.available_liquidity,
            total_yield_distributed: stats.total_yield_distributed,
            total_loss_realised: stats.total_loss_realised,
            active_invoice_count: stats.active_invoice_count,
            total_shares: stats.total_shares,
            max_utilization_bps: stats.max_utilization_bps,
            funded_entry: p.pool.get_funded_amount(invoice_id),
            lp_shares: p.pool.balance(&p.lp),
            invoice_status: p.invoice.get_status(invoice_id),
            invoice_funded_amount: record.funded_amount,
            invoice_funded_at: record.funded_at,
            invoice_funding_pool: record.funding_pool.clone(),
            invoice_in_listed: p
                .invoice
                .has_status_membership(&InvoiceStatus::Listed, invoice_id),
            invoice_in_funded: p
                .invoice
                .has_status_membership(&InvoiceStatus::Funded, invoice_id),
            invoice_in_defaulted: p
                .invoice
                .has_status_membership(&InvoiceStatus::Defaulted, invoice_id),
            escrow_locked: p.escrow.get_locked(invoice_id),
            escrow_locked_at: p.escrow.get_locked_at(invoice_id),
            escrow_history_len: p.escrow.get_history(invoice_id).len(),
            escrow_was_released_to_issuer: p.escrow.was_released_to_issuer(invoice_id),
            pool_usdc: usdc.balance(&p.pool_id),
            escrow_usdc: usdc.balance(&p.escrow_id),
            issuer_usdc: usdc.balance(&p.issuer),
            buyer_usdc: usdc.balance(&p.buyer),
            lp_usdc: usdc.balance(&p.lp),
        }
    }

    /// Asserts every field of the pre-call snapshot is still exactly in place
    /// after the failed call, naming the contract area in each message.
    fn assert_unchanged(&self, after: &Snapshot, flow: &str) {
        macro_rules! same {
            ($field:ident, $area:expr) => {
                assert_eq!(
                    self.$field,
                    after.$field,
                    "{flow}: {} must be unchanged by the failed call",
                    stringify!($area)
                );
            };
        }
        same!(total_deposits, "pool.total_deposits");
        same!(total_funded, "pool.total_funded");
        same!(available_liquidity, "pool.available_liquidity");
        same!(total_yield_distributed, "pool.total_yield_distributed");
        same!(total_loss_realised, "pool.total_loss_realised");
        same!(active_invoice_count, "pool.active_invoice_count");
        same!(total_shares, "pool.total_shares");
        same!(max_utilization_bps, "pool.max_utilization_bps");
        same!(funded_entry, "pool.FundedInvoice entry");
        same!(lp_shares, "pool LP share balance");
        same!(invoice_status, "invoice.status");
        same!(invoice_funded_amount, "invoice.funded_amount");
        same!(invoice_funded_at, "invoice.funded_at");
        same!(invoice_funding_pool, "invoice.funding_pool");
        same!(invoice_in_listed, "invoice status index (Listed)");
        same!(invoice_in_funded, "invoice status index (Funded)");
        same!(invoice_in_defaulted, "invoice status index (Defaulted)");
        same!(escrow_locked, "escrow.locked record");
        same!(escrow_locked_at, "escrow.locked_at");
        same!(escrow_history_len, "escrow history");
        same!(
            escrow_was_released_to_issuer,
            "escrow issuer-release marker"
        );
        same!(pool_usdc, "pool USDC balance");
        same!(escrow_usdc, "escrow USDC balance");
        same!(issuer_usdc, "issuer USDC balance");
        same!(buyer_usdc, "buyer USDC balance");
        same!(lp_usdc, "LP USDC balance");
    }
}

/// # Funding flow rollback (issue #1017)
///
/// Drives the real `pool.fund_invoice` entry point with a real invoice whose
/// issuer is the pool contract address. Inside the call the pool commits
/// `TotalFunded` / `ActiveInvoiceCount` / `FundedInvoice`, real escrow locks
/// the principal (moving USDC and writing its record + history), and real
/// invoice flips to `Funded` — and only then does the final
/// `escrow.release_to_issuer` reject the pool as a payout recipient
/// (`EscrowError::InvalidRecipient` = #7).
///
/// The test proves the whole cross-contract frame reverts atomically: all
/// three contracts retain their exact pre-call state, and a replay surfaces
/// the same escrow error rather than `PoolError::AlreadyFunded` (#16) —
/// black-box proof that the pool's pre-committed funding entry was rolled
/// back rather than merely hidden.
#[test]
fn test_funding_flow_downstream_failure_rolls_back_all_contracts() {
    let p = setup();
    assert!(p.pool.deposit(&p.lp, &DEPOSIT) > 0);

    // Controlled issuer address: make the pool itself the invoice issuer so
    // escrow's recipient guard fails at the very last downstream call.
    let metadata: Map<SorobanString, SorobanString> = Map::new(&p.env);
    p.registry.register_issuer(&p.pool_id, &metadata);
    p.registry.verify_profile(&p.admin, &p.pool_id, &true);
    let invoice_id = create_and_list(&p, &p.pool_id, 86_400 * 30);

    let before = Snapshot::take(&p, &invoice_id);
    assert_eq!(before.invoice_status, InvoiceStatus::Listed as u32);
    assert_eq!(before.escrow_locked, 0);

    let err = std::format!("{:?}", p.pool.try_fund_invoice(&invoice_id).unwrap_err());
    assert!(
        err.contains("Error(Contract, #7)"),
        "fund_invoice must fail inside escrow.release_to_issuer with \
         EscrowError::InvalidRecipient (#7), got: {err}"
    );

    // All three contracts must look exactly like they did before the call,
    // even though escrow and invoice had already written state mid-frame.
    before.assert_unchanged(&Snapshot::take(&p, &invoice_id), "funding rollback");

    // If the pool's FundedInvoice entry had survived, a replay would stop at
    // the pool's own idempotency guard (AlreadyFunded = #16) before ever
    // reaching escrow. Seeing #7 again proves nothing was recorded.
    let replay = std::format!("{:?}", p.pool.try_fund_invoice(&invoice_id).unwrap_err());
    assert!(
        replay.contains("Error(Contract, #7)"),
        "replay must hit the same escrow failure, proving no pool-side \
         funding entry survived the rollback, got: {replay}"
    );
}

/// # Default flow rollback (issue #1017)
///
/// Funds an invoice through the real flow (escrow lock → invoice
/// `mark_funded` → escrow `release_to_issuer`), then leaves a live out-of-band
/// `escrow.lock` record whose `locked_at` sits inside escrow's 60-second
/// `DEFAULT_MIN_LOCK_SECONDS` grace window. The real `pool.handle_default`
/// entry point therefore dies inside real `escrow.handle_default`
/// (`EscrowError::NotAuthorized` = #3) while settling the default, and the
/// test proves every contract — including the escrow lock and token balances
/// created before the failing frame — is untouched afterwards.
///
/// Replaying the identical call after the grace window elapses succeeds and
/// settles the default consistently, pinning that the earlier failure was the
/// escrow grace-period guard rather than a broken fixture.
#[test]
fn test_default_flow_downstream_failure_rolls_back_all_contracts() {
    let p = setup();
    assert!(p.pool.deposit(&p.lp, &DEPOSIT) > 0);

    // Start the clock at t=10 so `locked_at` / `funded_at` are distinguishable
    // from the "no record" sentinel (0) in the assertions.
    p.env.ledger().set_timestamp(10);

    // Due 30s after creation: the default attempt below lands just after
    // maturity while the escrow lock's 60s grace window is still running.
    let invoice_id = create_and_list(&p, &p.issuer, 30);
    assert!(p.pool.fund_invoice(&invoice_id));

    // Out-of-band escrow lock (pre-#819-style lock still live at default
    // time). Uses the real escrow; the pool authorization it requires is
    // satisfied by the test environment's blanket auth mocks, exactly as the
    // per-contract suites do.
    const LOCK_AMOUNT: u128 = 500_000_000;
    p.escrow.lock(&invoice_id, &LOCK_AMOUNT, &p.issuer);
    let locked_at = p.escrow.get_locked_at(&invoice_id);
    assert_eq!(locked_at, 10);
    assert_eq!(p.escrow.get_locked(&invoice_id), LOCK_AMOUNT);

    // Past the invoice due date, still inside the escrow grace window.
    p.env.ledger().set_timestamp(locked_at + 30);

    let before = Snapshot::take(&p, &invoice_id);
    assert_eq!(before.invoice_status, InvoiceStatus::Funded as u32);
    assert!(before.funded_entry.is_some());
    assert_eq!(before.escrow_locked, LOCK_AMOUNT);

    let err = std::format!("{:?}", p.pool.try_handle_default(&invoice_id).unwrap_err());
    assert!(
        err.contains("Error(Contract, #3)"),
        "handle_default must fail inside escrow.handle_default with \
         EscrowError::NotAuthorized (#3) while the lock grace period runs, \
         got: {err}"
    );

    before.assert_unchanged(&Snapshot::take(&p, &invoice_id), "default rollback");

    // Same call, grace window elapsed: the default now settles end to end,
    // proving the rollback above was triggered by the real escrow guard.
    p.env.ledger().set_timestamp(locked_at + 60);
    assert!(p.pool.handle_default(&invoice_id));

    let usdc = MockTokenClient::new(&p.env, &p.usdc_id);
    assert_eq!(
        p.invoice.get_status(&invoice_id),
        InvoiceStatus::Defaulted as u32
    );
    assert_eq!(p.pool.get_funded_amount(&invoice_id), None);
    let stats = p.pool.get_stats();
    assert_eq!(stats.total_funded, 0);
    assert_eq!(stats.active_invoice_count, 0);
    assert_eq!(stats.total_loss_realised, FUNDED_AMOUNT);
    // Escrow returned the still-live lock to the pool, so LP capital was not
    // lost and TotalDeposits is untouched (issue #439 recovery branch).
    assert_eq!(stats.total_deposits, DEPOSIT);
    assert_eq!(p.escrow.get_locked(&invoice_id), 0);
    assert_eq!(usdc.balance(&p.escrow_id), 0);
    assert_eq!(usdc.balance(&p.pool_id), (DEPOSIT - FUNDED_AMOUNT) as i128);
}
