# Storage Schema

> **Updated:** 2025-07-25
> **Applies to:** All four Soroban smart contracts

This document describes the on-chain storage layout for every contract in the
TrusTrove protocol. Integrators, indexer operators, and contributors should
refer to this when designing off-chain read paths or auditing storage usage.

---

## Table of Contents

- [Storage Overview](#storage-overview)
- [TTL Management](#ttl-management)
- [Contract: registry\_contract](#contract-registry_contract)
- [Contract: invoice\_contract](#contract-invoice_contract)
- [Contract: escrow\_contract](#contract-escrow_contract)
- [Contract: pool\_contract](#contract-pool_contract)

---

## Storage Overview

Soroban storage is key-value, typed, and partitioned into two
`Env::storage()` namespaces:

| Namespace | Use | TTL | Used For |
|-----------|-----|-----|----------|
| `instance()` | Singleton keys | Extended on every state-changing call | Admin addresses, contract references, configuration values |
| `persistent()` | Keyed entries | Extended per-entry on write | Profiles, invoices, escrow records, LP positions |

All amount fields (`u128`) are denominated in **USDC stroops**
(1 USDC = 10,000,000 stroops, 7 decimals).

All timestamps (`u64`) are **Unix time** (seconds since epoch).

---

## TTL Management

All contracts follow a consistent TTL extension pattern:

| Storage Type | `live_until_ledger` | `seq` (TTL entries) |
|-------------|---------------------|---------------------|
| Instance (every state-changing call) | 100 | 2,000,000 |
| Persistent (after every `set()`) | 100 | 2,000,000 |

> **Important:** Every call to `persistent().set()` in the codebase is
> immediately followed by `persistent().extend_ttl()`. The `extend_instance_ttl()`
> helper is called at the end of every public state-changing function.

---

## Contract: registry_contract

### Instance Storage

| DataKey | Type | Description | Set During |
|---------|------|-------------|------------|
| `Admin` | `Address` | Contract admin address | `initialize()` |

### Persistent Storage

#### Profile Entry

| DataKey | Type | Description |
|---------|------|-------------|
| `Profile(Address)` | `Profile` | Profile record keyed by Stellar address |

#### Profile Enumeration Index

| DataKey | Type | Description |
|---------|------|-------------|
| `ProfileIndex(Role, u32)` | `Address` | The `index`-th registered address for `Role::Issuer` / `Role::Buyer`, in registration order |
| `ProfileCount(Role)` | `u32` | Number of populated `ProfileIndex` slots for that role |

Both keys are written by **every** registration path (`register_issuer`,
`register_buyer`, `batch_register_issuers`, `batch_register_buyers`) and are
TTL-extended with the same threshold/target as a profile entry
(`persistent().extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO)`), on write and on read.
Entries skipped as already-registered are never indexed twice, so
`ProfileCount(role)` always equals the number of addresses reachable through
`list_profiles(role, …)`.

#### Profile Structure

```
Profile {
    address:      Address,          // Stellar address (also the key)
    packed_flags: u32,              // Packed bit-field: see "Profile Flags" below
    registered_at: u64,             // Unix timestamp of registration
    metadata:     Map<String, String>, // Arbitrary key-value metadata
}
```

#### Profile Flags

`packed_flags` is a `u32` bitmask that encodes three boolean values. The bit
layout is an internal implementation detail and is NOT a stable external
contract; it may change without a corresponding change to this doc. Off-chain
consumers must not decode it directly — use the decoded contract views
`get_role`, `is_verified` and `get_verification_status` instead.

| Bit | Value | Meaning |
|-----|-------|---------|
| 0   | 0     | `Role::Issuer` |
| 0   | 1     | `Role::Buyer` |
| 1   | 0     | Not verified (`verified == false`) |
| 1   | 1     | Verified (`verified == true`) |
| 2   | 0     | Not revoked (`revoked == false`) |
| 2   | 1     | Revoked (`revoked == true`) |
| 3–31| —     | Reserved (always 0 in current code) |

##### Valid Flag Combinations

The three meaningful bits yield eight theoretical combinations, but only three
are valid in the protocol's state machine. The role bit (bit 0) is immutable
after registration; the verified and revoked bits are mutually exclusive — a
profile cannot be both verified and revoked simultaneously.

| packed_flags (binary) | Role    | Verified | Revoked | `VerificationStatus` | Description |
|-----------------------|---------|----------|---------|---------------------|-------------|
| `0b000` (0)           | Issuer  | false    | false   | `Pending`           | Registered, awaiting admin verification |
| `0b010` (2)           | Issuer  | true     | false   | `Verified`          | Active, eligible for invoice creation |
| `0b100` (4)           | Issuer  | false    | true    | `Revoked`           | Revoked, cannot originate new invoices |
| `0b001` (1)           | Buyer   | false    | false   | `Pending`           | Registered, awaiting admin verification |
| `0b011` (3)           | Buyer   | true     | false   | `Verified`          | Active, eligible as invoice counterparty |
| `0b101` (5)           | Buyer   | false    | true    | `Revoked`           | Revoked, cannot be counterparty on new invoices |

All other bit patterns (e.g. `0b110`, `0b111`) are invalid and never produced by
the contract. The `get_verification_status` view encodes this invariant:
- `Unregistered` — no `Profile` entry exists
- `Pending` — entry exists, `verified == false`, `revoked == false`
- `Verified` — entry exists, `verified == true`
- `Revoked` — entry exists, `revoked == true`

##### Decoding Example (matches `Profile` accessors)

```rust
// Equivalent to Profile::role()
let role = if (packed_flags & 1) != 0 { Role::Buyer } else { Role::Issuer };

// Equivalent to Profile::verified()
let verified = (packed_flags & 2) != 0;

// Equivalent to Profile::revoked()
let revoked = (packed_flags & 4) != 0;

// Equivalent to Profile::get_verification_status()
let status = if !profile_exists {
    VerificationStatus::Unregistered
} else if verified {
    VerificationStatus::Verified
} else if revoked {
    VerificationStatus::Revoked
} else {
    VerificationStatus::Pending
};
```

#### Indexing Notes

- Profiles are looked up by `is_verified(address)`; the per-role
  `ProfileIndex` / `ProfileCount` keys back the enumeration views
  `get_profile_count(role)` and `list_profiles(role, start, limit)`.
- `list_profiles` returns pages of at most 50 addresses (the same cap as the
  batch registration entry points) and panics with `PageSizeExceeded` (`#8`)
  above it, so a single call can never walk the whole index. Pages are
  contiguous and non-overlapping: page `n` is
  `list_profiles(role, n * limit, limit)`.
- The index is append-only. The registry has no deregistration path —
  `revoke` only flips the profile's verification flags — so a slot is never
  rewritten or freed, and revoked profiles stay enumerated.
- `get_profile(address)` returns the full `Profile` struct or panics with
  `NotFound` if absent.
- `get_verification_status(address)` returns a three-valued enum
  (`Unregistered`, `Verified`, `Revoked`).

### Storage Key Count

**Approximately 2 keys per registered address** — one `Profile` record and one
`ProfileIndex(role, n)` slot — **plus 2 constant `ProfileCount(role)` keys**
(one per role) that exist once the first profile of that role is registered.

---

## Contract: invoice_contract

### Instance Storage

| DataKey | Type | Description | Set During |
|---------|------|-------------|------------|
| `Admin` | `Address` | Contract admin | `initialize()` |
| `RegistryContract` | `Address` | Registry contract address | `initialize()` |
| `PoolContract` | `Address` | Pool contract address | `set_pool_contract()` |
| `Counter` | `u64` | Monotonically increasing invoice counter | `initialize()`, incremented on `create()` |
| `ExpiryWindow` | `u64` | Listing expiry window in seconds (default `604800`) | `set_expiry_window()` |
| `SupportedAssetCount` | `u32` | Number of supported funding assets | `add_supported_asset()` |
| `SupportedAsset(Address)` | `bool` | True if the asset is allowed for new invoices. *Note: Under the `pool_factory` model, an asset listed here should also be registered via `pool_factory::register_asset` to ensure `fund_invoice` is reachable.* | `add_supported_asset()` |

### Persistent Storage

#### Primary Invoice Entry

| DataKey | Type | Description |
|---------|------|-------------|
| `Invoice(BytesN<32>)` | `Invoice` | Full invoice record keyed by SHA-256 invoice ID |

#### Invoice Structure

```
Invoice {
    id:               BytesN<32>,       // SHA-256 hash of (issuer, buyer,
                                        //   face_value, due_date, counter, asset)
    issuer:           Address,          // Invoice creator
    buyer:            Address,          // Obligated repayer
    face_value:       u128,             // Full invoice value (stroops)
    discount_bps:     u32,              // Discount in basis points (max 5000)
    funded_amount:    u128,             // Amount actually funded (≤ face_value)
    due_date:         u64,              // Maturity timestamp
    status:           InvoiceStatus,    // Current lifecycle state
    created_at:       u64,
    listed_at:        Option<u64>,
    funded_at:        Option<u64>,
    shipped_at:       Option<u64>,
    issuer_confirmed: bool,
    buyer_confirmed:  bool,
    repaid_at:        Option<u64>,
    funding_asset:    Address,          // Token contract address
    funding_pool:     Option<Address>,  // Pool that funded this invoice
}
```

#### InvoiceStatus Enum (u32 repr)

```rust
Created   = 0   // initial state
Listed    = 1   // ready for funding
Funded    = 2   // escrow lock placed
Active    = 3   // goods shipped
Confirmed = 4   // dual delivery confirmation
Repaid    = 5   // buyer has repaid
Defaulted = 6   // past due without repayment
Expired   = 7   // listing expired before funding
Cancelled = 8   // issuer cancelled before listing
```

#### Index Entries

| DataKey | Type | Description | TTL extended |
|---------|------|-------------|--------------|
| `IssuerIndexCount(Address)` | `u32` | Number of invoices for this issuer | On write (`extend_issuer_index`) and on read (`get_by_issuer`) |
| `IssuerIndexEntry(Address, u32)` | `BytesN<32>` | Invoice ID by issuer + index | On write (`extend_issuer_index`) and on read (`get_by_issuer`) |
| `BuyerIndexCount(Address)` | `u32` | Number of invoices for this buyer | On write (`extend_buyer_index`) and on read (`get_by_buyer`) |
| `BuyerIndexEntry(Address, u32)` | `BytesN<32>` | Invoice ID by buyer + index | On write (`extend_buyer_index`) and on read (`get_by_buyer`) |
| `StatusIndexCount(u32)` | `u32` | Number of invoices in this status | On write (`extend_status_index` / `remove_from_status_index`) and on read (`get_by_status`) |
| `StatusIndexEntry(u32, u32)` | `BytesN<32>` | Invoice ID by status + index | On write (`extend_status_index` / `remove_from_status_index`) and on read (`get_by_status`) |
| `StatusCount(u32)` | `u64` | Count of invoices in each status | On write (`increment_status_count` / `decrement_status_count`, issue #830) |

#### Indexing Notes

- Three index families (`IssuerIndex`, `BuyerIndex`, `StatusIndex`) each
  maintain a count key and an ordered list of entries.
- Index entries are appended — no compaction on status transitions (entries
  are added to the new status but not removed from the old).
- `get_by_status()`, `get_by_issuer()`, and `get_by_buyer()` are paginated
  (`page`, `page_size`, capped at `MAX_PAGE_SIZE`). Each call reads at most
  `page_size` index entries, so cost is bounded regardless of index size.
- `get_by_status()` still filters each page to entries whose current
  `invoice.status` matches (to handle stale index entries), so a page may
  contain fewer than `page_size` results.

### Storage Key Count

**Approximately 5 + (7 × number_of_invoices)** keys per active invoice:
- 5 instance keys
- 1 invoice entry
- 1 issuer index entry (+1 for count if first)
- 1 buyer index entry (+1 for count if first)
- 1 status index entry (+1 for each status transition)

---

## Contract: escrow_contract

### Instance Storage

| DataKey | Type | Description | Set During |
|---------|------|-------------|------------|
| `Admin` | `Address` | Contract admin | `initialize()` |
| `PoolContract` | `Address` | Authorised pool contract | `initialize()` |
| `InvoiceContract` | `Address` | Invoice contract address | `initialize()` |
| `UsdcAsset` | `Address` | USDC token contract address | `initialize()` |
| `Paused` | `bool` | Emergency circuit-breaker flag, via `trusttrove_pause::PauseState::Paused` (issue #716). Absent means not paused. | `pause()` / `unpause()` |
| `TimeoutWindow` | `u64` | Seconds after `locked_at` before `release_after_timeout` may permissionlessly return a lock to the pool. Absent means the 30-day default; capped at 365 days. | `set_timeout_window()` |

### Persistent Storage

#### Escrow Lock Entry

| DataKey | Type | Description |
|---------|------|-------------|
| `Locked(BytesN<32>)` | `EscrowRecord` | Lock record keyed by invoice ID. Created by `lock()` and removed by `release_to_pool()` (full release), `handle_default()`, or when the residual is fully repaid. |
| `IssuerReleased(BytesN<32>)` | `u128` | Durable payout evidence written by `release_to_issuer` when it consumes the `Locked` record (issue #819); consumed by exactly one `release_to_pool` and read by `was_released_to_issuer` for `pool.handle_default`. Written when the `Locked` record is removed during `release_to_issuer`. |

```
EscrowRecord {
    invoice_id: BytesN<32>,
    amount:     u128,      // USDC stroops
    locked_at:  u64,       // Unix timestamp
    issuer:     Address,   // Issuer authorized to receive released funds
}
```

#### History Entry

| DataKey | Type | Description |
|---------|------|-------------|
| `History(BytesN<32>)` | `Vec<EscrowEvent>` | Append-only event log keyed by invoice ID. Every state transition (`lock`, `release_to_issuer`, `release_to_pool`, `handle_default`, `release_after_timeout`) appends an `EscrowEvent`. The vector grows as payments are made and is TTL-extended on every write. |

```
EscrowEvent {
    invoice_id: BytesN<32>,
    action:     EscrowAction,   // Locked | ReleasedToIssuer | ReleasedToPool | DefaultHandled | TimeoutReleased
    amount:     u128,
    timestamp:  u64,
    caller:     Option<Address>, // Set for `DefaultHandled` events
}
```

#### Partial Release and Residual Lock

The escrow supports partial `release_to_pool` calls. The behavior is:

1. On each `release_to_pool(invoice_id, repayment_amount)` call, if a live `Locked` record exists with `amount > repayment_amount`:
   - The `released_to_pool` event is emitted with the `repayment_amount`.
   - The `Locked` record's `amount` field is reduced by `repayment_amount` (in-place update).
   - The original `locked_at` timestamp is preserved.
   - `get_locked()` now returns the reduced residual amount.
2. If `repayment_amount >= record.amount`:
   - The full remaining lock is repaid.
   - The `Locked` record is removed (consumed).
   - If an `IssuerReleased` marker was present (funding path already paid issuer), it is also removed.
   - A second `release_to_pool` call on the same invoice (with no live lock and no marker) reverts with `NotFound` — the `NotFound` guard (#2) ensures idempotent settlement at most once per funded invoice.
3. The `History` vector appends a `ReleasedToPool` event on each call, enabling indexers to track cumulative repayment progress and the residual lock state over time.

#### Final-Release Removal Rule

The `Locked` record (and optionally the `IssuerReleased` marker) is removed under exactly one of these conditions:

- **Full repayment**: `release_to_pool` is called with `repayment_amount >= record.amount`, consuming the entire lock.
- **Default handling**: `handle_default` is called and a live `Locked` record exists (not yet paired with an issuer release). The record is removed and funds returned to the pool.
- **Timeout release**: `release_after_timeout` is called after `now >= locked_at + timeout_window`. The record is removed and funds returned to the pool.
- **Second call guard**: A second `release_to_pool` call on an invoice that has already had its lock fully consumed and marker consumed reverts with `NotFound`, preventing double-release.

The `History` vector entries are never removed — they form an append-only log of all escrow actions for the invoice. Indexers can reconstruct the residual lock state by summing `ReleasedToPool` amounts and comparing against the original `Locked.amount`.

### Storage Key Count

**Approximately 3 keys per funded invoice** — a transient `Locked` record (removed on full release/default/timeout), an
`IssuerReleased` marker (live between funding and repayment, consumed on repayment), and one
`History` vector (append-only, grows with each escrow action, shares TTL with the lock).

---

## Contract: pool_contract

### Instance Storage

| DataKey | Type | Description | Set During |
|---------|------|-------------|------------|
| `Admin` | `Address` | Contract admin | `initialize()` |
| `InvoiceContract` | `Address` | Invoice contract address | `initialize()` |
| `EscrowContract` | `Address` | Escrow contract address | `initialize()` |
| `FundingAsset` | `Address` | Asset this pool instance funds invoices with | `initialize()` |
| `TotalShares` | `u128` | Total LP shares outstanding | `initialize() = 0` |
| `TotalDeposits` | `u128` | Total USDC principal deposited | `initialize() = 0` |
| `TotalFunded` | `u128` | Total USDC deployed to invoices | `initialize() = 0` |
| `TotalYieldDistributed` | `u128` | Cumulative yield distributed | `initialize() = 0` |
| `ActiveInvoiceCount` | `u32` | Currently funded invoices | `initialize() = 0` |
| `MaxUtilizationBps` | `u32` | Max utilization cap (bps) | `initialize() = 8500` |
| `ProtocolFeeBps` | `u32` | Protocol fee in basis points (max 2000 bps = 20%) | `initialize() = 0` |
| `TreasuryAddress` | `Address` | Protocol treasury destination for fee cuts | `initialize() = treasury (may equal admin)` |
| `MinInitialDeposit` | `u128` | Minimum first deposit an empty pool accepts, in `FundingAsset` stroops | `initialize()` |
| `ShareName` | `String` | SEP-41 `name()` of this pool's LP share token | `initialize()` |
| `ShareSymbol` | `String` | SEP-41 `symbol()` of this pool's LP share token | `initialize()` |
| `ShareDecimals` | `u32` | SEP-41 `decimals()` of this pool's LP share token (falls back to 7 when unset) | `initialize()` |

### Persistent Storage

#### LP Position Keys

| DataKey | Type | Description |
|---------|------|-------------|
| `LPShares(Address)` | `u128` | Share balance per LP address |
| `LPDepositCount(Address)` | `u32` | Number of deposits made by this LP |
| `LPYieldEarned(Address)` | `u128` | Cumulative yield earned (updated on withdraw) |
| `LPInitialDeposit(Address)` | `u128` | Total principal deposited by LP (tracked for yield calculation) |

#### SEP-41 Share Transfers

LP shares move between addresses through two public entry points that share
one balance-movement path (`move_shares`) and emit the same standard
`transfer(from, to, amount)` event:

- `transfer(from, to, amount)` — the standard SEP-41 entry point. Generic
  `soroban_sdk::token::Client` consumers call this directly against the pool
  address.
- `transfer_from(spender, from, to, amount)` — moves shares against an
  `approve` grant, debiting the allowance.
- `transfer_shares(from, to, amount)` — **non-standard** legacy alias of
  `transfer`, kept for integrators that predate the SEP-41 surface. New code
  must use `transfer`.

Transfers only move balances: `TotalShares`, `TotalDeposits`, `TotalFunded`,
yield accounting, and share price are untouched. Self-transfers are no-ops.
The recipient's `LPShares` entry is created on first credit, exactly as a
deposit would create it.

#### SEP-41 Allowance Keys

| DataKey | Type | Description |
|---------|------|-------------|
| `Allowance(Address, Address)` | `ShareAllowance { amount: i128, expiration_ledger: u32 }` | Shares `from` let `spender` move, until `expiration_ledger`. Removed when fully spent or revoked with `approve(..., amount = 0)`; read back as `0` once expired. |

#### Funded Invoice

| DataKey | Type | Description |
|---------|------|-------------|
| `FundedInvoice(BytesN<32>)` | `u128` | Funded amount keyed by invoice ID |

### Protocol Fee & Yield Distribution Math

When repayments occur via `receive_repayment` or `receive_repayment_with_refund`, gross yield is computed as:
$$\text{yield\_amount} = \text{amount} - \text{funded\_amount} - \text{refund}$$

With a configured protocol fee (`fee_bps > 0`):
$$\text{protocol\_cut} = \frac{\text{yield\_amount} \times \text{fee\_bps}}{10\,000}$$
$$\text{lp\_yield} = \text{yield\_amount} - \text{protocol\_cut}$$

- **Treasury Transfer:** If `protocol_cut > 0`, the pool transfers `protocol_cut` USDC directly to `TreasuryAddress`. At the default `fee_bps == 0`, no transfer is made.
- **LP Accounting:** Only `lp_yield` (not the full gross yield) is added to `TotalDeposits` and `TotalYieldDistributed`.
- **Admin Configuration:** The fee defaults to 0 bps at initialization and can be modified up to 2000 bps (20%) along with the treasury address by the contract admin via `set_protocol_fee(fee_bps, treasury)`.

### Storage Key Count

**Approximately 8 instance + (5 × number_of_LPs) + (1 × number_of_funded_invoices).**

In practice most pools will have:
- 8 instance keys (constant)
- 5 keys per active LP
- 1 key per funded invoice (removed on repayment/default)

---

## Contract: pool_factory

### Instance Storage

| DataKey | Type | Description | Set During |
|---------|------|-------------|------------|
| `Admin` | `Address` | Contract admin | `initialize()` |
| `AssetCount` | `u32` | Number of registered assets | `register_asset()` / `register_existing_pool()` |
| `AssetIndex(u32)` | `Address` | Ordered index of registered assets | `register_asset()` / `register_existing_pool()` |
| `PoolForAsset(Address)` | `Address` | The pool contract deployed/tracked for an asset | `register_asset()` / `register_existing_pool()` |

### Storage Key Count

**Approximately 2 + (2 × number_of_assets).**

Each registered asset requires two keys: one for its position in the list (`AssetIndex`) and one mapping its address to a pool (`PoolForAsset`).


## Gas / Budget Estimates

> **Note:** These are approximate and will vary with network conditions.
> Update after mainnet deployment.

| Operation | Read Entries | Write Entries | Relative Cost |
|-----------|-------------|---------------|---------------|
| `registry::register_issuer` | 1 | 2 (+ TTL) | Low |
| `invoice::create` | 5+ | 6+ (+ TTL entries) | Medium |
| `invoice::repay` | 3 | 3 (+ TTL + token transfer) | Medium-High |
| `pool::deposit` | 5 | 5 (+ TTL + token transfer) | Medium |
| `pool::fund_invoice` | 6+ | 5 (+ TTL + cross-contract calls) | High |
| `escrow::lock` | 2 | 2 (+ TTL + token transfer) | Medium |

See [LIMITATIONS.md](./LIMITATIONS.md) for current testnet budget constraints.

---

## Upgrade Path

The invoice, escrow, pool, and registry contracts expose an admin-gated
`upgrade(new_wasm_hash: BytesN<32>)` entry point backed by Soroban's
`env.deployer().update_current_contract_wasm`. First install the new Wasm on the
network, then call `upgrade` with its hash from the contract's stored admin
address. The contract is updated in place, so its address and storage remain
unchanged, and an `upgraded` event carrying the new Wasm hash is emitted. Calls
from any address other than the stored admin fail authorization. Protect the admin
with a multisig as described in [DEPLOYMENT.md](../DEPLOYMENT.md). See that
document for the redeployment rollback procedure.
