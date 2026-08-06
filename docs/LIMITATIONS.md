# Limitations

> **Updated:** 2026-07-29
> **Applies to:** TrusTrove protocol on Stellar testnet

This document captures known limitations, testnet-specific constraints,
budget/cost estimates, unimplemented features, and edge cases that are not
yet handled.

---

## Table of Contents

- [Testnet Limitations](#testnet-limitations)
- [Gas & Budget](#gas--budget)
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
CPU instructions, memory, and ledger I/O. The following are approximate
relative costs based on code analysis.

| Operation | Cross-Contract Calls | Token Transfers | Relative Budget |
|-----------|---------------------|-----------------|-----------------|
| `registry::register_issuer` | 0 | 0 | Very Low |
| `registry::revoke` | 0 | 0 | Very Low |
| `invoice::create` | 2 (`is_verified` ×2) | 0 | Low |
| `invoice::list_for_financing` | 0 | 0 | Low |
| `invoice::mark_funded` | 0 | 0 | Low |
| `invoice::repay` | 1 (`receive_repayment`) | 1 (buyer → pool) | Medium |
| `invoice::trigger_default` | 1 (`handle_default`) | 0 | Medium |
| `pool::deposit` | 0 | 1 (LP → pool) | Low-Medium |
| `pool::withdraw` | 0 | 1 (pool → LP) | Medium |
| `pool::fund_invoice` | 6 (`get_status`, `get_funding_asset`, `get_face_value`, `get_discount_bps`, `lock`, `mark_funded`) | 1 (pool → escrow) | **High** |
| `pool::receive_repayment` | 0 | 0 | Low |
| `pool::handle_default` | 1 (`escrow::handle_default`) | 1 (escrow → pool) | Medium |
| `escrow::lock` | 0 | 1 (pool → escrow) | Medium |
| `escrow::release_to_issuer` | 0 | 1 (escrow → issuer) | Medium |
| `escrow::release_to_pool` | 0 | 1 (escrow → pool, partial allowed) | Medium |
| `escrow::handle_default` | 0 | 1 (escrow → pool) | Medium |

### Budget Considerations

- `pool::fund_invoice` is the most expensive operation — it makes 6
  cross-contract calls (4 invoice reads + `escrow::lock` + `invoice::mark_funded`)
  and performs 1 token transfer. On a congested testnet, this may exceed the
  per-ledger resource limit.
- Read-only view functions (`get_stats`, `get`, `get_profile`, etc.) incur
  minimal cost as they only read from storage.
- Index enumeration functions (`get_by_status`, `get_by_issuer`,
  `get_by_buyer`) scale linearly with the number of entries and may become
  expensive for issuers/buyers with many invoices. The status index also
  performs O(1) membership checks via `DataKey::StatusMembership` to filter
  out removed entries without loading the full invoice.

### Storing the `History` Vector

`escrow::append_history` stores a growable `Vec<EscrowEvent>` under
`DataKey::History(invoice_id)`. Each event adds an element without
compaction. For heavily-used contracts, this vector may grow large enough
to exceed the per-entry size limit (~100 KB in practice).

---

## Known Gaps

### Issuer Release Not Wired (Issue #56)

After `fund_invoice` locks USDC in escrow, the pooled funds should be
released to the issuer via `escrow.release_to_issuer()`. This call is
**not yet wired** into `fund_invoice`. See
[Issue #56](https://github.com/TrusTrove/TrusTrove-contract/issues/56).
**Status:** Unresolved. This is the highest-priority gap before mainnet.
**Workaround:** An admin or automated off-chain bot must call
`escrow.release_to_issuer(invoice_id, issuer)` separately after funding.

### No Early Repayment Incentive Logic

The protocol does not apply dynamic discount rates based on how early
repayment occurs. The buyer pays the full `face_value` regardless of how
early they repay, so there is no on-chain rebate proportional to the time
remaining on the invoice.

### No Accessor for `funded_amount` on Invoice Contract

While `get_face_value`, `get_discount_bps`, and `get_funding_asset` are
exposed as per-field view functions (one storage read each), there is no
dedicated `get_funded_amount` accessor. Callers currently read the full
`Invoice` struct via `get()`.

### No Duplicate Invoice Detection

The `create` function generates an invoice ID from a SHA-256 hash of
(issuer, buyer, face_value, due_date, counter, asset). The counter
guarantees uniqueness even for otherwise identical invoices, but there is
no user-visible warning when an issuer re-submits the same commercial
terms.

---

## Unhandled Edge Cases

### What Happens When…

| Scenario | Current Behaviour | Notes |
|----------|------------------|-------|
| LP deposits in a pool with 0 shares and 0 deposits | 1:1 share minting | Correct |
| LP deposits when `total_shares > 0` but `total_deposits = 0` | Division by zero would panic | Cannot happen in practice (shares only minted alongside deposits) |
| Buyer repays exactly on `due_date` | `invoice.repay` succeeds; `trigger_default` panics with `DueDateNotPassed` | Asymmetric: `repay` has no time gate beyond `status == Confirmed`; `trigger_default` requires `current_time > due_date` (strict) |
| `confirm_delivery` called after invoice is already confirmed | Panics with `InvalidStatusTransition` because status is `Confirmed` not `Active` | Correct — prevents replay |
| `withdraw` when pool has funded invoices but no available liquidity | Panics with `InsufficientLiquidity` | Correct — prevents breaking the pool invariant |
| Admin calls `set_max_utilization` to 0 | All `fund_invoice` calls will fail (utilization ≥ 0% ≥ cap of 0) | This is a denial-of-service vector available to admin |
| Calling `revoke` on an unregistered address | Panics with `NotFound` | Per spec |
| Calling `batch_register_issuers` with more than 50 entries | Panics with `BatchSizeExceeded` | Gas protection |
| A buyer repays *less* than `face_value` | Not possible on the buyer path — `invoice.repay` transfers the full `face_value` from buyer to pool | Partial amounts are only possible through `escrow::release_to_pool` during default / partial-repayment flows; see [Resolved](#resolved) |
| An issuer creates an invoice with `due_date = current_time + 1` second | Invoice accepted | `create` requires `due_date > env.ledger().timestamp()` (strict); `now + 1` passes, `now` does not |
| The pool has exactly 0 USDC balance after funding all deposits | `withdraw` succeeds for unfunded amounts, but funded invoices are locked | Escrow holds funded USDC, pool holds only unfunded USDC |
| `trigger_default` while status is exactly at `due_date` (timestamp) | Panics with `DueDateNotPassed` | The guard is `if current_time <= due_date`, so one full second must elapse past `due_date` before default can fire |

---

## Planned But Not Implemented

These features are tracked in the repository's issue tracker. Cross-links
are provided where available.

### UI / Frontend

- Event-driven Go indexer (see [TrusTrove-app](https://github.com/TrusTrove/TrusTrove-app))
- Transaction simulation before signature

### Smart Contracts

- Emergency pause mechanism (`admin_pause() / admin_unpause()`)
- Multi-sig admin (3-of-5 Stellar signers)
- LP-governed invoice funding (stake LP tokens to vote on invoices)
- Dynamic utilization-based interest rate model
- Batch invoice creation
- Swap-free multi-asset pools (USDC + XLM)
- On-chain governance for protocol parameters

### Tooling & Infrastructure

- Mainnet deployment support (currently testnet only)
- Formal verification or symbolic execution audit
- `cargo-soroban` integration for automated WASM budget reporting
- TypeScript SDK for contract interaction
- Automated fuzz testing across all four contracts
- Smart-contract upgrade mechanism (`__constructor` + `__upgrade`)

> **Note:** Partial repayments are now supported on the **escrow** path
> (`escrow::release_to_pool(invoice_id, repayment_amount)` accepts a
> partial `repayment_amount` and tracks the residual in the escrow
> record). They are **not** yet supported on the buyer-driven repayment
> path (`invoice::repay` still requires the full `face_value`). This
> item has been removed from the active scope above and moved to the
> [Resolved](#resolved) section.

---

## Resolved

These limitations were previously listed here but have been fixed since
the 2025-07-25 revision.

- **Index entries not compacted on status transition.** `move_status_index`
  now performs an O(1) remove from the old status membership set
  (`DataKey::StatusMembership`) and an O(1) append to the new status
  index, so `get_by_status()` filters stale entries via the membership
  marker at constant cost. Historical `StatusIndexEntry` rows that point
  to a moved invoice still occupy a slot in the per-status index but
  are skipped at read time — they are not reclaimed on disk. See
  PR #121 (*Fix O(n) status index filtering with O(1) membership lookup*).
- **Pool does not track individual LP yield accrual.** `pool::withdraw`
  now writes `DataKey::LPYieldEarned(lp)` and `pool::get_lp_position`
  returns the running yield figure per LP. Yield is therefore visible
  before the LP triggers a withdrawal.
- **Partial default-side repayment.** `escrow::release_to_pool` accepts
  a `repayment_amount` that may be less than the original locked amount,
  keeps the residual escrow record in place, and emits history events
  per partial release. See PR #120
  (`feat/partial-repayment-default-flow`). Buyer-driven partial
  repayments via `invoice.repay` remain a future-feature — see
  [Known Gaps](#known-gaps).
- **Security findings #94, #96, #98, #103.** Addressed together in
  PR #124 (`fix: address security issues #94, #96, #98, #103`).

---

## Related Issues

- [#56 — Wire issuer release into fund_invoice](https://github.com/TrusTrove/TrusTrove-contract/issues/56) (open)
- [#285 — Event schema documentation](https://github.com/TrusTrove/TrusTrove-contract/issues/285)
- [#286 — Gas budget report](https://github.com/TrusTrove/TrusTrove-contract/issues/286)
- [#287 — Storage schema docs](https://github.com/TrusTrove/TrusTrove-contract/issues/287)
- [#294 — Testnet limitation docs](https://github.com/TrusTrove/TrusTrove-contract/issues/294)
- [#307 — General repo documentation](https://github.com/TrusTrove/TrusTrove-contract/issues/307)
- [#460 — LIMITATIONS.md stale date](https://github.com/TrusTrove/TrusTrove-contract/issues/460) (this PR fixes it)

Resolved items reference the PRs that closed them:

- Issue #67 — status index O(1) filtering (closed by PR #121)
- Issues #94, #96, #98, #103 — security (closed by PR #124)
- Partial default repayment (closed by PR #120)
