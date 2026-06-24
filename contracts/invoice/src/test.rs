#![cfg(test)]

use soroban_sdk::{
    contract, contractimpl, contracttype, testutils::Address as _, testutils::Ledger, Address,
    BytesN, Env, IntoVal, Symbol,
};

use crate::{InvoiceContract, InvoiceContractClient, InvoiceStatus};

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
            .extend_ttl(&DataKey(address), 100, 2_000_000);
    }
}

#[contracttype]
pub struct DataKey(Address);

#[contract]
pub struct MockPool;

#[contractimpl]
impl MockPool {
    pub fn handle_default(_env: Env, _invoice_id: BytesN<32>) -> bool {
        true
    }

    pub fn get_usdc_asset(env: Env) -> Address {
        let key = Symbol::new(&env, "asset");
        env.storage().instance().get(&key).unwrap()
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

    let usdc_asset = Address::generate(&env);

    (env, client, issuer, buyer, registry_client, usdc_asset)
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

#[test]
fn test_create_invoice_with_verified_parties() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let face_value: u128 = 1_000_000_000;
    let due_date = env.ledger().timestamp() + 86400;

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
#[should_panic(expected = "Error(Contract, #6)")]
fn test_create_fails_zero_face_value() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    client.create(&issuer, &buyer, &0, &due_date, &usdc);
}

#[test]
#[should_panic(expected = "Error(Contract, #7)")]
fn test_create_fails_past_due_date() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    env.ledger().set_timestamp(86400);
    let past_date = env.ledger().timestamp() - 1;
    client.create(&issuer, &buyer, &1_000_000_000, &past_date, &usdc);
}

#[test]
fn test_list_for_financing() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    let result = client.list_for_financing(&invoice_id, &200);
    assert!(result);

    let invoice = client.get(&invoice_id);
    assert_eq!(invoice.status, InvoiceStatus::Listed);
    assert_eq!(invoice.discount_bps, 200);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_list_fails_wrong_status() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);
    client.list_for_financing(&invoice_id, &300);
}

#[test]
#[should_panic(expected = "Error(Contract, #9)")]
fn test_list_fails_discount_too_high() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &5001);
}

#[test]
fn test_full_lifecycle() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);

    let funded_amount: u128 = 980_000_000;
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
    let due_date = env.ledger().timestamp() + 86400;

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
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
    let due_date = env.ledger().timestamp() + 86400;

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let invoices = client.get_by_buyer(&buyer);
    assert_eq!(invoices.len(), 2);
}

#[test]
fn test_get_by_status_returns_correct_invoices() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 2);
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
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);

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
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
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

    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);

    client.confirm_delivery(&invoice_id, &stranger);
}

#[test]
fn test_trigger_default_requires_past_due_date() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool_id = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool_id);
    client.mark_funded(&invoice_id, &pool_id, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &buyer);

    env.ledger().set_timestamp(due_date + 1);

    let result = client.trigger_default(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Defaulted);
}

#[test]
fn test_get_by_status_filters_correctly() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;

    let id1 = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.create(&issuer, &buyer, &2_000_000_000, &due_date, &usdc);

    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 2);

    client.list_for_financing(&id1, &200);
    let created = client.get_by_status(&InvoiceStatus::Created);
    assert_eq!(created.len(), 1);
    let listed = client.get_by_status(&InvoiceStatus::Listed);
    assert_eq!(listed.len(), 1);
}

#[test]
#[should_panic(expected = "Error(Contract, #10)")]
fn test_double_confirmation_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    client.mark_shipped(&invoice_id);
    client.confirm_delivery(&invoice_id, &issuer);
    client.confirm_delivery(&invoice_id, &issuer);
}

#[test]
fn test_status_transitions_full_lifecycle() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Created);

    client.list_for_financing(&invoice_id, &200);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Listed);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
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
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let xlm = Address::generate(&env);
    let xlm_pool = mock_pool_with_asset(&env, &xlm);
    client.set_pool_contract(&xlm_pool);
    client.mark_funded(&invoice_id, &xlm_pool, &xlm, &980_000_000);
}

#[test]
fn test_mark_funded_succeeds_with_matching_asset() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    let pool = mock_pool_with_asset(&env, &usdc);
    client.set_pool_contract(&pool);
    let result = client.mark_funded(&invoice_id, &pool, &usdc, &980_000_000);
    assert!(result);
    let inv = client.get(&invoice_id);
    assert_eq!(inv.funding_pool, Some(pool));
}

#[test]
fn test_create_invoice_with_xlm_asset() {
    let (env, client, issuer, buyer, _, _usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let xlm_asset = Address::generate(&env);

    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &xlm_asset);
    let invoice = client.get(&invoice_id);

    assert_eq!(invoice.funding_asset, xlm_asset);
    assert_eq!(invoice.status, InvoiceStatus::Created);
}

#[test]
fn test_get_funding_asset_returns_correct_asset() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    let asset = client.get_funding_asset(&invoice_id);
    assert_eq!(asset, usdc);
}

#[test]
fn test_expire_listing_succeeds_by_issuer() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

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
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

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
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Fast forward ledger time by only 5 days (less than 7 days)
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 5 * 24 * 60 * 60);

    client.expire_listing(&invoice_id);
}

#[test]
#[should_panic(expected = "Error(Contract, #8)")]
fn test_expire_listing_wrong_status_panics() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    // Fast forward ledger time
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    client.expire_listing(&invoice_id);
}

#[test]
fn test_expire_listing_configurable_window() {
    let (env, client, issuer, buyer, _, usdc) = setup();
    let due_date = env.ledger().timestamp() + 86400;
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);
    client.list_for_financing(&invoice_id, &200);

    // Set expiry window to 1 day (86400 seconds)
    client.set_expiry_window(&86400);
    assert_eq!(client.get_expiry_window(), 86400);

    // Fast forward by 1 day + 1 second
    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 86400 + 1);

    let result = client.expire_listing(&invoice_id);
    assert!(result);
    assert_eq!(client.get(&invoice_id).status, InvoiceStatus::Expired);
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
    let due_date = env.ledger().timestamp() + 86400;

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "create",
            args: (
                issuer.clone(),
                buyer.clone(),
                1_000_000_000u128,
                due_date,
                usdc.clone(),
            )
                .into_val(&env),
            sub_invokes: &[],
        },
    }]);
    let invoice_id = client.create(&issuer, &buyer, &1_000_000_000, &due_date, &usdc);

    env.mock_auths(&[soroban_sdk::testutils::MockAuth {
        address: &issuer,
        invoke: &soroban_sdk::testutils::MockAuthInvoke {
            contract: &contract_id,
            fn_name: "list_for_financing",
            args: (invoice_id.clone(), 200u32).into_val(&env),
            sub_invokes: &[],
        },
    }]);
    client.list_for_financing(&invoice_id, &200);

    env.ledger()
        .set_timestamp(env.ledger().timestamp() + 7 * 24 * 60 * 60 + 1);

    // Calling expire_listing without mocking auths for issuer or admin should panic due to failed require_auth.
    client.expire_listing(&invoice_id);
}
