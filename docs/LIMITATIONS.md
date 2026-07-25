# Limitations

> **Updated:** 2025-07-25
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
| `invoice::repay` | 1 (`receive_repayment_with_refund`) | 1 (buyer → pool) | Medium |
| `invoice::trigger_default` | 2 (`handle_default` ×2) | 0 | Medium |
| `pool::deposit` | 0 | 1 (LP → pool) | Low-Medium |
| `pool::withdraw` | 0 | 1 (pool → LP) | Medium |
| `pool::fund_invoice` | 4 total (get_status, get_funding_asset, get_face_value, get_discount_bps, lock, mark_funded) | 1 (pool → escrow) | **High** |
| `pool::receive_repayment` | 0 | 0 | Low |
| `pool::handle_default` | 1 (escrow::handle_default) | 1 (escrow → pool) | Medium |
| `escrow::lock` | 0 | 1 (pool → escrow) | Medium |
| `escrow::release_to_issuer` | 0 | 1 (escrow → issuer) | Medium |
| `escrow::handle_default` | 0 | 1 (escrow → pool) | Medium |

### Budget Considerations

- `pool::fund_invoice` is the most expensive operation — it makes 4
  cross-contract calls and performs 1 token transfer. On a congested testnet,
  this may exceed the per-ledger resource limit.
- Read-only view functions (`get_stats`, `get`, `get_profile`, etc.) incur
  minimal cost as they only read from storage.
- Index enumeration functions (`get_by_status`, `get_by_issuer`,
  `get_by_buyer`) scale linearly with the number of entries and may become
  expensive for issuers/buyers with many invoices.

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

The `repay_early` function exists but the protocol does not apply dynamic
discount rates based on how early the repayment occurs. The implementation
uses a linear pro-rata discount calculation (`elapsed / term × discount`)
instead of a yield curve.

### No Accessor for `funded_amount` on Invoice Contract

While `get_face_value` and `get_discount_bps` are exposed as public view
functions, there is no `get_funded_amount` accessor. Callers currently read
the full `Invoice` struct via `get()`.

### Index Entries Are Not Compacted on Status Transition

When an invoice transitions from `Created` to `Listed`, a new entry is
appended to the `StatusIndex` for `Listed`, but the old entry for `Created`
is left in place. `get_by_status()` handles this by filtering by current
status, but the stale entries accumulate over time, increasing storage costs.

### No Duplicate Invoice Detection

The `create` function generates an invoice ID from a SHA-256 hash of
(issuer, buyer, face_value, due_date, counter, asset). The counter
guarantees uniqueness even for otherwise identical invoices, but there is
no check for duplicate invoices with the same parameters.

### Pool Does Not Track Individual LP Yield Accrual

Yield is only realised when an LP withdraws (via the `withdraw` function's
pro-rata calculation). There is no per-LP yield accrual ledger that tracks
yield earned but not yet withdrawn. This means an LP cannot see their
unrealised yield without doing a manual computation.

---

## Unhandled Edge Cases

### What Happens When…

| Scenario | Current Behaviour | Notes |
|----------|------------------|-------|
| LP deposits in a pool with 0 shares and 0 deposits | 1:1 share minting | Correct |
| LP deposits when `total_shares > 0` but `total_deposits = 0` | Division by zero would panic | Cannot happen in practice (shares only minted alongside deposits) |
| Buyer repays exactly on `due_date` | `invoice.repay` succeeds, but `trigger_default` also checks `<= due_date` | Race condition: `trigger_default` blocks on due date (must be *past*), but `repay` succeeds any time after confirmation |
| `confirm_delivery` called after invoice is already confirmed | Panics with `InvalidStatusTransition` because status is `Confirmed` not `Active` | Correct — prevents replay |
| `withdraw` when pool has funded invoices but no available liquidity | Panics with `InsufficientLiquidity` | Correct — prevents breaking the pool invariant |
| Admin calls `set_max_utilization` to 0 | All `fund_invoice` calls will fail (utilization > cap) | This is a denial-of-service vector available to admin |
| Calling `revoke` on an unregistered address | Panics with `NotFound` | Per spec |
| Calling `batch_register_issuers` with more than 50 entries | Panics with `BatchSizeExceeded` | Gas protection |
| A buyer repays *less* than `face_value` | Not possible — `repay` calls `token.transfer(&buyer, &pool, &face_value)` for the full amount | Partial repayments are not supported |
| An issuer creates an invoice with `due_date = current_time + 1` second | Succeeds | Minimal buffer accepted |
| The pool has exactly 0 USDC balance after funding all deposits | `withdraw` succeeds for unfunded amounts, but funded invoices are locked | Escrow holds funded USDC, pool holds only unfunded USDC |

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
- Partial repayments
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

---

## Related Issues

- [#56 — Wire issuer release into fund_invoice](https://github.com/TrusTrove/TrusTrove-contract/issues/56)
- [#285 — Event schema documentation](https://github.com/TrusTrove/TrusTrove-contract/issues/285)
- [#286 — Gas budget report](https://github.com/TrusTrove/TrusTrove-contract/issues/286)
- [#287 — Storage schema docs](https://github.com/TrusTrove/TrusTrove-contract/issues/287)
- [#294 — Testnet limitation docs](https://github.com/TrusTrove/TrusTrove-contract/issues/294)
- [#307 — General repo documentation (this issue)](https://github.com/TrusTrove/TrusTrove-contract/issues/307)
