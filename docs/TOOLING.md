# Developer & Release Tooling

This document covers the typed client binding generator, testnet ABI
compatibility checker, and release checklist verifier introduced for issues
#1205, #1206, and #1212.

All three tools are **read-only** with respect to on-chain state and preserve
existing `DataKey` storage decoding across all contracts — no contract storage
layouts, discriminants, or serialization formats are altered.

---

## 1. TypeScript Client Bindings (`#1205`)

`scripts/generate_ts_bindings.py` extracts the `contractspecv0` custom section
from each compiled Soroban WASM artifact under `target/wasm32v1-none/release/`
and generates strongly typed TypeScript interfaces, enums, tagged unions, error
codes, and client method signatures under [`bindings/ts/`](../bindings/ts/index.ts):

- `bindings/ts/registry.ts` (`RegistryClient`, `RegistryError`, `Profile`, `Role`, `VerificationStatus`)
- `bindings/ts/invoice.ts` (`InvoiceClient`, `InvoiceError`, `Invoice`, `InvoiceStatus`, `Attestation`, `FundingContext`)
- `bindings/ts/escrow.ts` (`EscrowClient`, `EscrowError`, `EscrowRecord`, `EscrowEvent`, `EscrowAction`)
- `bindings/ts/pool.ts` (`PoolClient`, `PoolError`, `PoolStats`, `LPPosition`, `PoolConfig`)
- `bindings/ts/pool_factory.ts` (`PoolFactoryClient`, `PoolFactoryError`, `PoolStats`)
- `bindings/ts/index.ts` (barrel re-export of all contract namespaces and `ContractName`)

### Local Usage

```bash
# 1. Build release WASMs
cargo build --workspace --release --target wasm32v1-none

# 2. Regenerate committed TypeScript bindings from compiled WASMs
python3 scripts/generate_ts_bindings.py --write

# 3. Verify bindings are up to date (exit 1 if stale)
python3 scripts/generate_ts_bindings.py --check
```

When Rust/WASM artifacts are not built locally, pass `--from-source` to parse
the Rust contract declarations directly.

### CI Enforcement

`.github/workflows/ci.yml` runs `python3 scripts/generate_ts_bindings.py --check`
immediately after `cargo build --workspace --release --target wasm32v1-none`,
failing the build if any contract signature change was not reflected in
`bindings/ts/`.

---

## 2. Testnet ABI Compatibility Check (`#1206`)

`scripts/check_abi_compat.py` performs a read-only comparison between local
contract specs (`target/wasm32v1-none/release/*.wasm` or `--from-source`) and
live deployed Stellar contracts.

### What It Checks

- **Incompatible changes (exit `1`):**
  - Removed or renamed public entry points (`removed_function`)
  - Parameter count changes (`arity_changed`)
  - Parameter type changes (`param_type_changed`)
  - Return type changes (`return_type_changed`)
  - Struct field additions/removals/type changes (`struct_field_*`, which break XDR decoding)
  - Union variant or enum discriminant changes (`union_*`, `enum_*`)
- **Compatible additions (exit `0`, informational):**
  - Newly added public entry points (`added_function`)

### Local Usage

```bash
# Compare compiled WASMs against canonical testnet addresses in README.md via Soroban RPC
python3 scripts/check_abi_compat.py --network testnet --mode rpc

# Advisory report only (prints findings, exits 0)
python3 scripts/check_abi_compat.py --report-only --summary-file abi-report.md

# Compare using stellar CLI or offline baseline WASMs
python3 scripts/check_abi_compat.py --mode stellar
python3 scripts/check_abi_compat.py --remote-wasm-dir path/to/baseline-wasms
```

### CI & Scheduled Workflow

`.github/workflows/abi-compat.yml` runs:
- `unit-tests` on every push/PR (`scripts/test_check_abi_compat.py`);
- `testnet-abi-check` on a nightly schedule (`23 5 * * *`), manual
  `workflow_dispatch`, and contract/tooling PRs (advisory `--report-only` on PRs
  so step summaries surface drift without blocking PRs while testnet awaits a
  scheduled rotation; strict exit `1` on schedule/dispatch).

---

## 3. Release Checklist Verifier (`#1212`)

`scripts/verify_release.py` is a read-only pre-release and post-deploy gate
that validates:

1. **Manifest completeness** — `version`, `network`, `network_passphrase`,
   `contracts`, `assets`, and `wasm_hashes` are all present and well-formed.
2. **WASM artifact integrity & SHA-256 hashes** — every release `.wasm` exists,
   has a valid WebAssembly header and `contractspecv0` section, and matches
   `manifest.wasm_hashes`.
3. **Target network consistency** — `network`, `network_passphrase`, `rpc_url`,
   and `.env` (`STELLAR_NETWORK`, `STELLAR_NETWORK_PASSPHRASE`, `STELLAR_RPC_URL`,
   `HORIZON_URL`) all agree and contain no cross-network URL contamination.
4. **Configured contract & asset references** — every required and optional
   contract ID is a valid `C...` Stellar StrKey (CRC16-XModem verified),
   addresses are pairwise distinct, `.env` / `.env.deployed` / `deployments.json`
   match the manifest, and optional `--check-onchain` verifies on-chain WASM
   hashes and reciprocal wiring (`pool ↔ invoice ↔ escrow`).

### Local Usage

```bash
# Generate a release manifest from compiled WASMs + .env + deployments.json
python3 scripts/verify_release.py \
  --generate-manifest release-manifest.json \
  --network testnet \
  --env-file .env \
  --deployments-json deployments.json

# Validate manifest, WASM hashes, target network, and local env files
python3 scripts/verify_release.py \
  --manifest release-manifest.json \
  --network testnet \
  --env-file .env \
  --env-deployed .env.deployed \
  --deployments-json deployments.json

# Also verify on-chain WASM hashes and reciprocal wiring (read-only)
python3 scripts/verify_release.py \
  --manifest release-manifest.json \
  --network testnet \
  --env-file .env \
  --check-onchain

# Verify compiled release WASM artifacts standalone (used in CI)
python3 scripts/verify_release.py --check-wasm-only
```

### Exit Codes

| Code | Meaning |
|---|---|
| `0` | All checks passed |
| `1` | One or more validation checks failed |
| `2` | Missing file, malformed JSON, or invalid CLI arguments |

---

## 4. Deployed Code Hash Preflight (`#1299`)

`scripts/check_deployed_hashes.py` is a read-only gate that runs from the
deploy scripts before any reference-setting call. It fetches each deployed
contract's WASM hash from Soroban RPC and compares it with the intended
release: the `wasm_hashes` map of a `--manifest`, or the local
`target/wasm32v1-none/release/*.wasm` artifacts when no manifest is given. Any
mismatch, missing artifact, or unreadable contract makes the script exit `1`,
so the deploy script aborts before wiring a contract ID that is running
unexpected code.

```bash
# Compare deployed IDs with an audited release manifest
python3 scripts/check_deployed_hashes.py \
  --manifest release-manifest.json \
  --network testnet \
  --contract registry=$REGISTRY_ID \
  --contract invoice=$INVOICE_ID \
  --contract escrow_usdc=$ESCROW_USDC_ID \
  --contract pool_usdc=$POOL_USDC_ID

# Compare against the locally built WASM instead
python3 scripts/check_deployed_hashes.py \
  --wasm-dir target/wasm32v1-none/release \
  --network testnet \
  --contract pool_usdc=$POOL_USDC_ID
```

Set `RELEASE_MANIFEST=/path/to/release-manifest.json` when running
`scripts/deploy.sh` or `scripts/deploy.ps1` to pin the preflight to the audited
release hashes.

---

## Unit Tests

Run all tooling unit tests without chain access or `stellar` CLI:

```bash
python3 -m unittest discover -s scripts -p 'test_generate_ts_bindings.py' -v
python3 -m unittest discover -s scripts -p 'test_check_abi_compat.py' -v
python3 -m unittest discover -s scripts -p 'test_verify_release.py' -v
python3 -m unittest discover -s scripts -p 'test_check_deployed_hashes.py' -v
```
