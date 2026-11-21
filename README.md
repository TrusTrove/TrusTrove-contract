<p align="center">
  <img src="https://trustrove.vercel.app/og-image.png" alt="TrusTrove Contracts" width="600" />
</p>


<h1 align="center">TrusTrove — Smart Contracts</h1>


<p align="center">
  Soroban smart contracts powering the TrusTrove trade finance protocol on Stellar. 
  The protocol uses a pool_factory + per-asset pool instance model, allowing independent liquidity pools per asset.
</p> 






<p align="center">
  <a href="https://github.com/TrusTrove/TrusTrove-contract/actions/workflows/ci.yml">
    <img src="https://img.shields.io/github/actions/workflow/status/TrusTrove/TrusTrove-contract/ci.yml?branch=main&label=build" />
  </a>
  <a href="https://codecov.io/gh/TrusTrove/TrusTrove-contract">
    <img src="https://img.shields.io/codecov/c/github/TrusTrove/TrusTrove-contract?label=coverage" />
  </a>
  <img src="https://img.shields.io/badge/rust-1.85.0-orange" />
  <img src="https://img.shields.io/badge/soroban--sdk-23.5.3-blueviolet" />
  <img src="https://img.shields.io/badge/network-Stellar%20Testnet-00c9a7" />
  <img src="https://img.shields.io/github/license/TrusTrove/TrusTrove-contract" />
</p>

<p align="center">
  <a href="https://trustrove.vercel.app">Live App</a> ·
  <a href="https://github.com/TrusTrove/TrusTrove-app">App Repo</a> ·
  <a href="https://stellar.expert/explorer/testnet">Stellar Explorer</a>
</p>

---

## What is TrusTrove?

TrusTrove is a decentralized trade finance protocol on Stellar. SMEs tokenize unpaid invoices and receive immediate USDC funding from a shared liquidity pool. Liquidity providers deposit USDC and earn yield from discount fees when invoices repay. No banks, no brokers — five deployable Soroban contracts power the protocol.

---

## Maintainers

| | Name | Role | GitHub | Telegram |
|---|---|---|---|---|
| | **Fuhad (K1NGD4VID)** | Founder & Lead Developer | [@k1ngd4vid](https://github.com/k1ngd4vid) | [@k1ngd4vid](https://t.me/k1ngd4vid) |

Join the contributor community: **[t.me/trusttrove](https://t.me/trusttrove)**

### Maintainer Tooling

Seed-issue generator scripts live in [`scripts/maintainer/`](./scripts/maintainer/), which is the **only supported location** for this tooling:

- `create_issues.py` — generate issues from a template
- `create-contract-issues.sh` / `create-contract-issues.ps1` — shell/PowerShell helpers

Run any script from the repo root, e.g. `bash scripts/maintainer/create-contract-issues.sh`.

> Any `create_issues.*` files found at the repo root are stale duplicates left over from before this tooling was consolidated under `scripts/maintainer/`. Do not use them — they lack the rate-limit/dedup guards the `scripts/maintainer/` versions have.

---

## Contracts

### registry_contract

Tracks verified SME issuers and buyers.

```
initialize(admin)
register_issuer(address, metadata) → bool
register_buyer(address, metadata) → bool
batch_register_issuers(entries) → Vec<Address>   ← admin only, max 50, returns skipped
batch_register_buyers(entries) → Vec<Address>    ← admin only, max 50, returns skipped
is_verified(address) → bool
get_profile(address) → Profile
get_profile_count(role) → u32                    ← issuers/buyers registered, O(1)
list_profiles(role, start, limit) → Vec<Address> ← registration-ordered page, limit ≤ 50
revoke(address) → bool
```

**The registry is enumerable without replaying events.** Every registration
path — single and batch, issuer and buyer — appends the new address to a
per-role index (`ProfileIndex(role, n)`) and bumps a per-role counter
(`ProfileCount(role)`), both TTL-extended like a profile entry. So an indexer
or admin dashboard can call `get_profile_count(role)` to size its last page and
then page through `list_profiles(role, start, limit)` with `limit ≤ 50`
(`PageSizeExceeded` above the cap). Skipped/already-registered batch entries
are never indexed twice, pages are contiguous and non-overlapping, and revoked
profiles remain enumerated (revocation flips verification, it does not
deregister the profile).

**Revocation is prospective, not retroactive.** `is_verified()` is re-checked
at every point where new business gets committed — `invoice.create()`,
`invoice.list_for_financing()`, and `pool.fund_invoice()` — so a revoked
issuer or buyer can't originate, list, or get funded on a new invoice. It is
**not** re-checked at any later lifecycle step (`mark_shipped`,
`confirm_delivery`, `repay`, `repay_early`, `trigger_default`): once an
invoice is `Funded`, pool capital is already committed and the repayment
terms are already fixed, so a later `revoke()` does not unwind, freeze, or
force-default an in-flight invoice. This is a deliberate choice — unwinding
committed capital on revocation would be disruptive to LPs and gameable
(e.g. an issuer could grief the pool by getting itself revoked mid-term to
force a default). Admins who need to stop a specific in-flight invoice have
`invoice.trigger_default()` (past due date) as the existing mechanism; there
is no separate "freeze this invoice" primitive.

### invoice_contract

Manages the full invoice lifecycle. Enforces valid state transitions. [Emits events](./docs/EVENTS.md#invoice-contract) consumed by the Go indexer.

```
Created → Listed → Funded → Active → Confirmed → Repaid
                                    ↘ Defaulted
```

```
create(issuer, buyer, face_value, due_date, funding_asset) → invoice_id
batch_create(issuer, entries) → Vec<invoice_id>   ← atomic, max 50 entries
submit_attestation(invoice_id, payload, signature) → bool
list_for_financing(invoice_id, discount_bps) → bool
batch_list_for_financing(entries) → Vec<invoice_id>  ← tolerant, max 50 entries
mark_funded(invoice_id, funded_amount) → bool   ← pool_contract only
mark_shipped(invoice_id) → bool
confirm_delivery(invoice_id, confirmer) → bool  ← dual confirmation required
repay(invoice_id) → bool
repay_partial(invoice_id, amount) → bool
repay_early(invoice_id) → bool
trigger_default(invoice_id) → bool
get(invoice_id) → Invoice
get_attestation(invoice_id) → Option<Attestation>
get_by_status(status, page, page_size) → Vec<Invoice>
get_by_issuer(address, page, page_size) → Vec<Invoice>
get_by_buyer(address, page, page_size) → Vec<Invoice>
get_invoice_count_by_issuer(address) → u32
get_invoice_count_by_buyer(address) → u32
get_counts() → Map<String, u64>
get_remaining_balance(invoice_id) → u128
set_agent_registry_contract(agent_registry_contract) → bool
```

#### `list_for_financing(invoice_id, discount_bps) → bool`
- Marks a `Created` invoice as available for financing (`Listed` status).
- `discount_bps`: Annualized discount rate in basis points (valid range: `0..=5000`, where 5,000 = 50% max). Values above 5,000 panic with `DiscountTooHigh` (`#9`).
- Only the invoice `issuer` can call this.
- Requires invoice to be in `Created` status.
- Requires an active, non-expired risk attestation from an authorized Underwrite agent (`AttestationExpired` (#30) if its `valid_until` has passed).

#### `batch_create(issuer, entries) → Vec<invoice_id>`
- `entries`: up to `MAX_BATCH_SIZE` (50) tuples of `(buyer, face_value, due_date, funding_asset)`.
- **Atomic**: one invalid entry reverts the entire batch, including invoices already created by earlier entries. Callers fix the bad entry and resubmit the whole batch.
- `issuer` authorizes **once** for the whole batch, not once per entry.
- Panics with `BatchSizeExceeded` (`#28`) above 50 entries.

#### `batch_list_for_financing(entries) → Vec<invoice_id>`
- `entries`: up to `MAX_BATCH_SIZE` (50) tuples of `(invoice_id, discount_bps)`.
- **Tolerant**: entries that fail validation (already listed, no attestation, bad discount) are skipped, not reverted. Returns the IDs that failed, in entry order; an empty result means every entry listed.
- Each listed invoice's own `issuer` authorizes, but only **once per distinct issuer** per batch.

### escrow_contract

Mediates USDC between the pool, issuer, and buyer. `pool.fund_invoice` locks the funded amount here and immediately releases it to the issuer in the same call (issue #819); repayment settles through `release_to_pool`, which consumes either a live lock or the issuer-release marker. Only callable by `pool_contract`.

```
lock(invoice_id, amount) → bool
release_to_issuer(invoice_id, issuer) → bool
release_to_pool(invoice_id, repayment_amount) → bool
handle_default(invoice_id, caller) → bool   ← admin or pool_contract
pause() / unpause()                  ← admin only (issue #716)
get_locked(invoice_id) → u128
```

When the escrow is paused, `initialize`, `lock`, `release_to_issuer`,
`release_to_pool`, and `handle_default` all revert with `ContractPaused`.
Read-only views (`get_locked`) stay callable.

### pool_contract

USDC liquidity pool where LP shares are a SEP-41 transferable token. Share price grows as invoices repay.

```
deposit(lp, usdc_amount) → shares
withdraw(lp, shares) → usdc_amount
fund_invoice(invoice_id) → bool         ← re-verifies issuer & buyer against registry_contract
batch_fund_invoice(invoice_ids) → Vec<invoice_id>  ← permissionless, max 50 entries
receive_repayment(invoice_id, amount) → bool  ← invoice_contract only
receive_repayment_with_refund(invoice_id, amount, refund, buyer) → bool ← invoice_contract only
handle_default(invoice_id) → bool
set_protocol_fee(fee_bps, treasury) → bool   ← admin only
get_protocol_fee_bps() → u32
get_treasury() → Address
set_min_discount(bps) → bool               ← admin only, yield floor enforced in fund_invoice (#829)
get_min_discount_bps() → u32
get_stats() → PoolStats
get_lp_position(address) → LPPosition
transfer(from, to, amount) → ()             ← SEP-41 share transfer (generic token::Client compatible)
transfer_shares(from, to, amount) → ()      ← non-standard legacy alias of transfer
approve(from, spender, amount, expiration_ledger) → ()   ← SEP-41 grant
allowance(from, spender) → i128                          ← 0 when spent/expired
transfer_from(spender, from, to, amount) → ()            ← spender auth, spends grant
decimals() → u32                     ← per-instance, set at initialize
name() → String                      ← set at initialize()
symbol() → String                    ← set at initialize()
```

---

<!-- BEGIN GENERATED CONTRACT API -->
### Canonical public contract API

Generated from current contractimpl declarations by python3 scripts/generate_contract_api.py --write.
These are client-facing signatures: the framework-provided env: Env parameter is omitted; all other argument names/types and return types are taken from the Rust declarations. Methods without an explicit return type are shown as returning ().

| Contract | Public function | Client-facing signature |
|---|---|---|
| Registry | `__constructor` | `__constructor(admin: Address) -> ()` |
| Registry | `upgrade` | `upgrade(new_wasm_hash: BytesN<32>) -> ()` |
| Registry | `register_issuer` | `register_issuer(address: Address, metadata: Map<String, String>) -> bool` |
| Registry | `batch_register_issuers` | `batch_register_issuers(entries: Vec<(Address, Map<String, String>)>) -> Vec<Address>` |
| Registry | `batch_register_buyers` | `batch_register_buyers(entries: Vec<(Address, Map<String, String>)>) -> Vec<Address>` |
| Registry | `register_buyer` | `register_buyer(address: Address, metadata: Map<String, String>) -> bool` |
| Registry | `update_profile` | `update_profile(address: Address, metadata: Map<String, String>) -> bool` |
| Registry | `update_metadata` | `update_metadata(address: Address, metadata: Map<String, String>) -> bool` |
| Registry | `get_profile` | `get_profile(address: Address) -> Profile` |
| Registry | `is_verified` | `is_verified(address: Address) -> bool` |
| Registry | `get_verification_status` | `get_verification_status(address: Address) -> VerificationStatus` |
| Registry | `get_role` | `get_role(address: Address) -> Option<Role>` |
| Registry | `get_profile_count` | `get_profile_count(role: Role) -> u32` |
| Registry | `list_profiles` | `list_profiles(role: Role, start: u32, limit: u32) -> Vec<Address>` |
| Registry | `revoke` | `revoke(address: Address) -> bool` |
| Registry | `reinstate` | `reinstate(address: Address) -> bool` |
| Registry | `verify_profile` | `verify_profile(caller: Address, address: Address, verify: bool) -> bool` |
| Registry | `transfer_ownership` | `transfer_ownership(new_admin: Address) -> ()` |
| Registry | `transfer_admin` | `transfer_admin(new_admin: Address) -> ()` |
| Registry | `set_verifier` | `set_verifier(verifier: Address) -> ()` |
| Registry | `remove_verifier` | `remove_verifier() -> ()` |
| Registry | `get_verifier` | `get_verifier() -> Option<Address>` |
| Registry | `pause` | `pause() -> ()` |
| Registry | `unpause` | `unpause() -> ()` |
| Registry | `get_admin` | `get_admin() -> Address` |
| Invoice | `__constructor` | `__constructor(admin: Address, registry_contract: Address) -> ()` |
| Invoice | `get_admin` | `get_admin() -> Option<Address>` |
| Invoice | `upgrade` | `upgrade(new_wasm_hash: BytesN<32>) -> ()` |
| Invoice | `get_registry_contract` | `get_registry_contract() -> Option<Address>` |
| Invoice | `set_pool_contract` | `set_pool_contract(pool_contract: Address) -> ()` |
| Invoice | `get_pool_contract` | `get_pool_contract() -> Option<Address>` |
| Invoice | `set_agent_registry_contract` | `set_agent_registry_contract(agent_registry_contract: Address) -> ()` |
| Invoice | `get_agent_registry_contract` | `get_agent_registry_contract() -> Option<Address>` |
| Invoice | `set_escrow_contract` | `set_escrow_contract(escrow_contract: Address) -> ()` |
| Invoice | `get_escrow_contract` | `get_escrow_contract() -> Option<Address>` |
| Invoice | `get_attestation` | `get_attestation(invoice_id: BytesN<32>) -> Option<Attestation>` |
| Invoice | `add_supported_asset` | `add_supported_asset(asset: Address) -> ()` |
| Invoice | `remove_supported_asset` | `remove_supported_asset(asset: Address) -> ()` |
| Invoice | `is_supported_asset` | `is_supported_asset(asset: Address) -> bool` |
| Invoice | `get_supported_asset_count` | `get_supported_asset_count() -> u32` |
| Invoice | `create` | `create(issuer: Address, buyer: Address, face_value: u128, due_date: u64, funding_asset: Address) -> BytesN<32>` |
| Invoice | `batch_create` | `batch_create(issuer: Address, entries: Vec<(Address, u128, u64, Address)>) -> Vec<BytesN<32>>` |
| Invoice | `list_for_financing` | `list_for_financing(invoice_id: BytesN<32>, discount_bps: u32) -> bool` |
| Invoice | `batch_list_for_financing` | `batch_list_for_financing(entries: Vec<(BytesN<32>, u32)>) -> Vec<BytesN<32>>` |
| Invoice | `cancel` | `cancel(invoice_id: BytesN<32>) -> bool` |
| Invoice | `submit_attestation` | `submit_attestation(invoice_id: BytesN<32>, payload: Bytes, signature: BytesN<65>) -> ()` |
| Invoice | `mark_funded` | `mark_funded(invoice_id: BytesN<32>, pool_address: Address, asset_address: Address, funded_amount: u128) -> bool` |
| Invoice | `mark_shipped` | `mark_shipped(invoice_id: BytesN<32>) -> bool` |
| Invoice | `confirm_delivery` | `confirm_delivery(invoice_id: BytesN<32>, confirmer: Address) -> bool` |
| Invoice | `repay_partial` | `repay_partial(invoice_id: BytesN<32>, amount: u128) -> bool` |
| Invoice | `repay` | `repay(invoice_id: BytesN<32>) -> bool` |
| Invoice | `repay_early` | `repay_early(invoice_id: BytesN<32>) -> bool` |
| Invoice | `trigger_default` | `trigger_default(invoice_id: BytesN<32>) -> bool` |
| Invoice | `set_default_grace_seconds` | `set_default_grace_seconds(seconds: u64) -> ()` |
| Invoice | `get_default_grace_seconds` | `get_default_grace_seconds() -> u64` |
| Invoice | `mark_defaulted` | `mark_defaulted(invoice_id: BytesN<32>) -> bool` |
| Invoice | `set_expiry_window` | `set_expiry_window(window: u64) -> ()` |
| Invoice | `get_expiry_window` | `get_expiry_window() -> u64` |
| Invoice | `expire_listing` | `expire_listing(invoice_id: BytesN<32>, caller: Address) -> bool` |
| Invoice | `get_status` | `get_status(invoice_id: BytesN<32>) -> u32` |
| Invoice | `has_status_membership` | `has_status_membership(status: InvoiceStatus, invoice_id: BytesN<32>) -> bool` |
| Invoice | `get_face_value` | `get_face_value(invoice_id: BytesN<32>) -> u128` |
| Invoice | `get_remaining_balance` | `get_remaining_balance(invoice_id: BytesN<32>) -> u128` |
| Invoice | `get_repaid_amount` | `get_repaid_amount(invoice_id: BytesN<32>) -> u128` |
| Invoice | `get_discount_bps` | `get_discount_bps(invoice_id: BytesN<32>) -> u32` |
| Invoice | `get_funding_terms` | `get_funding_terms(invoice_id: BytesN<32>) -> (u32, u128, u32)` |
| Invoice | `get_funding_context` | `get_funding_context(invoice_id: BytesN<32>) -> FundingContext` |
| Invoice | `get_funding_asset` | `get_funding_asset(invoice_id: BytesN<32>) -> Address` |
| Invoice | `get_funded_amount` | `get_funded_amount(invoice_id: BytesN<32>) -> u128` |
| Invoice | `get` | `get(invoice_id: BytesN<32>) -> Invoice` |
| Invoice | `get_by_status` | `get_by_status(status: InvoiceStatus, page: u32, page_size: u32) -> Vec<Invoice>` |
| Invoice | `get_by_issuer` | `get_by_issuer(address: Address, page: u32, page_size: u32) -> Vec<Invoice>` |
| Invoice | `get_by_buyer` | `get_by_buyer(address: Address, page: u32, page_size: u32) -> Vec<Invoice>` |
| Invoice | `get_invoice_count_by_issuer` | `get_invoice_count_by_issuer(address: Address) -> u32` |
| Invoice | `get_invoice_count_by_buyer` | `get_invoice_count_by_buyer(address: Address) -> u32` |
| Invoice | `get_counts` | `get_counts() -> Map<String, u64>` |
| Invoice | `get_issuer` | `get_issuer(invoice_id: BytesN<32>) -> Address` |
| Invoice | `get_buyer` | `get_buyer(invoice_id: BytesN<32>) -> Address` |
| Invoice | `get_due_date` | `get_due_date(invoice_id: BytesN<32>) -> u64` |
| Invoice | `transfer_ownership` | `transfer_ownership(new_admin: Address) -> ()` |
| Invoice | `pause` | `pause() -> ()` |
| Invoice | `unpause` | `unpause() -> ()` |
| Escrow | `__constructor` | `__constructor(admin: Address, pool_contract: Address, usdc_asset: Address) -> ()` |
| Escrow | `get_usdc_asset` | `get_usdc_asset() -> Address` |
| Escrow | `get_admin` | `get_admin() -> Address` |
| Escrow | `upgrade` | `upgrade(new_wasm_hash: BytesN<32>) -> ()` |
| Escrow | `get_pool_contract` | `get_pool_contract() -> Address` |
| Escrow | `get_invoice_contract` | `get_invoice_contract() -> Address` |
| Escrow | `set_invoice_contract` | `set_invoice_contract(invoice_contract: Address) -> ()` |
| Escrow | `lock` | `lock(invoice_id: BytesN<32>, amount: u128, issuer: Address) -> bool` |
| Escrow | `release_to_issuer` | `release_to_issuer(invoice_id: BytesN<32>, issuer: Address) -> bool` |
| Escrow | `release_to_pool` | `release_to_pool(invoice_id: BytesN<32>, repayment_amount: u128) -> bool` |
| Escrow | `was_released_to_issuer` | `was_released_to_issuer(invoice_id: BytesN<32>) -> bool` |
| Escrow | `handle_default` | `handle_default(invoice_id: BytesN<32>, caller: Address) -> bool` |
| Escrow | `set_timeout_window` | `set_timeout_window(window: u64) -> ()` |
| Escrow | `get_timeout_window` | `get_timeout_window() -> u64` |
| Escrow | `release_after_timeout` | `release_after_timeout(invoice_id: BytesN<32>) -> bool` |
| Escrow | `pause` | `pause() -> ()` |
| Escrow | `unpause` | `unpause() -> ()` |
| Escrow | `get_locked` | `get_locked(invoice_id: BytesN<32>) -> u128` |
| Escrow | `get_locked_at` | `get_locked_at(invoice_id: BytesN<32>) -> u64` |
| Escrow | `get_history` | `get_history(invoice_id: BytesN<32>) -> Vec<EscrowEvent>` |
| Pool | `__constructor` | `__constructor(admin: Address, invoice_contract: Address, escrow_contract: Address, funding_asset: Address, registry_contract: Address, treasury: Address, min_initial_deposit: u128, share_name: String, share_symbol: String, share_decimals: u32) -> ()` |
| Pool | `get_funding_asset` | `get_funding_asset() -> Address` |
| Pool | `get_usdc_asset` | `get_usdc_asset() -> Address` |
| Pool | `decimals` | `decimals() -> u32` |
| Pool | `name` | `name() -> String` |
| Pool | `symbol` | `symbol() -> String` |
| Pool | `get_admin` | `get_admin() -> Address` |
| Pool | `upgrade` | `upgrade(new_wasm_hash: BytesN<32>) -> ()` |
| Pool | `transfer_ownership` | `transfer_ownership(new_admin: Address) -> ()` |
| Pool | `pause` | `pause() -> ()` |
| Pool | `unpause` | `unpause() -> ()` |
| Pool | `get_fee_bps` | `get_fee_bps() -> u32` |
| Pool | `get_treasury_address` | `get_treasury_address() -> Address` |
| Pool | `get_invoice_contract` | `get_invoice_contract() -> Address` |
| Pool | `get_escrow_contract` | `get_escrow_contract() -> Address` |
| Pool | `deposit` | `deposit(lp: Address, usdc_amount: u128) -> u128` |
| Pool | `preview_deposit` | `preview_deposit(usdc_amount: u128) -> u128` |
| Pool | `preview_withdraw` | `preview_withdraw(shares: u128) -> u128` |
| Pool | `withdraw` | `withdraw(lp: Address, shares: u128) -> u128` |
| Pool | `transfer` | `transfer(from: Address, to: Address, amount: i128) -> ()` |
| Pool | `transfer_shares` | `transfer_shares(from: Address, to: Address, amount: i128) -> ()` |
| Pool | `approve` | `approve(from: Address, spender: Address, amount: i128, expiration_ledger: u32) -> ()` |
| Pool | `allowance` | `allowance(from: Address, spender: Address) -> i128` |
| Pool | `transfer_from` | `transfer_from(spender: Address, from: Address, to: Address, amount: i128) -> ()` |
| Pool | `fund_invoice` | `fund_invoice(invoice_id: BytesN<32>) -> bool` |
| Pool | `batch_fund_invoice` | `batch_fund_invoice(invoice_ids: Vec<BytesN<32>>) -> Vec<BytesN<32>>` |
| Pool | `receive_repayment` | `receive_repayment(invoice_id: BytesN<32>, amount: u128) -> bool` |
| Pool | `receive_repayment_with_refund` | `receive_repayment_with_refund(invoice_id: BytesN<32>, amount: u128, refund: u128, buyer: Address) -> bool` |
| Pool | `backfill_funded_invoice_buyer` | `backfill_funded_invoice_buyer(invoice_id: BytesN<32>) -> bool` |
| Pool | `handle_default` | `handle_default(invoice_id: BytesN<32>) -> bool` |
| Pool | `balance` | `balance(id: Address) -> i128` |
| Pool | `total_supply` | `total_supply() -> i128` |
| Pool | `get_stats` | `get_stats() -> PoolStats` |
| Pool | `get_lp_position` | `get_lp_position(lp: Address) -> LPPosition` |
| Pool | `get_utilization_rate` | `get_utilization_rate() -> u32` |
| Pool | `get_suggested_discount_bps` | `get_suggested_discount_bps() -> u32` |
| Pool | `get_funded_amount` | `get_funded_amount(invoice_id: BytesN<32>) -> Option<u128>` |
| Pool | `set_max_utilization` | `set_max_utilization(admin: Address, new_cap_bps: u32) -> bool` |
| Pool | `set_protocol_fee` | `set_protocol_fee(fee_bps: u32, treasury: Address) -> bool` |
| Pool | `get_protocol_fee_bps` | `get_protocol_fee_bps() -> u32` |
| Pool | `get_treasury` | `get_treasury() -> Address` |
| Pool | `set_min_discount` | `set_min_discount(bps: u32) -> bool` |
| Pool | `get_min_discount_bps` | `get_min_discount_bps() -> u32` |
| Pool Factory | `__constructor` | `__constructor(admin: Address) -> ()` |
| Pool Factory | `register_asset` | `register_asset(asset: Address, pool_wasm_hash: BytesN<32>, invoice_contract: Address, escrow_contract: Address) -> Address` |
| Pool Factory | `register_existing_pool` | `register_existing_pool(asset: Address, pool_address: Address) -> ()` |
| Pool Factory | `get_pool_for_asset` | `get_pool_for_asset(asset: Address) -> Option<Address>` |
| Pool Factory | `get_aggregate_stats` | `get_aggregate_stats() -> Vec<(Address, PoolStats)>` |
| Pool Factory | `list_assets` | `list_assets() -> Vec<Address>` |

**Total: 151 public entry points across five deployable contracts.**

The shared TTL crate (contracts/ttl/src/lib.rs) exports only the THRESHOLD and EXTEND_TO constants. It is a workspace utility, not a callable contract API, so it has no entry-point group in this table.
<!-- END GENERATED CONTRACT API -->

---

## Architecture & Fund Flow

The protocol employs a `pool_factory` + per-asset pool instance model. The `pool_factory` handles deploying and tracking an independent `pool_contract` instance for each supported funding asset (e.g., USDC, XLM). This design isolates risk, ensuring that a shortfall in one asset pool cannot cross-contaminate another. 

**Note**: This architecture explicitly supersedes closed issue #17's Option A/B question, resolved as Option B (parallel single-asset pools instead of a commingled multi-asset pool). The full rationale, storage layout, deploy flow and the remaining single-asset checklist are in [Pool Factory Design](./docs/POOL_FACTORY_DESIGN.md).

### Contract Interaction Map

```
                    ┌─────────────────┐
                    │  registry_contract │
                    │  (identity oracle) │
                    └────────┬────────┘
                             │ is_verified()
          ┌──────────────────▼──────────────────┐
          │           invoice_contract            │
          │  (lifecycle state machine & indexer)  │
          └──────┬────────────────────┬──────────┘
                 │ mark_funded()      │ receive_repayment()
                 │ trigger_default()  │ handle_default()
          ┌──────▼───────┐    ┌───────▼──────────┐
          │ pool_contract │    │  pool_contract   │◄──(deployed by)── ┌───────────────┐
          │  fund_invoice │    │  (repayment in)  │                  │ pool_factory  │
          └──────┬────────┘    └──────────────────┘                  └───────────────┘
                 │ lock()
          ┌──────▼────────────┐
          │  escrow_contract  │
          │  (Asset custody)  │
          └───────────────────┘
```

The `pool_contract` instances also call `registry_contract.is_verified()` directly
(not shown above) as part of `fund_invoice`, re-checking the issuer and
buyer before committing capital. See "Revocation is prospective, not
retroactive" above.

### Invoice Lifecycle & Fund Movement

Each step below documents what happens to USDC and which contracts are called.

#### Step 1 — Liquidity Provision (LP → Pool)
LPs deposit USDC into the pool and receive shares proportional to their contribution. Share price grows as invoices repay.

```
LP ──[USDC]──► Pool
Pool ──[shares]──► LP
```

#### Step 2 — Create & List (no funds move)
The issuer creates an invoice (recording `face_value`, `due_date`, `buyer`, `funding_asset`), then lists it with a `discount_bps` (between 0 and 5,000, representing 0% to 50% max yield) expressing the yield they will give up in exchange for immediate liquidity. If `discount_bps > 5000`, the transaction fails with `InvoiceError::DiscountTooHigh` (#9).


Before listing, an Underwrite agent must sign an `AttestationPayload` (v2 layout: `domain_separator`, `invoice_id`, `contract_address`, `network_id`, `risk_score`, `evidence_hash`, `agent_id`, `nonce`, `valid_until`) off-chain with a secp256k1 key. `contract_address` and `network_id` bind the signature to one deployed invoice contract on one network, so attestations cannot replay across testnet/mainnet or redeploys (issue #822); `valid_until` (seconds, ledger timestamp) bounds how long the assessment unlocks listing (issue #823). Anyone can relay this signature via `submit_attestation`, which recovers the signer and verifies it against the agent-registry contract (deployed separately from the `underwrite-contract` repo). The agent-registry address is configured via `set_agent_registry_contract` (admin-only). `list_for_financing` panics with `VerificationRequired` until a valid attestation exists for the invoice, and with `AttestationExpired` once the stored attestation's `valid_until` has passed. The exact interface the agent-registry must implement, and the payload/signature scheme agents must sign, are specified in [Agent-Registry Interface](./docs/AGENT_REGISTRY_INTERFACE.md).

```
No fund movement. Invoice status: Created → Listed.
```

#### Step 3 — Fund Invoice (Pool → Escrow → Issuer)
Anyone can call `pool.fund_invoice(invoice_id)`. The pool computes the funded amount, locks it in escrow, marks the invoice as funded, and releases the principal to the issuer — all inside this single call (issue #819, restoring #56).

```
funded_amount = face_value × (10000 − discount_bps) / 10000

Pool ──[funded_amount USDC]──► Escrow  (locked per invoice_id)
Escrow ──[funded_amount USDC]──► Issuer (released in the same call)
Invoice status: Listed → Funded
```

The pool retains `face_value − funded_amount` (the discount) as accrued yield, collectible when the buyer repays.

#### Step 4 — Release to Issuer (part of Step 3)
The payout is not a separate step anymore: `fund_invoice` calls `escrow.release_to_issuer(invoice_id, issuer)` immediately after `lock`, so the issuer holds the USDC in the same transaction and can ship goods.

```
Escrow ──[funded_amount USDC]──► Issuer
```

The earlier gap (issue #56) was fixed in 5f6dc7c, silently regressed by a later refactor, and re-wired by [#819](https://github.com/TrusTrove/TrusTrove-contract/issues/819). Because the escrow `Locked` record is consumed at funding, escrow keeps a durable `IssuerReleased(invoice_id)` marker instead: `release_to_pool` accepts that marker so repayment still settles (each marker or lock is consumed exactly once), and `escrow.handle_default` reports `false` for already-funded invoices — see Step 7 for how the pool books that.

#### Step 5 — Ship & Confirm (no funds move)
The issuer calls `mark_shipped`. Then **both** the issuer and the buyer must independently call `confirm_delivery`. Only when both confirmations are recorded does the invoice advance to `Confirmed`.

```
No fund movement. Invoice status: Funded → Active → Confirmed.
```

#### Step 6 — Repay (Buyer → Escrow → Pool)
The buyer calls `invoice.repay(invoice_id)` (or `repay_partial` / `repay_early`), which transfers USDC from the buyer into escrow; escrow releases it to the pool via `release_to_pool`, and the pool records the yield split.

```
Buyer ──[face_value USDC]──► Escrow ──[face_value USDC]──► Pool
  earned_by_pool = discount × elapsed / term   (capped at the full discount
                                                at/after due_date)
  refund_to_buyer = discount − earned_by_pool ──► Buyer
  Protocol fee cut: protocol_cut = earned_by_pool × fee_bps / 10000 ──► Treasury
  LP yield: lp_yield = earned_by_pool − protocol_cut
  TotalDeposits += lp_yield  (share price rises for all LPs)
  TotalYieldDistributed += lp_yield
Invoice status: → Repaid
```

`repay_early` is restricted to `Confirmed` invoices strictly before `due_date`; `repay` accepts `Funded`, `Active`, or `Confirmed` at any time and also refunds the unearned discount before maturity. Partial installments accumulate in escrow and settle at the pool only when the final one clears the balance. See [Repayment Timing and Refund Math](./docs/LIMITATIONS.md#repayment-timing-and-refund-math) for the allowed statuses, formulas, and worked examples.

**Protocol Fee:** Defaults to 0 bps at deployment, preserving 100% yield distribution to LPs. The contract admin can configure a fee up to 2000 bps (20%) and set the treasury destination address via `pool.set_protocol_fee(fee_bps, treasury)`.

**Yield Floor (min_discount_bps):** Defaults to 0 bps (disabled) at deployment, so funding behavior is unchanged until an admin opts in. `fund_invoice` is permissionless and, before this mechanism, accepted any listed invoice regardless of discount — an issuer/buyer pair could list at 1 bp and deploy LP capital for near-zero yield. The stored admin can set a floor between 0 and 5000 bps via `pool.set_min_discount(bps)` (bounded by the invoice contract's 5000-bps discount cap); `fund_invoice` then rejects any invoice whose `discount_bps` is below the floor with `PoolError::DiscountBelowMinimum`, and `pool.get_min_discount_bps()` reads the current value. Choosing the actual floor value is an admin/policy decision (#829).

Repayment does **not** rest on the funding lock: the buyer's USDC settles through escrow (`escrow.release_to_pool` consumes either a live lock or the Step 4 issuer-release marker) and lands at the pool. The escrow contract is otherwise involved in funding and issuer payout (Steps 3–4) and default handling (Step 7).

#### Step 7 — Default
If the invoice passes its `due_date` without reaching `Repaid`, any caller triggers `invoice.trigger_default`. The invoice contract calls `pool.handle_default`, which calls `escrow.handle_default`. For a normally funded invoice the principal was already paid to the issuer at funding (Step 3), so escrow has nothing to hand back; the pool verifies the `IssuerReleased` marker via `escrow.was_released_to_issuer` and books the unrecoverable principal as a real LP capital loss. If escrow *can* still return a live lock (out-of-band funding), the legacy path runs and deposits are preserved. A `false` from escrow with no marker means the release was skipped and reverts with `EscrowDefaultNotReleased` (#21).

```
invoice.trigger_default()
  └─► pool.handle_default()
        ├─► escrow.handle_default()      → false for funded invoices (already paid out)
        ├─► escrow.was_released_to_issuer → true (marker present, #819)
        └─► invoice.mark_defaulted()
Invoice status: → Defaulted
  TotalFunded -= funded_amount
  TotalLossRealised += funded_amount
  TotalDeposits -= funded_amount   (LP capital loss; shares are never burned)
```

### Summary Table

| Event | Source | Destination | Amount | Escrow involved? |
|---|---|---|---|---|
| LP deposit | LP wallet | Pool | `usdc_amount` | No |
| LP withdraw | Pool | LP wallet | `shares × price` | No |
| Fund invoice | Pool → Escrow → Issuer | Issuer | `face_value × (1 − discount)` | Yes — locks then releases in one call |
| Repay | Buyer wallet → Escrow → Pool | Pool | `face_value` (or the outstanding balance for partial flow) | Yes — `release_to_pool` settles |
| Default | Pool books LP loss *(principal already with issuer)* | Pool | `funded_amount` written off | Only if escrow still holds a live lock; otherwise marker-verified write-off (#819) |

### Security Invariants

- The escrow contract only accepts `lock()` calls from the registered `pool_contract`.
- `release_to_issuer` and `release_to_pool` are callable only by `pool_contract`.
- `handle_default` in escrow accepts the pool or the admin (emergency recovery path).
- `receive_repayment` in the pool is callable only by the registered `invoice_contract`.
- Every state transition in `invoice_contract` is guarded by an explicit status check; no skipping steps.

> **Detailed references:** [Threat Model](./docs/THREAT_MODEL.md) · [Storage Schema](./docs/STORAGE.md) · [Limitations](./docs/LIMITATIONS.md)

---

## Deployed Contracts (Stellar Testnet)

<!-- START_DEPLOYED_ADDRESSES -->
| Contract | Address |
|----------|---------|
| registry_contract | `CABGWVIZFF62FG67ZGFEP67NEEY4WYTMFURDMFTKKNRDAFPKPOJDTN4C` |
| invoice_contract | `CA4O3MR7LWHRSUDBNU6FY6UDFFYBN7TGBZXBDZB4OYYXFYXIFJ6RJF6B` |
| escrow_contract | `CAJWGUKDTTC3SKN4RAAY72J4DVIIYSCFHX6GIMNTT22ABMISJK4GBCEH` |
| pool_contract | `CAKEWH7SJCXGV2MH2WZYIX3QDPTSSBQFXYVYBOWAGLNBBZMPLE2US6CS` |
<!-- END_DEPLOYED_ADDRESSES -->

> **Note**: Testnet addresses are subject to rotation. See [DEPLOYMENT.md](./DEPLOYMENT.md#contract-address-lifecycle--rotation-policy) for our redeployment and lifecycle policy.

Verify on [Stellar Expert Testnet](https://stellar.expert/explorer/testnet)

---

## Asset Decimals and Amount Conversion

### Seven-Decimal USDC Assumption

All contracts treat USDC as having **7 decimal places**: `1 USDC = 10_000_000 stroops`.
This assumption is enforced at the contract level and is critical for correct
amount interpretation.

### Token Interface Amount Type

All amount fields across the four contract crates use `u128` and are
**denominated in stroops** (the base unit of the funding asset). The token
client (e.g., `soroban_sdk::token::Client`) handles the conversion between
human-readable units and stroops, but the contracts always work in stroops.

Conversion rules:
- **Human unit → stroops**: `amount_in_stroops = amount_in_human * 10_000_000`
- **stroops → human unit**: `amount_in_human = amount_in_stroops / 10_000_000`
- Rounding: integer division truncates (floor), so `1 stroop = 0.0000001 USDC`;
the smallest representable human amount is `1 USDC = 10_000_000 stroops`.

### Where Conversion Is Prohibited

Conversion between human units and stroops must **not** be performed inside
contract code. All contracts operate exclusively in stroops. Any conversion
must happen off-chain (in wallet UI, indexer, or integration code). The
contracts will reject amounts that would overflow `u128` or violate the
protocol's arithmetic invariants.

### Deployment and Test Examples

The following examples show the same USDC amount expressed in both human
units and stroops, for use in deployment scripts and test fixtures:

| Human Amount | Stroops (u128) | Context |
|-------------|----------------|---------|
| `1 USDC` | `10_000_000` | `DEFAULT_MIN_INITIAL_DEPOSIT` in pool tests |
| `100 USDC` | `1_000_000_000` | Typical test deposit amount |
| `1_000_000 USDC` | `100_000_000_000_000` | Large deposit / share price testing |
| `10_000_000 USDC` (1M) | `10_000_000_000_000_000` | Max test deposit |

**Rust test example** (from `contracts/pool/src/test.rs`):

```rust
// Deposit 10_000_000_000 stroops (= 1000 USDC)
let shares = te.pool.deposit(&te.lp, &10_000_000_000);
```

**Deployment example** (from `scripts/deploy.sh`):

```bash
# Fund the pool with 1_000_000 USDC (10_000_000_000_000 stroops)
as_contract(&usdc_id, || {
    env.storage().persistent().set(&lp_bal_key, &10_000_000_000_000_000i128);
});
```

### Summary
- USDC = 7 decimals → 1 USDC = 10_000_000 stroops
- All contract amounts are `u128` in stroops
- Convert human ↔ stroops off-chain only
- Integer division truncates; smallest human unit = 1 USDC


### Prerequisites

- Rust 1.85.0 (required — other versions either have WASM bugs or are blocked by Stellar CLI)
- [Stellar CLI](https://github.com/stellar/stellar-cli) v23 or newer (the contracts build against soroban-sdk 23 / protocol 23)
- [jq](https://jqlang.github.io/jq/download/) (latest) — required by `scripts/maintainer/update-readme-addresses.sh`, which runs automatically at the end of `deploy.sh`

### 1. Install Rust 1.85.0

The repo ships a `rust-toolchain.toml` at the root pinning channel `1.85.0` and target `wasm32v1-none`. `rustup` picks it up automatically when you run any `cargo`/`rustc` command in this directory — you do not have to set a default. If the toolchain isn't installed yet, run:

```bash
rustup toolchain install 1.85.0
rustup target add wasm32v1-none --toolchain 1.85.0
```

### 2. Clone and build

```bash
git clone https://github.com/TrusTrove/TrusTrove-contract.git
cd TrusTrove-contract
rustup run 1.85.0 stellar contract build
```

### 3. Run tests

```bash
cargo test --workspace
```

### 4. Deploy to testnet

See [DEPLOYMENT.md](./DEPLOYMENT.md) for the full deployment
guide including prerequisites, contract wiring order, and rollback.

```bash
# Create and fund a deployer account
bash scripts/setup-testnet.sh

# Fund via browser: https://friendbot.stellar.org/?addr=YOUR_ADDRESS

# Deploy all four contracts
bash scripts/deploy.sh
```

Or on Windows (PowerShell):

```powershell
./scripts/setup-testnet.ps1
./scripts/deploy.ps1
```

The deploy script prints all four contract IDs at the end. Paste them into `TrusTrove-app/.env.local`.

#### Stellar CLI Setup (Linux, macOS, Windows)

The deploy script requires the Stellar CLI. Choose one:

- **Linux/macOS:** Install globally per [Stellar docs](https://developers.stellar.org/docs/learn/developing-with-soroban/setup) — the script will find it on `PATH`.
- **Windows (native):** Use the PowerShell scripts (`scripts/setup-testnet.ps1` and `scripts/deploy.ps1`). Install the Stellar CLI to `Program Files (x86)\Stellar CLI\`, or set `STELLAR_BIN` environment variable. Run from PowerShell:
  ```powershell
  powershell ./scripts/setup-testnet.ps1
  powershell ./scripts/deploy.ps1
  ```
- **Windows (WSL):** Install Stellar CLI in your WSL environment, or install on Windows host and set `STELLAR_BIN` to the Windows path (e.g., `/mnt/c/Program Files (x86)/Stellar CLI/stellar.exe`).

---

## Known Centralization Risks & Roadmap

TrusTrove is in active development on Stellar testnet. Several centralization trade-offs were made deliberately to ship a working protocol quickly. They are documented here so contributors and users understand the current trust model and can help drive the path to a more decentralized design.

### Admin key controls critical operations

The deployer wallet that calls `initialize()` on each contract becomes its `admin`. That single key currently controls:

- Registering and revoking verified issuers/buyers (`registry_contract`)
- Emergency pausing of all four contracts (the escrow circuit breaker landed in issue #716)
- Triggering `handle_default` as a fallback recovery path (`escrow_contract`)

**Risk:** Loss or compromise of the admin key has a high blast radius. A single actor also introduces censorship risk for issuer onboarding.

**Mitigation:** Configure the admin as a Stellar account with signer thresholds
(for example, 3-of-5) before mainnet deployment. No contract change is needed;
see [DEPLOYMENT.md](DEPLOYMENT.md#multi-sig-admin) for setup instructions.

### `fund_invoice` was previously admin-gated

Prior to this change, `pool::fund_invoice` required `admin.require_auth()`, meaning capital allocation was entirely at the admin's discretion. This created censorship risk — the admin could favour certain issuers, block competitors, or halt funding entirely with no on-chain accountability.

**Current state (this release):** `fund_invoice` is now **permissionless**. Any caller can trigger funding for any invoice that passes the on-chain eligibility checks:
1. Invoice status must be `Listed` (status 1)
2. Invoice funding asset must match the pool's asset
3. Pool must have sufficient available liquidity

No off-chain approval or admin signature is required.

**Longer-term governance design (not yet implemented):**

The goal is LP-governed capital allocation:

- LPs stake their LP tokens to signal approval for specific invoices ("LP voting")
- An invoice becomes eligible once a quorum of LP-weighted votes approves it
- Admin retains only an emergency pause capability (circuit breaker), not funding control
- Governance parameters (quorum threshold, voting window) are upgradeable by LP vote

If you want to contribute to governance design, open an issue tagged `complexity:high` and link your proposal.

### `trigger_default` is permissionless and time-based (Resolved)

`invoice::trigger_default` is permissionless and requires no authorization (`admin.require_auth()` was removed). Anyone can trigger default processing once `now >= due_date`. This removes the single point of failure of an admin-gated default mechanism and ensures timely loss recognition and escrow fund recovery for liquidity pools without relying on admin intervention.

### Emergency pause mechanism (complete rollout)

All four contracts — `pool`, `invoice`, `registry`, and `escrow` — expose admin-gated `pause() / unpause()` entry points backed by the shared [`trusttrove-pause`](contracts/pause) crate (issues #713, #714, #715, #716). While paused, every state-changing call reverts with `ContractPaused` while read-only views remain live.

The escrow covers `initialize`, `lock`, `release_to_issuer`, `release_to_pool`, and `handle_default`, so a compromised escrow admin or a stuck release path can be frozen without touching funds already locked — `get_locked` stays readable throughout.

**Roadmap:** Back the admin key with a multi-sig.

---

## Contributing

We welcome contributions from Rust and Soroban developers. Read [CONTRIBUTING.md](./CONTRIBUTING.md) before opening a PR.

### Find an issue

Issues are labeled by contract and complexity:
- `complexity:low` — isolated function or test, good entry point
- `complexity:medium` — touches contract logic and storage
- `complexity:high` — cross-contract interactions or new mechanics

### Architecture Docs

Detailed references for contributors and integrators:

- [Threat Model](./docs/THREAT_MODEL.md) — trust assumptions, auth gates, attack vectors
- [Storage Schema](./docs/STORAGE.md) — on-chain data layout, TTL patterns, gas estimates
- [Limitations](./docs/LIMITATIONS.md) — testnet constraints, known gaps, unhandled edge cases
- [Event Catalog](./docs/EVENTS.md) — every emitted event, topics, data schema, and emitting contract
- [Pool Factory Design](./docs/POOL_FACTORY_DESIGN.md) — per-asset pool factory (Option B), storage layout, deploy flow, single-asset checklist
- [Agent-Registry Interface](./docs/AGENT_REGISTRY_INTERFACE.md) — `get_agent` / `Agent` spec and attestation signature scheme consumed by `submit_attestation`

### Key conventions

- All amounts use `u128` in stroops (1 USDC = 10,000,000)
- All timestamps use `u64` Unix seconds
- Every `persistent().set()` must be followed by `extend_ttl()`
- Use `panic_with_error!` with typed errors — no bare `panic!` or `unwrap()` in production paths

### Commit format

```
feat(registry): add batch issuer registration function
fix(pool): guard against division by zero when total_shares is 0
test(invoice): add full lifecycle integration test
```

If you have questions, reach us on Telegram: **[t.me/trusttrove](https://t.me/trusttrove)**

### Error Codes Reference

#### Invoice Contract (`InvoiceError`)

| Code | Variant | Description |
|:---:|---|---|
| 1 | `AlreadyInitialized` | Contract has already been initialized |
| 2 | `NotFound` | Invoice record or configuration key was not found |
| 3 | `NotAuthorized` | Caller lacks required authorization |
| 4 | `IssuerNotVerified` | Issuer is not verified in the registry contract |
| 5 | `BuyerNotVerified` | Buyer is not verified in the registry contract |
| 6 | `InvalidFaceValue` | Face value below `MIN_FACE_VALUE` (dust, zero included) |
| 7 | `InvalidDueDate` | Due date is in the past or exceeds maximum invoice lifetime |
| 8 | `InvalidStatusTransition` | Status does not allow the requested operation |
| 9 | `DiscountTooHigh` | `discount_bps` exceeds maximum permitted 5,000 (50%) |
| 10 | `AlreadyConfirmed` | Caller has already confirmed delivery |
| 11 | `DueDateNotPassed` | Invoice due date has not passed yet |
| 12 | `InvalidDiscount` | Discount computation resulted in an invalid state |
| 13 | `UnsupportedAsset` | Token asset is not registered as supported |
| 14 | `ListingNotExpired` | Invoice listing has not reached expiry |
| 15 | `MathOverflow` | Arithmetic operation overflowed or underflowed |
| 16 | `InvalidAmount` | Face value, repayment, or fee amount is invalid or zero |
| 17 | `CounterOverflow` | Internal invoice ID counter exceeded limit |
| 18 | `InvalidExpiryWindow` | Expiry window parameter is invalid |
| 19 | `InvalidParticipants` | Issuer and buyer cannot be the same address |
| 20 | `NotInitialized` | Contract has not been initialized |
| 21 | `UntrustedSigner` | Attestation signer does not match a verified agent |
| 22 | `AlreadyAttested` | Invoice already has an attestation recorded |
| 23 | `VerificationRequired` | Listing requires a valid underwriting attestation |
| 24 | `CrossContractCallFailed` | Inter-contract call to Escrow or Pool failed |
| 25 | `RepaymentExceedsBalance` | Repayment amount exceeds remaining invoice balance |
| 26 | `InvalidConfiguration` | Configuration value is invalid |
| 27 | `InvalidPageSize` | `page_size` exceeds `MAX_PAGE_SIZE` on a paginated `get_by_*` query |
| 28 | `BatchSizeExceeded` | Batch entry point was given more than `MAX_BATCH_SIZE` (50) entries |
| 29 | `PayloadBindingMismatch` | Attestation payload bound to a different contract address or network (issue #822) |
| 30 | `AttestationExpired` | Attestation payload/record past its `valid_until` at submission or listing (issue #823) |

#### Pool Contract (`PoolError`)

| Code | Variant | Description |
|:---:|---|---|
| 5 | `InsufficientLiquidity` | Pool does not have enough available liquidity to fund the invoice |
| 8 | `InvoiceNotListed` | Invoice is not in `Listed` status |
| 11 | `AssetMismatch` | Invoice funding asset does not match the pool's asset |
| 12 | `UtilizationCapExceeded` | Funding would breach the pool's utilization cap |
| 26 | `BatchSizeExceeded` | `batch_fund_invoice` was given more than `MAX_BATCH_SIZE` (50) entries |

See [`contracts/pool/src/errors.rs`](contracts/pool/src/errors.rs) for the full list.

---

## License


MIT — see [CHANGELOG.md](./CHANGELOG.md) for version history.

---

## Contributors

[![Contributors](https://contrib.rocks/image?repo=TrusTrove/TrusTrove-contract)](https://github.com/TrusTrove/TrusTrove-contract/graphs/contributors)
