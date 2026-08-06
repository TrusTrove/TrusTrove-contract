Closes #179

## Summary

Adds `extend_ttl` calls to the three registry read functions that were reading
persistent/instance storage without renewing the TTL of the entries they read.
The threshold (100) and target duration (2_000_000) match the corresponding
write paths exactly.

## Changes

### `get_profile(env, address) -> Profile`
- Reads `DataKey::Profile(address)` from persistent storage
- Calls `persistent().extend_ttl(&key, 100, 2_000_000)` using the same TTL
  policy as `register_issuer`, `register_buyer`, etc.
- Preserves the `RegistryError::NotFound` panic for missing entries

### `is_verified(env, address) -> bool`
- Restructured from `.map().unwrap_or()` to a `match` on `Option<Profile>`
- Calls `persistent().extend_ttl(&key, 100, 2_000_000)` only when the entry
  exists (no extra `.has()` lookup needed)
- Returns `false` for missing entries as before — no behavioral change

### `get_admin(env) -> Address`
- Reads `DataKey::Admin` from instance storage
- Calls `instance().extend_ttl(100, 2_000_000)` matching `initialize()`,
  `transfer_ownership()`, etc.
- Preserves the `RegistryError::NotFound` panic for uninitialized contracts

### Tests
4 new regression tests verify TTL is actually bumped after each read:
- `test_get_profile_extends_ttl` — drains TTL below 100, reads, asserts bump
- `test_is_verified_extends_ttl` — same pattern for `is_verified`
- `test_get_admin_extends_instance_ttl` — same pattern for instance TTL
- `test_is_verified_does_not_extend_ttl_for_unknown` — negative test

## Storage Efficiency
- No redundant reads: each function reads the entry exactly once
- `is_verified` uses the already-fetched `Option` to gate TTL extension
- No write operations added — only TTL extension
