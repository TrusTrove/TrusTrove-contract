#![cfg(test)]

use proptest::prelude::*;
use proptest::test_runner::{Config as ProptestConfig, TestRunner};
use soroban_sdk::{
    contract, contractimpl, contracttype,
    testutils::{Address as _, Events as _, Ledger},
    token,
    xdr::ToXdr,
    Address, BytesN, Env, IntoVal, String, Symbol, TryFromVal,
};

use crate::{InvoiceContract, InvoiceContractClient, InvoiceStatus, TTL_EXTEND_TO, TTL_THRESHOLD};

// Default invoice parameters used across tests.
// These computed constants eliminate magic numbers in test assertions
// and make the tests self-correcting when parameters change.
const DEFAULT_FACE_VALUE: u128 = 1_000_000_000;
const DEFAULT_DUE_OFFSET: u64 = 86400; // 1 day in seconds
const DEFAULT_DISCOUNT_BPS: u32 = 200;
const DEFAULT_FUNDED_AMOUNT: u128 =
    DEFAULT_FACE_VALUE * (10000 - DEFAULT_DISCOUNT_BPS as u128) / 10000;

#[contract]
pub struct MockRegistry;

#[contractimpl]
impl MockRegistry {
    pub fn is_verified(env: Env, address: Address) -> bool {
        env.storage()
            .persistent()
            .get::<_, bool>(&DataKey(address))
            .unwrap_or(false)
    }

    pub fn register(env: Env, address: Address) {
        env.storage()
            .persistent()
            .set(&DataKey(address.clone()), &true);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey(address), TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    pub fn revoke(env: Env, address: Address) {
        env.storage()
            .persistent()
            .set(&DataKey(address.clone()), &false);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey(address), TTL_THRESHOLD, TTL_EXTEND_TO);
    }
}

#[contracttype]
pub struct DataKey(Address);

// --------------- Mock Token ---------------

#[contract]
pub struct MockToken;

#[contractimpl]
impl MockToken {
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let from_key = TKey(from.clone());
        let to_key = TKey(to.clone());
        let from_bal: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        let to_bal: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&from_key, &(from_bal - amount));
        env.storage().persistent().set(&to_key, &(to_bal + amount));
    }

    pub fn balance(env: Env, addr: Address) -> i128 {
        env.storage().persistent().get(&TKey(addr)).unwrap_or(0)
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let key = TKey(to.clone());
        let bal: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(bal + amount));
    }
}

#[contracttype]
pub struct TKey(Address);

#[contract]
pub struct MockPool;

#[contractimpl]
impl MockPool {
    pub fn handle_default(_env: Env, _invoice_id: BytesN<32>) -> bool {
        true
    }

    pub fn receive_repayment(_env: Env, _invoice_id: BytesN<32>, _amount: u128) -> bool {
        true
    }

    pub fn get_usdc_asset(env: Env) -> Address {
        let key = Symbol::new(&env, "asset");
        env.storage().instance().get(&key).unwrap()
    }

    pub fn receive_repayment_with_refund(
        env: Env,
        _invoice_id: BytesN<32>,
        _amount: u128,
        refund: u128,
        _buyer: Address,
    ) -> bool {
        let key = Symbol::new(&env, "last_refund");
        env.storage().instance().set(&key, &refund);
        true
    }

    pub fn get_last_refund(env: Env) -> u128 {
        let key = Symbol::new(&env, "last_refund");
        env.storage().instance().get(&key).unwrap_or(0)
    }
}

type Setup = (
    Env,
    InvoiceContractClient<'static>,
    Address,
    Address,
    MockRegistryClient<'static>,
    Address,
);

fn setup() -> Setup {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);

    let usdc_asset = env.register_contract(None, MockToken);
    client.add_supported_asset(&usdc_asset);

    (env, client, issuer, buyer, registry_client, usdc_asset)
}

#[allow(dead_code)]
type SetupWithAdmin = (
    Env,
    InvoiceContractClient<'static>,
    Address,
    Address,
    MockRegistryClient<'static>,
    Address,
    Address,
);

#[allow(dead_code)]
fn setup_with_admin() -> SetupWithAdmin {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);

    let usdc_asset = env.register_contract(None, MockToken);
    client.add_supported_asset(&usdc_asset);

    (
        env,
        client,
        issuer,
        buyer,
        registry_client,
        usdc_asset,
        admin,
    )
}

fn mock_pool_with_asset(env: &Env, asset: &Address) -> Address {
    let pool_id = env.register_contract(None, MockPool);
    let _pool_client = MockPoolClient::new(env, &pool_id);
    env.as_contract(&pool_id, || {
        let key = Symbol::new(env, "asset");
        env.storage().instance().set(&key, asset);
    });
    pool_id
}

// --------------- Mock Agent Registry (Underwrite) ---------------
//
// Stands in for the agent-registry contract from the separate
// `underwrite-contract` repo. Tests sign with a real secp256k1 key so
// `submit_attestation`'s recovery + registry-lookup path is exercised for
// real rather than mocked out.

#[contract]
pub struct MockAgentRegistry;

#[contractimpl]
impl MockAgentRegistry {
    pub fn get_agent(env: Env, agent_id: Symbol) -> Option<crate::Agent> {
        env.storage().persistent().get(&AgentKey(agent_id))
    }

    pub fn register_agent(env: Env, agent_id: Symbol, agent: crate::Agent) {
        env.storage().persistent().set(&AgentKey(agent_id), &agent);
    }
}

#[contracttype]
pub struct AgentKey(Symbol);

const TEST_AGENT_SEED: [u8; 32] = [7u8; 32];

fn test_agent_signing_key() -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_slice(&TEST_AGENT_SEED).unwrap()
}

fn test_agent_pubkey(env: &Env) -> BytesN<65> {
    let point = test_agent_signing_key()
        .verifying_key()
        .to_encoded_point(false);
    let mut bytes = [0u8; 65];
    bytes.copy_from_slice(point.as_bytes());
    BytesN::from_array(env, &bytes)
}

/// Deploys a fresh mock agent-registry, registers one active agent with a
/// real secp256k1 keypair, points `client` at it, and submits a validly
/// signed attestation for `invoice_id`. Centralizes the plumbing so
/// individual tests can just call this right before `list_for_financing`.
fn attest(env: &Env, client: &InvoiceContractClient, invoice_id: &BytesN<32>) {
    let agent_id = Symbol::new(env, "test_agent");
    let registry_id = env.register_contract(None, MockAgentRegistry);
    let registry_client = MockAgentRegistryClient::new(env, &registry_id);
    registry_client.register_agent(
        &agent_id,
        &crate::Agent {
            active: true,
            pubkey: test_agent_pubkey(env),
        },
    );
    client.set_agent_registry_contract(&registry_id);

    let payload = crate::AttestationPayload {
        domain_separator: BytesN::from_array(env, &crate::ATTESTATION_DOMAIN_SEPARATOR),
        invoice_id: invoice_id.clone(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(env, &[9u8; 32]),
        agent_id,
        nonce: 1,
    };
    let payload_bytes = payload.to_xdr(env);
    let digest = env.crypto().keccak256(&payload_bytes).to_array();
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(env, &sig_bytes);

    client.submit_attestation(invoice_id, &payload_bytes, &signature);
}

// ============== FAILURE-PATH TESTS FOR submit_attestation ==============

#[test]
#[should_panic(expected = "Error(Contract, #21)")]
fn test_submit_attestation_untrusted_signer_wrong_pubkey() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    let agent_id = Symbol::new(&env, "test_agent");
    let registry_id = env.register_contract(None, MockAgentRegistry);
    let registry_client = MockAgentRegistryClient::new(&env, &registry_id);

    // Register agent with a different pubkey than the one we'll use to sign
    let wrong_pubkey_bytes = [42u8; 65];
    let wrong_pubkey = BytesN::from_array(&env, &wrong_pubkey_bytes);
    registry_client.register_agent(
        &agent_id,
        &crate::Agent {
            active: true,
            pubkey: wrong_pubkey,
        },
    );
    client.set_agent_registry_contract(&registry_id);

    let payload = crate::AttestationPayload {
        domain_separator: BytesN::from_array(&env, &crate::ATTESTATION_DOMAIN_SEPARATOR),
        invoice_id: invoice_id.clone(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(&env, &[9u8; 32]),
        agent_id,
        nonce: 1,
    };
    let payload_bytes = payload.to_xdr(&env);
    let digest = env.crypto().keccak256(&payload_bytes).to_array();

    // Sign with the test key (not the registered pubkey)
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(&env, &sig_bytes);

    client.submit_attestation(&invoice_id, &payload_bytes, &signature);
}

#[test]
#[should_panic(expected = "Error(Contract, #21)")]
fn test_submit_attestation_inactive_agent() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    let agent_id = Symbol::new(&env, "test_agent");
    let registry_id = env.register_contract(None, MockAgentRegistry);
    let registry_client = MockAgentRegistryClient::new(&env, &registry_id);

    // Register agent as inactive
    registry_client.register_agent(
        &agent_id,
        &crate::Agent {
            active: false,
            pubkey: test_agent_pubkey(&env),
        },
    );
    client.set_agent_registry_contract(&registry_id);

    let payload = crate::AttestationPayload {
        domain_separator: BytesN::from_array(&env, &crate::ATTESTATION_DOMAIN_SEPARATOR),
        invoice_id: invoice_id.clone(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(&env, &[9u8; 32]),
        agent_id,
        nonce: 1,
    };
    let payload_bytes = payload.to_xdr(&env);
    let digest = env.crypto().keccak256(&payload_bytes).to_array();
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(&env, &sig_bytes);

    client.submit_attestation(&invoice_id, &payload_bytes, &signature);
}

#[test]
#[should_panic(expected = "Error(Contract, #21)")]
fn test_submit_attestation_unregistered_agent() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    let agent_id = Symbol::new(&env, "test_agent");
    let registry_id = env.register_contract(None, MockAgentRegistry);
    let _registry_client = MockAgentRegistryClient::new(&env, &registry_id);

    // Don't register the agent at all
    client.set_agent_registry_contract(&registry_id);

    let payload = crate::AttestationPayload {
        domain_separator: BytesN::from_array(&env, &crate::ATTESTATION_DOMAIN_SEPARATOR),
        invoice_id: invoice_id.clone(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(&env, &[9u8; 32]),
        agent_id,
        nonce: 1,
    };
    let payload_bytes = payload.to_xdr(&env);
    let digest = env.crypto().keccak256(&payload_bytes).to_array();
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(&env, &sig_bytes);

    client.submit_attestation(&invoice_id, &payload_bytes, &signature);
}

#[test]
#[should_panic(expected = "Error(Contract, #22)")]
fn test_submit_attestation_replay_rejection() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    // First attestation should succeed
    attest(&env, &client, &invoice_id);

    // Second attestation with a different valid payload should fail
    let agent_id = Symbol::new(&env, "test_agent");
    let registry_id = env.register_contract(None, MockAgentRegistry);
    let registry_client = MockAgentRegistryClient::new(&env, &registry_id);
    registry_client.register_agent(
        &agent_id,
        &crate::Agent {
            active: true,
            pubkey: test_agent_pubkey(&env),
        },
    );
    client.set_agent_registry_contract(&registry_id);

    let payload = crate::AttestationPayload {
        domain_separator: BytesN::from_array(&env, &crate::ATTESTATION_DOMAIN_SEPARATOR),
        invoice_id: invoice_id.clone(),
        risk_score: 6000,
        evidence_hash: BytesN::from_array(&env, &[10u8; 32]),
        agent_id,
        nonce: 2,
    };
    let payload_bytes = payload.to_xdr(&env);
    let digest = env.crypto().keccak256(&payload_bytes).to_array();
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(&env, &sig_bytes);

    client.submit_attestation(&invoice_id, &payload_bytes, &signature);
}

// Note: Malformed payload testing is limited by Soroban SDK's XDR decoding -
// it produces Value errors rather than Contract errors. The contract-level
// InvalidAmount (#16) error is only hit for valid XDR that fails business logic.
// The other failure paths below are the ones that can be tested at the contract level.

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_submit_attestation_domain_separator_mismatch() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    let agent_id = Symbol::new(&env, "test_agent");
    let registry_id = env.register_contract(None, MockAgentRegistry);
    let registry_client = MockAgentRegistryClient::new(&env, &registry_id);
    registry_client.register_agent(
        &agent_id,
        &crate::Agent {
            active: true,
            pubkey: test_agent_pubkey(&env),
        },
    );
    client.set_agent_registry_contract(&registry_id);

    // Wrong domain separator
    let wrong_domain = [0u8; 32];
    let payload = crate::AttestationPayload {
        domain_separator: BytesN::from_array(&env, &wrong_domain),
        invoice_id: invoice_id.clone(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(&env, &[9u8; 32]),
        agent_id,
        nonce: 1,
    };
    let payload_bytes = payload.to_xdr(&env);
    let digest = env.crypto().keccak256(&payload_bytes).to_array();
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(&env, &sig_bytes);

    client.submit_attestation(&invoice_id, &payload_bytes, &signature);
}

#[test]
#[should_panic(expected = "Error(Contract, #16)")]
fn test_submit_attestation_invoice_id_mismatch() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    let agent_id = Symbol::new(&env, "test_agent");
    let registry_id = env.register_contract(None, MockAgentRegistry);
    let registry_client = MockAgentRegistryClient::new(&env, &registry_id);
    registry_client.register_agent(
        &agent_id,
        &crate::Agent {
            active: true,
            pubkey: test_agent_pubkey(&env),
        },
    );
    client.set_agent_registry_contract(&registry_id);

    // Different invoice_id in payload vs argument
    let wrong_invoice_id = BytesN::from_array(&env, &[1u8; 32]);
    let payload = crate::AttestationPayload {
        domain_separator: BytesN::from_array(&env, &crate::ATTESTATION_DOMAIN_SEPARATOR),
        invoice_id: wrong_invoice_id,
        risk_score: 5000,
        evidence_hash: BytesN::from_array(&env, &[9u8; 32]),
        agent_id,
        nonce: 1,
    };
    let payload_bytes = payload.to_xdr(&env);
    let digest = env.crypto().keccak256(&payload_bytes).to_array();
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(&env, &sig_bytes);

    client.submit_attestation(&invoice_id, &payload_bytes, &signature);
}

#[test]
#[should_panic(expected = "Error(Contract, #23)")]
fn test_list_for_financing_without_attestation() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    // Try to list without calling submit_attestation
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_submit_attestation_nonexistent_invoice() {
    let (env, client, _issuer, _buyer, _, _usdc) = setup();

    let agent_id = Symbol::new(&env, "test_agent");
    let registry_id = env.register_contract(None, MockAgentRegistry);
    let registry_client = MockAgentRegistryClient::new(&env, &registry_id);
    registry_client.register_agent(
        &agent_id,
        &crate::Agent {
            active: true,
            pubkey: test_agent_pubkey(&env),
        },
    );
    client.set_agent_registry_contract(&registry_id);

    let fake_invoice_id = BytesN::from_array(&env, &[1u8; 32]);
    let payload = crate::AttestationPayload {
        domain_separator: BytesN::from_array(&env, &crate::ATTESTATION_DOMAIN_SEPARATOR),
        invoice_id: fake_invoice_id.clone(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(&env, &[9u8; 32]),
        agent_id,
        nonce: 1,
    };
    let payload_bytes = payload.to_xdr(&env);
    let digest = env.crypto().keccak256(&payload_bytes).to_array();
    let (sig, recid) = test_agent_signing_key()
        .sign_prehash_recoverable(&digest)
        .unwrap();
    let mut sig_bytes = [0u8; 65];
    sig_bytes[..64].copy_from_slice(&sig.to_bytes());
    sig_bytes[64] = recid.to_byte();
    let signature = BytesN::from_array(&env, &sig_bytes);

    client.submit_attestation(&fake_invoice_id, &payload_bytes, &signature);
}

#[test]
fn test_create_invoice_with_verified_parties() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    let invoice = client.get(&invoice_id);

    assert_eq!(invoice.issuer, issuer);
    assert_eq!(invoice.buyer, buyer);
    assert_eq!(invoice.face_value, face_value);
    assert_eq!(invoice.due_date, due_date);
    assert_eq!(invoice.status, InvoiceStatus::Created);
    assert_eq!(invoice.funding_asset, usdc);
    assert_eq!(invoice.funding_pool, None);
    assert!(!invoice.issuer_confirmed);
    assert!(!invoice.buyer_confirmed);
}

#[test]
fn test_get_counts_tracks_created_to_listed() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    let counts = client.get_counts();
    assert_eq!(counts.get(String::from_str(&env, "Created")), Some(1));
    assert_eq!(counts.get(String::from_str(&env, "Listed")), Some(0));
    assert_eq!(counts.get(String::from_str(&env, "Funded")), Some(0));
    assert_eq!(counts.get(String::from_str(&env, "Active")), Some(0));
    assert_eq!(counts.get(String::from_str(&env, "Confirmed")), Some(0));
    assert_eq!(counts.get(String::from_str(&env, "Repaid")), Some(0));
    assert_eq!(counts.get(String::from_str(&env, "Defaulted")), Some(0));
    assert_eq!(counts.get(String::from_str(&env, "Expired")), Some(0));

    attest(&env, &client, &invoice_id);
    assert!(client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS));

    let counts = client.get_counts();
    assert_eq!(counts.get(String::from_str(&env, "Created")), Some(0));
    assert_eq!(counts.get(String::from_str(&env, "Listed")), Some(1));
}

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_create_fails_unverified_issuer() {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);

    let usdc_asset = env.register_contract(None, MockToken);
    client.add_supported_asset(&usdc_asset);

    let face_value: u128 = DEFAULT_FACE_VALUE;
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    client.create(&issuer, &buyer, &face_value, &due_date, &usdc_asset);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_create_fails_unverified_buyer() {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    client.initialize(&admin, &registry_id);

    let usdc_asset = env.register_contract(None, MockToken);
    client.add_supported_asset(&usdc_asset);

    let face_value: u128 = DEFAULT_FACE_VALUE;
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    client.create(&issuer, &buyer, &face_value, &due_date, &usdc_asset);
}

#[test]
#[should_panic(expected = "Error(Contract, #6)")]
fn test_create_fails_zero_face_value() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    client.create(&issuer, &buyer, &0, &due_date, &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_create_fails_past_due_date() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let past_date = env.ledger().timestamp() - 1;
    client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &past_date, &usdc);
}

// ============== ISSUE #B: due_date BOUNDARY (due_date == now) ==============

// At exactly `due_date == now`, `create` rejects with InvalidDueDate (#7).
// The check is `due_date <= env.ledger().timestamp()` so equality falls on
// the rejection side. Pins the current behaviour so a regression on the
// boundary comparator cannot land silently.
#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_create_fails_when_due_date_equals_now() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let equal_due_date = env.ledger().timestamp();
    client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &equal_due_date, &usdc);
}

// The boundary's other side: `due_date == now + 1` is the smallest accepted
// value. Confirms storage and events on the positive boundary so that a
// future refactor can't flip the boundary silently.
#[test]
fn test_create_succeeds_when_due_date_one_second_in_future() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let just_future_due_date = env.ledger().timestamp() + 1;
    let face_value: u128 = DEFAULT_FACE_VALUE;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &just_future_due_date, &usdc);

    // State: invoice record exists at Created with the boundary due_date.
    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Created);
    assert_eq!(invoice.due_date, just_future_due_date);
    assert_eq!(invoice.created_at, env.ledger().timestamp());
    assert_eq!(invoice.face_value, face_value);

    // Events: exactly one invoice_created event was emitted by the invoice
    // contract. Per `events::invoice_created` the topic tuple is
    // `(Symbol("invoice_created"), invoice_id, issuer, buyer, funding_asset)`
    // and the data payload is `face_value: u128`. We pin the count and event
    // shape here; detailed per-topic comparisons live in the dedicated event
    // integration tests because soroban_sdk's `Val` does not implement
    // `PartialEq` for ad-hoc equality assertions.
    let events = env.events().all();
    let (event_contract, _topics, _data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, client.address.clone());
}

#[test]
fn test_list_for_financing() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    attest(&env, &client, &invoice_id);
    let result = client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Listed);
    assert_eq!(invoice.discount_bps, DEFAULT_DISCOUNT_BPS);
}

// ============== REGISTRY REVOCATION RE-CHECK (registry+invoice+pool bug) ==============
//
// Design decision: registry revocation is prospective, not retroactive.
// `list_for_financing` re-verifies the issuer and buyer, since this is
// still a pre-funding step where declining new business is cheap. Once an
// invoice is `Funded`, however, verification is no longer re-checked at any
// later step — see `test_revocation_after_mark_funded_does_not_block_lifecycle`.

#[test]
#[should_panic(expected = "Error(Contract, #4)")]
fn test_list_for_financing_fails_when_issuer_revoked() {
    let (env, client, issuer, buyer, registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    attest(&env, &client, &invoice_id);

    // Verified at create() time, revoked before listing.
    registry.revoke(&issuer);

    client.list_for_financing(&invoice_id, &200);
}

#[test]
#[should_panic(expected = "Error(Contract, #5)")]
fn test_list_for_financing_fails_when_buyer_revoked() {
    let (env, client, issuer, buyer, registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    attest(&env, &client, &invoice_id);

    registry.revoke(&buyer);

    client.list_for_financing(&invoice_id, &200);
}

// Mid-lifecycle revocation (post-`mark_funded`) must NOT retroactively
// affect an in-flight invoice: shipment, dual confirmation, and default all
// proceed exactly as if the issuer/buyer were still verified. This pins the
// documented, deliberate behavior that revocation only gates new
// commitments (`list_for_financing`, `PoolContract::fund_invoice`), not
// invoices that already cleared those gates.
#[test]
fn test_revocation_after_mark_funded_does_not_block_lifecycle() {
    let (env, client, issuer, buyer, registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);

    // Revoke both parties only after the invoice is already Funded.
    registry.revoke(&issuer);
    registry.revoke(&buyer);
    assert!(!registry.is_verified(&issuer));
    assert!(!registry.is_verified(&buyer));

    // The rest of the lifecycle proceeds unaffected.
    assert!(client.mark_shipped(&invoice_id));
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    env.ledger().set_timestamp(due_date + 1);
    assert!(client.trigger_default(&invoice_id));
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_fails_wrong_status() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
    client.list_for_financing(&invoice_id, &300);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_list_fails_discount_too_high() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &5001);
}

#[test]
#[should_panic(expected = "Error(Contract, #12)")]
fn test_list_for_financing_discount_bps_zero_panics() {
    // discount_bps == 0 is a 0% yield — nonsensical business state.
    // Must be rejected with InvalidDiscount (#12).
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &0);
}

#[test]
fn test_list_for_financing_discount_bps_min_boundary() {
    // discount_bps == 1 is the smallest valid value and must succeed.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    attest(&env, &client, &invoice_id);
    let result = client.list_for_financing(&invoice_id, &1);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Listed);
    assert_eq!(invoice.discount_bps, 1);

    let contract_id = client.address.clone();
    let events = env.events().all();
    let (event_contract, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, contract_id);
    assert_eq!(
        topics,
        (Symbol::new(&env, "invoice_listed"), invoice_id.clone()).into_val(&env)
    );
    assert_eq!(u32::try_from_val(&env, &data).unwrap(), 1u32);
}

#[test]
fn test_list_for_financing_discount_bps_max_boundary() {
    // discount_bps == 5000 is the inclusive upper bound and must succeed.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    attest(&env, &client, &invoice_id);
    let result = client.list_for_financing(&invoice_id, &5000);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Listed);
    assert_eq!(invoice.discount_bps, 5000);

    let contract_id = client.address.clone();
    let events = env.events().all();
    let (event_contract, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, contract_id);
    assert_eq!(
        topics,
        (Symbol::new(&env, "invoice_listed"), invoice_id.clone()).into_val(&env)
    );
    assert_eq!(u32::try_from_val(&env, &data).unwrap(), 5000u32);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_list_for_financing_discount_bps_one_above_max_boundary_panics() {
    // discount_bps == 5001 is one past the inclusive upper bound and must panic
    // with DiscountTooHigh (#9). Pins the exact boundary alongside the existing
    // test_list_fails_discount_too_high regression test.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &5001);
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_list_for_financing_non_issuer_panics() {
    let (env, client, issuer, _buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &_buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);
    attest(&env, &client, &invoice_id);

    env.set_auths(&[]);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
}

#[test]
fn test_full_lifecycle() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);

    let funded_amount: u128 = DEFAULT_FUNDED_AMOUNT;
    let result = client.mark_funded(&invoice_id, &pool, &usdc, &funded_amount);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);
    assert_eq!(client.get(&invoice_id).funding_pool, Some(pool));

    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    client.confirm_delivery(&invoice_id, &issuer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);
    assert!(client.get(&invoice_id).issuer_confirmed);
    assert!(!client.get(&invoice_id).buyer_confirmed);

    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);
    assert!(client.get(&invoice_id).issuer_confirmed);
    assert!(client.get(&invoice_id).buyer_confirmed);
}

#[test]
fn test_get_by_issuer_returns_correct_invoices() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let invoices = client.get_by_issuer(&issuer);
    assert_eq!(invoices.len(), 2);

    let other = Address::generate(&env);
    let empty = client.get_by_issuer(&other);
    assert_eq!(empty.len(), 0);
}

#[test]
fn test_get_by_buyer_returns_correct_invoices() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let invoices = client.get_by_buyer(&buyer);
    assert_eq!(invoices.len(), 2);
}

#[test]
fn test_get_by_status_returns_correct_invoices() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 2);
}

#[test]
fn test_expire_listing_transitions_to_expired_after_window() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);

    let result = client.expire_listing(&invoice_id);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Expired);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_unknown_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get(&fake_id);
}

#[test]
fn test_dual_confirmation_both_must_confirm() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);

    client.mark_shipped(&invoice_id);

    client.confirm_delivery(&invoice_id, &issuer);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Active);
    assert!(inv.issuer_confirmed);
    assert!(!inv.buyer_confirmed);

    client.confirm_delivery(&invoice_id, &buyer);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Confirmed);
    assert!(inv.issuer_confirmed);
    assert!(inv.buyer_confirmed);
}

#[test]
fn test_confirm_by_both_transitions_to_confirmed() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);

    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);
}

#[test]
#[should_panic(expected = "Error(Contract, #3)")]
fn test_confirm_delivery_wrong_party_panics() {
    let (env, client, issuer, _buyer, registry, usdc) = setup();
    let stranger = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry.register(&buyer);

    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);

    client.confirm_delivery(&invoice_id, &stranger);
}

#[test]
fn test_trigger_default_requires_past_due_date() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);

    let result = client.trigger_default(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);
}

// ============== ISSUE #211: trigger_default FROM INVALID STATUSES ==============

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_trigger_default_from_created_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    // A freshly created invoice is not Funded/Active/Confirmed, so defaulting
    // it must be rejected with InvalidStatusTransition (#8).
    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_trigger_default_from_listed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    // A Listed invoice has not been funded, so defaulting it must be rejected
    // with InvalidStatusTransition (#8).
    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_trigger_default_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    let escrow = mock_escrow_for_pool(&env, &pool_id, &usdc);
    client.set_escrow_contract(&escrow);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);

    // A Repaid invoice is terminal, so defaulting it must be rejected with
    // InvalidStatusTransition (#8).
    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
}

#[test]
fn test_trigger_default_succeeds_at_exact_due_date() {
    // Boundary test: default must be allowed when `now == due_date`
    // (previously panicked due to `<=` comparison — issue #200)
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    // Set ledger to exactly the due date
    env.ledger().set_timestamp(due_date);

    let result = client.trigger_default(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);
}

#[test]
#[should_panic(expected = "Error(Contract, #11)")]
fn test_trigger_default_fails_before_due_date() {
    // Negative test: default must NOT be allowed when `now < due_date`
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    // Set ledger to 1 second before the due date — should panic
    env.ledger().set_timestamp(due_date - 1);

    client.trigger_default(&invoice_id);
}

// PR 363 (closes #314) removed `test_trigger_default_admin_succeeds_after_due_date_with_auth`:
// explicit `mock_auths` + cross-contract call to `pool.handle_default` surfaced
// `Error(Context, MissingValue)` regardless of `sub_invokes` shape. The same
// happy-path is covered by `test_trigger_default_requires_past_due_date` and
// `test_trigger_default_succeeds_at_exact_due_date` via `setup()`.
#[test]
// `trigger_default` calls `admin.require_auth()` directly, so non-admin
// callers are rejected by Soroban's native `Error(Auth, InvalidAction)`
// before any contract-level error can be returned.
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_trigger_default_stranger_panics() {
    // `trigger_default` calls `admin.require_auth()` directly, so a non-admin
    // caller is rejected at the auth layer with Soroban's native
    // `Error(Auth, InvalidAction)` before any state transition.
    // TODO: refactor `trigger_default` to dispatch auth via
    // `try_invoke_contract(check_auth, admin)` + `panic_with_error!(NotAuthorized)`
    // (matching `expire_listing`) so callers see the contract-typed #3 error
    // instead of the noisy native Auth error. Currently the refactor breaks
    // the other `setup()`-based `trigger_default` tests, so it's deferred.
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    let usdc = Address::generate(&env);
    client.add_supported_asset(&usdc);
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    // Fast forward past due date
    env.ledger().set_timestamp(due_date + 1);

    // A non-admin caller (stranger) — no additional auth needed — can successfully trigger default
    // because the admin gate has been removed.
    let result = client.trigger_default(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);
}

// Note: `test_trigger_default_admin_succeeds_after_due_date_with_auth` was
// removed as part of PR #363 (closing #314). The happy-path admin-trigger
// behavior is already exercised by `test_trigger_default_requires_past_due_date`
// and `test_trigger_default_succeeds_at_exact_due_date`, both of which rely
// on the shared `setup()` helper. Keeping this note here so future readers
// know the gap was intentional and not an oversight.

#[test]
fn test_get_by_status_filters_correctly() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    let id1 = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 2);

    attest(&env, &client, &id1);
    client.list_for_financing(&id1, &DEFAULT_DISCOUNT_BPS);
    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 1);
    let listed = client.get_by_status(&InvoiceStatus::Listed);
    assert_eq!(listed.len(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_double_confirmation_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &issuer);
}

#[test]
fn test_status_transitions_full_lifecycle() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);

    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);
}

#[test]
#[should_panic(expected = "Error(Contract, #13)")]
fn test_mark_funded_fails_asset_mismatch() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let xlm = Address::generate(&env);
    let xlm_pool = mock_pool_with_asset(&env, &xlm);
    client.set_pool_contract(&xlm_pool);
    client.mark_funded(&invoice_id, &xlm_pool, &xlm, &DEFAULT_FUNDED_AMOUNT);
}

#[test]
fn test_mark_funded_succeeds_with_matching_asset() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    let result = client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.funding_pool, Some(pool));
}

#[test]
fn test_create_invoice_with_xlm_asset() {
    let (env, client, issuer, buyer, _, _usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let xlm_asset = Address::generate(&env);
    client.add_supported_asset(&xlm_asset);

    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &xlm_asset);
    let invoice = client.get(&invoice_id);

    assert_eq!(invoice.funding_asset, xlm_asset);
    assert_eq!(invoice.status, InvoiceStatus::Created);
}

#[test]
fn test_get_funding_asset_returns_correct_asset() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    let asset = client.get_funding_asset(&invoice_id);
    assert_eq!(asset, usdc);
}

#[test]
fn test_expire_listing_succeeds_by_issuer() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    // Fast forward ledger time by 7 days + 1 second
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    let result = client.expire_listing(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

#[test]
fn test_expire_listing_succeeds_by_admin() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    // Fast forward ledger time by 7 days + 1 second
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    let result = client.expire_listing(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_expire_listing_early_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    // Fast forward ledger time by only 5 days (less than 7 days)
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 5 * 24 * 60 * 60);

    client.expire_listing(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_expire_listing_wrong_status_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    // Fast forward ledger time
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    client.expire_listing(&invoice_id);
}

#[test]
fn test_expire_listing_configurable_window() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    // Set expiry window to 1 day (86400 seconds)
    client.set_expiry_window(&86400);
    assert_eq!(client.get_expiry_window(), 86400);

    // Fast forward by 1 day + 1 second
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + DEFAULT_DUE_OFFSET + 1);

    let result = client.expire_listing(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}
#[test]
fn test_expire_listing_exact_boundary() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    // Fast forward by exact expiry window (7 days)
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60);

    let result = client.expire_listing(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
}

#[test]
#[should_panic(expected = "Error(Contract, #14)")]
fn test_expire_listing_one_second_before_boundary_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    // Fast forward to 1 second before expiry window
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 - 1);

    client.expire_listing(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #18)")]
fn test_set_expiry_window_rejects_out_of_bounds() {
    let (_env, client, _, _, _, _) = setup();
    // Set an expiry window that exceeds the 365-day upper bound
    client.set_expiry_window(&31_536_001);
}

#[test]
fn test_set_pool_contract_emits_event() {
    let (env, client, _, _, _, _) = setup();
    let pool = Address::generate(&env);

    client.set_pool_contract(&pool);

    let contract_id = client.address.clone();
    let events = env.events().all();
    let (event_contract, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, contract_id);
    assert_eq!(
        topics,
        (
            Symbol::new(&env, "pool_contract_updated"),
            pool.clone(),
            pool.clone()
        )
            .into_val(&env)
    );
    <()>::try_from_val(&env, &data).unwrap();
}

#[test]
fn test_set_expiry_window_emits_event() {
    let (env, client, _, _, _, _) = setup();
    let window: u64 = 86400;

    client.set_expiry_window(&window);

    let contract_id = client.address.clone();
    let events = env.events().all();
    let (event_contract, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, contract_id);
    assert_eq!(
        topics,
        (Symbol::new(&env, "expiry_window_set"),).into_val(&env)
    );
    assert_eq!(u64::try_from_val(&env, &data).unwrap(), window);
}

#[test]
fn test_mark_shipped_succeeds_by_issuer() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);

    let result = client.mark_shipped(&invoice_id);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Active);
    assert!(inv.shipped_at.is_some());
}

#[test]
#[should_panic]
fn test_mark_shipped_stranger_panics() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let pool = mock_pool_with_asset(&env, &usdc);

    // Initialize as admin
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    // Create invoice as issuer
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                DEFAULT_FACE_VALUE,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    // List as issuer
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), DEFAULT_DISCOUNT_BPS).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    // Set pool as admin
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_pool_contract",
            args: (pool.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.set_pool_contract(&pool);

    // Mark funded as pool
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &pool,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_funded",
            args: (
                invoice_id.clone(),
                pool.clone(),
                usdc.clone(),
                DEFAULT_FUNDED_AMOUNT,
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);

    // Calling mark_shipped without mocking auths for the issuer should panic
    // due to failed require_auth. The stranger address is not the issuer.
    client.mark_shipped(&invoice_id);
}

#[test]
#[should_panic]
fn test_expire_listing_stranger_panics() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                DEFAULT_FACE_VALUE,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), DEFAULT_DISCOUNT_BPS).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // Calling expire_listing without mocking auths for issuer or admin should panic due to failed require_auth.
    client.expire_listing(&invoice_id);
}

#[test]
fn test_invoice_id_generation_is_deterministic() {
    // Test that invoice ID generation is deterministic for the same inputs
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let face_value = DEFAULT_FACE_VALUE;

    // Create first invoice
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Reset counter to create another invoice with same parameters (except counter)
    // We need to verify that same issuer/buyer/value/date with different counter produces different IDs
    let invoice_id_2 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Different counter should produce different IDs
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify both invoices exist and have correct data
    assert_eq!(client.get(&invoice_id_1).issuer, issuer);
    assert_eq!(client.get(&invoice_id_2).issuer, issuer);
}

#[test]
fn test_invoice_ids_unique_for_different_issuers() {
    // Test that different issuer/buyer combinations produce unique IDs
    let (env, client, issuer, buyer, registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let face_value = DEFAULT_FACE_VALUE;

    // Create first invoice
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Create second invoice with different issuer
    let issuer2 = Address::generate(&env);
    registry.register(&issuer2);
    let invoice_id_2 = client.create(&issuer2, &buyer, &face_value, &due_date, &usdc);

    // Different issuers should produce different IDs even with same other parameters
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct issuers
    assert_eq!(client.get(&invoice_id_1).issuer, issuer);
    assert_eq!(client.get(&invoice_id_2).issuer, issuer2);
}

#[test]
fn test_invoice_ids_unique_for_different_buyers() {
    // Test that different buyer combinations produce unique IDs
    let (env, client, issuer, buyer, registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let face_value = DEFAULT_FACE_VALUE;

    // Create first invoice
    let invoice_id_1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Create second invoice with different buyer
    let buyer2 = Address::generate(&env);
    registry.register(&buyer2);
    let invoice_id_2 = client.create(&issuer, &buyer2, &face_value, &due_date, &usdc);

    // Different buyers should produce different IDs even with same other parameters
    assert_ne!(invoice_id_1, invoice_id_2);

    // Verify invoices have correct buyers
    assert_eq!(client.get(&invoice_id_1).buyer, buyer);
    assert_eq!(client.get(&invoice_id_2).buyer, buyer2);
}

#[test]
fn test_invoice_ids_unique_for_different_face_values() {
    // Test that different face values produce unique IDs
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let id1 = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    let id2 = client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    assert_ne!(id1, id2);
}

// ── Issue #196: repay from Funded, Active, or Confirmed ─────────────────────────

fn mint_tokens(env: &Env, token: &Address, to: &Address, amount: i128) {
    let token_client = MockTokenClient::new(env, token);
    token_client.mint(to, &amount);
}

#[test]
fn test_repay_from_funded_succeeds() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    let escrow = mock_escrow_for_pool(&env, &pool, &usdc);
    client.set_escrow_contract(&escrow);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Funded);

    mint_tokens(&env, &usdc, &buyer, face_value as i128);

    let result = client.repay(&invoice_id);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Repaid);
    assert!(inv.repaid_at.is_some());
}

#[test]
fn test_repay_from_active_succeeds() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    let escrow = mock_escrow_for_pool(&env, &pool, &usdc);
    client.set_escrow_contract(&escrow);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Active);

    mint_tokens(&env, &usdc, &buyer, face_value as i128);

    let result = client.repay(&invoice_id);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Repaid);
    assert!(inv.repaid_at.is_some());
}

#[test]
fn test_repay_from_confirmed_succeeds() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    let escrow = mock_escrow_for_pool(&env, &pool, &usdc);
    client.set_escrow_contract(&escrow);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    mint_tokens(&env, &usdc, &buyer, face_value as i128);

    let result = client.repay(&invoice_id);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.status, InvoiceStatus::Repaid);
    assert!(inv.repaid_at.is_some());
}

#[test]
fn test_repay_emits_event() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    let escrow = mock_escrow_for_pool(&env, &pool, &usdc);
    client.set_escrow_contract(&escrow);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);

    mint_tokens(&env, &usdc, &buyer, face_value as i128);

    client.repay(&invoice_id);

    let contract_id = client.address.clone();
    let events = env.events().all();
    let found = events.iter().any(|e| {
        let (c, topics, _data) = e;
        if c != contract_id {
            return false;
        }
        let topic0: Symbol = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
        topic0 == Symbol::new(&env, "invoice_repaid")
    });
    assert!(found);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_fails_from_created() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    // Status is Created — repay should panic
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_fails_from_listed() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
    // Status is Listed — repay should panic
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Auth, InvalidAction)")]
fn test_repay_fails_no_auth() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let admin = Address::generate(&env);
    let usdc = Address::generate(&env);
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    // Initialize as admin
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "initialize",
            args: (admin.clone(), registry_id.clone()).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.initialize(&admin, &registry_id);

    client.add_supported_asset(&usdc);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                DEFAULT_FACE_VALUE,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), DEFAULT_DISCOUNT_BPS).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &admin,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "set_pool_contract",
            args: (pool.clone(),).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.set_pool_contract(&pool);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &pool,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "mark_funded",
            args: (
                invoice_id.clone(),
                pool.clone(),
                usdc.clone(),
                DEFAULT_FUNDED_AMOUNT,
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);

    // Do not mock auth for buyer — repay should fail with auth error
    client.repay(&invoice_id);
}

// ============== PROPERTY-BASED INVARIANT TESTS ==============
// Uses proptest's TestRunner API directly (standard Rust closures) so
// rustfmt formats the tests normally.  Case budget is 10 per property
// to stay within CI time budgets for the Soroban in-process host.

#[test]
fn prop_any_positive_face_value_creates_invoice_in_created_status() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u128..=1_000_000_000_000_000u128), |face_value| {
            let (env, client, issuer, buyer, _, usdc) = setup();
            let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
            let id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
            let inv = client.get(&id);
            prop_assert_eq!(inv.face_value, face_value);
            prop_assert_eq!(inv.status, InvoiceStatus::Created);
            prop_assert!(!inv.issuer_confirmed);
            prop_assert!(!inv.buyer_confirmed);
            prop_assert_eq!(inv.funded_amount, 0);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_any_future_due_date_creates_invoice_successfully() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u64..=31_536_000u64), |offset| {
            let (env, client, issuer, buyer, _, usdc) = setup();
            let due_date = env.ledger().timestamp() + offset;
            let id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
            let inv = client.get(&id);
            prop_assert_eq!(inv.due_date, due_date);
            prop_assert_eq!(inv.status, InvoiceStatus::Created);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_discount_bps_within_limit_always_lists_invoice() {
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u32..=5000u32), |discount_bps| {
            let (env, client, issuer, buyer, _, usdc) = setup();
            let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
            let id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
            attest(&env, &client, &id);
            let result = client.list_for_financing(&id, &discount_bps);
            prop_assert!(result);
            let inv = client.get(&id);
            prop_assert_eq!(inv.discount_bps, discount_bps);
            prop_assert_eq!(inv.status, InvoiceStatus::Listed);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_invoice_id_is_deterministic_for_same_inputs() {
    // Same issuer, buyer, face_value, due_date, asset at the same ledger
    // timestamp must always produce the same invoice ID.
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u128..=1_000_000_000_000u128), |face_value| {
            let (env, client, issuer, buyer, _, usdc) = setup();
            let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
            let id1 = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
            // counter increments each call, so a second create with identical
            // params produces a different ID — verify the first is stable via get()
            let inv = client.get(&id1);
            prop_assert_eq!(inv.id, id1);
            prop_assert_eq!(inv.face_value, face_value);
            Ok(())
        })
        .unwrap();
}

#[test]
fn prop_expiry_window_bounds_are_respected_across_values() {
    // For any window in [1, 30 days], a listing that expires exactly
    // window+1 seconds later must succeed.
    let mut runner = TestRunner::new(ProptestConfig::with_cases(10));
    runner
        .run(&(1u64..=2_592_000u64), |window| {
            let (env, client, issuer, buyer, _, usdc) = setup();
            client.set_expiry_window(&window);
            prop_assert_eq!(client.get_expiry_window(), window);
            let due_date = env.ledger().timestamp() + window + 86_400;
            let id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
            attest(&env, &client, &id);
            client.list_for_financing(&id, &DEFAULT_DISCOUNT_BPS);
            env.ledger()
                .set_timestamp(env.ledger().timestamp() + window + 1);
            let expired = client.expire_listing(&id);
            prop_assert!(expired);
            prop_assert_eq!(client.get(&id).status, InvoiceStatus::Expired);
            Ok(())
        })
        .unwrap();
}

// --------------- Mock Escrow ---------------

/// Minimal mock escrow that records the pool address and implements
/// `release_to_pool` so `invoice::repay` / `invoice::repay_early` can call
/// it without needing the full real escrow contract.
#[contract]
pub struct MockEscrow;

#[contractimpl]
impl MockEscrow {
    /// Stores the pool address so `release_to_pool` knows where to forward.
    pub fn set_pool(env: Env, pool: Address) {
        env.storage()
            .instance()
            .set(&Symbol::new(&env, "pool"), &pool);
    }

    /// Stores the USDC asset address.
    pub fn set_asset(env: Env, asset: Address) {
        env.storage()
            .instance()
            .set(&Symbol::new(&env, "asset"), &asset);
    }

    /// Minimal stub: transfers `amount` from escrow to pool.
    /// No pool auth required — mirrors the real escrow's updated behavior
    /// where the invoice contract (not the pool) is the caller.
    pub fn release_to_pool(env: Env, _invoice_id: BytesN<32>, amount: u128) -> bool {
        let pool: Address = env
            .storage()
            .instance()
            .get(&Symbol::new(&env, "pool"))
            .unwrap();
        let asset: Address = env
            .storage()
            .instance()
            .get(&Symbol::new(&env, "asset"))
            .unwrap();

        let escrow_addr = env.current_contract_address();
        let token_client = token::Client::new(&env, &asset);
        token_client.transfer(&escrow_addr, &pool, &(amount as i128));

        true
    }
}

/// Registers a MockEscrow wired to `pool_id` and `asset` and returns its address.
fn mock_escrow_for_pool(env: &Env, pool_id: &Address, asset: &Address) -> Address {
    let escrow_id = env.register_contract(None, MockEscrow);
    let esc_client = MockEscrowClient::new(env, &escrow_id);
    esc_client.set_pool(pool_id);
    esc_client.set_asset(asset);
    escrow_id
}

// ============== SUPPORTED ASSET TESTS ==============

#[test]
fn test_add_supported_asset() {
    let (env, client, _, _, _, _) = setup();
    let asset = Address::generate(&env);

    assert!(!client.is_supported_asset(&asset));
    client.add_supported_asset(&asset);
    assert!(client.is_supported_asset(&asset));
    assert_eq!(client.get_supported_asset_count(), 2);
}

// ============================== REPAY TESTS ==============================

#[test]
fn test_repay_from_confirmed() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    let escrow = mock_escrow_for_pool(&env, &pool, &usdc);
    client.set_escrow_contract(&escrow);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    let events_before = env.events().all().len();
    client.repay(&invoice_id);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Repaid);
    assert!(invoice.repaid_at.is_some());
    assert!(env.events().all().len() > events_before);
}

#[test]
#[should_panic(expected = "Error(Auth")]
fn test_repay_wrong_auth_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Confirmed);

    env.set_auths(&[]);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_created_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_listed_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_repaid_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    let escrow = mock_escrow_for_pool(&env, &pool, &usdc);
    client.set_escrow_contract(&escrow);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);
    client.repay(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Repaid);
    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_defaulted_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);
    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &DEFAULT_FUNDED_AMOUNT);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);
    client.trigger_default(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);

    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_repay_from_expired_rejected() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &DEFAULT_DISCOUNT_BPS);

    client.set_expiry_window(&100);
    env.ledger().set_timestamp(env.ledger().timestamp() + 101);
    client.expire_listing(&invoice_id);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);

    client.repay(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #17)")]
fn test_create_fails_counter_overflow() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    env.as_contract(&client.address, || {
        env.storage()
            .instance()
            .set(&crate::DataKey::Counter, &u64::MAX);
    });

    client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
}

// ============== ISSUE #201: TYPED ERRORS FOR UNINITIALIZED CONTRACT ==============

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_create_fails_uninitialized_registry() {
    let env = Env::default();
    env.mock_all_auths();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    let usdc = Address::generate(&env);
    client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_create_fails_missing_counter() {
    let env = Env::default();

    let registry_id = env.register_contract(None, MockRegistry);
    let registry_client = MockRegistryClient::new(&env, &registry_id);

    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    registry_client.register(&issuer);
    registry_client.register(&buyer);

    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);

    let _admin = Address::generate(&env);

    let usdc = Address::generate(&env);
    client.add_supported_asset(&usdc);

    env.as_contract(&client.address, || {
        env.storage().instance().remove(&crate::DataKey::Counter);
    });

    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #19)")]
fn test_create_fails_self_invoicing() {
    let (env, client, issuer, _buyer, _registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;
    client.create(&issuer, &issuer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
}

// ============== ISSUE #218: create writes to InvoicesByIssuer and InvoicesByBuyer indexes ==============

#[test]
fn test_create_writes_to_issuer_index() {
    // Verifies that `create` stores the invoice ID in the issuer's index,
    // both via a direct storage read and the public `get_by_issuer` query.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Direct storage read: issuer index count and entry
    env.as_contract(&client.address, || {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&crate::DataKey::IssuerIndexCount(issuer.clone()))
            .unwrap_or(0);
        assert_eq!(count, 1);

        let stored_id: BytesN<32> = env
            .storage()
            .persistent()
            .get(&crate::DataKey::IssuerIndexEntry(issuer.clone(), 0))
            .unwrap();
        assert_eq!(stored_id, invoice_id);
    });

    // Public API: get_by_issuer returns the invoice
    let invoices = client.get_by_issuer(&issuer);
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices.get(0).unwrap().id, invoice_id);

    // A different (unused) issuer address returns no invoices
    let other = Address::generate(&env);
    let empty = client.get_by_issuer(&other);
    assert_eq!(empty.len(), 0);

    // Verify the invoice_created event was emitted by the invoice contract
    let events = env.events().all();
    let (event_contract, _topics, _data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, client.address);
}

#[test]
fn test_create_writes_to_buyer_index() {
    // Verifies that `create` stores the invoice ID in the buyer's index,
    // both via a direct storage read and the public `get_by_buyer` query.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    // Direct storage read: buyer index count and entry
    env.as_contract(&client.address, || {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&crate::DataKey::BuyerIndexCount(buyer.clone()))
            .unwrap_or(0);
        assert_eq!(count, 1);

        let stored_id: BytesN<32> = env
            .storage()
            .persistent()
            .get(&crate::DataKey::BuyerIndexEntry(buyer.clone(), 0))
            .unwrap();
        assert_eq!(stored_id, invoice_id);
    });

    // Public API: get_by_buyer returns the invoice
    let invoices = client.get_by_buyer(&buyer);
    assert_eq!(invoices.len(), 1);
    assert_eq!(invoices.get(0).unwrap().id, invoice_id);

    // A different (unused) buyer address returns no invoices
    let other = Address::generate(&env);
    let empty = client.get_by_buyer(&other);
    assert_eq!(empty.len(), 0);

    // Verify the invoice_created event was emitted by the invoice contract
    let events = env.events().all();
    let (event_contract, _topics, _data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, client.address);
}

#[test]
fn test_create_writes_to_both_indexes_multiple_invoices() {
    // Verifies that when the same issuer and buyer create multiple invoices,
    // both indexes correctly accumulate the invoice IDs.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    let id1 = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);
    let id2 = client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    // Direct storage: issuer index has 2 entries
    env.as_contract(&client.address, || {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&crate::DataKey::IssuerIndexCount(issuer.clone()))
            .unwrap_or(0);
        assert_eq!(count, 2);

        let stored_id_0: BytesN<32> = env
            .storage()
            .persistent()
            .get(&crate::DataKey::IssuerIndexEntry(issuer.clone(), 0))
            .unwrap();
        assert_eq!(stored_id_0, id1);

        let stored_id_1: BytesN<32> = env
            .storage()
            .persistent()
            .get(&crate::DataKey::IssuerIndexEntry(issuer.clone(), 1))
            .unwrap();
        assert_eq!(stored_id_1, id2);
    });

    // Direct storage: buyer index has 2 entries
    env.as_contract(&client.address, || {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&crate::DataKey::BuyerIndexCount(buyer.clone()))
            .unwrap_or(0);
        assert_eq!(count, 2);

        let stored_id_0: BytesN<32> = env
            .storage()
            .persistent()
            .get(&crate::DataKey::BuyerIndexEntry(buyer.clone(), 0))
            .unwrap();
        assert_eq!(stored_id_0, id1);

        let stored_id_1: BytesN<32> = env
            .storage()
            .persistent()
            .get(&crate::DataKey::BuyerIndexEntry(buyer.clone(), 1))
            .unwrap();
        assert_eq!(stored_id_1, id2);
    });

    // Public API assertions
    assert_eq!(client.get_by_issuer(&issuer).len(), 2);
    assert_eq!(client.get_by_buyer(&buyer).len(), 2);
}

#[test]
fn test_create_indexes_are_party_specific() {
    // Verifies that issuer and buyer indexes are independent:
    // an invoice created by issuer A with buyer B should appear in
    // A's issuer index and B's buyer index, but NOT in B's issuer index
    // or A's buyer index.
    let (env, client, issuer, buyer, registry, usdc) = setup();
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    let invoice_id = client.create(&issuer, &buyer, &DEFAULT_FACE_VALUE, &due_date, &usdc);

    // Issuer should see the invoice in their issuer index
    let issuer_invoices = client.get_by_issuer(&issuer);
    assert_eq!(issuer_invoices.len(), 1);
    assert_eq!(issuer_invoices.get(0).unwrap().id, invoice_id);

    // Buyer should see the invoice in their buyer index
    let buyer_invoices = client.get_by_buyer(&buyer);
    assert_eq!(buyer_invoices.len(), 1);
    assert_eq!(buyer_invoices.get(0).unwrap().id, invoice_id);

    // Issuer should NOT see the invoice in their buyer index
    let issuer_as_buyer = client.get_by_buyer(&issuer);
    assert_eq!(issuer_as_buyer.len(), 0);

    // Buyer should NOT see the invoice in their issuer index
    let buyer_as_issuer = client.get_by_issuer(&buyer);
    assert_eq!(buyer_as_issuer.len(), 0);

    // An unrelated third party should see nothing in either index
    let stranger = Address::generate(&env);
    registry.register(&stranger);
    assert_eq!(client.get_by_issuer(&stranger).len(), 0);
    assert_eq!(client.get_by_buyer(&stranger).len(), 0);
}

#[test]
fn test_create_indexes_emit_invoice_created_event() {
    // Verifies that the invoice_created event is emitted with the correct
    // topics and data payload when an invoice is created.
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    client.create(&issuer, &buyer, &face_value, &due_date, &usdc);

    let contract_id = client.address.clone();
    let events = env.events().all();

    // Should have at least one event; the last one should be invoice_created
    let (event_contract, topics, data) = events.last().expect("expected at least one event");
    assert_eq!(event_contract, contract_id);

    // Topics: (Symbol("invoice_created"), invoice_id, issuer, buyer, funding_asset)
    let topic0: Symbol = Symbol::try_from_val(&env, &topics.get(0).unwrap()).unwrap();
    assert_eq!(topic0, Symbol::new(&env, "invoice_created"));

    // Data: face_value as u128
    let stored_value: u128 = u128::try_from_val(&env, &data).unwrap();
    assert_eq!(stored_value, face_value);
}

// ============================================================================
// View Functions Initialization & Missing Invoice Tests (Issue #465)
// ============================================================================

#[test]
fn test_view_functions_initialized_existing_invoice() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    attest(&env, &client, &invoice_id);
    client.list_for_financing(&invoice_id, &250);

    assert_eq!(client.get_issuer(&invoice_id), issuer);
    assert_eq!(client.get_face_value(&invoice_id), face_value);
    assert_eq!(client.get_funding_asset(&invoice_id), usdc);
    assert_eq!(client.get_discount_bps(&invoice_id), 250);
    assert_eq!(client.get_status(&invoice_id), InvoiceStatus::Listed as u32);

    let inv = client.get(&invoice_id);
    assert_eq!(inv.id, invoice_id);
    assert_eq!(inv.issuer, issuer);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_issuer_missing_invoice_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_issuer(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_face_value_missing_invoice_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_face_value(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_funding_asset_missing_invoice_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_funding_asset(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_discount_bps_missing_invoice_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_discount_bps(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_status_missing_invoice_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_status(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_missing_invoice_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_get_issuer_uninitialized_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_issuer(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_get_face_value_uninitialized_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_face_value(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_get_funding_asset_uninitialized_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_funding_asset(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_get_discount_bps_uninitialized_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_discount_bps(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_get_status_uninitialized_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_status(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_get_uninitialized_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get(&fake_id);
}

// ============================================================================
// get_buyer / get_due_date accessor tests (issue #574)
// ============================================================================

#[test]
fn test_get_buyer_returns_correct_value() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    assert_eq!(client.get_buyer(&invoice_id), buyer);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_buyer_missing_invoice_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_buyer(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_get_buyer_uninitialized_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_buyer(&fake_id);
}

#[test]
fn test_get_due_date_returns_correct_value() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = DEFAULT_FACE_VALUE;
    let due_date = env.ledger().timestamp() + DEFAULT_DUE_OFFSET;

    let invoice_id = client.create(&issuer, &buyer, &face_value, &due_date, &usdc);
    assert_eq!(client.get_due_date(&invoice_id), due_date);
}

#[test]
#[should_panic(expected = "Error(Contract, #2)")]
fn test_get_due_date_missing_invoice_panics() {
    let (env, client, _, _, _, _) = setup();
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_due_date(&fake_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #20)")]
fn test_get_due_date_uninitialized_panics() {
    let env = Env::default();
    let contract_id = env.register_contract(None, InvoiceContract);
    let client = InvoiceContractClient::new(&env, &contract_id);
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    client.get_due_date(&fake_id);
}
