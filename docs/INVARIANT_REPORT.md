# Invariant report (post-deploy check)

A read-only cross-contract probe operators run after a deploy — or on a
schedule against a running network — to confirm the stack is self-consistent.
Issue #1210.

The entry point is `scripts/invariant_report.sh`. It only ever makes view
calls (`stellar contract invoke --send=no`), so it is safe to run against
mainnet and submits no transactions.

## What it checks

1. **Reciprocal addresses.** `pool.get_escrow_contract` must equal the
   deployed escrow id; the same for `pool.get_invoice_contract`,
   `invoice.get_escrow_contract`, and `invoice.get_agent_registry_contract`.
   A mismatch means one contract was swapped out without re-pointing the
   other — the class of bug that makes `fund_invoice` revert from day one.
2. **Asset consistency.** `pool.get_funding_asset` must equal the configured
   `USDC_ISSUER`, and `invoice.is_supported_asset(USDC)` must be `true`.
3. **Pool counters.** `pool.get_pool_stats` must parse, and
   `total_funded <= total_deposits` — the single invariant that protects
   every solvency calculation downstream.
4. **Representative invoice read.** `escrow.get_locked(sample_invoice)` must
   return a well-formed payload. The sample id defaults to the zero hash
   (an empty slot) so the check works on a brand-new deployment; pass a
   real invoice id with `--sample-invoice` once one exists.

A failure prints the expected and actual values side by side and the script
exits `1`. Missing config or a missing `stellar` CLI exit `2`.

## Local usage

```bash
./scripts/invariant_report.sh                     # defaults to testnet
./scripts/invariant_report.sh --network mainnet
./scripts/invariant_report.sh --sample-invoice $ID
```

Addresses are read from the `.env` file that `./scripts/deploy.sh` writes.
Nothing new is added to storage; every value comes from the existing
`DataKey` entries, so older instances deployed before this script existed
are reported correctly without a migration.

## CI usage

`.github/workflows/invariant-report.yml` wires this script into two
triggers:

- `workflow_dispatch`, so an operator can run the probe against any
  configured network after a deploy; and
- a nightly cron (`17 4 * * *`) against testnet, so a silent regression in
  a referenced contract surfaces in CI rather than on a user's next
  `withdraw`.

Addresses are hydrated into `.env` from repository secrets:
`DEPLOYER_ACCOUNT`, `REGISTRY_CONTRACT_ID`, `INVOICE_CONTRACT_ID`,
`POOL_USDC_CONTRACT_ID`, `POOL_XLM_CONTRACT_ID`, `ESCROW_USDC_CONTRACT_ID`,
`ESCROW_XLM_CONTRACT_ID`, `USDC_ISSUER`.

## Tests

`scripts/test_invariant_report.py` runs under the existing
`unittest discover` CI step and covers:

- successful path with a stubbed `stellar` CLI;
- invalid-input rejection (missing env var, missing `.env`, unknown flag);
- idempotency (two consecutive runs produce identical summaries).

The stub never touches the network, so the unit job needs no secrets and
no CLI install.

## Release procedure

Add an invariant-report run to the post-deploy checklist:

```
1. ./scripts/deploy.sh              # or redeploy whatever changed
2. ./scripts/invariant_report.sh    # must exit 0 before announcing
3. Rotate secrets / publish release notes
```

A nonzero exit is a hard block — do not announce a release until the
report comes back clean.
