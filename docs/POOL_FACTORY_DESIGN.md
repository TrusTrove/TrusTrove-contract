# Pool Factory Design: Per-Asset Pool Instances (Option B)

> **Issue:** #735. **Supersedes:** #17 (multi-asset pool support), and settles
> its Option A/B question in favour of **Option B**.
> **Related:** [POOL_FACTORY_MIGRATION.md](./POOL_FACTORY_MIGRATION.md)
> (adopting the existing testnet USDC pool as instance #1),
> [SEP41_DESIGN.md](./SEP41_DESIGN.md) (per-instance share metadata).
> **Code:** [`contracts/pool_factory/`](../contracts/pool_factory/),
> [`contracts/pool/src/lib.rs`](../contracts/pool/src/lib.rs).

---

## 1. Decision

TrusTrove supports more than one funding asset by running **one independent
single-asset `pool` contract per asset**, deployed and indexed by a small
**`pool_factory`** contract. We do **not** turn `pool` into one commingled
multi-asset pool.

| | Option A: one multi-asset pool | **Option B: factory + per-asset pools (chosen)** |
|---|---|---|
| Risk isolation | A default or bug in one asset's book hits every LP | A shortfall in one asset pool cannot touch another |
| Share pricing | Needs an on-chain exchange-rate oracle to value a mixed book in one share unit | Each pool prices shares in its own asset's base units; no oracle |
| Changes to `pool` | Rewrite of storage, share math and every invariant/property test | `pool` stays single-asset; only its asset becomes a parameter |
| LP choice | LPs take exposure to the whole basket | LPs pick the asset they want exposure to |
| Repo idiom | One large contract | Small single-purpose contracts (`invoice`, `pool`, `registry`, `escrow`, `ttl`, `pause`) |
| Cost | — | One extra contract; liquidity is fragmented per asset |

Liquidity fragmentation is the accepted cost: a USDC LP cannot fund an XLM
invoice. Cross-asset funding would need swaps, which is out of scope (see the
"Swap-free multi-asset pools" roadmap item in [LIMITATIONS.md](./LIMITATIONS.md)).

---

## 2. Why a new contract, not an extension of `RegistryContract`

`RegistryContract` (`contracts/registry/`) is a **KYC/identity oracle**: it
stores `Profile`s keyed by participant address, with a role
(`Issuer`/`Buyer`), a verification flag and metadata, and it is consulted by
`invoice::create` and `pool::fund_invoice` through `is_verified`. Putting pool
instances there would be the wrong boundary because:

1. **Different subject.** Registry entries describe *people/accounts*; factory
   entries describe *deployed contracts*. They share neither key space nor
   lifecycle (profiles are revoked/reinstated; pools are deployed once and
   never replaced).
2. **Different privilege.** Registering a pool deploys code and wires it to
   custody funds. That should not be reachable by the registry's operational
   roles (e.g. the verifier role added in #732), and registry admin rotation
   should not silently change who can create pools.
3. **Deployment needs.** The factory must call `env.deployer()` with itself as
   deployer so pool addresses are deterministic per asset. Adding deploy
   authority to the identity oracle widens its blast radius for no benefit.
4. **Upgrade independence.** The factory can be redeployed (it only indexes
   addresses; migration is `register_existing_pool`) without touching the
   registry's persistent profile data, and vice-versa.
5. **Repo idiom.** Every other concern already has its own contract; the
   factory stays ~400 lines with five entry points.

---

## 3. Storage layout (`contracts/pool_factory/src/types.rs`)

All keys live in **instance** storage.

| Key | Type | Written by | Meaning |
|-----|------|------------|---------|
| `DataKey::Admin` | `Address` | `initialize` | Factory admin. Becomes the admin **and** initial treasury of every pool it deploys. |
| `DataKey::AssetCount` | `u32` | `initialize` (0), `record_pool` (+1) | Number of registered assets; upper bound for `AssetIndex`. |
| `DataKey::AssetIndex(u32)` | `Address` | `record_pool` | `i`-th registered asset, in registration order. Append-only. |
| `DataKey::PoolForAsset(Address)` | `Address` | `record_pool` | The pool instance for an asset. Written once; never replaced. |

Invariants:

- `PoolForAsset(a)` exists ⇔ `a` appears exactly once in
  `AssetIndex(0..AssetCount)`.
- An asset can be registered at most once (`AssetAlreadyRegistered`, #3), on
  either path, so an existing instance (and its LPs) can never be orphaned by
  repointing the mapping.
- The mapping is written **last** in `register_asset`, so a reader can never
  observe a pool that is deployed but not yet initialized.

Constants (`contracts/pool_factory/src/constants.rs`) passed to every deployed
instance: `DEFAULT_MIN_INITIAL_DEPOSIT = 10_000_000`,
`DEFAULT_SHARE_NAME = "TrusTrove Pool Shares"`,
`DEFAULT_SHARE_SYMBOL = "TT-POOL"`, `DEFAULT_SHARE_DECIMALS = 7`.

> **Scaling note:** instance storage is loaded on every call, so the factory's
> instance entry grows with each registered asset. That is fine for the
> expected handful of assets; if the asset list ever grows into the dozens,
> `AssetIndex`/`PoolForAsset` should move to persistent storage.

---

## 4. Deploy / register flow

### 4.1 New asset: `register_asset(asset, pool_wasm_hash, invoice_contract, escrow_contract) -> Address`

```
admin ──register_asset──▶ pool_factory
                            │ 1. admin.require_auth()
                            │ 2. assert_unregistered(asset)
                            │ 3. salt = sha256(asset.to_xdr())
                            │ 4. pool = env.deployer()
                            │          .with_current_contract(salt)
                            │          .deploy(pool_wasm_hash)
                            │ 5. registry = invoice_contract.get_registry_contract()
                            │ 6. pool.initialize(admin, invoice_contract, escrow_contract,
                            │        asset, registry, admin /*treasury*/,
                            │        DEFAULT_MIN_INITIAL_DEPOSIT,
                            │        DEFAULT_SHARE_NAME, DEFAULT_SHARE_SYMBOL,
                            │        DEFAULT_SHARE_DECIMALS)
                            │      └─ pool cross-checks escrow.get_usdc_asset() == asset
                            │ 7. record_pool(asset, pool)
                            │ 8. emit pool_instance_created(asset, pool)
                            ▼
                          returns pool
```

Design points:

- **Deterministic address.** Deployer = factory, salt = `sha256(XDR(asset))`,
  so an asset's pool address is predictable off-chain before submission and
  independent of registration order. Hashing the XDR works for both contract
  and account-addressed assets.
- **Registry is derived, not passed.** It's read from the invoice contract so
  the pool re-verifies participants against the same registry the invoice
  contract used.
- **Escrow must pre-exist** and be initialized for the same asset; the pool's
  `initialize` rejects a mismatch (`PoolError::EscrowAssetMismatch`).
- **Per-asset decimals.** The factory always passes the 7-decimal defaults.
  For an asset with different decimals, initialize the pool directly with the
  right `min_initial_deposit` / `share_decimals` and adopt it via §4.2.

### 4.2 Existing pool: `register_existing_pool(asset, pool_address)`

Admin-only. It skips deployment, checks that
`pool_address.get_funding_asset() == asset` (`InvalidPool`, #5, otherwise),
then calls the same `record_pool` and emits `existing_pool_registered`. This
is the migration path for the pre-factory USDC pool — see
[POOL_FACTORY_MIGRATION.md](./POOL_FACTORY_MIGRATION.md).

### 4.3 Reads

- `get_pool_for_asset(asset) -> Option<Address>`: the one lookup for both
  registration paths.
- `list_assets() -> Vec<Address>`: unbounded walk of `AssetIndex`.
- `get_aggregate_stats() -> Vec<(Address, PoolStats)>`: one `get_stats`
  call per pool; failing pools are skipped. `PoolStats` is a hand-copied mirror
  of `trusttrove_pool::PoolStats` and **must be kept field-for-field in sync**,
  or every call silently skips every pool.

---

## 5. How `invoice::add_supported_asset` composes with the factory

The two lists answer different questions and are kept **separate on purpose**:

| | `invoice` `SupportedAsset(asset)` | `pool_factory` `PoolForAsset(asset)` |
|---|---|---|
| Question | May an issuer *create* an invoice denominated in this asset? | Which pool *funds* invoices in this asset? |
| Set by | Invoice admin, `add_supported_asset` / `remove_supported_asset` | Factory admin, `register_asset` / `register_existing_pool` |
| Checked in | `invoice::create` (`UnsupportedAsset`) | Off-chain routing today; on-chain once §6.2 lands |

Required operational ordering for a new asset `X`:

1. Deploy + initialize an escrow for `X`.
2. `pool_factory.register_asset(X, …)` → pool `P_X`.
3. Wire `P_X` into the invoice contract (see §6.2 — **currently a blocker**).
4. LPs deposit into `P_X`.
5. **Last**, `invoice.add_supported_asset(X)`.

Supporting the asset in `invoice` before a pool exists would let issuers create
and list invoices that nothing can fund; they would just expire. Removing an
asset from `invoice` only stops *new* invoices; outstanding ones keep settling
through their recorded `funding_pool`.

The invoice contract should **not** read the factory to decide
`SupportedAsset`. That would make invoice creation depend on a second
contract's availability and couple the two admins' decisions. A consistency
check (e.g. an off-chain script asserting every `SupportedAsset` has a
`PoolForAsset`) is enough.

---

## 6. Single-asset assumptions checklist

This is the checklist for follow-up implementation issues. It records every
place that still assumes one asset or one pool, as of this doc. Line numbers
are approximate and will drift; search for the quoted symbol.

### 6.1 `contracts/pool/src/lib.rs` and friends

Most of the pool was already generalised during the factory work: the old
`DataKey::UsdcAsset` became `DataKey::FundingAsset`
(`contracts/pool/src/types.rs`), the old `fn usdc()` became
`fn funding_asset()`, and the minimum deposit is configurable per instance. What
remains:

| # | Location | Hardcode | Follow-up |
|---|----------|----------|-----------|
| P1 | `types.rs` `DataKey::FundingAsset` (formerly `DataKey::UsdcAsset`) | Done: per-instance asset key. | None. Keep the discriminant stable for deployed pools. |
| P2 | `lib.rs` `fn funding_asset()` (~L2077, formerly `fn usdc()`) | Done: reads `FundingAsset`. | None. |
| P3 | `lib.rs` `get_usdc_asset()` (~L252) | Deprecated alias of `get_funding_asset`. | Remove in a breaking release, after integrators move to `get_funding_asset`. |
| P4 | `constants.rs` `DEFAULT_MIN_INITIAL_DEPOSIT` (~L22, formerly `MIN_INITIAL_DEPOSIT`) and its "1 USDC = 10_000_000 stroops" comment (~L17, ~L55) | The fallback assumes 7 decimals. | Factory should take `min_initial_deposit` per asset (see F1). |
| P5 | `constants.rs` `DEFAULT_SHARE_DECIMALS = 7` (~L62) | Same 7-decimal assumption, for instances that predate `share_decimals`. | Same as P4. |
| P6 | `lib.rs` `fund_one` / `fund_invoice`: `invoice_asset != usdc_id` → `AssetMismatch` (~L1236–1244) | Already compares against `funding_asset()`; the only leftover is the `usdc_id` variable name. | Rename to `pool_asset`. This check is **correct and must stay**: it stops a pool funding an invoice in another asset. |
| P7 | `lib.rs` `initialize`: calls `escrow.get_usdc_asset()` (~L160) | Depends on escrow's USDC-named getter. | Switch to `get_funding_asset` once escrow exposes it (E1). |
| P8 | `lib.rs` `deposit` / `withdraw` / `preview_*` / refund paths: `usdc_amount`, `usdc_id`, `usdc_to_return`, `usdc_value` locals (~L566–778, ~L1488, ~L2091–2140, ~L2265) and "Deposits USDC…" rustdoc | Naming only, no behaviour. | Rename to `amount` / `asset` and update rustdoc. Argument names are part of the contract spec (they show in `stellar contract invoke --help`), so rename in a release that's allowed to change the interface. |

### 6.2 Outside `pool`: blockers for running more than one pool

These matter more than P1–P8. Until they're fixed, the factory can deploy many
pools but the invoice contract will only let **one** of them fund invoices.

| # | Location | Hardcode | Follow-up |
|---|----------|----------|-----------|
| I1 | `contracts/invoice/src/lib.rs` `DataKey::PoolContract` + `set_pool_contract` (~L193) | One pool per invoice contract. | Replace with a per-asset mapping (`PoolForAsset(asset)` in invoice), or let invoice ask the factory. |
| I2 | `invoice::mark_funded`: `pool_address != configured_pool` → `NotAuthorized` (~L1321–1337; the check is duplicated) | Only the single configured pool may fund. | Authorise `pool_address` iff it is the pool registered for `invoice.funding_asset`. Remove the duplicate check. |
| I3 | `invoice` `DataKey::EscrowContract` + `set_escrow_contract` (~L366), used by `repay_partial` (~L1577) and `repay_early` (~L1795) | One escrow per invoice contract, and escrows are single-asset. | Make the escrow per-asset too, or resolve it from the funding pool. |
| I4 | `invoice` wiring checks (`assert_valid_wiring_address`) | Assume exactly one pool and one escrow when checking for aliased addresses. | Update once I1/I3 change the shape. |
| E1 | `contracts/escrow/src/lib.rs` `DataKey::UsdcAsset`, `usdc_client()`, `get_usdc_asset()` (~L60–95) | USDC naming; behaviour is already per-instance. | Add `get_funding_asset` (keep `get_usdc_asset` as an alias), rename the key's Rust identifier only, and keep the discriminant for deployed escrows. |
| F1 | `pool_factory::register_asset` | Always passes the 7-decimal `DEFAULT_*` values and generic share metadata. | Accept `min_initial_deposit`, `share_name`, `share_symbol`, `share_decimals` per asset. |
| F2 | `pool_factory` | No `require_not_paused`, and no TTL extension on its instance storage. | Add `trusttrove-pause` and `extend_ttl` like the other contracts. |

Already multi-pool safe: repayment and default routing read the invoice's own
`funding_pool` (set in `mark_funded`), so once I1/I2 land, each invoice settles
back to whichever pool funded it.

---

## 7. Testing (once implementation issues are scoped)

`cargo test` for this doc-only issue: **N/A**. Follow-up issues should add:

- Factory: two assets → two pools with distinct deterministic addresses;
  duplicate registration rejected on both paths; `register_existing_pool`
  rejects a pool for the wrong asset (already covered in
  `contracts/pool_factory/src/test.rs`).
- Invoice (after I1/I2): an XLM invoice can be funded only by the XLM pool,
  and the USDC pool's `fund_invoice` on it fails with `AssetMismatch`
  (pool-side) or `NotAuthorized` (invoice-side).
- End-to-end: two pools funding, repaying and defaulting independently, with
  `get_aggregate_stats` reporting both and neither pool's accounting moving
  when the other's invoice defaults.
