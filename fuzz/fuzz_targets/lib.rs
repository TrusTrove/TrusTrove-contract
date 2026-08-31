//! Shared fuzzing utilities for TrusTrove contracts

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    xdr::ToXdr,
    Address, BytesN, Env, Symbol,
};
use trusttrove_escrow::{EscrowContract, EscrowContractClient};
use trusttrove_invoice::{
    AttestationPayload, ATTESTATION_DOMAIN_SEPARATOR, Agent, InvoiceContract, InvoiceContractClient,
};
use trusttrove_pool::{PoolContract, PoolContractClient};
use trusttrove_registry::{RegistryContract, RegistryContractClient};
use k256::ecdsa::SigningKey;

/// Mock token for testing
#[soroban_sdk::contract]
pub struct MockToken;

#[soroban_sdk::contractimpl]
impl MockToken {
    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        let from_key = BalanceKey(from.clone());
        let to_key = BalanceKey(to.clone());
        let from_bal: i128 = env.storage().persistent().get(&from_key).unwrap_or(0);
        let to_bal: i128 = env.storage().persistent().get(&to_key).unwrap_or(0);
        env.storage().persistent().set(&from_key, &(from_bal - amount));
        env.storage().persistent().set(&to_key, &(to_bal + amount));
    }

    pub fn balance(env: Env, addr: Address) -> i128 {
        env.storage().persistent().get(&BalanceKey(addr)).unwrap_or(0)
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let key = BalanceKey(to.clone());
        let bal: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(bal + amount));
    }
}

#[soroban_sdk::contracttype]
pub struct BalanceKey(pub Address);

/// Mock pool for testing (mimics the pool contract interface needed by invoice)
#[soroban_sdk::contract]
pub struct MockPool;

#[soroban_sdk::contractimpl]
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

/// Mock registry for testing
#[soroban_sdk::contract]
pub struct MockRegistry;

#[soroban_sdk::contractimpl]
impl MockRegistry {
    pub fn is_verified(env: Env, address: Address) -> bool {
        env.storage()
            .persistent()
            .get::<_, bool>(&RegKey(address))
            .unwrap_or(false)
    }

    pub fn register(env: Env, address: Address) {
        env.storage().persistent().set(&RegKey(address.clone()), &true);
        env.storage()
            .persistent()
            .extend_ttl(&RegKey(address), 1000, 2000);
    }

    pub fn revoke(env: Env, address: Address) {
        env.storage().persistent().set(&RegKey(address.clone()), &false);
        env.storage()
            .persistent()
            .extend_ttl(&RegKey(address), 1000, 2000);
    }
}

#[soroban_sdk::contracttype]
pub struct RegKey(pub Address);

/// Mock agent registry for attestation
#[soroban_sdk::contract]
pub struct MockAgentRegistry;

#[soroban_sdk::contractimpl]
impl MockAgentRegistry {
    pub fn get_agent(env: Env, agent_id: Symbol) -> Option<Agent> {
        env.storage().persistent().get(&AgentKey(agent_id))
    }

    pub fn register_agent(env: Env, agent_id: Symbol, agent: Agent) {
        env.storage().persistent().set(&AgentKey(agent_id), &agent);
    }
}

#[soroban_sdk::contracttype]
pub struct AgentKey(pub Symbol);

const TEST_AGENT_SEED: [u8; 32] = [7u8; 32];

pub fn test_agent_signing_key() -> SigningKey {
    SigningKey::from_slice(&TEST_AGENT_SEED).unwrap()
}

pub fn test_agent_pubkey(env: &Env) -> BytesN<65> {
    let point = test_agent_signing_key()
        .verifying_key()
        .to_encoded_point(false);
    let mut bytes = [0u8; 65];
    bytes.copy_from_slice(point.as_bytes());
    BytesN::from_array(env, &bytes)
}

pub fn test_agent_id(env: &Env) -> Symbol {
    Symbol::new(env, "test_agent")
}

pub fn mock_pool_with_asset(env: &Env, asset: &Address) -> Address {
    let pool_id = env.register_contract(None, MockPool);
    let _pool_client = MockPoolClient::new(env, &pool_id);
    env.as_contract(&pool_id, || {
        let key = Symbol::new(env, "asset");
        env.storage().instance().set(&key, asset);
    });
    pool_id
}

/// Submits a validly signed attestation for `invoice_id`
pub fn attest_invoice(
    env: &Env,
    invoice: &InvoiceContractClient,
    invoice_id: &BytesN<32>,
) {
    let payload = AttestationPayload {
        domain_separator: BytesN::from_array(env, &ATTESTATION_DOMAIN_SEPARATOR),
        invoice_id: invoice_id.clone(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(env, &[9u8; 32]),
        agent_id: test_agent_id(env),
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

    invoice.submit_attestation(invoice_id, &payload_bytes, &signature);
}

/// Test environment for pool fuzzing
pub struct PoolTestEnv {
    pub env: Env,
    pub pool: PoolContractClient<'static>,
    pub pool_id: Address,
    pub invoice: InvoiceContractClient<'static>,
    pub registry: MockRegistryClient<'static>,
    pub usdc_id: Address,
    pub admin: Address,
    pub issuer: Address,
    pub buyer: Address,
    pub lp: Address,
}

impl PoolTestEnv {
    pub fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths_allowing_non_root_auth();

        let admin = Address::generate(&env);
        let issuer = Address::generate(&env);
        let buyer = Address::generate(&env);
        let lp = Address::generate(&env);

        let registry_id = env.register_contract(None, MockRegistry);
        let registry = MockRegistryClient::new(&env, &registry_id);
        registry.register(&issuer);
        registry.register(&buyer);

        let usdc_id = env.register_contract(None, MockToken);

        let lp_bal_key = BalanceKey(lp.clone());
        env.as_contract(&usdc_id, || {
            env.storage()
                .persistent()
                .set(&lp_bal_key, &100_000_000_000_000i128);
        });
        let buyer_bal_key = BalanceKey(buyer.clone());
        env.as_contract(&usdc_id, || {
            env.storage()
                .persistent()
                .set(&buyer_bal_key, &100_000_000_000_000i128);
        });
        let admin_bal_key = BalanceKey(admin.clone());
        env.as_contract(&usdc_id, || {
            env.storage()
                .persistent()
                .set(&admin_bal_key, &100_000_000_000_000i128);
        });

        let invoice_id = env.register_contract(None, InvoiceContract);
        let escrow_id = env.register_contract(None, EscrowContract);
        let pool_id = env.register_contract(None, PoolContract);

        let invoice = InvoiceContractClient::new(&env, &invoice_id);
        invoice.initialize(&admin, &registry_id);

        let escrow = EscrowContractClient::new(&env, &escrow_id);
        escrow.initialize(&admin, &pool_id, &invoice_id, &usdc_id);

        let pool = PoolContractClient::new(&env, &pool_id);
        pool.initialize(&admin, &invoice_id, &escrow_id, &usdc_id, &registry_id);

        invoice.add_supported_asset(&usdc_id);

        invoice.set_pool_contract(&pool_id);
        invoice.set_escrow_contract(&escrow_id);

        let agent_registry_id = env.register_contract(None, MockAgentRegistry);
        let agent_registry = MockAgentRegistryClient::new(&env, &agent_registry_id);
        agent_registry.register_agent(
            &test_agent_id(&env),
            &Agent {
                active: true,
                pubkey: test_agent_pubkey(&env),
            },
        );
        invoice.set_agent_registry_contract(&agent_registry_id);

        pool.set_max_utilization(&admin, &10000);

        PoolTestEnv {
            env,
            pool,
            pool_id,
            invoice,
            registry,
            usdc_id,
            admin,
            issuer,
            buyer,
            lp,
        }
    }
}

/// Test environment for invoice fuzzing
pub struct InvoiceTestEnv {
    pub env: Env,
    pub invoice: InvoiceContractClient<'static>,
    pub registry: MockRegistryClient<'static>,
    pub usdc_id: Address,
    pub issuer: Address,
    pub buyer: Address,
    pub admin: Address,
}

impl InvoiceTestEnv {
    pub fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let registry_id = env.register_contract(None, MockRegistry);
        let registry = MockRegistryClient::new(&env, &registry_id);

        let issuer = Address::generate(&env);
        let buyer = Address::generate(&env);
        registry.register(&issuer);
        registry.register(&buyer);

        let contract_id = env.register_contract(None, InvoiceContract);
        let invoice = InvoiceContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        invoice.initialize(&admin, &registry_id);

        let usdc_id = env.register_contract(None, MockToken);
        invoice.add_supported_asset(&usdc_id);

        InvoiceTestEnv {
            env,
            invoice,
            registry,
            usdc_id,
            issuer,
            buyer,
            admin,
        }
    }
}

/// Test environment for escrow fuzzing
pub struct EscrowTestEnv {
    pub env: Env,
    pub escrow: EscrowContractClient<'static>,
    pub pool: Address,
    pub invoice_contract: Address,
    pub usdc_id: Address,
    pub contract_id: Address,
}

impl EscrowTestEnv {
    pub fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let pool = Address::generate(&env);
        let invoice_contract = Address::generate(&env);
        let usdc_id = env.register_contract(None, MockToken);

        let pool_bal_key = BalanceKey(pool.clone());
        env.as_contract(&usdc_id, || {
            env.storage()
                .persistent()
                .set(&pool_bal_key, &10_000_000_000_000i128);
        });

        let contract_id = env.register_contract(None, EscrowContract);
        let escrow = EscrowContractClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        escrow.initialize(&admin, &pool, &invoice_contract, &usdc_id);

        EscrowTestEnv {
            env,
            escrow,
            pool,
            invoice_contract,
            usdc_id,
            contract_id,
        }
    }
}

/// Test environment for registry fuzzing
pub struct RegistryTestEnv {
    pub env: Env,
    pub registry: RegistryContractClient<'static>,
    pub admin: Address,
}

impl RegistryTestEnv {
    pub fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        let admin = Address::generate(&env);
        let contract_id = env.register_contract(None, RegistryContract);
        let registry = RegistryContractClient::new(&env, &contract_id);
        registry.initialize(&admin);
        RegistryTestEnv { env, registry, admin }
    }
}

pub fn generate_invoice_id(env: &Env, counter: u64) -> BytesN<32> {
    let mut arr = [0u8; 32];
    let bytes = (env.ledger().timestamp() + counter).to_be_bytes();
    arr[0..8].copy_from_slice(&bytes);
    BytesN::from_array(env, &arr)
}