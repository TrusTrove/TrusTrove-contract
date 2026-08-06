# Changelog

All notable changes to this project will be documented in this file.


The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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

We感谢 the following researchers for responsibly disclosing
vulnerabilities:

_(No disclosures yet)_
