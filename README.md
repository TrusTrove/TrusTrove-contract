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
  <img src="https://img.shields.io/badge/soroban--sdk-21.7.6-blueviolet" />
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

TrusTrove is a decentralized trade finance protocol on Stellar. SMEs tokenize unpaid invoices and receive immediate USDC funding from a shared liquidity pool. Liquidity providers deposit USDC and earn yield from discount fees when invoices repay. No banks, no brokers — four Soroban smart contracts handle everything.

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
is_verified(address) → bool
get_profile(address) → Profile
revoke(address) → bool
```

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
submit_attestation(invoice_id, payload, signature) → bool
list_for_financing(invoice_id, discount_bps) → bool
mark_funded(invoice_id, funded_amount) → bool   ← pool_contract only
mark_shipped(invoice_id) → bool
confirm_delivery(invoice_id, confirmer) → bool  ← dual confirmation required
repay(invoice_id) → bool
repay_partial(invoice_id, amount) → bool
repay_early(invoice_id) → bool
trigger_default(invoice_id) → bool
get(invoice_id) → Invoice
get_attestation(invoice_id) → Option<Attestation>
get_by_status(status) → Vec<Invoice>
get_by_issuer(address) → Vec<Invoice>
get_counts() → Map<String, u64>
get_remaining_balance(invoice_id) → u128
set_agent_registry_contract(agent_registry_contract) → bool
```

#### `list_for_financing(invoice_id, discount_bps) → bool`
- Marks a `Created` invoice as available for financing (`Listed` status).
- `discount_bps`: Annualized discount rate in basis points (valid range: `0..=5000`, where 5,000 = 50% max). Values above 5,000 panic with `DiscountTooHigh` (`#9`).
- Only the invoice `issuer` can call this.
- Requires invoice to be in `Created` status.
- Requires an active risk attestation from an authorized Underwrite agent.


### escrow_contract

Holds USDC between pool funding and issuer payout. Only callable by `pool_contract`.

```
lock(invoice_id, amount) → bool
release_to_issuer(invoice_id, issuer) → bool
release_to_pool(invoice_id, repayment_amount) → bool
handle_default(invoice_id, caller) → bool   ← admin or pool_contract
get_locked(invoice_id) → u128
```

### pool_contract

USDC liquidity pool with share-based LP accounting. Share price grows as invoices repay.

```
deposit(lp, usdc_amount) → shares
withdraw(lp, shares) → usdc_amount
fund_invoice(invoice_id) → bool         ← re-verifies issuer & buyer against registry_contract
receive_repayment(invoice_id, amount) → bool  ← invoice_contract only
receive_repayment_with_refund(invoice_id, amount, refund, buyer) → bool ← invoice_contract only
handle_default(invoice_id) → bool
set_protocol_fee(fee_bps, treasury) → bool   ← admin only
get_protocol_fee_bps() → u32
get_treasury() → Address
get_stats() → PoolStats
get_lp_position(address) → LPPosition
transfer(from, to, amount) → ()             ← SEP-41 share transfer
approve(from, spender, amount, expiration_ledger) → ()   ← SEP-41 grant
allowance(from, spender) → i128                          ← 0 when spent/expired
transfer_from(spender, from, to, amount) → ()            ← spender auth, spends grant
decimals() → u32                     ← per-instance, set at initialize
name() → String                      ← set at initialize()
symbol() → String                    ← set at initialize()
```

---

## Architecture & Fund Flow

The protocol employs a `pool_factory` + per-asset pool instance model. The `pool_factory` handles deploying and tracking an independent `pool_contract` instance for each supported funding asset (e.g., USDC, XLM). This design isolates risk, ensuring that a shortfall in one asset pool cannot cross-contaminate another. 

**Note**: This architecture explicitly supersedes closed issue #17's Option A/B question, resolved as Option B (parallel single-asset pools instead of a commingled multi-asset pool).

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


Before listing, an Underwrite agent must sign an `AttestationPayload` (containing `domain_separator`, `invoice_id`, `risk_score`, `evidence_hash`, `agent_id`, `nonce`) off-chain with a secp256k1 key. Anyone can relay this signature via `submit_attestation`, which recovers the signer and verifies it against the agent-registry contract (deployed separately from the `underwrite-contract` repo). The agent-registry address is configured via `set_agent_registry_contract` (admin-only). `list_for_financing` panics with `VerificationRequired` until a valid attestation exists for the invoice.

```
No fund movement. Invoice status: Created → Listed.
```

#### Step 3 — Fund Invoice (Pool → Escrow)
Anyone can call `pool.fund_invoice(invoice_id)`. The pool computes the funded amount, locks it in escrow, and marks the invoice as funded.

```
funded_amount = face_value × (10000 − discount_bps) / 10000

Pool ──[funded_amount USDC]──► Escrow  (locked per invoice_id)
Invoice status: Listed → Funded
```

The pool retains `face_value − funded_amount` (the discount) as accrued yield, collectible when the buyer repays.

#### Step 4 — Release to Issuer (Escrow → Issuer) ⚠️ Known Gap
The pool contract is expected to call `escrow.release_to_issuer(invoice_id, issuer)` so that the locked USDC reaches the issuer who can then ship goods.

```
Escrow ──[funded_amount USDC]──► Issuer
```

**⚠️ This call is not yet wired into `fund_invoice` (see [Issue #56](https://github.com/TrusTrove/TrusTrove-contract/issues/56)).** In the current deployment, issuers do not automatically receive USDC after an invoice is funded. This is the highest-priority gap before mainnet.

#### Step 5 — Ship & Confirm (no funds move)
The issuer calls `mark_shipped`. Then **both** the issuer and the buyer must independently call `confirm_delivery`. Only when both confirmations are recorded does the invoice advance to `Confirmed`.

```
No fund movement. Invoice status: Funded → Active → Confirmed.
```

#### Step 6 — Repay (Buyer → Pool, bypassing Escrow)
The buyer calls `invoice.repay(invoice_id)`, which transfers `face_value` USDC **directly from the buyer to the pool**, then calls `pool.receive_repayment` to account for the yield.

```
Buyer ──[face_value USDC]──► Pool
  Pool books yield: face_value − funded_amount = gross yield
  Protocol fee cut: protocol_cut = gross_yield × fee_bps / 10000 ──► Treasury
  LP yield: lp_yield = gross_yield − protocol_cut
  TotalDeposits += lp_yield  (share price rises for all LPs)
  TotalYieldDistributed += lp_yield
Invoice status: Confirmed → Repaid
```

**Protocol Fee:** Defaults to 0 bps at deployment, preserving 100% yield distribution to LPs. The contract admin can configure a fee up to 2000 bps (20%) and set the treasury destination address via `pool.set_protocol_fee(fee_bps, treasury)`.

Repayment does **not** flow through escrow. The escrow contract is only involved in funding (Step 3), the missing issuer release (Step 4), and default recovery (Step 7).

#### Step 7 — Default (Escrow → Pool)
If the invoice passes its `due_date` without reaching `Repaid`, any caller triggers `invoice.trigger_default`. The invoice contract calls `pool.handle_default`, which in turn calls `escrow.handle_default` — returning the still-locked `funded_amount` to the pool.

```
invoice.trigger_default()
  └─► pool.handle_default()
        └─► escrow.handle_default()
              └─► Escrow ──[funded_amount USDC]──► Pool
Invoice status: → Defaulted
  TotalFunded -= funded_amount  (liquidity freed)
```

### Summary Table

| Event | Source | Destination | Amount | Escrow involved? |
|---|---|---|---|---|
| LP deposit | LP wallet | Pool | `usdc_amount` | No |
| LP withdraw | Pool | LP wallet | `shares × price` | No |
| Fund invoice | Pool | Escrow | `face_value × (1 − discount)` | Yes — locks |
| Release to issuer *(gap)* | Escrow | Issuer | `funded_amount` | Yes — releases |
| Repay | Buyer wallet | Pool | `face_value` | No |
| Default recovery | Escrow | Pool | `funded_amount` | Yes — releases |

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

## Quick Start

### Prerequisites

- Rust 1.85.0 (required — other versions either have WASM bugs or are blocked by Stellar CLI)
- [Stellar CLI](https://github.com/stellar/stellar-cli) (latest)
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
- Emergency pausing (not yet implemented — see roadmap below)
- Triggering `handle_default` as a fallback recovery path (`escrow_contract`)

**Risk:** Loss or compromise of the admin key has a high blast radius. A single actor also introduces censorship risk for issuer onboarding.

**Roadmap:** Migrate admin to a multi-sig (e.g., 3-of-5 Stellar signers) before any mainnet deployment.

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

### No emergency pause mechanism

There is currently no circuit breaker. If a critical bug is found post-deployment the only recourse is to stop directing traffic to the affected contracts via the frontend.

**Roadmap:** Add an `admin_pause() / admin_unpause()` function pair to each contract, guarded behind multi-sig, that blocks state-changing calls while reads remain live.

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
| 6 | `InvalidFaceValue` | Face value is zero |
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

---

## License


MIT — see [CHANGELOG.md](./CHANGELOG.md) for version history.

---

## Contributors

[![Contributors](https://contrib.rocks/image?repo=TrusTrove/TrusTrove-contract)](https://github.com/TrusTrove/TrusTrove-contract/graphs/contributors)
// fix
