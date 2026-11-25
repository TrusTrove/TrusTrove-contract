# Changelog

All notable changes to this project will be documented in this file.


The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Changed

- `escrow`: `default_resolved` audit event data payload updated to `(u128, u64)`
  representing `(amount, lock_age)` to record elapsed lock seconds alongside
  the invoice ID, pool, caller, and resolved amount. Failed default attempts
  produce no event (#1193)
- `pool`: `get_share_price` now documents its rounding. The 1e18 fixed-point
  price is `TotalDeposits * SHARE_PRICE_SCALE / TotalShares` truncated toward
  zero, so it is deterministic and never overstates a share. Tests pin the
  value for an empty pool (`0`), after the first deposit (exactly 1.0), after
  yield (`1_006_666_666_666_666_666` for 30.2e9 over 30e9) and after a default
  (`673_333_333_333_333_333`), the truncation against round-to-nearest, that
  the view needs no authorization, and the `Overflow` boundary (#1166)
- `invoice`: `list_for_financing` and `batch_list_for_financing` now require
  `discount_bps` in `1..=5000`. A zero discount is rejected with
  `InvoiceError::InvalidDiscount` (the batch path reports it as a failed
  entry), and values above 5000 keep failing with `DiscountTooHigh`. Adds a
  proptest over the rejected range (`0` and `5001..=u32::MAX`) that checks the
  exact error and that the invoice and `get_counts()` are unchanged, plus
  pinned boundary cases for 1, 5000 and 5001 (#849)

### Fixed

- `invoice`: cap repayment yield at the full discount at maturity, including
  late settlement through `repay` or `repay_partial`, without multiplying by
  elapsed time after maturity (#948)

### Added

- `escrow`: `get_last_release_recipient(invoice_id)` view returning the address
  that received the most recent successful release or default for an invoice,
  recorded only after the token transfer succeeds. Failed transfers never
  update it, and the recipient lives in a new appended storage key so existing
  records still decode (#1296)
- `escrow`: admin-settable minimum lock amount via `set_min_lock_amount` and
  `get_min_lock_amount`, with a `min_lock_amount_set` event. The default `0`
  preserves the previous any-non-zero-amount behaviour, future locks below the
  minimum fail with `AmountBelowMinimum`, and existing locks remain releasable
  (#1297)
- `escrow`: `get_custody_coverage(invoice_id)` preflight view returning
  `CustodyCoverage { tracked_amount, token_balance, can_release }` without
  transferring funds or changing storage (#1298)
- `tooling`: `scripts/check_deployed_hashes.py`, run by both deploy scripts
  before the first reference-setting call. It compares each deployed
  contract's on-chain code hash with the intended release manifest (or the
  locally built WASM) and aborts wiring on any mismatch (#1299)
- `escrow`: `get_history_count(invoice_id)` view, returning the number of
  history events recorded for an invoice (`u32`) or `0` when absent. Preserves
  access after active lock removal and enables planning paginated reads
  without loading full history vectors into memory (#1195)
- `escrow`: `get_release_authorization(caller)` view, returning a
  `ReleaseAuthorization` that reports which escrow write entry points
  (`lock`, `release_to_issuer`, `release_to_pool`, `handle_default`,
  `release_after_timeout`) the address may call under the current
  configuration, including the pinned invoice reference and the pause state.
  Read-only, needs no auth, and never replaces the write paths' own
  `require_auth` checks (#1198)
- `invoice`: `get_issuer_invoice_ids(address, page, page_size)` view returning
  a bounded page of one issuer's invoice IDs in creation order without loading
  any `Invoice` record. `page_size` must be in `1..=MAX_PAGE_SIZE` (50),
  otherwise it panics with `InvalidPageSize`. Out-of-range pages return empty.
  `get_by_issuer` and the storage layout are unchanged (#1150)
- `pool`: `share_price_updated` event emitted on successful yield-bearing
  repayments with the invoice ID and the old/new 18-decimal fixed-point share
  prices, plus the `get_share_price()` view using the same encoding. Failed and
  zero-yield repayments emit nothing (#1171)
- `pool`: `get_lifetime_deposited_volume()` view backed by a cumulative counter
  incremented by every successful `deposit`. Withdrawals never reduce it, and
  older instances read `0` until their first deposit (#1172)
- `invoice`: `get_invoice_ids_by_status(status, cursor, page_size)` view and
  `InvoiceIdPage { ids, next }` type for indexers. It returns up to
  `page_size` (`1..=MAX_PAGE_SIZE`, else `InvalidPageSize`) invoice IDs for
  one status without decoding invoices, plus a `next` cursor that is `None`
  when the walk is complete. It walks the status index backwards, so
  invoices that stay in the status are never skipped while others change
  status, though one may be returned twice. `get_by_status` and the storage
  layout are unchanged (#1152)
- `pool`: `get_lifetime_withdrawn_volume()` view backed by a cumulative counter
  incremented only by successful LP `withdraw` amounts. Failed calls and
  internal funding transfers leave it unchanged (#1173)
- `invoice`: `exists(invoice_id) -> bool` view, so callers can check an ID
  without trapping `NotFound` from `get`. It is a pure presence check on the
  invoice record: checking a missing ID creates no storage, and checking an
  existing one does not extend its TTL. Invoices in every status, including
  cancelled and repaid ones, exist (#1153)
- `pool`: `get_lifetime_repaid_volume()` view backed by a cumulative counter of
  the full amount received by successful `receive_repayment` /
  `receive_repayment_with_refund` calls, including early repayments. Replayed
  settlements are rejected with `InvoiceNotFound` before they can double count
  (#1174)
- `pool`: `get_pool_config()` view, returning a `PoolConfig` snapshot of the
  pool's references (admin, funding asset, invoice, escrow and registry
  contracts, treasury) and funding policy (utilization cap, protocol fee,
  minimum discount, minimum initial deposit, batch size cap) in one read-only
  call. Unset policy keys report the same defaults the write paths use, and
  storage is unchanged (#1188)
- `pool`: admin-settable maximum time to maturity for newly funded invoices.
  `set_max_time_to_maturity(max_seconds)` (stored admin only, emits
  `max_maturity_updated(old, new)`) and `get_max_time_to_maturity()`, stored
  as `DataKey::MaxTimeToMaturity`. `fund_invoice` and `batch_fund_invoice`
  reject an invoice whose `due_date - now` exceeds the maximum with the new
  `PoolError::MaturityTooLong` (#30); an invoice exactly at the maximum is
  accepted. The default `0` means unrestricted and skips the extra
  `get_due_date` read, and already-funded invoices are never re-checked (#1184)
- `registry`: `get_profile_count(role)` view, returning how many profiles are
  registered for `Role::Issuer` / `Role::Buyer` in O(1) from a single stored
  counter (#841)
- `registry`: `list_profiles(role, start, limit)` view, returning a page of
  registered addresses for a role in registration order, so indexers and admin
  dashboards no longer have to replay `issuer_registered` / `buyer_registered`
  events from genesis. `limit` is capped at 50 (mirroring the batch cap) and a
  larger page panics with the new `RegistryError::PageSizeExceeded` (#8) (#841)
- `registry`: per-role enumeration index, written by every registration path
  (`register_issuer`, `register_buyer`, `batch_register_issuers`,
  `batch_register_buyers`) as `DataKey::ProfileIndex(Role, u32)` plus
  `DataKey::ProfileCount(Role)`, TTL-extended like a profile entry. Skipped
  (already-registered) batch entries are never indexed twice (#841)
- `pool`: SEP-41 `approve`, `allowance` and `transfer_from`, so third
  parties can move a share holder's allowance under the spender's own
  auth (#756)
- `pool`: SEP-41 metadata `decimals`, `name` and `symbol`, all three supplied
  to the constructor (`__constructor` since #827, formerly `initialize`) and
  stored per instance, so a pool funding an asset with
  other than 7 decimals reports the precision its shares actually have
  (#757)
- `pool`: `allowance_approved` event emitted on every successful
  `approve` (#756)
- `pool`: `get_funded_amount(invoice_id)` read-only view returning
  `Option<u128>` for the pool's active funding of an invoice: `Some(amount)`
  while funded, `None` before funding and after repayment/default (#839)
- `pool`: `protocol_fee_updated` event assertion coverage for
  `set_protocol_fee` (#767)
- Deployment scripts now pass the full `pool` constructor argument list
  (`__constructor` since #827, formerly `initialize`: `funding_asset`,
  `registry_contract`, `treasury`, `min_initial_deposit`,
  `share_name`, `share_symbol`, `share_decimals`)
- `invoice`: `get_buyer_invoice_ids(address, page, page_size)` view returning
  a bounded page of one buyer's invoice IDs in creation order without loading
  any `Invoice` record. `page_size` must be in `1..=MAX_PAGE_SIZE` (50),
  otherwise it panics with `InvalidPageSize`. Out-of-range pages return empty.
  `get_by_buyer` and the storage layout are unchanged (#1151)

### Changed

- `registry`, `invoice`, `escrow`, `pool` and `pool_factory`: the `initialize`
  entry point is replaced by a protocol `__constructor`, so deployment and
  initialization are now a single atomic transaction (issue #827). There is
  no longer a window in which a deployed contract exists but is uninitialized,
  the `AlreadyInitialized` error codes are unreachable (kept as reserved
  discriminants), and `scripts/deploy.sh` / `scripts/deploy.ps1` deploy each
  contract with its constructor arguments — pre-computing the pool's
  salt-derived address so escrow can be constructed against it in one step.
  In-place `upgrade` calls never re-run the constructor; storage migrations
  need an explicit entry point (see `docs/STORAGE.md`, Upgrade Path).
- `pool`'s constructor (`__constructor` since #827, formerly `initialize`)
  accepts three new arguments: `share_name`, `share_symbol`
  and `share_decimals`
- `invoice`: **breaking, attestation wire format (v2).** `AttestationPayload`
  gains `contract_address`, `network_id` and `valid_until`, and
  `ATTESTATION_DOMAIN_SEPARATOR` is bumped from `…v1_` to `…v2_`. Agents in
  the `underwrite-contract` repo must emit the new layout in lockstep;
  v1-signed payloads no longer decode (#822, #823). See
  `docs/AGENT_REGISTRY_INTERFACE.md` for the normative spec
- `invoice`: `create` now rejects any `face_value < MIN_FACE_VALUE`
  (100 000 stroops = 0.01 USDC) with `InvalidFaceValue`, instead of only
  rejecting zero (#824)
- `scripts/deploy.sh` / `scripts/verify.sh`: after every `initialize` the
  deployer's own address is read back from the chain via `get_admin` and a
  mismatch aborts the deployment; `verify.sh` now compares `get_admin`
  against the deployer for registry, invoice, both pools and both escrows
  instead of just printing it (#825)

### Security

- `invoice`: attestation signatures are now bound to the verifying contract
  instance and network id, closing cross-deployment/cross-network replay of
  attestations (`PayloadBindingMismatch`, new error #29) (#822)
- `invoice`: attestations expire. `valid_until` (seconds, ledger timestamp)
  is enforced at `submit_attestation` and again at
  `list_for_financing`/`batch_list_for_financing` with the new
  `AttestationExpired` error (#30) (#823)
- `invoice`: dust face values that can never be funded (pool's
  `funded_amount` rounds them to zero) can no longer be created to bloat
  the issuer/buyer/status indexes (#824)
- `scripts`: front-run initialization of freshly deployed contracts is
  detected post-`initialize` by asserting on-chain `get_admin` equals the
  deployer, with an explicit redeploy procedure on mismatch (#825)

### Testing

- `invoice`: cover attestation persistence and missing entries, all nine
  lifecycle status counts against their indexes, and the issuer view's
  stored address and typed missing-invoice error (#945, #946, #947)
- `invoice`: verify repayment balances before, at, and after maturity through
  the real pool and escrow, including installments and large-value overflow
  regression coverage (#948)

- `pool`: cover the refund-transfer failure path of
  `receive_repayment_with_refund` with a token stub that fails *after*
  applying its balance moves, proving the settlement accounting
  (`TotalDeposits`, `TotalFunded`, `ActiveInvoiceCount`, the `FundedInvoice`
  entry) and the buyer's balance are rolled back together with the failed
  transfer, so a failed refund cannot leave a partially settled invoice (#968)

- `pool`: cover the `get_lp_position` share-price overflow at the public view
  boundary with a seeded overflowing state (total shares, total deposits and
  LP shares), asserting the typed `PoolError::Overflow` (#13) and that the
  failed read leaves the pool state untouched (#972)

- `pool.deposit` gained regression coverage pinning the behaviour the
  internal `mint` helper refactor relies on: the `lp_deposited` event
  payload is unchanged and `TotalShares` stays in step with LP balances
  (#758)

## [0.1.0] - 2025-01-01

### Added

- Registry contract for verified issuer/buyer management
- Invoice contract with full lifecycle state machine
- Escrow contract for USDC custody during funding
- Pool contract with share-based LP accounting
- Deployment scripts (`setup-testnet.sh`, `deploy.sh`, `verify.sh`)
- CI workflow with formatting, clippy, build, and test checks
- Security policy (SECURITY.md)
- Code of Conduct (CODE_OF_CONDUCT.md)
- Contributing guidelines (CONTRIBUTING.md)

### Changed

- `pool.fund_invoice` is now permissionless (previously
  required admin auth)

### Known Issues

- Issuer release not wired into `fund_invoice` (Issue #56)
- No emergency pause mechanism
- Admin key is a single signer (multi-sig planned)

## Security Researchers

We thank the following researchers for responsibly disclosing
vulnerabilities:

_(No disclosures yet)_
