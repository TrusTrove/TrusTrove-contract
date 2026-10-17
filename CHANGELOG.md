# Changelog

All notable changes to this project will be documented in this file.


The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

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
  to `initialize` and stored per instance, so a pool funding an asset with
  other than 7 decimals reports the precision its shares actually have
  (#757)
- `pool`: `allowance_approved` event emitted on every successful
  `approve` (#756)
- `pool`: `get_funded_amount(invoice_id)` read-only view returning
  `Option<u128>` for the pool's active funding of an invoice: `Some(amount)`
  while funded, `None` before funding and after repayment/default (#839)
- `pool`: `protocol_fee_updated` event assertion coverage for
  `set_protocol_fee` (#767)
- Deployment scripts now pass the full `pool.initialize` argument list
  (`funding_asset`, `registry_contract`, `treasury`, `min_initial_deposit`,
  `share_name`, `share_symbol`, `share_decimals`)

### Changed

- `pool.initialize` accepts three new arguments: `share_name`, `share_symbol`
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
