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

#### Profile Structure

```
Profile {
    address:      Address,          // Stellar address (also the key)
    packed_flags: u32,              // Bit 0: role (0=Issuer, 1=Buyer)
                                    // Bit 1: verified status
    registered_at: u64,             // Unix timestamp of registration
    metadata:     Map<String, String>, // Arbitrary key-value metadata
}
```

#### Indexing Notes

- Profiles are looked up by `is_verified(address)` — no secondary index exists.
- `get_profile(address)` returns the full `Profile` struct or panics with
  `NotFound` if absent.
- `get_verification_status(address)` returns a three-valued enum
  (`Unregistered`, `Verified`, `Revoked`).

### Storage Key Count

**Approximately 1 key per registered address.**

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
```

#### Index Entries

| DataKey | Type | Description |
|---------|------|-------------|
| `IssuerIndexCount(Address)` | `u32` | Number of invoices for this issuer |
| `IssuerIndexEntry(Address, u32)` | `BytesN<32>` | Invoice ID by issuer + index |
| `BuyerIndexCount(Address)` | `u32` | Number of invoices for this buyer |
| `BuyerIndexEntry(Address, u32)` | `BytesN<32>` | Invoice ID by buyer + index |
| `StatusIndexCount(u32)` | `u32` | Number of invoices in this status |
| `StatusIndexEntry(u32, u32)` | `BytesN<32>` | Invoice ID by status + index |
| `StatusCount(u32)` | `u64` | Count of invoices in each status |

#### Indexing Notes

- Three index families (`IssuerIndex`, `BuyerIndex`, `StatusIndex`) each
  maintain a count key and an ordered list of entries.
- Index entries are appended — no compaction on status transitions (entries
  are added to the new status but not removed from the old).
- `get_by_status()` reads all entries for a status and only returns those
  whose current `invoice.status` matches (to handle stale index entries).

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

### Persistent Storage

#### Escrow Lock Entry

| DataKey | Type | Description |
|---------|------|-------------|
| `Locked(BytesN<32>)` | `EscrowRecord` | Lock record keyed by invoice ID |

```
EscrowRecord {
    invoice_id: BytesN<32>,
    amount:     u128,      // USDC stroops
    locked_at:  u64,       // Unix timestamp
}
```

#### History Entry

| DataKey | Type | Description |
|---------|------|-------------|
| `History(BytesN<32>)` | `Vec<EscrowEvent>` | Append-only event log keyed by invoice ID |

```
EscrowEvent {
    invoice_id: BytesN<32>,
    action:     EscrowAction,   // Locked | ReleasedToIssuer | ReleasedToPool | DefaultHandled
    amount:     u128,
    timestamp:  u64,
}
```

#### Lifecycle

1. `lock()` creates an `EscrowRecord` and transfers USDC from pool → escrow.
2. `release_to_issuer()` or `release_to_pool()` transfers USDC out and
   **removes** the `Locked` key (caller must re-lock for a new funding cycle).
3. `handle_default()` returns locked USDC to the pool and **removes** the key.
4. On removal, the TTL-granted entries also expire organically.

### Storage Key Count

**Approximately 2 keys per funded invoice** — one `Locked` record and one
`History` vector (which shares its TTL with the lock).

---

## Contract: pool_contract

### Instance Storage

| DataKey | Type | Description | Set During |
|---------|------|-------------|------------|
| `Admin` | `Address` | Contract admin | `initialize()` |
| `InvoiceContract` | `Address` | Invoice contract address | `initialize()` |
| `EscrowContract` | `Address` | Escrow contract address | `initialize()` |
| `UsdcAsset` | `Address` | USDC token contract address | `initialize()` |
| `TotalShares` | `u128` | Total LP shares outstanding | `initialize() = 0` |
| `TotalDeposits` | `u128` | Total USDC principal deposited | `initialize() = 0` |
| `TotalFunded` | `u128` | Total USDC deployed to invoices | `initialize() = 0` |
| `TotalYieldDistributed` | `u128` | Cumulative yield distributed | `initialize() = 0` |
| `ActiveInvoiceCount` | `u32` | Currently funded invoices | `initialize() = 0` |
| `MaxUtilizationBps` | `u32` | Max utilization cap (bps) | `initialize() = 8500` |

### Persistent Storage

#### LP Position Keys

| DataKey | Type | Description |
|---------|------|-------------|
| `LPShares(Address)` | `u128` | Share balance per LP address |
| `LPDepositCount(Address)` | `u32` | Number of deposits made by this LP |
| `LPYieldEarned(Address)` | `u128` | Cumulative yield earned (updated on withdraw) |
| `LPInitialDeposit(Address)` | `u128` | Total principal deposited by LP (tracked for yield calculation) |

#### Funded Invoice

| DataKey | Type | Description |
|---------|------|-------------|
| `FundedInvoice(BytesN<32>)` | `u128` | Funded amount keyed by invoice ID |

### Storage Key Count

**Approximately 6 instance + (5 × number_of_LPs) + (1 × number_of_funded_invoices).**

In practice most pools will have:
- 6 instance keys (constant)
- 5 keys per active LP
- 1 key per funded invoice (removed on repayment/default)

---

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

The contracts do **not** currently implement the Stellar
`__constructor`/`__upgrade` pattern. An upgrade requires deploying a new
contract and wiring the frontend to the new address. See
[DEPLOYMENT.md](../DEPLOYMENT.md) for the rollback procedure.
