# Agent-Registry Interface (consumed by `invoice::submit_attestation`)

> **Status:** normative spec, interface version **v2** (payload binding +
> expiry, issues #822 / #823; the `AttestationPayload` layout and the
> domain separator are the versioned parts — `get_agent` / `Agent` are
> unchanged from v1).
> **Source of truth:** [`contracts/invoice/src/lib.rs`](../contracts/invoice/src/lib.rs)
> (`submit_attestation`, `ATTESTATION_DOMAIN_SEPARATOR`) and
> [`contracts/invoice/src/types.rs`](../contracts/invoice/src/types.rs)
> (`Agent`, `AttestationPayload`, `Attestation`). If this document and the code
> ever disagree, the code wins — and this document has a bug.

`InvoiceContract::submit_attestation` is the only place TrusTrove depends on
Underwrite's agent-registry contract, which is built and deployed from the
separate `underwrite-contract` repo. TrusTrove has **no crate dependency** on
that repo: the call is a raw `env.invoke_contract`, and the return type is a
hand-maintained local mirror. This document is the contract between the two
repos — anything listed here as **MUST** can't change on either side without
a coordinated upgrade.

---

## 1. Where the dependency lives

| Item | Location |
|------|----------|
| Registry address storage | `DataKey::AgentRegistryContract` (instance storage) in `contracts/invoice/src/types.rs` |
| Setter (admin-only) | `set_agent_registry_contract(agent_registry_contract: Address)` |
| Getter | `get_agent_registry_contract() -> Option<Address>` |
| The cross-contract call | `submit_attestation` in `contracts/invoice/src/lib.rs` |
| Local mirror of the return type | `pub struct Agent` in `contracts/invoice/src/types.rs` |
| Reference mock used in tests | `MockAgentRegistry` in `contracts/invoice/src/test.rs` |
| Deploy-time config | `AGENT_REGISTRY_CONTRACT=` in `.env.example` |

The call made by `submit_attestation` is exactly:

```rust
let mut args = Vec::new(&env);
args.push_back(decoded.agent_id.clone().into_val(&env));
let agent: Option<Agent> =
    env.invoke_contract(&registry, &Symbol::new(&env, "get_agent"), args);
```

---

## 2. Required registry function: `get_agent`

```rust
fn get_agent(env: Env, agent_id: Symbol) -> Option<Agent>
```

| Requirement | Detail |
|-------------|--------|
| **Name** | MUST be exported under the exact function name `get_agent`. |
| **Arguments** | MUST take exactly one argument, `agent_id: Symbol` (an `ScVal::Symbol`, not an `Address` or `String`). It is the `agent_id` field taken verbatim from the signed `AttestationPayload`. |
| **Return** | MUST return `Option<Agent>` — `ScVal::Void` for `None`, or the `Agent` map below for `Some`. |
| **Unknown agent** | SHOULD return `None` rather than panic. `None` is mapped to `InvoiceError::UntrustedSigner` (#21); a panic aborts the whole transaction with a host error instead of a TrusTrove error code. |
| **Auth** | MUST NOT call `require_auth` on anything. The invoice contract invokes it with no authorization entries; submission is deliberately permissionless. |
| **Side effects** | SHOULD be a read-only view. It runs inside the invoice contract's invocation, so its CPU/memory/ledger-read cost is billed to whoever relays `submit_attestation`. |

### 2.1 The `Agent` struct

```rust
#[contracttype]
#[derive(Clone, Debug)]
pub struct Agent {
    pub active: bool,
    pub pubkey: BytesN<65>,
}
```

| Field | Type | Meaning |
|-------|------|---------|
| `active` | `bool` | `false` makes every attestation by this agent fail with `UntrustedSigner`. Use it to suspend an agent without deleting it. |
| `pubkey` | `BytesN<65>` | The agent's secp256k1 public key as a **65-byte uncompressed SEC1 point** (`0x04 ‖ X(32) ‖ Y(32)`). This is the format `env.crypto().secp256k1_recover` returns, and it is compared byte-for-byte. Compressed (33-byte) keys will never match. |

**Encoding constraints (MUST):**

- A `#[contracttype]` struct with named fields encodes as an `ScVal::Map`
  keyed by `ScVal::Symbol` field names. Decoding on the TrusTrove side
  expects **exactly** the keys `active` and `pubkey` — no more, no fewer.
  Adding a field to the registry's `Agent` (e.g. `name`, `created_at`) will
  make the conversion fail and **every** `submit_attestation` call will trap.
  If the registry needs a richer record, it must expose it under a different
  function and keep `get_agent` returning this exact shape.
- Field names are part of the wire format. Renaming `pubkey` → `public_key`
  is a breaking change.

---

## 3. What `submit_attestation` verifies

```rust
pub fn submit_attestation(
    env: Env,
    invoice_id: BytesN<32>,
    payload: Bytes,       // XDR-encoded AttestationPayload
    signature: BytesN<65>,
)
```

### 3.1 `AttestationPayload` (the signed message)

```rust
#[contracttype]
pub struct AttestationPayload {
    pub domain_separator: BytesN<32>,
    pub invoice_id: BytesN<32>,
    pub contract_address: Address,
    pub network_id: BytesN<32>,
    pub risk_score: u32,
    pub evidence_hash: BytesN<32>,
    pub agent_id: Symbol,
    pub nonce: u64,
    pub valid_until: u64,
}
```

| Field | Rule enforced on-chain |
|-------|------------------------|
| `domain_separator` | MUST equal `ATTESTATION_DOMAIN_SEPARATOR` (see §3.2), else `InvalidAmount` (#16). |
| `invoice_id` | MUST equal the `invoice_id` argument, else `InvalidAmount` (#16). |
| `contract_address` | MUST equal `env.current_contract_address()` — the deployed invoice contract this signature is for (issue #822), else `PayloadBindingMismatch` (#29). |
| `network_id` | MUST equal `env.ledger().network_id()` (the 32-byte network identifier of the executing ledger, carried verbatim into the payload by the signer), else `PayloadBindingMismatch` (#29). |
| `risk_score` | Basis points, intended range 0–10000. **Not range-checked on-chain** — stored as-is. |
| `evidence_hash` | Opaque 32-byte hash of the off-chain evidence. Stored as-is. |
| `agent_id` | Passed to `get_agent`. Because it is inside the signed bytes, a relayer cannot swap it. |
| `nonce` | Caller-chosen. **Not tracked on-chain** — the one-attestation-per-invoice rule is the replay guard. |
| `valid_until` | Unix timestamp in **seconds** (ledger timestamp semantics). MUST be `>= env.ledger().timestamp()` at submission, else `AttestationExpired` (#30); equality is valid (strict `>` comparator). Copied onto the stored `Attestation` and re-checked at listing — `list_for_financing` panics with `AttestationExpired` (#30) once `timestamp > valid_until` (issue #823). |

**Why `contract_address` + `network_id` are required (issue #822).**
`invoice_id` is a deterministic hash of (issuer, buyer, face_value,
due_date, counter, asset) and the counter restarts at 0 on every fresh
deployment, so the same inputs produce the *same id* on testnet, mainnet,
or a redeployed contract. Without these fields an attestation signed for
one deployment replays on any other deployment trusting the same agent
key. Signers MUST fill both fields from the target deployment (fetch the
contract's `network_id` from the network descriptor, not from config
guesswork).

**Serialization.** `payload` is the `ToXdr` encoding of the struct: an
`ScVal::Map` with `Symbol` keys in the canonical (sorted) order Soroban
requires for maps. Off-chain signers should produce it with the Soroban SDK
(`payload.to_xdr(&env)`) or an equivalent `ScVal` builder (e.g.
`xdr.ScVal.scvMap(...)` in `@stellar/stellar-sdk`), not by hand-concatenating
fields. The **same raw bytes** are both decoded and hashed, so whatever bytes
are signed must also decode cleanly as an `AttestationPayload`.

### 3.2 Domain separator

```rust
pub const ATTESTATION_DOMAIN_SEPARATOR: [u8; 32] = *b"TrusTrove.InvoiceAttestation.v2_";
```

| Form | Value |
|------|-------|
| ASCII | `TrusTrove.InvoiceAttestation.v2_` (32 bytes, trailing `_` is padding) |
| Hex | `5472757354726f76652e496e766f6963654174746573746174696f6e2e76325f` |

The `v2` suffix is the scheme version. v1 was the pre-#822/#823 layout; it
was retired with the separator bump so signatures over the old six-field
payload can never satisfy the new check (they fail both the separator
comparison and XDR decoding of the added fields). Any further change to the
payload layout or signature scheme must ship with a new separator so old
signatures can't be replayed against the new format.

### 3.3 Signature scheme

| Step | Detail |
|------|--------|
| Digest | `keccak256(payload)` over the raw XDR bytes — **not** SHA-256, and no Ethereum `"\x19Ethereum Signed Message"` prefix. |
| Curve | secp256k1 ECDSA, recoverable. |
| `signature` layout | 65 bytes: `r(32) ‖ s(32) ‖ recovery_id(1)`. |
| Recovery id | Raw `0`/`1` (as produced by `k256`'s `RecoveryId::to_byte()`). Ethereum-style `27`/`28` `v` values MUST be normalised by subtracting 27 before submission. |
| `s` normalisation | Signers should emit low-S signatures (the default in `k256` and most libraries); the Soroban host rejects malformed/non-canonical signatures with a host crypto error. |
| Match | The recovered 65-byte uncompressed key must equal `Agent.pubkey` exactly, and `Agent.active` must be `true`. |

### 3.4 Check order and failure modes

Checks run in this order; the first failure aborts the call.

| # | Check | Failure |
|---|-------|---------|
| 1 | Contract not paused | pause crate's `ContractPaused` error |
| 2 | Invoice `invoice_id` exists | `NotFound` (#2) |
| 3 | No attestation already stored for `invoice_id` | `AlreadyAttested` (#22) |
| 4 | `payload` decodes as `AttestationPayload` | `InvalidAmount` (#16) |
| 5 | `domain_separator` matches | `InvalidAmount` (#16) |
| 6 | `payload.invoice_id == invoice_id` | `InvalidAmount` (#16) |
| 7 | `payload.contract_address == env.current_contract_address()` and `payload.network_id == env.ledger().network_id()` | `PayloadBindingMismatch` (#29) |
| 8 | `env.ledger().timestamp() <= payload.valid_until` | `AttestationExpired` (#30) |
| 9 | Signature recovers to a public key | host crypto error (not a TrusTrove code) |
| 10 | `AgentRegistryContract` is configured | `NotFound` (#2) |
| 11 | `get_agent(agent_id)` call succeeds and decodes as `Option<Agent>` | host error (registry trapped, missing function, or shape mismatch per §2.1) |
| 12 | Result is `Some(agent)` with `agent.active && agent.pubkey == recovered` | `UntrustedSigner` (#21) |

On success the contract stores an `Attestation { agent_id, risk_score,
evidence_hash, submitted_at, valid_until }` under
`DataKey::Attestation(invoice_id)` and emits `attestation_submitted` (see
[EVENTS.md](./EVENTS.md)). That record is what unlocks
`list_for_financing`: without it listing fails with `VerificationRequired`
(#23), and with it stored but past `valid_until` listing fails with
`AttestationExpired` (#30) (issue #823).

---

## 4. Trust model

- **Permissionless relay.** Anyone may call `submit_attestation`; trust comes
  only from the signature recovering to an active registered key.
- **The registry is fully trusted for key lookup.** Whoever controls the
  agent-registry contract (or the invoice admin, who can repoint
  `AgentRegistryContract`) can authorise arbitrary attestation keys. See
  [THREAT_MODEL.md](./THREAT_MODEL.md).
- **Deactivation is not retroactive.** Setting `active = false` blocks future
  attestations only; attestations already stored remain recorded — but they
  stop unlocking listing once `valid_until` passes (issue #823), so the
  damage window of a stale or compromised key is bounded by the validity
  window agents sign into their payloads.
- **Attestations expire.** `valid_until` is chosen by the signing agent at
  underwriting time and enforced at both submission and listing. There is
  no on-chain replace path: `AlreadyAttested` still fires for an invoice
  whose stored attestation has expired, so such an invoice can no longer be
  listed and drains through the normal cancel/expiry lifecycle instead —
  agents SHOULD sign windows that comfortably cover the issuer's intended
  listing schedule.
- **One attestation per invoice.** There is no overwrite or revoke path for an
  attestation, so a compromised key's past attestations must be handled
  off-chain (or by not funding those invoices).

---

## 5. Minimal conforming implementation

The test mock in `contracts/invoice/src/test.rs` is the reference
implementation of this interface:

```rust
#[contract]
pub struct MockAgentRegistry;

#[contractimpl]
impl MockAgentRegistry {
    pub fn get_agent(env: Env, agent_id: Symbol) -> Option<Agent> {
        env.storage().persistent().get(&AgentKey(agent_id))
    }

    pub fn register_agent(env: Env, agent_id: Symbol, agent: Agent) {
        env.storage().persistent().set(&AgentKey(agent_id), &agent);
    }
}
```

A production registry will add admin auth, events and TTL management to the
write path, but `get_agent`'s signature and the `Agent` shape must stay
exactly as above.

---

## 6. Changing this interface

1. Bump the domain separator (`…v2_`) if the payload or signature scheme
   changes.
2. Keep `get_agent` / `Agent` stable; add new functions rather than new
   fields.
3. Update `Agent` in `contracts/invoice/src/types.rs`, the mock in
   `contracts/invoice/src/test.rs`, and this document in the same PR.
4. Coordinate the deploy order with `underwrite-contract`: deploy the new
   registry first, then repoint with `set_agent_registry_contract`.
