# Contributing to TrusTrove Contracts


Thank you for contributing to TrusTrove's Soroban contracts. Keep changes focused,
tested, and tied to an issue.


## Claim an issue


1. Choose an open issue that is not assigned.
2. Comment `.take` (or state that you are working on it).
3. Wait for the issue to show you as the assignee before starting.
4. Ask in the issue if its scope overlaps another ticket.


Issues use complexity labels:

| Label | Expected scope |
| --- | --- |
| `complexity:low` | An isolated function, test, or documentation change |
| `complexity:medium` | Contract logic or storage changes |
| `complexity:high` | Cross-contract behavior or new protocol mechanics |

Contract and type labels further identify the affected area, such as `registry`,
`invoice`, `escrow`, `pool`, `test`, or `documentation`.


## Create a branch


Start from the latest `main` and create one branch per issue:


```bash
git switch main
git pull --ff-only
git switch -c fix/123-short-description
```

Use a descriptive prefix such as `fix/`, `feat/`, `test/`, or `docs/`. Do not mix
unrelated refactors or formatting changes into the issue branch.

## Make and test changes


Prefer the smallest change that satisfies the issue acceptance criteria. Follow the
storage and error conventions documented in the
[README](./README.md#key-conventions).

Run the affected crate first for quick feedback:


```bash
cargo test -p trusttrove-registry
```


Before opening a pull request, run the same checks enforced by CI:


```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo build --workspace
cargo build --workspace --release --target wasm32v1-none
cargo test --workspace
```

Add positive and negative tests where applicable. New public functions must include
rustdoc for arguments, authorization, panics, and return values.

CI also measures line coverage with [cargo-tarpaulin](https://github.com/xd009642/tarpaulin).
It never fails the build on a coverage number, but you can reproduce the report
locally to check that new code is exercised:


```bash
cargo install cargo-tarpaulin@0.32.7 --locked
cargo tarpaulin --workspace --engine llvm --out html --output-dir coverage
```

Open `coverage/tarpaulin-report.html` to see which lines are uncovered. The same
report is published as the `coverage-report` artifact on every CI run.

## Test recipes

The workspace pins Rust in `rust-toolchain.toml`. Install that toolchain (including
the `wasm32v1-none` target and `clippy`/`rustfmt` components) with `rustup`, then
run the suite from the repository root:

```bash
cargo test -p trusttrove-pool   # focused crate while iterating
cargo test --workspace          # everything, including tests/integration
```

Tests live next to each contract in `contracts/*/src/test.rs`; end-to-end flows
live in `tests/integration/tests/`. Use the real contract clients for cross-contract
behavior and reserve stubs for the single call you need to make fail.

### Fixtures and wiring

- `trusttrove_test_utils::setup_full_protocol()` deploys the real registry, invoice,
  escrow, pool, and a `MockToken` funding asset, registers a verified issuer and
  buyer, wires the contracts together, and returns a `FullProtocol` with all
  clients and addresses. Start there for integration-style tests.
- Per-crate `setup()` helpers (for example in `contracts/pool/src/test.rs`) add
  domain defaults such as a funded LP, raised utilization cap, and invoice
  attestation.
- Contract initialization order is invoice → escrow → pool. `pool.initialize`
  cross-checks `escrow.get_usdc_asset()`, so an uninitialized or mismatched
  escrow fails immediately.
- Token balances for `MockToken` can be seeded with `mint`:
  `MockTokenClient::new(&env, &usdc_id).mint(&lp, &1_000_000_000);`.
  Balances are persistent `TKey(address)` entries; reading them through
  `MockTokenClient::balance` keeps tests independent of storage layout.

### Authorization

Most suites start with `env.mock_all_auths_allowing_non_root_auth()` because
contract-to-contract calls (pool → escrow → token) need non-root authorization.
`env.mock_all_auths()` is enough when only the root invocation should be mocked.
Use an explicit entry when the test is about *which* address authorized a call:

```rust
use soroban_sdk::testutils::{MockAuth, MockAuthInvoke};
use soroban_sdk::IntoVal;

env.mock_auths(&[MockAuth {
    address: &admin,
    invoke: &MockAuthInvoke {
        contract: &pool_id,
        fn_name: "set_max_utilization",
        args: (admin.clone(), 9_000u32).into_val(&env),
        sub_invokes: &[],
    },
}]);
```

To assert that a call is rejected, clear the blanket mocks first and use the
`try_` client entry point. This minimal example reuses the full-protocol fixture:

```rust
use soroban_sdk::testutils::Address as _;
use trusttrove_test_utils::setup_full_protocol;

#[test]
fn set_max_utilization_rejects_unauthorized_caller() {
    let te = setup_full_protocol();
    let attacker = Address::generate(&te.env);

    // Drop the blanket mock installed by the fixture so auth is enforced.
    te.env.set_auths(&[]);

    assert!(
        te.pool.try_set_max_utilization(&attacker, &9_000).is_err(),
        "a non-admin caller must not change the utilization cap"
    );
    // Also assert the cap is unchanged with te.pool.get_stats().
}
```

### Ledger controls

Drive time-dependent paths (invoice due dates, escrow timeouts, repayments) with
the test ledger instead of sleeping:

```rust
// Set an absolute starting point when the contract records timestamps.
env.ledger().set_timestamp(1_000);

// Advance past a due date or timeout window.
let due_date = env.ledger().timestamp() + 86_400;
env.ledger().set_timestamp(due_date + 1);
```

Ledger changes are scoped to the `Env`; every test starts from a fresh one.

### Cross-contract example

This exercises the real pool → token path (`deposit` calls the funding asset's
`transfer`) and verifies both sides of the call:

```rust
use trusttrove_test_utils::{setup_full_protocol, MockTokenClient};

#[test]
fn deposit_transfers_funding_asset_from_lp_to_pool() {
    let te = setup_full_protocol();
    let amount = 5_000_000_000u128;
    let usdc = MockTokenClient::new(&te.env, &te.usdc_id);

    let lp_before = usdc.balance(&te.lp);
    let pool_before = usdc.balance(&te.pool_id);

    // Cross-contract call: pool -> token.transfer(lp, pool, amount).
    let shares = te.pool.deposit(&te.lp, &amount);

    assert_eq!(shares, amount);
    assert_eq!(usdc.balance(&te.lp), lp_before - amount as i128);
    assert_eq!(usdc.balance(&te.pool_id), pool_before + amount as i128);
}
```

When asserting atomicity, perform the failing call through a `try_` entry point,
then compare pool totals, instance/persistent keys, and the other contract's
state before and after. Stubs that must fail after an earlier cross-contract
call can record that the earlier call happened in a process static (outside
contract storage) so the observation survives the rollback.

## RUSTSEC advisory exceptions

CI runs `cargo audit` against `audit.toml` in the repo root. If a RUSTSEC
advisory affects a transitive dependency and has no available fix, do not
disable or skip the audit step. Instead add a time-boxed, justified entry to
`audit.toml`:

```toml
[advisories]
ignore = [
    "RUSTSEC-2024-0000", # <why no fix exists> — revisit by <YYYY-MM-DD>
]
```

Each entry must include the advisory ID, a one-line reason there is no fix
available yet, and a revisit date no more than 90 days out. Remove the entry
as soon as a fixed version of the dependency is available.

## Commit changes

Use the conventional format already shown in the README:

```text
feat(registry): add batch issuer registration
fix(pool): reject zero-share deposits
test(invoice): cover the repayment lifecycle
docs(repo): document the review process
```

Keep commits reviewable and avoid generated files unless the issue requires them.

## Open and review a pull request

1. Push the issue branch and open a pull request against `main`.
2. Complete the pull request template and include `Closes #123`.
3. Summarize the behavior change and list the commands used to validate it.
4. Mark unfinished work as a draft.
5. Address review comments with focused follow-up commits.

Maintainers review correctness, test coverage, scope, and compatibility with the
other contracts. A pull request is ready to merge after required approvals and CI
checks pass.
