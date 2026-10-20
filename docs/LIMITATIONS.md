# Limitations

> **Updated:** 2026-10-20
> **Applies to:** TrusTrove protocol on Stellar testnet

This document captures known limitations, testnet-specific constraints,
budget/cost estimates, unimplemented features, and edge cases that are not
yet handled.

---

## Table of Contents

- [Testnet Limitations](#testnet-limitations)
- [Gas & Budget](#gas--budget)
- [Repayment Timing and Refund Math](#repayment-timing-and-refund-math)
- [Known Gaps](#known-gaps)
- [Unhandled Edge Cases](#unhandled-edge-cases)
- [Planned But Not Implemented](#planned-but-not-implemented)
- [Resolved](#resolved)
- [Related Issues](#related-issues)

---

## Testnet Limitations

### Friendbot-Derived Accounts

Testnet accounts are funded via Friendbot, which provides a fixed amount of
test XLM. All USDC on testnet is issued by the testnet USDC issuer and has no
real-world value. This means:

- **No real economic incentives** are at play — behaviour on testnet may not
  reflect mainnet dynamics.
- **Friendbot rate limits** can slow down testing. If `setup-testnet.sh`
  returns a `400` from Friendbot, the account is likely already funded
  (the error is benign).

### Ephemeral Validators

Stellar testnet validators may reset state periodically. Do not depend on
testnet contract state for production data.

### No Formal Audit

All contracts are unaudited. See [SECURITY.md](../SECURITY.md#audit-status)
for details.

### Deprecated Test CLI Commands

The `stellar contract` subcommand used in deploy scripts may change with CLI
upgrades. The repository pins `Rust 1.85.0` and the `wasm32v1-none` target,
but the Stellar CLI is expected to be at the latest version
(see [DEPLOYMENT.md](../DEPLOYMENT.md#prerequisites)).

---

## Gas & Budget

### Soroban Resource Model

Soroban charges fees based on a budget model: each operation consumes
CPU instructions, memory, and ledger I/O.

**Measured numbers** for the hot paths (`invoice::create`, `invoice::repay`,
`pool::deposit`, `pool::fund_invoice`) come from the automated budget harness
in [`contracts/pool/src/bench.rs`](../contracts/pool/src/bench.rs) (#734). It
runs the real registry/invoice/escrow/pool contracts at 0, 10 and 50
outstanding invoices and reports CPU instructions, memory bytes, ledger
entries written, bytes written and TTL bumps per call — see
[Automated Budget Benchmarks](#automated-budget-benchmarks) below. Rows marked
**[measured](#automated-budget-benchmarks)** take their CPU/memory figures from
that output; the remaining rows' cross-contract-call and transfer counts and
the *Relative Budget* column are still derived from code analysis.

| Operation | Cross-Contract Calls | Token Transfers | CPU Instructions | Memory (Bytes) | Relative Budget |
|-----------|---------------------|-----------------|------------------|----------------|-----------------|
| `registry::register_issuer` | 0 | 0 | — | — | Very Low |
| `registry::revoke` | 0 | 0 | — | — | Very Low |
| `registry::get_profile_count` | 0 | 0 | — | — | Very Low |
| `registry::list_profiles` | 0 | 0 | — | — | Low (O(limit), ≤ 50) |
| `invoice::create` | 2 (`is_verified` ×2) | 0 | [measured](#automated-budget-benchmarks) | [measured](#automated-budget-benchmarks) | Low |
| `invoice::batch_create` | 2 per entry (`is_verified` ×2) | 0 | — | — | Low per entry, **up to 50× `create`** |
| `invoice::list_for_financing` | 0 | 0 | — | — | Low |
| `invoice::batch_list_for_financing` | 0 | 0 | — | — | Low per entry, **up to 50× `list_for_financing`** |
| `invoice::mark_funded` | 0 | 0 | — | — | Low |
| `invoice::repay` | 1 (`receive_repayment`) | 1 (buyer → pool) | [measured](#automated-budget-benchmarks) | [measured](#automated-budget-benchmarks) | Medium |
| `invoice::trigger_default` | 1 (`handle_default`) | 0 | — | — | Medium |
| `pool::deposit` (before refactor) | 0 | 1 (LP → pool) | ~456,800 | ~73,400 | Low-Medium |
| `pool::deposit` (after `mint()`) | 0 | 1 (LP → pool) | 457,992 | 73,495 | Low-Medium |
| `pool::withdraw` (before refactor) | 0 | 1 (pool → LP) | ~450,500 | ~63,550 | Medium |
| `pool::withdraw` (after `burn()`) | 0 | 1 (pool → LP) | 451,634 | 63,660 | Medium |
| `pool::fund_invoice` | 4 (`get_funding_context`, `lock`, `release_to_issuer`, `mark_funded`) | 2 (pool → escrow, escrow → issuer; #819 pays the issuer inside the call) | [measured](#automated-budget-benchmarks) | [measured](#automated-budget-benchmarks) | **High** |
| `pool::batch_fund_invoice` | 4 per entry | 2 per entry | — | — | **High** per entry, **up to 50× `fund_invoice`** |
| `pool::receive_repayment` | 0 | 0 | — | — | Low |
| `pool::handle_default` | 2 (`escrow::handle_default`, plus `escrow::was_released_to_issuer` when no live lock remains; #819) | 1 (escrow → pool) only when escrow still holds a live lock | — | — | Medium |
| `pool_factory::register_existing_pool` | 0 | 0 | 69,486 | 7,604 | Low |
| `escrow::lock` | 0 | 1 (pool → escrow) | — | — | Medium |
| `escrow::release_to_issuer` | 0 | 1 (escrow → issuer) | — | — | Medium |
| `escrow::release_to_pool` | 0 | 1 (escrow → pool, partial allowed) | — | — | Medium |
| `escrow::handle_default` | 0 | 1 (escrow → pool) | — | — | Medium |

Every mutating entry point above also reads the shared pause flag
(`trusttrove-pause`), adding one instance-storage read per call. That read is
instance-local and costs the same in every contract; it is also what pushed a few
long-running integration tests past the default _test-host_ budget, which is why
`pool_factory`'s test `setup()` lifts that budget — see
[Batch Operations](#batch-operations).

### Automated Budget Benchmarks

`bench_core_flows_budget` in
[`contracts/pool/src/bench.rs`](../contracts/pool/src/bench.rs) measures each
hot path with the Soroban test host's budget tracker (`env.budget()`), resetting
the budget immediately before the measured call so setup work never leaks into
a row. Ledger I/O is derived by diffing the ledger snapshot before and after
the call.

```bash
# Print the table (and write target/budget-bench.csv)
cargo test -p trusttrove-pool bench_ -- --nocapture

# Refresh the committed baseline, then review the diff
BENCH_OUT=docs/benchmarks/budget.csv cargo test -p trusttrove-pool bench_
git diff docs/benchmarks/budget.csv
```

Output is a sorted CSV with no timestamps, so two runs diff cleanly:

```text
op,volume,cpu_insns,mem_bytes,entries_written,bytes_written,ttl_bumps
invoice::create,0,…,…,…,…,…
invoice::create,10,…
…
pool::fund_invoice,50,…
```

| Column | Meaning |
|--------|---------|
| `volume` | Invoices already funded and outstanding when the call runs (0 / 10 / 50) |
| `cpu_insns` | CPU instructions charged by the test host |
| `mem_bytes` | Memory bytes charged by the test host |
| `entries_written` | Ledger entries created, modified or deleted |
| `bytes_written` | XDR size of the written entries (post-call) |
| `ttl_bumps` | Entries whose TTL was extended without a data change |

The test also **fails** if any measured call exceeds the per-transaction network
limits (100M CPU instructions / 40 MiB memory), so a change that pushes
`fund_invoice` past what a single transaction can carry is caught in CI.
Limitations: ledger *reads* are not counted separately (their cost is included
in `cpu_insns`), and test-host numbers track — but are not identical to — the
fees simulated by RPC `simulateTransaction` against a real network.

### Measured Benchmarks: `deposit()` and `withdraw()` (SEP-41 mint/burn refactor)

In preparation for SEP-41 token interface compliance, `deposit()` and `withdraw()`'s internal LP share storage updates were refactored from inline writes into shared internal `mint()` and `burn()` helpers. Resource measurements were taken via Soroban SDK test environment budget tracking (`env.budget()` in `test_gas_benchmark_deposit_and_withdraw` in `contracts/pool/src/test.rs`):

| Operation    | Implementation                                   | CPU Instructions | Memory (Bytes) | Delta (CPU)     | Delta (Memory) |
| ------------ | ------------------------------------------------ | ---------------- | -------------- | --------------- | -------------- |
| `deposit()`  | Inline `DataKey::LPShares` writes (pre-refactor) | 456,800          | 73,400         | Baseline        | Baseline       |
| `deposit()`  | Routed via `Self::mint()` (post-refactor)        | 457,992          | 73,495         | +1,192 (+0.26%) | +95 (+0.13%)   |
| `withdraw()` | Inline `DataKey::LPShares` writes (pre-refactor) | 450,500          | 63,550         | Baseline        | Baseline       |
| `withdraw()` | Routed via `Self::burn()` (post-refactor)        | 451,634          | 63,660         | +1,134 (+0.25%) | +110 (+0.17%)  |

The benchmark demonstrates negligible gas overhead (~0.25% CPU instruction delta from standard helper call frames) with zero regression to ledger write patterns or storage footprint.

### Budget Considerations

- `pool::fund_invoice` is the most expensive operation — it makes 6
  cross-contract calls (4 invoice reads + `escrow::lock` + `invoice::mark_funded`)
  and performs 1 token transfer. On a congested testnet, this may exceed the
  per-ledger resource limit.
- Read-only view functions (`get_stats`, `get`, `get_profile`, etc.) incur
  minimal cost as they only read from storage.
- Index enumeration functions (`get_by_status`, `get_by_issuer`,
  `get_by_buyer`) are paginated (`page`, `page_size`, capped at
  `MAX_PAGE_SIZE`), so each call hydrates a bounded number of invoices no
  matter how many entries the index holds (issue #71). Callers page until an
  empty result is returned. The status index is append-only, so a page can
  return fewer than `page_size` invoices when it contains stale entries whose
  invoice has since moved to another status.
- `get_invoice_count_by_issuer` and `get_invoice_count_by_buyer` avoid that
  cost entirely: they read a single stored counter (`u32`) in O(1), so
  pagination and badge UIs should prefer them over `.len()` on the
  full-fetch views.
- `registry::list_profiles` is bounded per call: it returns at most 50
  addresses (`PageSizeExceeded` above that, mirroring the 50-entry batch cap)
  and costs O(1) in `limit`, not in the total number of registered profiles.
  Read `registry::get_profile_count(role)` once to size the final page, then
  walk `start = 0, limit, 2 * limit, …` until a page comes back short. Each
  enumerated address costs one persistent entry plus its TTL bump, so a page of
  50 is roughly 50 reads — indexers that want thousands of addresses should
  page off-chain rather than in one call.

### Batch Operations

`invoice::batch_create`, `invoice::batch_list_for_financing`, and
`pool::batch_fund_invoice` bound their input at `MAX_BATCH_SIZE` (50 entries)
with `BatchSizeExceeded` (`InvoiceError::28` / `PoolError::26`). The cap exists
so a caller cannot turn one transaction into an unbounded number of cross-contract
calls; a full 50-entry batch is the practical ceiling and can exceed the
per-ledger resource limit on a congested testnet. Callers that hit a budget
error should split the work across several transactions.

The three batch entry points deliberately differ in failure semantics, because
their failure modes differ:

| Entry point                | On a bad entry                      | Why                                                                                                                                                                  |
| -------------------------- | ----------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `batch_create`             | Reverts the whole batch             | Creation consumes the shared `Counter`; a partial success would burn counter values and persist a prefix the caller cannot identify without re-reading every invoice |
| `batch_list_for_financing` | Skips the entry, returns its ID     | The interesting failures are per-invoice state an issuer listing a backlog cannot avoid (already listed, missing attestation, revoked verification)                  |
| `batch_fund_invoice`       | Stops the batch, returns funded IDs | A liquidity/utilization stop is a normal outcome when an LP fills the pool; partial funding would drain the pool in a way no single `fund_invoice` could             |

`batch_fund_invoice` still reverts on _eligibility_ errors (not listed, already
funded by this pool, unverified issuer/buyer, asset mismatch), because those are
caller mistakes that should be fixed rather than silently skipped. Callers
reconcile the returned funded IDs against their input to see which entries a
capacity stop skipped.

### Storing the `History` Vector

`escrow::append_history` stores a growable `Vec<EscrowEvent>` under
`DataKey::History(invoice_id)`. Each event adds an element without
compaction. For heavily-used contracts, this vector may grow large enough
to exceed the per-entry size limit (~100 KB in practice).

---

## Repayment Timing and Refund Math

This is the authoritative description of when each repayment entry point may
run and how the buyer/LP split is calculated. The invoice contract computes
the split; the pool only enforces bounds (see
`receive_repayment_with_refund`'s trust-boundary note).

### Terms

`funded_amount` is recorded by `mark_funded`; `discount =
face_value - funded_amount`; `term = due_date - funded_at`; and `elapsed =
now - funded_at`. The arithmetic is integer division (it floors
`earned_by_pool`), so any remaining discount of at least one stroop produces a
refund of at least one stroop:

```text
earned_by_pool =
  discount                          if term == 0 or elapsed >= term
  discount * elapsed / term         otherwise

refund_to_buyer = discount - earned_by_pool
```

### Allowed Statuses and Timing

| Entry point | Allowed status | Timing | Amount pulled from buyer | Refund |
| --- | --- | --- | --- | --- |
| `repay` | `Funded`, `Active`, `Confirmed` | Any time; `due_date` does not block it | `remaining_balance` (delegates to `repay_partial`) | Prorated before `due_date`; zero at/after `due_date` |
| `repay_partial` | `Funded`, `Active`, `Confirmed` | Any time | `amount` | Split is computed only when the final installment clears the balance |
| `repay_early` | `Confirmed` only | Strictly before `due_date`; at/after reverts with `InvalidStatusTransition` | `face_value` | Prorated from `funded_at` |

### Fund Movements

The buyer transfers the full `face_value` to escrow; escrow releases it to the
pool through `release_to_pool`; the pool books `earned_by_pool` as yield and
transfers `refund_to_buyer` back to the buyer. Of the earned yield, the protocol
fee (`protocol_cut`) goes to treasury and the remainder to LPs. Partial
repayments accumulate in escrow and only settle at the pool when the final
installment arrives. Settlement consumes the funded entry and moves the invoice
to `Repaid`, so a second repayment (whether through `repay` or any other entry
point) fails with `InvalidStatusTransition` — the yield is booked exactly once.
Note that `repay` before maturity also refunds the unearned discount;
`repay_early` is not the only path with a rebate.

### Worked Example

This matches `tests/integration/tests/lifecycle.rs`. Face value
`10_000_000_000`, 200 bps discount (`200_000_000`), 30-day term
(`2_592_000` seconds):

| Repaid at | `earned_by_pool` | `refund_to_buyer` | Buyer net cost |
| --- | --- | --- | --- |
| `funded_at` | 0 | 200,000,000 | 9,800,000,000 |
| `funded_at + term / 2` | 100,000,000 | 100,000,000 | 9,900,000,000 |
| `due_date - 1` | 199,999,922 | 78 | 9,999,999,922 |
| `due_date` or later | 200,000,000 | 0 | 10,000,000,000 |

At `funded_at` the pool books no yield (`total_yield_distributed` is
unchanged), while at `due_date - 1` the pool receives the complement of the
78-stroop refund exactly once. `repay_early` at `due_date` and later is
rejected; use `repay` for mature invoices.

---

## Known Gaps

### LP Withdrawal Yield Uses Aggregate Principal

The pool stores one cumulative principal amount per LP rather than deposit
lots. A partial withdrawal allocates that amount pro rata across the LP's
remaining shares. This is an average-basis approximation, not FIFO or HIFO
accounting; after deposits at different share prices it can attribute a
different yield amount to a particular withdrawal. Total USDC returned and
pool share ownership are unaffected, but the LP's cumulative `yield_earned`
report can differ based on the chosen lot-accounting policy.

For example, an LP deposits 1,000 USDC, the pool gains 2%, then the LP deposits
another 1,000 USDC. Withdrawing 500 shares returns about 510 USDC. FIFO would
attribute 500 USDC principal and 10 USDC yield, while aggregate-basis
accounting records about 504.95 USDC principal and 5.05 USDC yield. The
roughly 4.95 USDC difference is about 49.5% of the FIFO yield for this
withdrawal. The regression test in `contracts/pool/src/test.rs` covers this
scenario. Per-deposit lots would be required if the protocol needs a specific
FIFO/HIFO tax or reporting policy.

### Issuer Release Not Wired (Issue #56)

**Status: Resolved** by
[#819](https://github.com/TrusTrove/TrusTrove-contract/issues/819) — see
[Resolved](#resolved). `fund_invoice` now releases the funded amount to the
issuer in the same call (`pool → escrow.lock → escrow.release_to_issuer`),
restoring the flow originally wired in 5f6dc7c and later dropped. No
off-chain bot is required. Repayment (`escrow.release_to_pool`) and default
(`pool.handle_default`) were adapted to the lock record being consumed at
funding time.

### No Duplicate Invoice Detection

The `create` function generates an invoice ID from a SHA-256 hash of
(issuer, buyer, face_value, due_date, counter, asset). The counter
guarantees uniqueness even for otherwise identical invoices, but there is
no user-visible warning when an issuer re-submits the same commercial
terms.

---

## Unhandled Edge Cases

### What Happens When…

| Scenario                                                               | Current Behaviour                                                                                   | Notes                                                                                                                                   |
| ---------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------- |
| LP deposits in a pool with 0 shares and 0 deposits                     | 1:1 share minting                                                                                   | Correct                                                                                                                                 |
| LP deposits when `total_shares > 0` but `total_deposits = 0`           | Division by zero would panic                                                                        | Cannot happen in practice (shares only minted alongside deposits)                                                                       |
| Buyer repays exactly on `due_date`                                     | `invoice.repay` succeeds with no refund; `trigger_default` panics with `DueDateNotPassed`           | Asymmetric: all repayment paths have no due-date upper bound; `trigger_default` requires `current_time > due_date` (strict)             |
| `confirm_delivery` called after invoice is already confirmed           | Panics with `InvalidStatusTransition` because status is `Confirmed` not `Active`                    | Correct — prevents replay                                                                                                               |
| `withdraw` when pool has funded invoices but no available liquidity    | Panics with `InsufficientLiquidity`                                                                 | Correct — prevents breaking the pool invariant                                                                                          |
| Admin calls `set_max_utilization` to 0                                 | All `fund_invoice` calls will fail (utilization ≥ 0% ≥ cap of 0)                                    | This is a denial-of-service vector available to admin                                                                                   |
| Calling `revoke` on an unregistered address                            | Panics with `NotFound`                                                                              | Per spec                                                                                                                                |
| Calling `batch_register_issuers` with more than 50 entries             | Panics with `BatchSizeExceeded`                                                                     | Gas protection                                                                                                                          |
| A buyer repays _less_ than `face_value`                                | Supported via `invoice.repay_partial`; installments accumulate in escrow until the balance clears    | A full `repay` pulls only the outstanding `remaining_balance`; the buyer refund is computed once, when the final installment settles     |
| An issuer creates an invoice with `due_date = current_time + 1` second | Invoice accepted                                                                                    | `create` requires `due_date > env.ledger().timestamp()` (strict); `now + 1` passes, `now` does not                                      |
| The pool has exactly 0 USDC balance after funding all deposits         | `withdraw` succeeds for unfunded amounts, but funded invoices are locked                            | The issuer holds the funded USDC (paid out at funding, #819); escrow holds no balance for funded invoices and the pool holds only unfunded USDC  |
| `trigger_default` while status is exactly at `due_date` (timestamp)    | Panics with `DueDateNotPassed`                                                                      | The guard is `if current_time <= due_date`, so one full second must elapse past `due_date` before default can fire                      |

---

## Planned But Not Implemented

These features are tracked in the repository's issue tracker. Cross-links
are provided where available.

### UI / Frontend

- Event-driven Go indexer (see [TrusTrove-app](https://github.com/TrusTrove/TrusTrove-app))
- Transaction simulation before signature

### Smart Contracts

- Multi-sig admin (3-of-5 Stellar signers)
- LP-governed invoice funding (stake LP tokens to vote on invoices). Design
  proposal: [NEW_DESIGN.md](NEW_DESIGN.md) (issue #718)
- Dynamic utilization-based interest rate model
- Swap-free multi-asset pools (USDC + XLM)
- On-chain governance for protocol parameters

### Tooling & Infrastructure

- Mainnet deployment support (currently testnet only)
- Formal verification or symbolic execution audit
- `cargo-soroban` integration for automated WASM budget reporting — test-host
  budget harness shipped (#734, [Automated Budget Benchmarks](#automated-budget-benchmarks));
  on-network WASM fee reporting via `simulateTransaction` is still planned
- TypeScript SDK for contract interaction
- Automated fuzz testing across all four contracts
- Smart-contract upgrade mechanism (`__constructor` + `__upgrade`)

> **Note:** Partial repayments are supported on both paths. On the
> **escrow** path, `escrow::release_to_pool(invoice_id, repayment_amount)`
> accepts a partial `repayment_amount` and tracks the residual in the
> escrow record. On the **buyer-driven** path,
> `invoice.repay_partial(invoice_id, amount)` accumulates installments
> against the invoice's `remaining_balance` and only releases the escrow
> to the pool once that balance reaches zero. See [Resolved](#resolved).

---

## Resolved

These limitations were previously listed here but have been fixed since
the 2025-07-25 revision.

> **Correction (issue
> [#835](https://github.com/TrusTrove/TrusTrove-contract/issues/835)):**
> An earlier entry here claimed that "Index entries not compacted on status
> transition" was resolved because `move_status_index` performs O(1)
> membership checks via `DataKey::StatusMembership` (crediting PR #121).
> No such storage key exists anywhere in `contracts/invoice/src/lib.rs`.
> In reality `move_status_index` and the `extend_*_index` helpers linearly
> scan the index on every transition, index rows are still not compacted,
> and `get_by_status` skips stale rows only by loading each invoice and
> comparing its status. The item remains open and the O(n) linear-scan
> behaviour is tracked in
> [#831](https://github.com/TrusTrove/TrusTrove-contract/issues/831);
> this document will be updated again when that lands.

- **Pool does not track individual LP yield accrual.** `pool::withdraw`
  now writes `DataKey::LPYieldEarned(lp)` and `pool::get_lp_position`
  returns the running yield figure per LP. Yield is therefore visible
  before the LP triggers a withdrawal.
- **Partial default-side repayment.** `escrow::release_to_pool` accepts
  a `repayment_amount` that may be less than the original locked amount,
  keeps the residual escrow record in place, and emits history events
  per partial release. See PR #120
  (`feat/partial-repayment-default-flow`).
- **Buyer-driven partial repayment.** `invoice.repay_partial(invoice_id,
  amount)` accepts an installment up to the invoice's
  `remaining_balance`, leaves the invoice in its current status while a
  balance remains, and transitions to `Repaid` only once `repaid_amount`
  reaches `face_value`. Overpayments are rejected with
  `InvoiceError::RepaymentExceedsBalance` (error code 25). See issue
  [#728](https://github.com/TrusTrove/TrusTrove-contract/issues/728).
- **Security findings #94, #96, #98, #103.** Addressed together in
  PR #124 (`fix: address security issues #94, #96, #98, #103`).
- **Issuer payout at funding (#56, restored by #819).**
  `pool::fund_invoice` calls `escrow::release_to_issuer(invoice_id, issuer)`
  immediately after `escrow::lock`, so the issuer receives `funded_amount`
  in the same transaction (originally wired in 5f6dc7c, dropped by a later
  refactor, re-wired by
  [#819](https://github.com/TrusTrove/TrusTrove-contract/issues/819)).
  Because the `Locked` record is consumed at funding, repayment and default
  key off durable payout evidence instead:
  - `release_to_issuer` writes `DataKey::IssuerReleased(invoice_id)`
    recording the paid-out amount.
  - `escrow::release_to_pool` accepts either a live `Locked` record or the
    marker — exactly one, consumed on use — so repayment still settles and
    a second release still reverts with `NotFound` (#2 guard preserved).
  - `escrow::handle_default` returns `false` when no live lock remains (a
    funded invoice has none). `pool::handle_default` then reads
    `escrow::was_released_to_issuer`: marker present means the principal is
    already with the issuer, so the default books a real LP capital loss
    (`TotalDeposits -= funded_amount`, keeping deposit accounting equal to
    the pool's token balance; shares are still never burned, preserving
    #439's no-burn property). No marker still reverts with
    `EscrowDefaultNotReleased` (#21).

---

## Related Issues

- [#56 — Wire issuer release into fund_invoice](https://github.com/TrusTrove/TrusTrove-contract/issues/56) (closed; the payout was later dropped by a refactor and re-wired by [#819](https://github.com/TrusTrove/TrusTrove-contract/issues/819))
- [#285 — Event schema documentation](https://github.com/TrusTrove/TrusTrove-contract/issues/285)
- [#286 — Gas budget report](https://github.com/TrusTrove/TrusTrove-contract/issues/286)
- [#287 — Storage schema docs](https://github.com/TrusTrove/TrusTrove-contract/issues/287)
- [#294 — Testnet limitation docs](https://github.com/TrusTrove/TrusTrove-contract/issues/294)
- [#307 — General repo documentation](https://github.com/TrusTrove/TrusTrove-contract/issues/307)
- [#460 — LIMITATIONS.md stale date](https://github.com/TrusTrove/TrusTrove-contract/issues/460) (this PR fixes it)
- [#831 — Index helpers linearly scan whole indexes](https://github.com/TrusTrove/TrusTrove-contract/issues/831) (open — tracks the linear-scan performance work; update this document when a real index/membership structure lands)
- [#835 — LIMITATIONS.md documents a `DataKey::StatusMembership` that does not exist](https://github.com/TrusTrove/TrusTrove-contract/issues/835) (this PR fixes it)

Resolved items reference the PRs that closed them:

- Issue #67 — status index filtering (closed by PR #121; the O(1) membership claim was retracted — see [#835](https://github.com/TrusTrove/TrusTrove-contract/issues/835) and [#831](https://github.com/TrusTrove/TrusTrove-contract/issues/831))
- Issues #94, #96, #98, #103 — security (closed by PR #124)
- Partial default repayment (closed by PR #120)
