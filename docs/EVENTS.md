# Event Catalog

This document lists every event emitted by the TrusTrove smart contracts. Events are published via `env.events().publish(topics, data)` and are indexed by the Go indexer for off-chain consumption.

All amounts are in **USDC stroops** (1 USDC = 10,000,000 stroops).  
All timestamps are **Unix seconds** (u64).  
Invoice IDs are **BytesN<32>** (32-byte identifiers).

---

## Invoice Contract

**Contract:** `invoice_contract`  
**Source:** `contracts/invoice/src/events.rs`

### `invoice_created`

Emitted when a new invoice is created.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_created"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| `topic[2]` | `Address` | Issuer address |
| `topic[3]` | `Address` | Buyer address |
| `topic[4]` | `Address` | Funding asset (USDC token contract) |
| **Data** | `u128` | Face value of the invoice |

**Emitted by:** `create()` in `contracts/invoice/src/lib.rs:276`

---

### `invoice_listed`

Emitted when an invoice is listed for financing.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_listed"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `u32` | Discount in basis points (1 bp = 0.01%) |

**Emitted by:** `list_for_financing()` in `contracts/invoice/src/lib.rs:335`

---

### `invoice_funded`

Emitted when an invoice is funded by the pool.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_funded"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `u128` | Funded amount |

**Emitted by:** `mark_funded()` in `contracts/invoice/src/lib.rs:398`

---

### `invoice_shipped`

Emitted when the issuer marks the invoice as shipped.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_shipped"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `()` | None |

**Emitted by:** `mark_shipped()` in `contracts/invoice/src/lib.rs:444`

---

### `delivery_confirmed`

Emitted when either the issuer or buyer confirms delivery.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"delivery_confirmed"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| `topic[2]` | `Address` | Confirmer address (issuer or buyer) |
| **Data** | `()` | None |

**Emitted by:** `confirm_delivery()` in `contracts/invoice/src/lib.rs:513`

---

### `both_confirmed`

Emitted when both issuer and buyer have confirmed delivery (invoice advances to Confirmed).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"both_confirmed"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `()` | None |

**Emitted by:** `confirm_delivery()` in `contracts/invoice/src/lib.rs:508`

---

### `invoice_repaid`

Emitted when the buyer repays the invoice.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_repaid"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `u128` | Repaid amount (face value) |

**Emitted by:** `repay()` in `contracts/invoice/src/lib.rs:600` and `trigger_default()` (recovery path) at line 673

---

### `invoice_defaulted`

Emitted when an invoice defaults (past due date, not repaid).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_defaulted"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `()` | None |

**Emitted by:** `trigger_default()` in `contracts/invoice/src/lib.rs:741`

---

### `invoice_expired`

Emitted when an invoice expires (extended past due date without action).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_expired"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `()` | None |

**Emitted by:** `check_expiry()` in `contracts/invoice/src/lib.rs:844`

---

### `expiry_window_set`

Emitted when the admin updates the expiry window.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"expiry_window_set"` |
| **Data** | `u64` | New expiry window in seconds |

**Emitted by:** `set_expiry_window()` in `contracts/invoice/src/lib.rs:755`

---

### `ownership_transferred`

Emitted when contract ownership is transferred to a new admin.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"ownership_transferred"` |
| `topic[1]` | `Address` | Previous admin |
| `topic[2]` | `Address` | New admin |
| **Data** | `()` | None |

**Emitted by:** `transfer_ownership()` in `contracts/invoice/src/lib.rs:1180`

---

### `pool_contract_updated`

Emitted when the pool contract address is updated.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"pool_contract_updated"` |
| `topic[1]` | `Address` | Old pool contract |
| `topic[2]` | `Address` | New pool contract |
| **Data** | `()` | None |

**Emitted by:** `update_pool_contract()` in `contracts/invoice/src/lib.rs:116` and `118`

---

### `contract_initialized`

Emitted when the contract is initialized.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"contract_initialized"` |
| `topic[1]` | `Address` | Admin address |
| `topic[2]` | `Address` | Registry contract address |
| **Data** | `()` | None |

**Emitted by:** `initialize()` in `contracts/invoice/src/lib.rs:82`

---

## Pool Contract

**Contract:** `pool_contract`  
**Source:** `contracts/pool/src/events.rs`

### `lp_deposited`

Emitted when an LP deposits USDC into the pool.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"lp_deposited"` |
| `topic[1]` | `Address` | LP address |
| **Data** | `(u128, u128)` | `(usdc_amount, shares_issued)` |

**Emitted by:** `deposit()` in `contracts/pool/src/lib.rs:210`

---

### `lp_withdrawn`

Emitted when an LP withdraws USDC from the pool.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"lp_withdrawn"` |
| `topic[1]` | `Address` | LP address |
| **Data** | `(u128, u128)` | `(usdc_amount, shares_burned)` |

**Emitted by:** `withdraw()` in `contracts/pool/src/lib.rs:314`

---

### `invoice_funded`

Emitted when the pool funds an invoice.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_funded"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `u128` | Funded amount |

**Emitted by:** `fund_invoice()` in `contracts/pool/src/lib.rs:464`

---

### `repayment_received`

Emitted when the pool receives repayment for an invoice.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"repayment_received"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `(u128, u128)` | `(amount, yield_amount)` — amount = face value, yield = discount earned |

**Emitted by:** `receive_repayment()` in `contracts/pool/src/lib.rs:551` and `handle_default()` at line 660

---

### `invoice_defaulted`

Emitted when the pool handles a defaulted invoice.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_defaulted"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `u128` | Loss amount (funded amount lost) |

**Emitted by:** `handle_default()` in `contracts/pool/src/lib.rs:741`

---

### `ownership_transferred` (dead code)

Emitted when pool ownership is transferred. Currently unused (marked `#[allow(dead_code)]`).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"ownership_transferred"` |
| `topic[1]` | `Address` | Old admin |
| **Data** | `Address` | New admin |

**Source:** `contracts/pool/src/events.rs:39`

---

## Escrow Contract

**Contract:** `escrow_contract`  
**Source:** `contracts/escrow/src/events.rs`

### `funds_locked`

Emitted when funds are locked in escrow for an invoice.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"funds_locked"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `u128` | Locked amount |

**Emitted by:** `lock()` in `contracts/escrow/src/lib.rs:118`

---

### `released_to_issuer`

Emitted when locked funds are released to the issuer.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"released_to_issuer"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| `topic[2]` | `Address` | Issuer address |
| **Data** | `u128` | Released amount |

**Emitted by:** `release_to_issuer()` in `contracts/escrow/src/lib.rs:184`

---

### `released_to_pool`

Emitted when locked funds are released back to the pool (on repayment).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"released_to_pool"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| `topic[2]` | `Address` | Pool contract address |
| **Data** | `u128` | Released amount (repayment amount) |

**Emitted by:** `release_to_pool()` in `contracts/escrow/src/lib.rs:243`

---

### `default_resolved`

Emitted when default is resolved and funds returned to pool.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"default_resolved"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| `topic[2]` | `Address` | Pool contract address |
| **Data** | `u128` | Returned amount |

**Emitted by:** `handle_default()` in `contracts/escrow/src/lib.rs:311`

---

### `ownership_transferred` (dead code)

Emitted when escrow ownership is transferred. Currently unused (marked `#[allow(dead_code)]`).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"ownership_transferred"` |
| `topic[1]` | `Address` | Old admin |
| **Data** | `Address` | New admin |

**Source:** `contracts/escrow/src/events.rs:44`

---

## Registry Contract

**Contract:** `registry_contract`  
**Source:** `contracts/registry/src/events.rs`

### `issuer_registered`

Emitted when an issuer is registered (single or batch).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"issuer_registered"` |
| `topic[1]` | `Address` | Issuer address |
| **Data** | `()` | None |

**Emitted by:** `register_issuer()` in `contracts/registry/src/lib.rs:102` and `batch_register_issuers()` at line 145

---

### `buyer_registered`

Emitted when a buyer is registered.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"buyer_registered"` |
| `topic[1]` | `Address` | Buyer address |
| **Data** | `()` | None |

**Emitted by:** `register_buyer()` in `contracts/registry/src/lib.rs:207`

---

### `metadata_updated`

Emitted when a profile's metadata is updated.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"metadata_updated"` |
| `topic[1]` | `Address` | Profile address |
| **Data** | `()` | None |

**Emitted by:** `update_metadata()` in `contracts/registry/src/lib.rs:243`

---

### `address_revoked`

Emitted when an address is revoked (issuer or buyer).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"address_revoked"` |
| `topic[1]` | `Address` | Revoked address |
| **Data** | `()` | None |

**Emitted by:** `revoke()` in `contracts/registry/src/lib.rs:359`

---

### `batch_registered`

Emitted when a batch registration completes.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"batch_registered"` |
| **Data** | `(u32, u32)` | `(registered_count, skipped_count)` |

**Emitted by:** `batch_register_issuers()` in `contracts/registry/src/lib.rs:149`

---

### `profile_verified`

Emitted when a profile's verification status is checked/updated.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"profile_verified"` |
| `topic[1]` | `Address` | Profile address |
| **Data** | `bool` | Verification status (true = verified) |

**Emitted by:** `is_verified()` in `contracts/registry/src/lib.rs:380`

---

### `ownership_transferred`

Emitted when registry ownership is transferred.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"ownership_transferred"` |
| `topic[1]` | `Address` | Old admin |
| `topic[2]` | `Address` | New admin |
| **Data** | `()` | None |

**Emitted by:** `transfer_ownership()` in `contracts/registry/src/lib.rs:410`

---

## Cross-Reference: Duplicate Event Names

Note that some event names appear in multiple contracts. When indexing, filter by contract address:

| Event Name | Contracts |
|------------|-----------|
| `invoice_funded` | `invoice_contract`, `pool_contract` |
| `invoice_defaulted` | `invoice_contract`, `pool_contract` |
| `ownership_transferred` | All four contracts |

---

## Indexing Tips for Go Indexer

1. **Filter by contract address** — each contract has a unique address on Stellar
2. **Use topics for filtering** — `topic[0]` is always the event name (Symbol)
3. **Data decoding** — payloads use Soroban SDK xdr encoding; decode based on the schemas above
4. **Invoice lifecycle** — track `invoice_created` → `invoice_listed` → `invoice_funded` → `invoice_shipped` → `delivery_confirmed` (×2) → `both_confirmed` → `invoice_repaid` / `invoice_defaulted`
5. **Pool events** — `lp_deposited` / `lp_withdrawn` track LP positions; `repayment_received` tracks yield accrual
6. **Escrow events** — `funds_locked` → `released_to_issuer` OR `released_to_pool` / `default_resolved`

---

## Related Documentation

- [README](../README.md) — contract overview and deployment addresses
- [THREAT_MODEL.md](./THREAT_MODEL.md) — trust assumptions and auth gates
- [STORAGE.md](./STORAGE.md) — on-chain data layout and TTL patterns
- [LIMITATIONS.md](./LIMITATIONS.md) — known gaps and testnet constraints