# Deployment Guide

This document covers prerequisites, deployment flow, contract
wiring order, verification, and rollback for the TrusTrove
smart contracts on Stellar.

## Prerequisites

| Tool | Version | Install |
|------|---------|---------|
| Rust | 1.85.0 | `rustup toolchain install 1.85.0` |
| wasm32v1-none target | — | `rustup target add wasm32v1-none --toolchain 1.85.0` |
| Stellar CLI | latest | See [Stellar CLI docs](https://github.com/stellar/stellar-cli) |
| jq | latest | Required by `scripts/maintainer/update-readme-addresses.sh`, run automatically at the end of `deploy.sh`. See [jq docs](https://jqlang.github.io/jq/download/) |

The repo ships `rust-toolchain.toml` pinning channel `1.85.0` and
target `wasm32v1-none`. `rustup` picks it up automatically.

### Environment Variables

Copy `.env.example` to `.env` and fill in:

| Variable | Description |
|----------|-------------|
| `DEPLOYER_ACCOUNT` | Key alias for the deployer (default: `deployer`) |
| `USDC_ISSUER` | Stellar address of the USDC issuer on testnet |
| `XLM_ASSET` | XLM asset contract address (or native) - EXPERIMENTAL/INCOMPLETE |

## Testnet Deployment

### 1. Create and Fund Deployer

```bash
bash scripts/setup-testnet.sh
```

This creates a key named `deployer` and funds it via Friendbot.
Wait ~10 seconds after running before proceeding.

### 2. Deploy Contracts

```bash
bash scripts/deploy.sh
```

The script is **idempotent** — re-running skips already-deployed
contracts. Deployed addresses are saved to `.deployed-addresses`.

#### Flags

| Flag | Behavior |
|------|----------|
| *(none)* | Resume mode — skips already-deployed contracts |
| `--fresh` | Ignore saved addresses and redeploy everything |
| `--resume` | Explicit resume (same as default) |
| `--dry-run` | Print what would be deployed without executing |
| `--only <contract>` | Redeploy a single contract (hotfix mode) |
| `--help` | Show usage |

#### Single-Contract Hotfix Redeploys

To hotfix a single contract without rerunning the full pipeline, pass
`--only <contract>` where `<contract>` is one of:
`registry`, `invoice`, `escrow_usdc`, `pool_usdc`, `escrow_xlm`, `pool_xlm`.

```bash
# Redeploy only the invoice contract, keeping all other addresses intact:
bash scripts/deploy.sh --only invoice
```

When `--only` is passed, the script:
1. Clears only the saved address and init flag for the targeted contract.
2. Loads all other saved addresses from `.deployed-addresses` for cross-contract references.
3. Runs `deploy_contract` + `invoke_init` only for the specified contract.
4. Generates `deployments.json` and updates `README.md` at the end (same as a full deploy).

### 3. Post-Deploy Wiring

After `deploy.sh` completes, copy the generated contract IDs
into the TrusTrove frontend:

```bash
cat .env.deployed
```

Paste the values into `trusttrove-app/.env.local`.

## Contract Wiring Order

The deploy script initializes contracts in a specific order
because each contract references others. Below is a detailed wiring table.

### Wiring Table

| Step | Setter / Initializer | Source Address | Destination Contract | Required Order |
|------|----------------------|----------------|----------------------|----------------|
| 1 | `registry.initialize()` | Deployer admin | — | First — no dependencies |
| 2 | `invoice.initialize()` | Deployer admin | Registry contract | After registry is live |
| 3 | `escrow_usdc.initialize()` | Deployer admin | Pool contract, USDC asset | After pool is deployed |
| 4 | `pool_usdc.initialize()` | Deployer admin | Invoice contract, Escrow contract, USDC asset | After invoice + escrow are live |
| 5 | `escrow_usdc.set_invoice_contract()` (optional, #997) | Escrow admin | — | After escrow + pool are live (defense-in-depth pin) |
| 6 | `invoice.set_pool_contract()` | Invoice admin | Pool contract | After pool is initialized |
| 7 | `invoice.set_escrow_contract()` | Invoice admin | Escrow contract | After escrow is initialized (prevents `InvoiceError::NotFound`) |
| 8 | `invoice.add_supported_asset(usdc)` | Invoice admin | — | After escrow is wired (otherwise `create` rejects with `UnsupportedAsset`) |
| 9 | `invoice.add_supported_asset(xlm)` (optional) | Invoice admin | — | After escrow is wired (EXPERIMENTAL) |
| 10 | `invoice.set_agent_registry_contract()` (optional) | Invoice admin | Agent registry contract | Only when `AGENT_REGISTRY_CONTRACT` is set (see AGENT_REGISTRY_WIRING) |

The registry must be deployed first because all other contracts
call `is_verified()` on it during initialization.

### Post-Deploy Verification

After `deploy.sh` completes, verify all stored references are correctly
wired with the following read-only calls:

```bash
# Verify registry is initialized and responsive
stellar contract invoke --id $REGISTRY_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- get_admin

# Verify invoice contract references
stellar contract invoke --id $INVOICE_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- get_escrow_contract
stellar contract invoke --id $INVOICE_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- get_agent_registry_contract
stellar contract invoke --id $INVOICE_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- is_supported_asset $USDC_ISSUER

# Verify pool contract references
stellar contract invoke --id $POOL_USDC_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- get_invoice_contract
stellar contract invoke --id $POOL_USDC_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- get_escrow_contract
stellar contract invoke --id $POOL_USDC_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- get_funding_asset

# Verify escrow contract references
stellar contract invoke --id $ESCROW_USDC_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- get_pool_contract
stellar contract invoke --id $ESCROW_USDC_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- get_usdc_asset
stellar contract invoke --id $ESCROW_USDC_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- get_invoice_contract

# Verify timeout window is set (or defaults to 30 days)
stellar contract invoke --id $ESCROW_USDC_ID \
  --source $DEPLOYER_ACCOUNT --network testnet \
  -- get_timeout_window

# Check .env.deployed was generated correctly
cat .env.deployed
```

The built-in verification script also checks these references:
```bash
bash scripts/verify.sh
```

Or on Windows (PowerShell):
```powershell
powershell ./scripts/verify.ps1
```

This checks each contract responds to a read-only query
(`get_admin`, `get_counts`, `get_stats`, `get_locked`) and that the
invoice contract's post-deploy wiring is in place
(`get_escrow_contract`, `get_agent_registry_contract`,
`is_supported_asset`).

### Contract Address Lifecycle & Rotation Policy

(...) (rest of the existing section unchanged)

## Multi-sig Admin

Each protocol contract with an `admin` parameter accepts a Stellar account
address as its admin. A classic Stellar account can require multiple signers
for authorization by configuring its signer weights and thresholds; Soroban
then checks the account's configured threshold when the admin authorizes a
contract call. Configure this before
deploying contracts and pass the multisig account's `G...` address as `admin`
to each `initialize()` call. Keep the individual signer secret keys separate.
The bundled deployment scripts currently set admin to the deployer's address;
for a multisig deployment, initialize each contract with the multisig address
and submit those initialization transactions with the multisig threshold met.

### Testnet Example: 3-of-5

Create and fund five testnet signer accounts, then use Stellar Laboratory's
Testnet transaction builder to submit `Set Options` operations from the admin
account with these settings:

| Signer | Weight |
|--------|--------|
| Signer 1 | 1 |
| Signer 2 | 1 |
| Signer 3 | 1 |
| Signer 4 | 1 |
| Signer 5 | 1 |

Set the low, medium, and high thresholds to `3`. Each `Set Options` transaction
must be authorized by the account's current threshold. Set the account's master
signer weight to `0` so its own key does not count as one of the five signers.
The initial signer/threshold update must still be authorized using the
account's current settings. After setup,
record the admin account's `G...` address and use it as the admin for each
contract initialization. When invoking manually or adapting deployment
automation, pass this address as `--admin` instead of the deployer address and
submit the initialization transaction with the configured threshold. Subsequent
admin-only invocations, including contract upgrades, must likewise carry
authorization satisfying the 3-of-5 threshold.

## Agent Registry Wiring

`AGENT_REGISTRY_CONTRACT` in `.env.example` refers to the agent-registry
contract from the separate `underwrite-contract` repo. `deploy.sh` and
`deploy.ps1` wire it automatically **when** `AGENT_REGISTRY_CONTRACT` is
set in `.env`; otherwise the step is skipped with a clear warning.

To wire it manually (or re-wire it after rotating the agent registry):

1. Deploy the agent-registry contract from the `underwrite-contract` repo.
2. Set `AGENT_REGISTRY_CONTRACT` in your `.env` to its address.
3. Call `invoice.set_agent_registry_contract` with that address:

   ```bash
   stellar contract invoke \
     --id "$INVOICE_CONTRACT_ID" \
     --source "$DEPLOYER_ACCOUNT" \
     --network "$STELLAR_NETWORK" \
     -- set_agent_registry_contract \
        --agent_registry_contract "$AGENT_REGISTRY_CONTRACT"
   ```

This step is only required if agent-attested invoice submission is used;
skip it otherwise.

## Mainnet Deployment

Mainnet deployment is not yet supported. Before mainnet:

- [ ] Admin configured as a multi-sig account (see [Multi-sig Admin](#multi-sig-admin))
- [ ] Emergency pause mechanism implemented
- [ ] Security audit completed
- [ ] Issuer release wiring (Issue #56) resolved

When mainnet support is added, the same `deploy.sh` script
will work with `--network public` by updating `.env`.

## Contract Verification

After deployment, verify all contracts are live:

```bash
bash scripts/verify.sh
```

Or on Windows (PowerShell):

```powershell
powershell ./scripts/verify.ps1
```

This checks each contract responds to a read-only query
(`get_admin`, `get_counts`, `get_stats`, `get_locked`) and that the
invoice contract's post-deploy wiring is in place
(`get_escrow_contract`, `get_agent_registry_contract`,
`is_supported_asset`).

You can also verify on [Stellar Expert Testnet](https://stellar.expert/explorer/testnet).

## Contract Address Lifecycle & Rotation Policy

Testnet contract addresses are ephemeral and may be rotated at any time during development. When rotated, old addresses will drift silently and are no longer maintained.

### Rotation Conditions

Deployed addresses are saved in `.deployed-addresses`:

```
registry=<CONTRACT_ID>
invoice=<CONTRACT_ID>
escrow_usdc=<CONTRACT_ID>
pool_usdc=<CONTRACT_ID>
escrow_xlm=<CONTRACT_ID> (EXPERIMENTAL)
pool_xlm=<CONTRACT_ID> (EXPERIMENTAL)
```

- **Default mode** (resume): Reuses addresses found in `.deployed-addresses` and skips already-deployed contracts.
- **Fresh mode** (`--fresh`): Removes `.deployed-addresses` and starts clean, forcing a complete rotation of all contract addresses on-chain.

### Integrator Expectations

The automated update of `README.md` with live testnet addresses only occurs when the full `deploy.sh` pipeline is run to completion by an operator with valid deployer credentials and an active Stellar CLI session. It relies on the local, gitignored `deployments.json` and does not run automatically on every address change or in CI.

> [!NOTE]
> **Single-Contract Hotfix Redeploys:** Use `bash scripts/deploy.sh --only <contract>` (e.g. `--only invoice`) to redeploy a single contract without re-running the full pipeline. The script will automatically update `deployments.json` and `README.md` with the new address. See the [Single-Contract Hotfix](#single-contract-hotfix-redeploys) section under **Flags** above for full details.

Integrators and contributors should:
1. Treat testnet addresses as volatile.
2. Regularly pull the latest changes from the `main` branch to synchronize with the current testnet environment.
3. Check `README.md` for the current canonical testnet addresses rather than hardcoding them in local environments.

## Rollback

There is no automated rollback. To redeploy from scratch:

```bash
bash scripts/deploy.sh --fresh
```

This ignores all saved addresses and deploys fresh contracts.
The old contract addresses will still exist on-chain but are
no longer referenced by the application.

To manually invalidate old deployments:
1. Revoke all verified issuers/buyers from the old registry
2. Pause LP deposits on old pool contracts (when pause is implemented)
3. Deploy new contracts with `--fresh`
4. Update `trustrove-app/.env.local` with new addresses
5. Notify LPs to withdraw from old pools and deposit into new ones

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `stellar CLI not found` | Install Stellar CLI or add to PATH |
| `DEPLOYER_ACCOUNT not set` | Copy `.env.example` to `.env` and fill in values |
| Deploy hangs waiting for contract | Check network connection; testnet may be slow |
| `400` from Friendbot | Account likely already funded — this is normal |
| Contract not found on verify | Wait longer after deploy; re-run `verify.sh` |

## Storage TTL and Index Maintenance

All invoice storage follows the workspace TTL policy
(`trusttrove-ttl`: `THRESHOLD = 500_000`, `EXTEND_TO = 2_000_000` ledgers,
roughly 115 days). Since issue #832 the paginated views
`get_by_issuer`, `get_by_buyer` and `get_by_status` re-bump every persistent
entry they read — the `*IndexCount` keys, the `IssuerIndexEntry` /
`BuyerIndexEntry` / `StatusIndexEntry` rows and the hydrated `Invoice`
records — so any index that is still actively queried keeps itself alive,
including invoices in terminal statuses (`Repaid`, `Cancelled`, `Defaulted`,
`Expired`) that never receive another state transition.

Entries that are *never* read still expire at their write-time TTL and get
archived; a subsequent read then fails (the transaction aborts on the real
network). Two keeper options prevent or recover from that:

1. **Poll-based keep-alive (recommended, no privileged access):** a keeper
   periodically calls `get_by_issuer` / `get_by_buyer` / `get_by_status` for
   the indexes it cares about, paging within `MAX_PAGE_SIZE`. Every read
   re-bumps the touched entries.
2. **Restore after archival:** fetch the archived entries' footprints from
   history and restore them before the next read:

   ```bash
   stellar contract restore \
     --contract <CONTRACT_ID> \
     --restore-footprint <FOOTPRINT_LEDGER_KEY_FILE> \
     --source <KEEPER_ACCOUNT> --network <NETWORK>
   ```

   `stellar ledger-entry read <ledger-key-key>` (or an RPC
   `getLedgerEntries` call) can be used to confirm an entry is missing
   before restoring.
