# Threat Model

> **Updated:** 2026-10-21
> **Applies to:** All four Soroban smart contracts (registry, invoice, escrow, pool)

This document describes the trust assumptions, authentication gates, and
attack surfaces of the TrusTrove protocol. It is intended for security
researchers, integrators, and contributors evaluating the protocol's risk
profile.

---

## Table of Contents

- [Trust Model](#trust-model)
- [Authentication Gates](#authentication-gates)
- [Registry Profile States and Transitions](#registry-profile-states-and-transitions)
- [Attack Vectors](#attack-vectors)
- [Centralization Risks](#centralization-risks)
- [Defence-in-Depth](#defence-in-depth)
- [Out-of-Scope](#out-of-scope)

---

## Trust Model

### Roles

| Role | Description | Trust Level |
|------|-------------|-------------|
| **Admin** | Deployer address that calls `initialize()` on each contract. Controls profile management, default triggers, and configuration. | **High** — single point of failure |
| **Issuer** | A verified SME that creates invoices and requests funding. | Low — only self-authenticates |
| **Buyer** | A verified counterparty obligated to repay an invoice at maturity. | Low — only self-authenticates |
| **LP** | Liquidity provider that deposits USDC into a pool. | Low — only self-authenticates |
| **Anonymous caller** | Any Stellar account that can call permissionless functions. | None — no auth required |

### Trust Boundaries

```
  ┌─────────────────────────────────────────────────────┐
  │                  Contract Layer                      │
  │  ┌──────────┐  ┌───────────┐  ┌─────────┐  ┌────┐  │
  │  │ Registry │  │  Invoice  │  │ Escrow  │  │Pool│  │
  │  └────┬─────┘  └─────┬─────┘  └────┬────┘  └─┬──┘  │
  │       │              │              │          │     │
  │       └──────────────┴──────────────┴──────────┘     │
  │                 Trust boundary                        │
  │     (contracts trust each other's registered addr)    │
  └───────────────────────────────────────────────────────┘
            ▲                          ▲
            │                          │
   Verified via registry         Self-authenticated
   (issuer / buyer)              (LP deposits / withdrawals)
```

**Key trust assumption:** All contracts trust the addresses stored during
`initialize()` — if any stored reference is wrong, the caller loses funds
irrevocably.

---

## Authentication Gates

Every state-changing function enforces one of these auth patterns:

### Pattern A — Self-Auth (`address.require_auth()`)

The caller authenticates only themselves. No admin or third party is involved.

| Function | Contract | Who Auths |
|----------|----------|-----------|
| `register_issuer` | Registry | The issuer address |
| `register_buyer` | Registry | The buyer address |
| `update_metadata` | Registry | The profile address |
| `create` | Invoice | The issuer |
| `list_for_financing` | Invoice | The issuer |
| `mark_shipped` | Invoice | The issuer |
| `confirm_delivery` | Invoice | The confirmer (issuer _or_ buyer) |
| `repay` | Invoice | The buyer |
| `expire_listing` | Invoice | The caller (stored issuer _or_ admin) |
| `deposit` | Pool | The LP |
| `withdraw` | Pool | The LP |

### Pattern B — Admin-Auth (`admin.require_auth()`)

Only the stored admin address may call this function.

| Function | Contract | Why |
|----------|----------|-----|
| `revoke` | Registry | Prevents unauthorised removal of profiles |
| `reinstate` | Registry | Kept admin-only (not delegated to the verifier role, #732) |
| `set_verifier`, `remove_verifier` | Registry | Assigning the verifier role is an admin decision (#732) |
| `batch_register_issuers` | Registry | Batch operations gated by admin |
| `set_pool_contract` | Invoice | Wiring a new pool changes fund flows |
| `set_expiry_window` | Invoice | Configuration parameter |
| `fund_invoice` | Pool | (*currently*) Capital allocation from pool |
| `set_max_utilization` | Pool | Risk parameter |
| `transfer_ownership` | All | Changes the admin key |

### Pattern B2 — Admin-or-Verifier (`caller.require_auth()` + role check)

`caller` must be the stored admin **or** the stored verifier
(`DataKey::Verifier`), and must sign. Lets routine KYC decisions be delegated
to an operational key without exposing admin powers (#732).

| Function | Contract | Why |
|----------|----------|-----|
| `verify_profile` | Registry | Day-to-day verification; the verifier cannot transfer admin/ownership, pause, revoke, reinstate or reassign the role |

> **Note:** `verify_profile(caller, address, false)` also marks a profile
> revoked, so a verifier *can* un-verify a profile through this path. The
> dedicated `revoke` / `reinstate` entry points (and their `address_revoked` /
> `address_reinstated` events) stay admin-only, and the admin can remove a
> misbehaving verifier at any time with `remove_verifier`.

### Pattern C — Cross-Contract Auth (`contract.require_auth()`)

One contract authenticates as the caller of another contract's function.

| Function | Contract | Authenticated Caller | Why |
|----------|----------|---------------------|-----|
| `mark_funded` | Invoice | The pool contract | Prevents faking funding records |
| `receive_repayment` | Pool | The invoice contract | Prevents faking repayment events |
| `receive_repayment_with_refund` | Pool | The invoice contract | Same as above |
| `handle_default` | Pool | The invoice contract | Prevents premature default recovery |
| `lock` | Escrow | The pool contract | Only the pool can lock funds |
| `release_to_issuer` | Escrow | The pool contract | Only the pool can release to issuer |
| `release_to_pool` | Escrow | The pool contract | Only the pool can reclaim escrowed funds |

---

## Escrow Initialization and Repayment Authorization Model

### Stored References from `initialize()`

The escrow contract stores the following references during `initialize(env, admin, pool_contract, usdc_asset)`:

| Storage Key | Value Type | Description |
|-------------|------------|-------------|
| `Admin` | `Address` | Contract admin, must authorize `initialize()` and admin-gated operations |
| `PoolContract` | `Address` | Authorized pool contract; only it may call `lock`, `release_to_issuer`, `release_to_pool` |
| `InvoiceContract` | `Address` | Invoice contract address; pinned via `set_invoice_contract()` for `release_to_pool` validation (#997) |
| `UsdcAsset` | `Address` | USDC token contract; used to create the token client for all transfers |
| `TimeoutWindow` | `u64` | Seconds after `locked_at` before `release_after_timeout` may permissionlessly return a lock to the pool. Defaults to 30 days; capped at 365 days |
| `Paused` | `bool` | Emergency circuit-breaker flag. While paused, `lock`, `release_to_issuer`, `release_to_pool`, `handle_default` all revert with `ContractPaused` |

### Authorization for Each Escrow Function

| Function | Caller Authorization | Key Storage Reference Used |
|----------|----------------------|-----------------------------|
| `lock` | `pool.require_auth()` — the configured pool contract must sign. This is the primary cross-contract auth gate (Pattern C). | `PoolContract`, `UsdcAsset` |
| `release_to_issuer` | `pool.require_auth()` — the configured pool contract must sign. The issuer must match the stored `record.issuer` from the `Locked` record. | `PoolContract`, `UsdcAsset`, `Locked` record (amount, issuer) |
| `release_to_pool` | `invoice_contract.require_auth()` — the invoice contract must sign (issue #820). Additionally, if a pinned invoice contract reference exists (via `set_invoice_contract(#997)`), the pool-resolved address must match the pin or the call is rejected with `InvalidConfig`. The function consumes either a live `Locked` record or the `IssuerReleased` marker. | `PoolContract`, `InvoiceContract` (pinned or resolved), `IssuerReleased` marker, `Locked` record, `repayment_amount` |
| `handle_default` | `caller.require_auth()` — must equal either the stored admin or the pool contract. The grace period (`DEFAULT_MIN_LOCK_SECONDS`, measured from `locked_at`) must have elapsed. If no `Locked` record exists (because funding path released to issuer per #819), returns `false` and the pool accounts for it as an unrecoverable LP capital loss. | `PoolContract`, `Admin`, `Locked` record (or absence thereof), `locked_at`, `DEFAULT_MIN_LOCK_SECONDS` |

### Invoice-Contract Binding for Repayment

Repayment flows require the invoice contract to be properly wired:

1. During deployment, the pool is initialized with `invoice_contract` via `initialize(invoice_contract, escrow_contract, ...)` (DEPLOYMENT.md §"Contract Wiring Order", step 7).
2. The pool's `fund_invoice` call wires the invoice into escrow via `escrow.lock()` followed immediately by `escrow.release_to_issuer()` (issue #819), so the issuer is paid at funding time and the `Locked` record is removed.
3. At repayment time, `buyer → invoice.repay → escrow.release_to_pool → pool`. The `release_to_pool` entry point:
   - Resolves the invoice contract from the pool's `get_invoice_contract` view (#820)
   - When a pinned reference exists via `set_invoice_contract(#997)`, validates that the pool-resolved address matches the pin — rejecting misconfigured pools that have drifted from the admin's pin
   - Accepts evidence of a prior lock: either the live `Locked` record (principal still in escrow) **or** the `IssuerReleased` marker (principal already paid to the issuer at funding). Whichever exists is consumed, so a full settlement can succeed at most once per funded invoice.
4. The repayment amount is the buyer's repayment routed through escrow to the pool; the full `face_value` including yield is settled when the invoice lifecycle reaches `Confirmed` status.

### State Sequence Matching the Tests

The following state sequence is tested and documented for end-to-end correctness:

```
1. initialize(escrow, pool, usdc) — stores PoolContract, UsdcAsset, Admin references
2. fund_invoice(invoice_id) — pool → escrow.lock → escrow.release_to_issuer → issuer paid; Lock record removed; IssuerReleased marker written
3. buyer repays: release_to_pool(invoice_id, repayment_amount) — consumes either Locked or IssuerReleased marker
   - If partial: residual Lock amount reduced, original locked_at preserved
   - If full: Lock/Marker removed, funds returned to pool
4. handle_default(invoice_id, caller) — if Lock still live (not yet paired with issuer release): grace period check; return funds to pool; remove Lock
   - If no Lock exists (funding path already paid issuer): returns false; pool accounts as unrecoverable loss
5. release_after_timeout(invoice_id) — permissionless exit when now >= locked_at + timeout_window; returns stale lock to pool
```

All amounts are in **USDC stroops** (1 USDC = 10,000,000 stroops, 7 decimals). Timestamps are **Unix seconds**. Invoice IDs are **BytesN<32>** (32-byte identifiers).

### Defense-in-Depth Notes

- The `IssuerReleased` marker (issue #819) is the critical bridge between the funding path and repayment/default paths. Without it, `release_to_pool` and `handle_default` cannot distinguish "principal already paid to issuer" from "lock removed out of band".
- The pinned invoice contract reference (#997) is a defense-in-depth measure: it prevents a pool whose `get_invoice_contract` configuration has drifted from the escrow admin's pin from silently redirecting repayments.
- The `handle_default` function gracefully handles both pre-#819 and post-#819 invoice states: it returns `false` when no `Locked` record exists (post-#819 default case) and the pool accounts for the loss, rather than erroneously returning funds.
- The `release_to_pool` function's dual-consumption guard (consuming whichever of `Locked` or `IssuerReleased` exists) ensures idempotent settlement at most once per funded invoice.


| Function | Contract | Signers |
|----------|----------|---------|
| `initialize` | All | The incoming admin (prevents assigning an uncontrolled address) |
| `transfer_ownership` | All | Current admin + new admin (prevents accidental transfers) |

### Pattern E — Permissionless (no auth)

Any account can call these.

| Function | Contract | Notes |
|----------|----------|-------|
| `get_profile`, `is_verified`, `get_verification_status`, `get_admin`, `get_verifier` | Registry | Read-only views |
| `get`, `get_status`, `get_face_value`, `get_discount_bps`, `get_funding_asset`, `get_issuer`, `get_by_status`, `get_by_issuer`, `get_by_buyer`, `get_invoice_count_by_issuer`, `get_invoice_count_by_buyer`, `get_counts`, `get_expiry_window` | Invoice | Read-only views |
| `get_locked`, `get_history` | Escrow | Read-only views |
| `get_stats`, `get_lp_position`, `get_utilization_rate`, `get_usdc_asset` | Pool | Read-only views |
| `trigger_default` | Invoice | Callable by any account once `now >= due_date` |

---

## Registry Profile States and Transitions

Every registry profile managed by [`contracts/registry/src/lib.rs`](../contracts/registry/src/lib.rs)
is in exactly one verification state. The `get_verification_status` view
decodes it; an address with no profile at all reports `Unregistered`, which
is the pre-registration state rather than a profile state.

| State | Storage flags | `is_verified()` |
|-------|---------------|-----------------|
| `Pending` | `verified = false`, `revoked = false` | `false` |
| `Verified` | `verified = true`, `revoked = false` | `true` |
| `Revoked` | `verified = false`, `revoked = true` | `false` |

### Transition table

| From | Entry point | To | Authorization |
|------|-------------|----|---------------|
| `Unregistered` | [`register_issuer`](../contracts/registry/src/lib.rs) / [`register_buyer`](../contracts/registry/src/lib.rs) | `Pending` | The registered address itself (self-auth) |
| `Unregistered` | `batch_register_issuers` / `batch_register_buyers` | `Pending` | Admin only |
| `Pending` | `verify_profile(caller, address, true)` | `Verified` | Admin or verifier (#732) |
| `Pending` | `verify_profile(caller, address, false)` | `Revoked` | Admin or verifier (#732) |
| `Pending` | `revoke` | `Pending` (no-op — a never-verified profile is untouched) | Admin only |
| `Pending` | `reinstate` | `Pending` (no-op — only operates on `Revoked` profiles, #606) | Admin only |
| `Verified` | `revoke` | `Revoked` | Admin only |
| `Verified` | `verify_profile(caller, address, false)` | `Revoked` | Admin or verifier (#732) |
| `Verified` | `verify_profile(caller, address, true)` | `Verified` (no state change, but a `profile_verified` event is re-emitted per call, #987) | Admin or verifier (#732) |
| `Revoked` | `reinstate` | `Verified` | Admin only |
| `Revoked` | `verify_profile(caller, address, true)` | `Verified` | Admin or verifier (#732) |
| `Revoked` | `revoke` | `Revoked` (idempotent no-op, no event re-emitted) | Admin only |

Notes:

- There is **no deregistration**: a revoked profile stays registered and
  enumerated by `list_profiles`; revocation only flips the verification
  flags.
- `revoke` only acts on a `Verified` profile; to un-verify any registered
  profile (including a `Pending` one) the verifier path
  `verify_profile(caller, address, false)` is the general mechanism.
- Re-registering an existing address panics with `AlreadyRegistered`, so a
  profile can never return to `Pending` after its first registration.

### Caller authorization for verification and admin entry points

| Function | Who may call | Exact authorization | Source |
|----------|--------------|---------------------|--------|
| [`verify_profile`](../contracts/registry/src/lib.rs) | Stored admin **or** stored verifier (#732) | `caller.require_auth()` after a role check; any other caller panics with `Unauthorized` | [`contracts/registry/src/lib.rs`](../contracts/registry/src/lib.rs) |
| [`revoke`](../contracts/registry/src/lib.rs) | Stored admin only | `admin.require_auth()` | [`contracts/registry/src/lib.rs`](../contracts/registry/src/lib.rs) |
| [`reinstate`](../contracts/registry/src/lib.rs) | Stored admin only | `admin.require_auth()` | [`contracts/registry/src/lib.rs`](../contracts/registry/src/lib.rs) |
| [`transfer_ownership`](../contracts/registry/src/lib.rs) | Current admin **and** incoming admin | Dual auth: `admin.require_auth()` + `new_admin.require_auth()`, so ownership cannot be transferred to a non-consenting address | [`contracts/registry/src/lib.rs`](../contracts/registry/src/lib.rs) |
| [`transfer_admin`](../contracts/registry/src/lib.rs) | Current admin only | `admin.require_auth()`; the incoming admin does **not** sign — a deliberate key-rotation escape hatch that is strictly weaker than `transfer_ownership` | [`contracts/registry/src/lib.rs`](../contracts/registry/src/lib.rs) |

---

## Attack Vectors

### 1. Admin Key Compromise

**Risk:** The admin key controls profile revocations, pool funding,
and ownership transfer. Compromise of this key has a high blast radius.

**Mitigation:**
- `transfer_ownership` requires dual auth (current + new admin) — a stolen key
  cannot transfer ownership to the attacker without the new key also signing.
- The escrow `handle_default` has an alternative caller (pool contract), so
  admin compromise does not grant unilateral fund recovery.
- **Planned:** Multi-sig admin (see [Roadmap section in README](../README.md#known-centralization-risks--roadmap)).

### 2. Cross-Contract Spoofing

**Risk:** A malicious contract deployed at the same address as a legitimate
contract could fake `require_auth()` calls.

**Mitigation:**
- Contract addresses are hardcoded during `initialize()` and cannot be changed
  without admin auth.
- The invoice contract checks the pool address on every `mark_funded` call,
  and the pool checks the invoice address on `receive_repayment` and `handle_default`.

### 3. Dust-Attack on Pool Shares

**Risk:** After the pool accrues yield, the share price rises above 1.0.
A tiny deposit can round down to 0 shares while the USDC is still transferred
into the pool, effectively donating the depositor's funds to existing LPs.

**Mitigation:** The `deposit()` function rejects any deposit that would mint
0 shares with `PoolError::MinimumDeposit (#14)`. The check runs before the
token transfer, so no funds leave the depositor on the rejection path.

### 4. Replay of State Transitions

**Risk:** An attacker could try to call state-transition functions multiple
times on the same invoice (e.g., double-funding).

**Mitigation:** Every state transition in the invoice contract checks the
current status before proceeding. `list_for_financing` only works on `Created`
invoices; `mark_funded` only on `Listed`; `mark_shipped` only on `Funded`;
`confirm_delivery` only on `Active`; `repay` only on `Confirmed`; and
`trigger_default` only on `Funded`/`Active`/`Confirmed`. Once the status
advances, the previous function cannot be called again.

Additionally, escrow's `lock()` function rejects re-locking an already-locked
invoice with `EscrowError::AlreadyLocked`.

### 5. Integer Over/Underflow

**Risk:** Arithmetic errors in amount calculations could lead to incorrect
fund allocations.

**Mitigation:**
- All contracts use Rust `u128` for amounts. The release profile enables
  `overflow-checks = true` (see [Cargo.toml](../Cargo.toml)).
- `receive_repayment` validates `amount >= funded_amount`.
- `receive_repayment_with_refund` validates `refund <= amount - funded_amount`.
- `withdraw` validates `shares <= lp_shares` and `usdc_to_return <= available`.
- `handle_default` in the pool uses `saturating_sub` for deposit reduction.
- The invoice's `repay` functions use `saturating_sub` for discount calculations.

### 6. Front-Running on Invoice Expiry

**Risk:** An attacker could front-run an `expire_listing` call to fund an
invoice that would otherwise expire.

**Mitigation:** `expire_listing` only succeeds when `current_time > listed_at
+ expiry_window`. If the expiry window has passed, anyone can call
`expire_listing`, but a funder could still call `fund_invoice` in the same
ledger (before `expire_listing`). This is an accepted race condition given
the current permissionless funding model.

### 7. Utilization Cap Bypass

**Risk:** An admin could set `max_utilization_bps` above 10000 (100%) to
allow over-leveraging the pool.

**Mitigation:** `set_max_utilization` validates `new_cap_bps <= 10000` and
rejects values above 10000.

### 8. Batch Registration Overflow

**Risk:** An attacker could submit a very large batch of issuers to exhaust
gas or storage.

**Mitigation:** `batch_register_issuers` limits entries to `≤ 50` and
panics with `RegistryError::BatchSizeExceeded` otherwise.

---

## Centralization Risks

| Risk | Current State | Planned Mitigation |
|------|---------------|--------------------|
| Single admin key | Admin is a single Stellar address | Multi-sig (3-of-5) before mainnet |
| Admin-gated `fund_invoice` | Pool funding requires admin auth | LP-governed allocation (see [README](../README.md#known-centralization-risks--roadmap)) |
| No emergency pause | No circuit breaker exists | `admin_pause() / admin_unpause()` before mainnet |
| Centralised registry | Admin controls all registrations/revocations | On-chain governance (longer-term) |
| Single oracle for verification | Registry is the sole source of truth for identity | Multi-sig registry admin |

### Blast Radius: Admin Key

If the admin key is compromised or malicious:

| What They Can Do | What They Cannot Do |
|-----------------|---------------------|
| Revoke any profile | Transfer admin to a non-consenting address (dual auth) |
| Register fraudulent profiles | Directly withdraw LP funds |
| Set a new pool contract | Trigger defaults before due date (`now >= due_date` enforced on-chain) |
| Change pool utilization cap (≤10000 bps) | Bypass the utilization cap (>10000 bps) |
| Expire listings | Mint unbacked LP shares |
| | Release escrowed funds to themselves (only pool contract can) |

---

## Defence-in-Depth

### Separation of Concerns

- **Registry** has no access to any funds.
- **Escrow** holds USDC but cannot release it without the pool contract.
- **Pool** manages accounting but does not hold USDC directly (it holds a
  balance from deposits, but escrow holds funded amounts).
- **Invoice** is a pure state machine with no fund custody.

### TTL Management

Every `persistent().set()` is immediately followed by `extend_ttl()` with
`live_until_ledger` = 100 and `seq` = 2_000_000. Instance storage is also
extended on every state-changing call via `extend_instance_ttl()`.

See [STORAGE.md](./STORAGE.md) for the full storage key schema and TTL
parameters.

### No External Dependencies Beyond Stellar

The contracts depend only on `soroban-sdk` and each other. There are no
oracle integrations, bridge contracts, or off-chain relayers whose compromise
could affect on-chain state.

---

## Out-of-Scope

The following are explicitly outside this threat model:

- **Stellar protocol-level attacks** (consensus, validator compromise, etc.)
  — report to [Stellar Development Foundation](https://stellar.org).
- **Freighter / wallet vulnerabilities** — report to the respective wallet team.
- **Frontend phishing or social engineering** — users are responsible for
  verifying transaction details before signing.
- **Off-chain indexer downtime** — the contracts function independently of
  the indexer; missed events do not affect fund safety.
- **Testnet-only risks** (Friendbot dust, ephemeral validators) — see
  [LIMITATIONS.md](./LIMITATIONS.md).

---

## Reporting

Report vulnerabilities to **security@trusttrove.xyz** or via the
[Security Advisory](https://github.com/TrusTrove/TrusTrove-contract/security/advisories)
tab. See [SECURITY.md](../SECURITY.md) for our disclosure policy and response
timeline.
