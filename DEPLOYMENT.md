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
| `--help` | Show usage |

### 3. Post-Deploy Wiring

After `deploy.sh` completes, copy the generated contract IDs
into the TrusTrove frontend:

```bash
cat .env.deployed
```

Paste the values into `trusttrove-app/.env.local`.

## Contract Wiring Order

The deploy script initializes contracts in a specific order
because each contract references others:

```
1. registry_contract
   └─ no dependencies

2. invoice_contract
   └─ needs: registry_contract

3. escrow_contract (USDC)
   └─ needs: pool_contract, invoice_contract, USDC asset

4. pool_contract (USDC)
   └─ needs: invoice_contract, escrow_contract, USDC asset

5. escrow_contract (XLM) [EXPERIMENTAL]
   └─ needs: pool_contract, invoice_contract, XLM asset

6. pool_contract (XLM) [EXPERIMENTAL]
   └─ needs: invoice_contract, escrow_contract, XLM asset

7. Wire pool into invoice
   └─ invoice.set_pool_contract(pool_usdc)
```

The registry must be deployed first because all other contracts
call `is_verified()` on it during initialization.

## Agent Registry Wiring

`AGENT_REGISTRY_CONTRACT` in `.env.example` refers to the agent-registry
contract from the separate `underwrite-contract` repo. Wiring it in is a
manual, optional step and is **not** performed by `deploy.sh` or
`deploy.ps1`:

1. Deploy the agent-registry contract from the `underwrite-contract` repo.
2. Set `AGENT_REGISTRY_CONTRACT` in your `.env` to its address.
3. Call `invoice.set_agent_registry_contract` with that address:

   ```bash
   stellar contract invoke \
     --id "$INVOICE_CONTRACT_ID" \
     --source "$DEPLOYER_ACCOUNT" \
     --network "$STELLAR_NETWORK" \
     -- set_agent_registry_contract --contract "$AGENT_REGISTRY_CONTRACT"
   ```

This step is only required if agent-attested invoice submission is used;
skip it otherwise.

## Mainnet Deployment

Mainnet deployment is not yet supported. Before mainnet:

- [ ] Admin key migrated to multi-sig
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
(`get_admin`, `get_counts`, `get_stats`, `get_locked`).

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

When an address rotation occurs, the `README.md` is automatically updated with the latest live testnet addresses during the build/deploy step. Integrators and contributors should:
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
