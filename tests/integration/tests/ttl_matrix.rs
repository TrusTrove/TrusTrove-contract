extern crate std;

use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::storage::Persistent as _,
    testutils::Address as _, testutils::BytesN as _, testutils::Ledger as _, xdr::ToXdr, Address,
    BytesN, Env, Map, String, Symbol,
};

use trusttrove_escrow::{EscrowContract, EscrowContractClient};
use trusttrove_invoice::{InvoiceContract, InvoiceContractClient, InvoiceStatus};
use trusttrove_pool::{
    PoolContract, PoolContractClient, DEFAULT_MIN_INITIAL_DEPOSIT, DEFAULT_SHARE_DECIMALS,
};
use trusttrove_registry::{RegistryContract, RegistryContractClient, Role, VerificationStatus};
use trusttrove_ttl::{EXTEND_TO as TTL_EXTEND_TO, THRESHOLD as TTL_THRESHOLD};

// --------------- Mock USDC Token ---------------

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
}

#[contracttype]
pub struct TKey(Address);

// --------------- Mock Agent Registry ---------------

#[contract]
pub struct MockAgentRegistry;

#[contractimpl]
impl MockAgentRegistry {
    pub fn get_agent(env: Env, agent_id: Symbol) -> Option<trusttrove_invoice::Agent> {
        env.storage().persistent().get(&AgentKey(agent_id))
    }

    pub fn register_agent(env: Env, agent_id: Symbol, agent: trusttrove_invoice::Agent) {
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

fn test_agent_id(env: &Env) -> Symbol {
    Symbol::new(env, "test_agent")
}

fn attest_invoice(env: &Env, invoice_client: &InvoiceContractClient, invoice_id: &BytesN<32>) {
    let payload = trusttrove_invoice::AttestationPayload {
        domain_separator: BytesN::from_array(
            env,
            &trusttrove_invoice::ATTESTATION_DOMAIN_SEPARATOR,
        ),
        invoice_id: invoice_id.clone(),
        contract_address: invoice_client.address.clone(),
        network_id: env.ledger().network_id(),
        risk_score: 5000,
        evidence_hash: BytesN::from_array(env, &[9u8; 32]),
        agent_id: test_agent_id(env),
        nonce: 1,
        valid_until: u64::MAX,
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

    invoice_client.submit_attestation(invoice_id, &payload_bytes, &signature);
}

fn keep_instances_alive(env: &Env, contracts: &[&Address]) {
    for addr in contracts {
        env.as_contract(addr, || {
            env.storage()
                .instance()
                .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
        });
    }
}

/// Workspace-level test matrix verifying TTL renewal for every persistent DataKey family
/// across the TrusTrove contract suite:
/// - Invoices: `Invoice`, `SupportedAsset`
/// - Indexes: `ProfileIndex`, `ProfileCount`, `IssuerIndex*`, `BuyerIndex*`, `StatusIndex*`, `StatusCount`
/// - Attestations: `Attestation`
/// - LP Positions: `LPShares`, `LPDepositCount`, `LPInitialDeposit`, `LPYieldEarned`, `Allowance`
/// - Funded Entries: `FundedInvoice`, `FundedInvoiceBuyer`
/// - Escrow Locks: `Locked`
/// - Histories: `History`, `IssuerReleased`
/// - Profiles: `Profile`
#[test]
fn test_workspace_persistent_datakey_ttl_renewal_and_non_materialization_matrix() {
    let env = Env::default();
    env.mock_all_auths_allowing_non_root_auth();

    let admin = Address::generate(&env);
    let issuer = Address::generate(&env);
    let buyer = Address::generate(&env);
    let lp = Address::generate(&env);
    let spender = Address::generate(&env);
    let treasury = Address::generate(&env);
    let pool_id = Address::generate(&env);

    // Deploy Mock USDC Token
    let usdc_id = env.register(MockToken, ());
    for address in [&lp, &buyer, &admin, &pool_id] {
        let key = TKey(address.clone());
        env.as_contract(&usdc_id, || {
            env.storage()
                .persistent()
                .set(&key, &100_000_000_000_000i128);
        });
    }

    // Deploy Registry
    let registry_id = env.register(RegistryContract, (admin.clone(),));
    let registry = RegistryContractClient::new(&env, &registry_id);
    let metadata: Map<String, String> = Map::new(&env);
    registry.register_issuer(&issuer, &metadata);
    registry.verify_profile(&admin, &issuer, &true);

    // Deploy Invoice, Escrow, Pool
    let invoice_id = env.register(InvoiceContract, (admin.clone(), registry_id.clone()));
    let escrow_id = env.register(
        EscrowContract,
        (admin.clone(), pool_id.clone(), usdc_id.clone()),
    );
    let invoice = InvoiceContractClient::new(&env, &invoice_id);
    let escrow = EscrowContractClient::new(&env, &escrow_id);

    invoice.add_supported_asset(&usdc_id);
    invoice.set_pool_contract(&pool_id);
    invoice.set_escrow_contract(&escrow_id);

    let agent_reg_id = env.register(MockAgentRegistry, ());
    MockAgentRegistryClient::new(&env, &agent_reg_id).register_agent(
        &test_agent_id(&env),
        &trusttrove_invoice::Agent {
            active: true,
            pubkey: test_agent_pubkey(&env),
        },
    );
    invoice.set_agent_registry_contract(&agent_reg_id);

    let pool = PoolContractClient::new(&env, &pool_id);
    env.register_at(
        &pool_id,
        PoolContract,
        (
            admin.clone(),
            invoice_id.clone(),
            escrow_id.clone(),
            usdc_id.clone(),
            registry_id.clone(),
            treasury.clone(),
            DEFAULT_MIN_INITIAL_DEPOSIT,
            String::from_str(&env, "TrusTrove USDC Pool Shares"),
            String::from_str(&env, "TT-USDC"),
            DEFAULT_SHARE_DECIMALS,
        ),
    );
    pool.set_max_utilization(&admin, &10000);

    // Setup LP position and allowance in Pool
    let deposit_amount = 50_000_000_000u128;
    pool.deposit(&lp, &deposit_amount);
    let expiration_ledger = env.ledger().sequence() + 10_000_000;
    pool.approve(&lp, &spender, &1_000_000_000i128, &expiration_ledger);

    // Register buyer in registry
    registry.register_buyer(&buyer, &metadata);
    registry.verify_profile(&admin, &buyer, &true);

    // Create Invoice 1: to be listed, funded, and locked in escrow
    let face_value_1 = 10_000_000_000u128;
    let due_date_1 = env.ledger().timestamp() + 30 * 86400;
    let inv1_id = invoice.create(&issuer, &buyer, &face_value_1, &due_date_1, &usdc_id);
    attest_invoice(&env, &invoice, &inv1_id);
    invoice.list_for_financing(&inv1_id, &500); // 5% discount
    pool.fund_invoice(&inv1_id);

    // Create Invoice 2: pool funding releases principal to issuer, creating IssuerReleased in Escrow
    let face_value_2 = 5_000_000_000u128;
    let due_date_2 = env.ledger().timestamp() + 30 * 86400;
    let inv2_id = invoice.create(&issuer, &buyer, &face_value_2, &due_date_2, &usdc_id);
    attest_invoice(&env, &invoice, &inv2_id);
    invoice.list_for_financing(&inv2_id, &500);
    pool.fund_invoice(&inv2_id);

    // Settle a portion of repayment and withdraw some shares to populate LPYieldEarned
    invoice.mark_shipped(&inv1_id);
    invoice.confirm_delivery(&inv1_id, &issuer);
    invoice.confirm_delivery(&inv1_id, &buyer);
    env.ledger().set_timestamp(due_date_1);
    invoice.repay(&inv1_id); // repays invoice 1, adds yield to pool
    pool.withdraw(&lp, &1_000_000_000u128); // withdraws shares, realizing yield into LPYieldEarned

    // Create Invoice 3: active funded entry in pool
    let face_value_3 = 8_000_000_000u128;
    let due_date_3 = env.ledger().timestamp() + 30 * 86400;
    let inv3_id = invoice.create(&issuer, &buyer, &face_value_3, &due_date_3, &usdc_id);
    attest_invoice(&env, &invoice, &inv3_id);
    invoice.list_for_financing(&inv3_id, &500);
    pool.fund_invoice(&inv3_id);

    // Active lock in escrow
    let escrow_lock_id = BytesN::random(&env);
    escrow.lock(&escrow_lock_id, &1_000_000_000u128, &issuer);

    // =========================================================================
    // Verify Representative Keys Exist for Every Persistent Family
    // =========================================================================
    let reg_profile_key = trusttrove_registry::DataKey::Profile(issuer.clone());
    let reg_index_key = trusttrove_registry::DataKey::ProfileIndex(Role::Issuer, 0);
    let reg_count_key = trusttrove_registry::DataKey::ProfileCount(Role::Issuer);

    let inv_entry_key = trusttrove_invoice::DataKey::Invoice(inv3_id.clone());
    let inv_supported_asset_key = trusttrove_invoice::DataKey::SupportedAsset(usdc_id.clone());
    let inv_attestation_key = trusttrove_invoice::DataKey::Attestation(inv3_id.clone());
    let inv_issuer_count_key = trusttrove_invoice::DataKey::IssuerIndexCount(issuer.clone());
    let inv_issuer_entry_key = trusttrove_invoice::DataKey::IssuerIndexEntry(issuer.clone(), 0);
    let inv_buyer_count_key = trusttrove_invoice::DataKey::BuyerIndexCount(buyer.clone());
    let inv_buyer_entry_key = trusttrove_invoice::DataKey::BuyerIndexEntry(buyer.clone(), 0);
    let inv_status_count_key =
        trusttrove_invoice::DataKey::StatusIndexCount(InvoiceStatus::Funded as u32);
    let inv_status_entry_key =
        trusttrove_invoice::DataKey::StatusIndexEntry(InvoiceStatus::Funded as u32, 0);
    let inv_status_count_metric_key =
        trusttrove_invoice::DataKey::StatusCount(InvoiceStatus::Funded as u32);

    let pool_lp_shares_key = trusttrove_pool::DataKey::LPShares(lp.clone());
    let pool_lp_deposit_count_key = trusttrove_pool::DataKey::LPDepositCount(lp.clone());
    let pool_lp_init_key = trusttrove_pool::DataKey::LPInitialDeposit(lp.clone());
    let pool_lp_yield_key = trusttrove_pool::DataKey::LPYieldEarned(lp.clone());
    let pool_funded_invoice_key = trusttrove_pool::DataKey::FundedInvoice(inv3_id.clone());
    let pool_funded_buyer_key = trusttrove_pool::DataKey::FundedInvoiceBuyer(inv3_id.clone());
    let pool_allowance_key = trusttrove_pool::DataKey::Allowance(lp.clone(), spender.clone());

    let escrow_locked_key = trusttrove_escrow::DataKey::Locked(escrow_lock_id.clone());
    let escrow_history_key = trusttrove_escrow::DataKey::History(escrow_lock_id.clone());
    let escrow_released_key = trusttrove_escrow::DataKey::IssuerReleased(inv2_id.clone());

    // Check all keys exist
    env.as_contract(&registry_id, || {
        assert!(env.storage().persistent().has(&reg_profile_key));
        assert!(env.storage().persistent().has(&reg_index_key));
        assert!(env.storage().persistent().has(&reg_count_key));
    });
    env.as_contract(&invoice_id, || {
        assert!(env.storage().persistent().has(&inv_entry_key));
        assert!(env.storage().persistent().has(&inv_supported_asset_key));
        assert!(env.storage().persistent().has(&inv_attestation_key));
        assert!(env.storage().persistent().has(&inv_issuer_count_key));
        assert!(env.storage().persistent().has(&inv_issuer_entry_key));
        assert!(env.storage().persistent().has(&inv_buyer_count_key));
        assert!(env.storage().persistent().has(&inv_buyer_entry_key));
        assert!(env.storage().persistent().has(&inv_status_count_key));
        assert!(env.storage().persistent().has(&inv_status_entry_key));
        assert!(env.storage().persistent().has(&inv_status_count_metric_key));
    });
    env.as_contract(&pool_id, || {
        assert!(env.storage().persistent().has(&pool_lp_shares_key));
        assert!(env.storage().persistent().has(&pool_lp_deposit_count_key));
        assert!(env.storage().persistent().has(&pool_lp_init_key));
        assert!(env.storage().persistent().has(&pool_lp_yield_key));
        assert!(env.storage().persistent().has(&pool_funded_invoice_key));
        assert!(env.storage().persistent().has(&pool_funded_buyer_key));
        assert!(env.storage().persistent().has(&pool_allowance_key));
    });
    env.as_contract(&escrow_id, || {
        assert!(env.storage().persistent().has(&escrow_locked_key));
        assert!(env.storage().persistent().has(&escrow_history_key));
        assert!(env.storage().persistent().has(&escrow_released_key));
    });

    // =========================================================================
    // Age All Keys Below TTL_THRESHOLD
    // =========================================================================
    // Measure remaining TTL and advance ledger to drain below THRESHOLD
    let sample_ttl: u32 = env.as_contract(&registry_id, || {
        env.storage().persistent().get_ttl(&reg_profile_key)
    });
    assert!(
        sample_ttl > TTL_THRESHOLD,
        "initial TTL should exceed THRESHOLD ({TTL_THRESHOLD}), got {sample_ttl}"
    );

    let drain_ledgers = sample_ttl - 100;
    env.ledger()
        .set_sequence_number(env.ledger().sequence() + drain_ledgers);

    // Keep instance storage of contracts alive so execution continues normally
    keep_instances_alive(&env, &[&registry_id, &invoice_id, &escrow_id, &pool_id]);

    // Verify all keys are now aged below TTL_THRESHOLD
    let assert_below_threshold = |contract: &Address, key_desc: &str, get_ttl: &dyn Fn() -> u32| {
        let ttl = env.as_contract(contract, get_ttl);
        assert!(
            ttl < TTL_THRESHOLD,
            "{key_desc} TTL should be below THRESHOLD ({TTL_THRESHOLD}) before renewal, got {ttl}"
        );
    };

    assert_below_threshold(&registry_id, "Registry Profile", &|| {
        env.storage().persistent().get_ttl(&reg_profile_key)
    });
    assert_below_threshold(&registry_id, "Registry ProfileIndex", &|| {
        env.storage().persistent().get_ttl(&reg_index_key)
    });
    assert_below_threshold(&registry_id, "Registry ProfileCount", &|| {
        env.storage().persistent().get_ttl(&reg_count_key)
    });

    assert_below_threshold(&invoice_id, "Invoice Entry", &|| {
        env.storage().persistent().get_ttl(&inv_entry_key)
    });
    assert_below_threshold(&invoice_id, "Invoice SupportedAsset", &|| {
        env.storage().persistent().get_ttl(&inv_supported_asset_key)
    });
    assert_below_threshold(&invoice_id, "Invoice Attestation", &|| {
        env.storage().persistent().get_ttl(&inv_attestation_key)
    });
    assert_below_threshold(&invoice_id, "Invoice IssuerIndexCount", &|| {
        env.storage().persistent().get_ttl(&inv_issuer_count_key)
    });
    assert_below_threshold(&invoice_id, "Invoice IssuerIndexEntry", &|| {
        env.storage().persistent().get_ttl(&inv_issuer_entry_key)
    });
    assert_below_threshold(&invoice_id, "Invoice BuyerIndexCount", &|| {
        env.storage().persistent().get_ttl(&inv_buyer_count_key)
    });
    assert_below_threshold(&invoice_id, "Invoice BuyerIndexEntry", &|| {
        env.storage().persistent().get_ttl(&inv_buyer_entry_key)
    });
    assert_below_threshold(&invoice_id, "Invoice StatusIndexCount", &|| {
        env.storage().persistent().get_ttl(&inv_status_count_key)
    });
    assert_below_threshold(&invoice_id, "Invoice StatusIndexEntry", &|| {
        env.storage().persistent().get_ttl(&inv_status_entry_key)
    });
    assert_below_threshold(&invoice_id, "Invoice StatusCount", &|| {
        env.storage()
            .persistent()
            .get_ttl(&inv_status_count_metric_key)
    });

    assert_below_threshold(&pool_id, "Pool LPShares", &|| {
        env.storage().persistent().get_ttl(&pool_lp_shares_key)
    });
    assert_below_threshold(&pool_id, "Pool LPDepositCount", &|| {
        env.storage()
            .persistent()
            .get_ttl(&pool_lp_deposit_count_key)
    });
    assert_below_threshold(&pool_id, "Pool LPInitialDeposit", &|| {
        env.storage().persistent().get_ttl(&pool_lp_init_key)
    });
    assert_below_threshold(&pool_id, "Pool LPYieldEarned", &|| {
        env.storage().persistent().get_ttl(&pool_lp_yield_key)
    });
    assert_below_threshold(&pool_id, "Pool FundedInvoice", &|| {
        env.storage().persistent().get_ttl(&pool_funded_invoice_key)
    });
    assert_below_threshold(&pool_id, "Pool FundedInvoiceBuyer", &|| {
        env.storage().persistent().get_ttl(&pool_funded_buyer_key)
    });
    assert_below_threshold(&pool_id, "Pool Allowance", &|| {
        env.storage().persistent().get_ttl(&pool_allowance_key)
    });

    assert_below_threshold(&escrow_id, "Escrow Locked", &|| {
        env.storage().persistent().get_ttl(&escrow_locked_key)
    });
    assert_below_threshold(&escrow_id, "Escrow History", &|| {
        env.storage().persistent().get_ttl(&escrow_history_key)
    });
    assert_below_threshold(&escrow_id, "Escrow IssuerReleased", &|| {
        env.storage().persistent().get_ttl(&escrow_released_key)
    });

    // =========================================================================
    // Call Documented Paths: Assert Key Renewal & Non-Materialization of Missing Keys
    // =========================================================================

    // 1. REGISTRY KEYS
    // Documented read paths: get_verification_status, get_profile_count, list_profiles
    assert_eq!(
        registry.get_verification_status(&issuer),
        VerificationStatus::Verified
    );
    assert_eq!(registry.get_profile_count(&Role::Issuer), 1);
    let issuer_page = registry.list_profiles(&Role::Issuer, &0, &10);
    assert_eq!(issuer_page.len(), 1);

    // Assert Registry keys renewed
    let assert_renewed = |contract: &Address, key_desc: &str, get_ttl: &dyn Fn() -> u32| {
        let ttl = env.as_contract(contract, get_ttl);
        assert!(
            ttl >= TTL_THRESHOLD,
            "{key_desc} should be renewed above THRESHOLD ({TTL_THRESHOLD}), got {ttl}"
        );
        assert!(
            ttl >= 1_990_000,
            "{key_desc} should be extended close to EXTEND_TO ({TTL_EXTEND_TO}), got {ttl}"
        );
    };

    assert_renewed(&registry_id, "Registry Profile", &|| {
        env.storage().persistent().get_ttl(&reg_profile_key)
    });
    assert_renewed(&registry_id, "Registry ProfileIndex", &|| {
        env.storage().persistent().get_ttl(&reg_index_key)
    });
    assert_renewed(&registry_id, "Registry ProfileCount", &|| {
        env.storage().persistent().get_ttl(&reg_count_key)
    });

    // Assert Registry non-materialization of missing keys
    let unknown_addr = Address::generate(&env);
    assert_eq!(
        registry.get_verification_status(&unknown_addr),
        VerificationStatus::Unregistered
    );
    env.as_contract(&registry_id, || {
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_registry::DataKey::Profile(unknown_addr.clone())),
            "missing Profile key must not be materialized"
        );
    });

    // 2. INVOICE KEYS
    // Documented read paths: is_supported_asset, get_by_issuer, get_by_buyer, get_by_status, get_counts, get_attestation
    assert!(invoice.is_supported_asset(&usdc_id));
    let by_issuer = invoice.get_by_issuer(&issuer, &0, &10);
    assert!(!by_issuer.is_empty());
    let by_buyer = invoice.get_by_buyer(&buyer, &0, &10);
    assert!(!by_buyer.is_empty());
    let by_status = invoice.get_by_status(&InvoiceStatus::Funded, &0, &10);
    assert!(!by_status.is_empty());
    let _counts = invoice.get_counts();
    assert!(invoice.get_attestation(&inv3_id).is_some());

    assert_renewed(&invoice_id, "Invoice SupportedAsset", &|| {
        env.storage().persistent().get_ttl(&inv_supported_asset_key)
    });
    assert_renewed(&invoice_id, "Invoice Entry", &|| {
        env.storage().persistent().get_ttl(&inv_entry_key)
    });
    assert_renewed(&invoice_id, "Invoice IssuerIndexCount", &|| {
        env.storage().persistent().get_ttl(&inv_issuer_count_key)
    });
    assert_renewed(&invoice_id, "Invoice IssuerIndexEntry", &|| {
        env.storage().persistent().get_ttl(&inv_issuer_entry_key)
    });
    assert_renewed(&invoice_id, "Invoice BuyerIndexCount", &|| {
        env.storage().persistent().get_ttl(&inv_buyer_count_key)
    });
    assert_renewed(&invoice_id, "Invoice BuyerIndexEntry", &|| {
        env.storage().persistent().get_ttl(&inv_buyer_entry_key)
    });
    assert_renewed(&invoice_id, "Invoice StatusIndexCount", &|| {
        env.storage().persistent().get_ttl(&inv_status_count_key)
    });
    assert_renewed(&invoice_id, "Invoice StatusIndexEntry", &|| {
        env.storage().persistent().get_ttl(&inv_status_entry_key)
    });
    assert_renewed(&invoice_id, "Invoice StatusCount", &|| {
        env.storage()
            .persistent()
            .get_ttl(&inv_status_count_metric_key)
    });
    assert_renewed(&invoice_id, "Invoice Attestation", &|| {
        env.storage().persistent().get_ttl(&inv_attestation_key)
    });

    // Assert Invoice non-materialization of missing keys
    let unknown_asset = Address::generate(&env);
    assert!(!invoice.is_supported_asset(&unknown_asset));
    let unknown_invoice = BytesN::random(&env);
    assert!(invoice.get_attestation(&unknown_invoice).is_none());
    let empty_issuer_page = invoice.get_by_issuer(&unknown_addr, &0, &10);
    assert!(empty_issuer_page.is_empty());
    let empty_status_page = invoice.get_by_status(&InvoiceStatus::Defaulted, &0, &10);
    assert!(empty_status_page.is_empty());

    env.as_contract(&invoice_id, || {
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_invoice::DataKey::SupportedAsset(unknown_asset)),
            "missing SupportedAsset must not be materialized"
        );
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_invoice::DataKey::Attestation(
                    unknown_invoice.clone()
                )),
            "missing Attestation must not be materialized"
        );
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_invoice::DataKey::IssuerIndexCount(
                    unknown_addr.clone()
                )),
            "missing IssuerIndexCount must not be materialized"
        );
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_invoice::DataKey::Invoice(unknown_invoice)),
            "missing Invoice must not be materialized"
        );
    });

    // 3. POOL KEYS
    // Documented read paths: get_lp_position, get_funded_amount, allowance
    let lp_pos = pool.get_lp_position(&lp);
    assert!(lp_pos.shares > 0);
    assert!(lp_pos.yield_earned > 0);
    assert_eq!(pool.allowance(&lp, &spender), 1_000_000_000i128);
    assert_eq!(pool.get_funded_amount(&inv3_id), Some(7_600_000_000u128));

    assert_renewed(&pool_id, "Pool LPShares", &|| {
        env.storage().persistent().get_ttl(&pool_lp_shares_key)
    });
    assert_renewed(&pool_id, "Pool LPDepositCount", &|| {
        env.storage()
            .persistent()
            .get_ttl(&pool_lp_deposit_count_key)
    });
    assert_renewed(&pool_id, "Pool LPInitialDeposit", &|| {
        env.storage().persistent().get_ttl(&pool_lp_init_key)
    });
    assert_renewed(&pool_id, "Pool LPYieldEarned", &|| {
        env.storage().persistent().get_ttl(&pool_lp_yield_key)
    });
    assert_renewed(&pool_id, "Pool Allowance", &|| {
        env.storage().persistent().get_ttl(&pool_allowance_key)
    });
    assert_renewed(&pool_id, "Pool FundedInvoice", &|| {
        env.storage().persistent().get_ttl(&pool_funded_invoice_key)
    });
    assert_renewed(&pool_id, "Pool FundedInvoiceBuyer", &|| {
        env.storage().persistent().get_ttl(&pool_funded_buyer_key)
    });

    // Assert Pool non-materialization of missing keys
    let unknown_lp = Address::generate(&env);
    let unknown_spender = Address::generate(&env);
    let unk_pos = pool.get_lp_position(&unknown_lp);
    assert_eq!(unk_pos.shares, 0);
    assert_eq!(pool.allowance(&unknown_lp, &unknown_spender), 0);
    let unknown_inv = BytesN::random(&env);
    assert_eq!(pool.get_funded_amount(&unknown_inv), None);

    env.as_contract(&pool_id, || {
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_pool::DataKey::LPShares(unknown_lp.clone())),
            "missing LPShares must not be materialized"
        );
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_pool::DataKey::Allowance(
                    unknown_lp,
                    unknown_spender
                )),
            "missing Allowance must not be materialized"
        );
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_pool::DataKey::FundedInvoice(unknown_inv)),
            "missing FundedInvoice must not be materialized"
        );
    });

    // 4. ESCROW KEYS
    // Documented read paths: get_locked, get_history, was_released_to_issuer
    assert!(escrow.get_locked(&escrow_lock_id) > 0);
    let history = escrow.get_history(&escrow_lock_id);
    assert!(!history.is_empty());
    assert!(escrow.was_released_to_issuer(&inv2_id));

    assert_renewed(&escrow_id, "Escrow Locked", &|| {
        env.storage().persistent().get_ttl(&escrow_locked_key)
    });
    assert_renewed(&escrow_id, "Escrow History", &|| {
        env.storage().persistent().get_ttl(&escrow_history_key)
    });
    assert_renewed(&escrow_id, "Escrow IssuerReleased", &|| {
        env.storage().persistent().get_ttl(&escrow_released_key)
    });

    // Assert Escrow non-materialization of missing keys
    let unknown_escrow_inv = BytesN::random(&env);
    assert_eq!(escrow.get_locked(&unknown_escrow_inv), 0);
    assert_eq!(escrow.get_locked_at(&unknown_escrow_inv), 0);
    assert!(escrow.get_history(&unknown_escrow_inv).is_empty());
    assert!(!escrow.was_released_to_issuer(&unknown_escrow_inv));

    env.as_contract(&escrow_id, || {
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_escrow::DataKey::Locked(
                    unknown_escrow_inv.clone()
                )),
            "missing Locked must not be materialized"
        );
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_escrow::DataKey::History(
                    unknown_escrow_inv.clone()
                )),
            "missing History must not be materialized"
        );
        assert!(
            !env.storage()
                .persistent()
                .has(&trusttrove_escrow::DataKey::IssuerReleased(
                    unknown_escrow_inv
                )),
            "missing IssuerReleased must not be materialized"
        );
    });
}
