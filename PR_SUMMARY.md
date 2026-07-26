## Summary

Adds a dedicated cross-contract integration test harness and a CI job that runs it separately from unit tests, closing the coverage gap for pool↔invoice↔escrow CPI paths.

## Changes

- **`tests/integration/`** — New workspace member with full lifecycle tests:
  - Positive path: deposit → create → list → fund → ship → confirm → repay, asserting state and events across all three contracts
  - Default lifecycle: fund → trigger_default → escrow funds returned to pool
  - Negative auth: `receive_repayment`, `handle_default`, `receive_repayment_with_refund` require correct `invoice_contract` auth
  - Rejection paths: asset mismatch, insufficient liquidity, double-funding
  - Multi-LP proportional yield verification

- **`.github/workflows/ci.yml`** — New `integration-tests` job that runs the harness separately

- **`Cargo.toml`** — Added `tests/integration` to workspace members

## Definition of Done

- [x] `tests/integration/` created with cross-contract lifecycle tests
- [x] CI job runs it separately (`integration-tests`)
- [x] Fails on regression (all test assertions will panic on unexpected state)
- [x] Negative auth tests use `mock_auths` / `set_auths` (not `mock_all_auths`)
- [x] Events asserted alongside state
- [x] No `build-and-test` job renamed (branch protection preserved)

Closes #313
