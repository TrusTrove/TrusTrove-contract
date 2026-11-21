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

### `partial_repayment_received`

Emitted when a buyer makes a partial repayment towards an invoice.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"partial_repayment_received"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `(u128, u128)` | Tuple of `(amount_paid, remaining_balance)` in USDC stroops |

**Emitted by:** `repay_partial()` in `contracts/invoice/src/lib.rs`

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

### `paused`

Emitted when the admin engages the emergency circuit breaker.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"paused"` |
| `topic[1]` | `Address` | Admin who authorized the pause |
| **Data** | `()` | None |

**Emitted by:** `pause()` in `contracts/invoice/src/lib.rs`

---

### `unpaused`

Emitted when the admin disengages the emergency circuit breaker.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"unpaused"` |
| `topic[1]` | `Address` | Admin who authorized the unpause |
| **Data** | `()` | None |

**Emitted by:** `unpause()` in `contracts/invoice/src/lib.rs`

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

### `attestation_submitted`

Emitted when an authorized agent submits a risk attestation for an invoice.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"attestation_submitted"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| `topic[2]` | `Symbol` | Agent ID |
| **Data** | `u32` | Risk score (0–10000 bps) |

**Emitted by:** `submit_attestation()` in `contracts/invoice/src/lib.rs:890`

---

### `agent_registry_contract_updated`

Emitted when the admin updates the agent registry contract address.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"agent_registry_contract_updated"` |
| `topic[1]` | `Address` | Old agent registry contract |
| `topic[2]` | `Address` | New agent registry contract |
| **Data** | `()` | None |

**Emitted by:** `set_agent_registry_contract()` in `contracts/invoice/src/lib.rs:230`

---

### `batch_invoices_created`

Emitted once per `batch_create()` call, after the per-invoice `invoice_created`
events, summarizing the batch outcome.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"batch_invoices_created"` |
| **Data** | `(u32, u32)` | `(created, failed)` counts |

> `failed` is always `0` today: `batch_create()` is atomic and reverts the whole
> batch on the first invalid entry, so a non-zero `failed` can never reach the
> chain. The field exists so indexers can key off a stable batch-summary shape.

**Emitted by:** `batch_create()` in `contracts/invoice/src/lib.rs`

---

### `batch_invoices_listed`

Emitted once per `batch_list_for_financing()` call, summarizing the outcome as
`(listed, failed)` counts. Unlike `batch_create()`, listing is per-entry
tolerant, so `failed` can legitimately be non-zero — the same IDs are also
returned by the call.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"batch_invoices_listed"` |
| **Data** | `(u32, u32)` | `(listed, failed)` counts |

**Emitted by:** `batch_list_for_financing()` in `contracts/invoice/src/lib.rs`

---

## Pool Contract

**Contract:** `pool_contract`  
**Source:** `contracts/pool/src/events.rs`

### `pool_initialized`

Emitted when the pool contract is deployed and initialized.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"pool_initialized"` |
| `topic[1]` | `Address` | Admin address |
| `topic[2]` | `Address` | Configured invoice contract address |
| `topic[3]` | `Address` | Configured escrow contract address |
| `topic[4]` | `Address` | Configured funding asset (USDC) contract address |
| **Data** | `()` | None |

**Emitted by:** `__constructor()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:16`

---

### `upgraded`

Emitted when the pool contract WASM bytecode is upgraded to a new hash.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"upgraded"` |
| `topic[1]` | `BytesN<32>` | New WASM bytecode hash |
| **Data** | `()` | None |

**Emitted by:** `upgrade()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:10`

---

### `lp_deposited`

Emitted when an LP deposits USDC into the pool.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"lp_deposited"` |
| `topic[1]` | `Address` | LP address |
| **Data** | `(u128, u128)` | `(usdc_amount, shares_issued)` in stroops |

**Emitted by:** `deposit()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:36`

---

### `lp_withdrawn`

Emitted when an LP withdraws USDC from the pool by burning shares.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"lp_withdrawn"` |
| `topic[1]` | `Address` | LP address |
| **Data** | `(u128, u128)` | `(usdc_amount, shares_burned)` in stroops |

**Emitted by:** `withdraw()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:44`

---

### `transfer`

Emitted by SEP-41 share-movement entry points (`transfer`, `transfer_shares`, and `transfer_from`) using the standard SEP-41 transfer schema so generic token indexers can track LP share movements.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"transfer"` |
| `topic[1]` | `Address` | Sender address (`from`) |
| **Data** | `(Address, i128)` | `(to, amount)` — recipient address and share amount |

**Emitted by:** `transfer()`, `transfer_shares()`, and `transfer_from()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:55`

---

### `invoice_funded`

Emitted when the pool funds an invoice.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_funded"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `u128` | Funded amount in USDC stroops |

**Emitted by:** `fund_invoice()` (and called per-invoice in `batch_fund_invoice()`) in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:63`

---

### `repayment_received`

Emitted when the pool receives repayment for an invoice (via `receive_repayment()` or `receive_repayment_with_refund()`).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"repayment_received"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `(u128, u128, u128)` | `(amount, lp_yield, protocol_cut)` in USDC stroops |

- `amount`: Total repaid face value / payment amount in USDC stroops.
- `lp_yield`: Yield distributed to liquidity providers (credited to `TotalDeposits` and `TotalYieldDistributed`).
- `protocol_cut`: Fee cut transferred to the protocol treasury address (0 if protocol fee is disabled).

> **Important**: `handle_default()` does **not** emit `repayment_received`. Defaults emit `invoice_defaulted` instead (with the defaulted funded principal payload).

**Emitted by:** `settle_repayment()` called by `receive_repayment()` and `receive_repayment_with_refund()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:78`

#### Examples

##### Example 1: Standard Repayment (`receive_repayment`)

When an invoice is repaid in full via `receive_repayment()`, the full discount surplus (`amount - funded_amount`) is split between LP yield and any configured protocol fee cut.

- Invoice Face Value: 1,000 USDC (`10_000_000_000` stroops)
- Funded Amount: 980 USDC (`9_800_000_000` stroops, discount = 200 bps)
- Repaid Amount: 1,000 USDC (`10_000_000_000` stroops)
- Gross Yield: `10_000_000_000 - 9_800_000_000 = 200_000_000` stroops (20 USDC)

**Case A: No protocol fee (`fee_bps = 0`)**
- `lp_yield`: `200_000_000` stroops
- `protocol_cut`: `0` stroops
- **Topics**: `[Symbol("repayment_received"), BytesN<32>(invoice_id)]`
- **Data**: `(10_000_000_000u128, 200_000_000u128, 0u128)`

**Case B: With protocol fee enabled (`fee_bps = 1000` / 10%)**
- `protocol_cut`: `200_000_000 * 1000 / 10000 = 20_000_000` stroops (transferred to treasury)
- `lp_yield`: `200_000_000 - 20_000_000 = 180_000_000` stroops
- **Topics**: `[Symbol("repayment_received"), BytesN<32>(invoice_id)]`
- **Data**: `(10_000_000_000u128, 180_000_000u128, 20_000_000u128)`

##### Example 2: Early Repayment with Buyer Refund (`receive_repayment_with_refund`)

When an invoice is repaid early via `receive_repayment_with_refund()`, part of the discount surplus is refunded to the buyer. Only the remaining surplus after refund (`amount - funded_amount - refund`) constitutes yield.

- Invoice Face Value: 1,000 USDC (`10_000_000_000` stroops)
- Funded Amount: 980 USDC (`9_800_000_000` stroops)
- Refund to Buyer: 12 USDC (`120_000_000` stroops transferred directly to buyer)
- Net Yield Surplus: `10_000_000_000 - 9_800_000_000 - 120_000_000 = 80_000_000` stroops (8 USDC)

**Case A: No protocol fee (`fee_bps = 0`)**
- `lp_yield`: `80_000_000` stroops
- `protocol_cut`: `0` stroops
- **Topics**: `[Symbol("repayment_received"), BytesN<32>(invoice_id)]`
- **Data**: `(10_000_000_000u128, 80_000_000u128, 0u128)`

**Case B: With protocol fee enabled (`fee_bps = 1000` / 10%)**
- `protocol_cut`: `80_000_000 * 1000 / 10000 = 8_000_000` stroops (transferred to treasury)
- `lp_yield`: `80_000_000 - 8_000_000 = 72_000_000` stroops
- **Topics**: `[Symbol("repayment_received"), BytesN<32>(invoice_id)]`
- **Data**: `(10_000_000_000u128, 72_000_000u128, 8_000_000u128)`

---

### `invoice_defaulted`

Emitted when the pool handles a defaulted invoice via `handle_default()`. Topics index the event name and the defaulted `invoice_id`. The data payload is the defaulted funded principal (`funded_amount`).

> **Note on Payload Semantics**: The data payload represents the invoice's defaulted funded principal in USDC stroops, and is **not necessarily an LP capital loss**. The pool executes one of two paths depending on escrow release status:
> - **Unrecoverable Loss Path (`escrow_released == false`, standard post-#819 flow):** When the funded principal was already paid out to the invoice issuer at funding time, escrow cannot recover funds on default. The pool absorbs the loss by reducing `TotalDeposits` by `funded_amount` (lowering the LP share price rather than burning shares), while `TotalFunded` is decremented.
> - **Recovered Principal Path (`escrow_released == true`, pre-#819 locks or unreleased escrow):** When escrow still held and returned the principal to the pool, `TotalDeposits` is deliberately left unchanged to preserve LP capital and share value (issue #439), while `TotalFunded` is decremented.

In both paths, `invoice_defaulted` emits `funded_amount` as its data payload and increments `TotalLossRealised`.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"invoice_defaulted"` |
| `topic[1]` | `BytesN<32>` | Invoice ID |
| **Data** | `u128` | Defaulted funded principal in USDC stroops (`funded_amount`; not necessarily the LP capital loss if escrow returned the principal) |

**Emitted by:** `handle_default()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:105`

#### Examples

##### Example 3A: Default with Unrecoverable Loss (`escrow_released == false`)

When an invoice defaults and the principal was already released to the issuer at funding time (the standard post-#819 flow), escrow returns `false` on `handle_default`. The pool verifies via `escrow.was_released_to_issuer` that funds reached the issuer, books the unrecoverable loss by reducing `TotalDeposits`, and emits `invoice_defaulted`.

- Invoice Face Value: 1,000 USDC (`10_000_000_000` stroops)
- Funded Amount: 980 USDC (`9_800_000_000` stroops)
- Escrow Status: Principal released to issuer (`escrow_released == false`, `was_released_to_issuer == true`)
- `TotalDeposits` Impact: Reduced by `9_800_000_000` stroops (realized LP capital loss, lowering share price)
- `TotalFunded` Impact: Decreased by `9_800_000_000` stroops
- `TotalLossRealised` Impact: Increased by `9_800_000_000` stroops
- **Topics**: `[Symbol("invoice_defaulted"), BytesN<32>(invoice_id)]`
- **Data**: `9_800_000_000u128` (defaulted funded principal)

##### Example 3B: Default with Recovered Principal (`escrow_released == true`)

When escrow still held the principal at default time (e.g., pre-#819 locks or unreleased escrow), `escrow.handle_default` transfers the principal back to the pool and returns `true`. The pool decrements `TotalFunded`, leaves `TotalDeposits` unchanged so LP capital and share price are preserved (issue #439), and emits `invoice_defaulted`.

- Invoice Face Value: 1,000 USDC (`10_000_000_000` stroops)
- Funded Amount: 980 USDC (`9_800_000_000` stroops)
- Escrow Status: Principal returned by escrow to pool (`escrow_released == true`)
- `TotalDeposits` Impact: **Unchanged** (LP capital and share value preserved)
- `TotalFunded` Impact: Decreased by `9_800_000_000` stroops
- `TotalLossRealised` Impact: Increased by `9_800_000_000` stroops
- **Topics**: `[Symbol("invoice_defaulted"), BytesN<32>(invoice_id)]`
- **Data**: `9_800_000_000u128` (defaulted funded principal)

> Note: Unlike repayments, no yield or protocol fee cut is involved in either path, and `repayment_received` is never emitted by `handle_default()`.

---

### `max_utilization_updated`

Emitted after the admin successfully changes the pool's maximum utilization
cap. Both values are expressed in basis points (`10_000` = 100%).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"max_utilization_updated"` |
| **Data** | `(u32, u32)` | `(old_cap_bps, new_cap_bps)` in basis points |

**Emitted by:** `set_max_utilization()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:103`

---

### `protocol_fee_updated`

Emitted when the admin changes the protocol fee and/or the treasury address. The
old value is included so an indexer can report what changed rather than only
what it changed to. A rejected call (fee above `MAX_PROTOCOL_FEE_BPS`) emits
nothing.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"protocol_fee_updated"` |
| **Data** | `(u32, u32, Address)` | `(old_fee_bps, new_fee_bps, treasury)` |

**Emitted by:** `set_protocol_fee()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:133`

---

### `min_discount_updated`

Emitted when the admin changes the pool's minimum discount floor (yield floor)
via `set_min_discount` (issue #829). Mirrors `protocol_fee_updated`: the old
value is included so an indexer can report what changed rather than only what
it changed to. A rejected call (non-admin, or floor above
`MAX_MIN_DISCOUNT_BPS` = 5000 bps) emits nothing.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"min_discount_updated"` |
| **Data** | `(u32, u32)` | `(old_min_bps, new_min_bps)` |

**Emitted by:** `set_min_discount()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:161`

---

### `allowance_approved`

Emitted by the SEP-41 `approve()` whenever an LP's spending grant changes.
`amount` is the new **total** grant (not a delta), and `amount == 0` is the
revoke signal.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"allowance_approved"` |
| `topic[1]` | `Address` | Share owner (`from`) |
| **Data** | `(Address, i128, u32)` | `(spender, amount, expiration_ledger)` |

**Emitted by:** `approve()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:144`

---

### `ownership_transferred`

Emitted when pool ownership is transferred to a new admin.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"ownership_transferred"` |
| `topic[1]` | `Address` | Old admin |
| **Data** | `Address` | New admin |

**Emitted by:** `transfer_ownership()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:111`

---

### `paused`

Emitted when the admin engages the emergency circuit breaker.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"paused"` |
| `topic[1]` | `Address` | Admin who authorized the pause |
| **Data** | `()` | None |

**Emitted by:** `pause()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:121`

---

### `unpaused`

Emitted when the admin disengages the emergency circuit breaker.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"unpaused"` |
| `topic[1]` | `Address` | Admin who authorized the unpause |
| **Data** | `()` | None |

**Emitted by:** `unpause()` in `contracts/pool/src/lib.rs`  
**Source:** `contracts/pool/src/events.rs:127`

---

## Escrow Contract

**Contract:** `escrow_contract`  
**Source:** `contracts/escrow/src/events.rs`

### `contract_initialized`

Emitted when the escrow contract is initialized.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"contract_initialized"` |
| `topic[1]` | `Address` | Admin address |
| `topic[2]` | `Address` | Pool contract address |
| `topic[3]` | `Address` | USDC asset address |
| **Data** | `()` | None |

**Emitted by:** `initialize()` in `contracts/escrow/src/lib.rs:49`

---

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

#### Partial Release and Residual Lock

When `release_to_pool` is called with an amount less than the total locked amount, the following occurs:

1. The `released_to_pool` event is emitted with the `repayment_amount` (the amount being repaid in this call).
2. The escrow `Locked` record's `amount` is reduced by `repayment_amount` (preserving the original `locked_at` timestamp).
3. The residual locked amount remains in escrow, and `get_locked()` will return the remaining balance.
4. A subsequent `release_to_pool` call can repay the remaining amount, at which point the `Locked` record is fully removed.

This append-only history means the `EscrowEvent` log records every `ReleasedToPool` action, allowing indexers to track the cumulative repayment progress and the residual lock state over time.

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

### `paused`

Emitted when the escrow's admin-gated emergency circuit breaker is engaged
(issue #716). `admin` is indexed so indexers can track who flipped the breaker.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"paused"` |
| `topic[1]` | `Address` | Admin that authorized the pause |
| **Data** | `()` | None |

**Emitted by:** `pause()` in `contracts/escrow/src/lib.rs`

---

### `unpaused`

Emitted when the escrow's admin-gated emergency circuit breaker is disengaged.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"unpaused"` |
| `topic[1]` | `Address` | Admin that authorized the unpause |
| **Data** | `()` | None |

**Emitted by:** `unpause()` in `contracts/escrow/src/lib.rs`

---

## Registry Contract

**Contract:** `registry_contract`  
**Source:** `contracts/registry/src/events.rs`

### `contract_initialized`

Emitted when the registry contract is initialized.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"contract_initialized"` |
| `topic[1]` | `Address` | Admin address |
| **Data** | `()` | None |

**Emitted by:** `initialize()` in `contracts/registry/src/lib.rs:55`

---

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

### `profile_updated`

Emitted when an account profile's details or metadata are updated.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"profile_updated"` |
| `topic[1]` | `Address` | Profile address |
| **Data** | `()` | None |

**Emitted by:** `update_profile()` in `contracts/registry/src/lib.rs:323` and `update_metadata()` at line 363

---

### `address_revoked`

Emitted when an address is revoked (issuer or buyer).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"address_revoked"` |
| `topic[1]` | `Address` | Revoked address |
| **Data** | `()` | None |

**Emitted by:** `revoke()` in `contracts/registry/src/lib.rs:498`

---

### `address_reinstated`

Emitted when a previously revoked address is reinstated.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"address_reinstated"` |
| `topic[1]` | `Address` | Reinstated address |
| **Data** | `()` | None |

**Emitted by:** `reinstate()` in `contracts/registry/src/lib.rs:546`

---

### `batch_registered`

Emitted when a batch registration completes.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"batch_registered"` |
| **Data** | `(u32, u32)` | `(registered_count, skipped_count)` |

**Emitted by:** `batch_register_issuers()` in `contracts/registry/src/lib.rs:155` and `batch_register_buyers()` at line 218

---

### `profile_verified`

Emitted when a profile's verification status is checked/updated.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"profile_verified"` |
| `topic[1]` | `Address` | Profile address |
| **Data** | `bool` | Verification status (true = verified) |

**Emitted by:** `verify_profile()` in `contracts/registry/src/lib.rs` — callable by the admin or the verifier role (#732).

---

### `verifier_set`

Emitted when the admin assigns (or replaces) the verifier role (#732).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"verifier_set"` |
| `topic[1]` | `Address` | Admin that made the change |
| **Data** | `Address` | New verifier |

**Emitted by:** `set_verifier()` in `contracts/registry/src/lib.rs`

---

### `verifier_removed`

Emitted when the admin clears the verifier role (#732).

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"verifier_removed"` |
| `topic[1]` | `Address` | Admin that made the change |
| **Data** | `()` | — |

**Emitted by:** `remove_verifier()` in `contracts/registry/src/lib.rs`

---

### `ownership_transferred`

Emitted when registry ownership is transferred.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"ownership_transferred"` |
| `topic[1]` | `Address` | Old admin |
| **Data** | `Address` | New admin |

**Emitted by:** `transfer_ownership()` in `contracts/registry/src/lib.rs:593`

---

### `admin_transferred`

Emitted when the registry admin role is transferred.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"admin_transferred"` |
| `topic[1]` | `Address` | Old admin |
| **Data** | `Address` | New admin |

**Emitted by:** `transfer_admin()` in `contracts/registry/src/lib.rs:631`

---

### `paused`

Emitted when the admin engages the emergency circuit breaker.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"paused"` |
| `topic[1]` | `Address` | Admin who authorized the pause |
| **Data** | `()` | None |

**Emitted by:** `pause()` in `contracts/registry/src/lib.rs`  
**Source:** `contracts/registry/src/events.rs`

---

### `unpaused`

Emitted when the admin disengages the emergency circuit breaker.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"unpaused"` |
| `topic[1]` | `Address` | Admin who authorized the unpause |
| **Data** | `()` | None |

**Emitted by:** `unpause()` in `contracts/registry/src/lib.rs`  
**Source:** `contracts/registry/src/events.rs`

---

## Pool Factory Contract

**Contract:** `pool_factory_contract`
**Source:** `contracts/pool_factory/src/events.rs`

### `contract_initialized`

Emitted when the pool factory is initialized.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"contract_initialized"` |
| `topic[1]` | `Address` | Admin address |
| **Data** | `()` | None |

**Emitted by:** `initialize()` in `contracts/pool_factory/src/lib.rs`

---

### `pool_instance_created`

Emitted when `register_asset()` deploys and initializes a new pool instance.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"pool_instance_created"` |
| `topic[1]` | `Address` | Registered asset address |
| **Data** | `Address` | Newly deployed pool address |

**Emitted by:** `register_asset()` in `contracts/pool_factory/src/lib.rs`

---

### `existing_pool_registered`

Emitted when `register_existing_pool()` registers a pre-existing pool for an asset.

| Field | Type | Description |
|-------|------|-------------|
| **Topics** | | |
| `topic[0]` | `Symbol` | `"existing_pool_registered"` |
| `topic[1]` | `Address` | Registered asset address |
| **Data** | `Address` | Existing pool address |

**Emitted by:** `register_existing_pool()` in `contracts/pool_factory/src/lib.rs`

---

## Cross-Reference: Duplicate Event Names

Note that some event names appear in multiple contracts. When indexing, filter by contract address:

| Event Name | Contracts |
|------------|-----------|
| `contract_initialized` | `invoice_contract`, `registry_contract`, `pool_factory_contract` |
| `invoice_funded` | `invoice_contract`, `pool_contract` |
| `invoice_defaulted` | `invoice_contract`, `pool_contract` |
| `ownership_transferred` | All four contracts |
| `paused` / `unpaused` | All four contracts |

---

## Indexing Tips for Go Indexer

1. **Filter by contract address** — each contract has a unique address on Stellar
2. **Use topics for filtering** — `topic[0]` is always the event name (Symbol)
3. **Data decoding** — payloads use Soroban SDK xdr encoding; decode based on the schemas above
4. **Invoice lifecycle** — track `invoice_created` → `invoice_listed` → `invoice_funded` → `invoice_shipped` → `delivery_confirmed` (×2) → `both_confirmed` → `invoice_repaid` / `invoice_defaulted`
5. **Pool events** — `lp_deposited` / `lp_withdrawn` track LP positions; `repayment_received` tracks yield accrual and protocol fee cuts; `invoice_defaulted` tracks defaulted funded principal
6. **Escrow events** — `funds_locked` → `released_to_issuer` OR `released_to_pool` / `default_resolved`
7. **Batch events** — `batch_invoices_created` and `batch_invoices_listed` are summary events emitted after the per-invoice events of the same call. `pool::batch_fund_invoice` has no summary event; reconcile it from the individual `invoice_funded` events instead, which is also what tells you which entries a capacity stop skipped.

---

## Related Documentation

- [README](../README.md) — contract overview and deployment addresses
- [THREAT_MODEL.md](./THREAT_MODEL.md) — trust assumptions and auth gates
- [STORAGE.md](./STORAGE.md) — on-chain data layout and TTL patterns
- [LIMITATIONS.md](./LIMITATIONS.md) — known gaps and testnet constraints
