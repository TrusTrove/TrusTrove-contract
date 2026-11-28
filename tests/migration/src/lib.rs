#![no_std]

//! Storage-migration fixture for contract upgrades (#1207).
//!
//! The fixture lives in `tests/storage_migration.rs`. It deploys the
//! **baseline** Wasm (the build currently on-chain), writes representative
//! registry, invoice, escrow and pool state through it, upgrades each contract
//! to the **candidate** Wasm with the admin `upgrade` entry point, and checks
//! that every persistent entry still decodes to the same value and stays
//! writable. Run it with `scripts/migration_fixture.sh`; see
//! `DEPLOYMENT.md` ("Pre-upgrade storage migration check").
