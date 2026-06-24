#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, token, xdr::ToXdr, Address, Bytes, BytesN, Env,
    IntoVal, Symbol, Vec,
};

mod errors;
mod events;
mod test;
mod types;

pub use errors::*;
pub use types::*;

#[contract]
pub struct InvoiceContract;

#[contractimpl]
impl InvoiceContract {
    pub fn initialize(env: Env, admin: Address, registry_contract: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, InvoiceError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::RegistryContract, &registry_contract);
        env.storage().instance().set(&DataKey::Counter, &0u64);
        Self::extend_instance_ttl(&env);
    }

    pub fn set_pool_contract(env: Env, pool_contract: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::PoolContract, &pool_contract);
        Self::extend_instance_ttl(&env);
    }

    pub fn create(
        env: Env,
        issuer: Address,
        buyer: Address,
        face_value: u128,
        due_date: u64,
        funding_asset: Address,
    ) -> BytesN<32> {
        issuer.require_auth();

        let registry_id: Address = env
            .storage()
            .instance()
            .get(&DataKey::RegistryContract)
            .unwrap();

        let mut args = Vec::new(&env);
        args.push_back(issuer.clone().into_val(&env));
        let issuer_verified: bool =
            env.invoke_contract(&registry_id, &Symbol::new(&env, "is_verified"), args);
        if !issuer_verified {
            panic_with_error!(&env, InvoiceError::IssuerNotVerified);
        }

        let mut args = Vec::new(&env);
        args.push_back(buyer.clone().into_val(&env));
        let buyer_verified: bool =
            env.invoke_contract(&registry_id, &Symbol::new(&env, "is_verified"), args);
        if !buyer_verified {
            panic_with_error!(&env, InvoiceError::BuyerNotVerified);
        }

        if face_value == 0 {
            panic_with_error!(&env, InvoiceError::InvalidFaceValue);
        }
        if due_date <= env.ledger().timestamp() {
            panic_with_error!(&env, InvoiceError::InvalidDueDate);
        }

        let counter: u64 = env.storage().instance().get(&DataKey::Counter).unwrap();
        let next_counter = counter + 1;
        env.storage()
            .instance()
            .set(&DataKey::Counter, &next_counter);

        let now = env.ledger().timestamp();
        let mut hash_input = Bytes::new(&env);
        let issuer_xdr = issuer.clone().to_xdr(&env);
        let buyer_xdr = buyer.clone().to_xdr(&env);
        for i in 0..32 {
            hash_input.push_back(issuer_xdr.get(i).unwrap());
        }
        for i in 0..32 {
            hash_input.push_back(buyer_xdr.get(i).unwrap());
        }
        for b in face_value.to_be_bytes() {
            hash_input.push_back(b);
        }
        for b in due_date.to_be_bytes() {
            hash_input.push_back(b);
        }
        for b in counter.to_be_bytes() {
            hash_input.push_back(b);
        }
        {
            let asset_xdr = funding_asset.clone().to_xdr(&env);
            for i in 0..32 {
                hash_input.push_back(asset_xdr.get(i).unwrap());
            }
        }
        let invoice_id: BytesN<32> = env.crypto().sha256(&hash_input).into();

        let invoice = Invoice {
            id: invoice_id.clone(),
            issuer: issuer.clone(),
            buyer: buyer.clone(),
            face_value,
            discount_bps: 0,
            funded_amount: 0,
            due_date,
            status: InvoiceStatus::Created,
            created_at: now,
            listed_at: None,
            funded_at: None,
            shipped_at: None,
            issuer_confirmed: false,
            buyer_confirmed: false,
            repaid_at: None,
            funding_asset: funding_asset.clone(),
            funding_pool: None,
        };

        let inv_key = DataKey::Invoice(invoice_id.clone());
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);

        self::extend_index(&env, &DataKey::InvoicesByIssuer(issuer), &invoice_id);
        self::extend_index(&env, &DataKey::InvoicesByBuyer(buyer), &invoice_id);
        self::extend_index(
            &env,
            &DataKey::InvoicesByStatus(InvoiceStatus::Created as u32),
            &invoice_id,
        );
        Self::extend_instance_ttl(&env);

        events::invoice_created(
            &env,
            &invoice_id,
            &invoice.issuer,
            &invoice.buyer,
            face_value,
            &funding_asset,
        );
        invoice_id
    }

    pub fn list_for_financing(env: Env, invoice_id: BytesN<32>, discount_bps: u32) -> bool {
        let inv_key = DataKey::Invoice(invoice_id.clone());
        let mut invoice: Invoice = env
            .storage()
            .persistent()
            .get(&inv_key)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.issuer.require_auth();
        if invoice.status != InvoiceStatus::Created {
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }
        if discount_bps > 5000 {
            panic_with_error!(&env, InvoiceError::DiscountTooHigh);
        }
        invoice.status = InvoiceStatus::Listed;
        invoice.discount_bps = discount_bps;
        invoice.listed_at = Some(env.ledger().timestamp());
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        Self::extend_instance_ttl(&env);

        self::move_status_index(
            &env,
            &invoice_id,
            InvoiceStatus::Created,
            InvoiceStatus::Listed,
        );
        events::invoice_listed(&env, &invoice_id, discount_bps);
        true
    }

    pub fn mark_funded(
        env: Env,
        invoice_id: BytesN<32>,
        pool_address: Address,
        asset_address: Address,
        funded_amount: u128,
    ) -> bool {
        pool_address.require_auth();

        let inv_key = DataKey::Invoice(invoice_id.clone());
        let mut invoice: Invoice = env
            .storage()
            .persistent()
            .get(&inv_key)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        if invoice.status != InvoiceStatus::Listed {
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }
        if asset_address != invoice.funding_asset {
            panic_with_error!(&env, InvoiceError::UnsupportedAsset);
        }

        invoice.status = InvoiceStatus::Funded;
        invoice.funded_amount = funded_amount;
        invoice.funded_at = Some(env.ledger().timestamp());
        invoice.funding_pool = Some(pool_address);
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        Self::extend_instance_ttl(&env);

        self::move_status_index(
            &env,
            &invoice_id,
            InvoiceStatus::Listed,
            InvoiceStatus::Funded,
        );
        events::invoice_funded(&env, &invoice_id, funded_amount);
        true
    }

    pub fn mark_shipped(env: Env, invoice_id: BytesN<32>) -> bool {
        let inv_key = DataKey::Invoice(invoice_id.clone());
        let mut invoice: Invoice = env
            .storage()
            .persistent()
            .get(&inv_key)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.issuer.require_auth();
        if invoice.status != InvoiceStatus::Funded {
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }
        invoice.status = InvoiceStatus::Active;
        invoice.shipped_at = Some(env.ledger().timestamp());
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        Self::extend_instance_ttl(&env);

        self::move_status_index(
            &env,
            &invoice_id,
            InvoiceStatus::Funded,
            InvoiceStatus::Active,
        );
        events::invoice_shipped(&env, &invoice_id);
        true
    }

    pub fn confirm_delivery(env: Env, invoice_id: BytesN<32>, confirmer: Address) -> bool {
        confirmer.require_auth();

        let inv_key = DataKey::Invoice(invoice_id.clone());
        let mut invoice: Invoice = env
            .storage()
            .persistent()
            .get(&inv_key)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        if invoice.status != InvoiceStatus::Active {
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }
        if confirmer != invoice.issuer && confirmer != invoice.buyer {
            panic_with_error!(&env, InvoiceError::NotAuthorized);
        }

        if confirmer == invoice.issuer {
            if invoice.issuer_confirmed {
                panic_with_error!(&env, InvoiceError::AlreadyConfirmed);
            }
            invoice.issuer_confirmed = true;
        }
        if confirmer == invoice.buyer {
            if invoice.buyer_confirmed {
                panic_with_error!(&env, InvoiceError::AlreadyConfirmed);
            }
            invoice.buyer_confirmed = true;
        }

        if invoice.issuer_confirmed && invoice.buyer_confirmed {
            invoice.status = InvoiceStatus::Confirmed;
            self::move_status_index(
                &env,
                &invoice_id,
                InvoiceStatus::Active,
                InvoiceStatus::Confirmed,
            );
            events::both_confirmed(&env, &invoice_id);
        }

        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        Self::extend_instance_ttl(&env);
        events::delivery_confirmed(&env, &invoice_id, &confirmer);
        true
    }

    pub fn repay(env: Env, invoice_id: BytesN<32>) -> bool {
        let inv_key = DataKey::Invoice(invoice_id.clone());
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&inv_key)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.buyer.require_auth();
        if invoice.status != InvoiceStatus::Confirmed {
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }

        let pool: Address = invoice
            .funding_pool
            .clone()
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        let face_value = invoice.face_value;
        let buyer = invoice.buyer.clone();
        let funding_asset = invoice.funding_asset.clone();

        let token = token::Client::new(&env, &funding_asset);
        token.transfer(&buyer, &pool, &(face_value as i128));

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        args.push_back(face_value.into_val(&env));
        let _: bool = env.invoke_contract(&pool, &Symbol::new(&env, "receive_repayment"), args);

        let mut updated = invoice;
        updated.status = InvoiceStatus::Repaid;
        updated.repaid_at = Some(env.ledger().timestamp());
        env.storage().persistent().set(&inv_key, &updated);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        Self::extend_instance_ttl(&env);

        self::move_status_index(
            &env,
            &invoice_id,
            InvoiceStatus::Confirmed,
            InvoiceStatus::Repaid,
        );
        events::invoice_repaid(&env, &invoice_id, updated.face_value);
        true
    }

    pub fn trigger_default(env: Env, invoice_id: BytesN<32>) -> bool {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        admin.require_auth();

        let inv_key = DataKey::Invoice(invoice_id.clone());
        let mut invoice: Invoice = env
            .storage()
            .persistent()
            .get(&inv_key)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));

        let valid_transition = invoice.status == InvoiceStatus::Funded
            || invoice.status == InvoiceStatus::Active
            || invoice.status == InvoiceStatus::Confirmed;
        if !valid_transition {
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }
        if env.ledger().timestamp() <= invoice.due_date {
            panic_with_error!(&env, InvoiceError::DueDateNotPassed);
        }

        let prev_status = invoice.status;
        invoice.status = InvoiceStatus::Defaulted;
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        Self::extend_instance_ttl(&env);

        self::move_status_index(&env, &invoice_id, prev_status, InvoiceStatus::Defaulted);

        let pool: Address = invoice
            .funding_pool
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let _: bool = env.invoke_contract(&pool, &Symbol::new(&env, "handle_default"), args);
        events::invoice_defaulted(&env, &invoice_id);
        true
    }

    pub fn get_status(env: Env, invoice_id: BytesN<32>) -> u32 {
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.status as u32
    }

    pub fn get_face_value(env: Env, invoice_id: BytesN<32>) -> u128 {
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.face_value
    }

    pub fn get_discount_bps(env: Env, invoice_id: BytesN<32>) -> u32 {
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.discount_bps
    }

    pub fn get_funding_asset(env: Env, invoice_id: BytesN<32>) -> Address {
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.funding_asset
    }

    pub fn get(env: Env, invoice_id: BytesN<32>) -> Invoice {
        env.storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound))
    }

    pub fn get_by_status(env: Env, status: InvoiceStatus) -> Vec<Invoice> {
        let ids: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::InvoicesByStatus(status as u32))
            .unwrap_or(Vec::new(&env));
        let mut result: Vec<Invoice> = Vec::new(&env);
        for i in 0..ids.len() {
            let id = ids.get(i).unwrap();
            let invoice: Invoice = env
                .storage()
                .persistent()
                .get(&DataKey::Invoice(id))
                .unwrap();
            result.push_back(invoice);
        }
        result
    }

    pub fn get_by_issuer(env: Env, address: Address) -> Vec<Invoice> {
        let ids: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::InvoicesByIssuer(address))
            .unwrap_or(Vec::new(&env));
        let mut result: Vec<Invoice> = Vec::new(&env);
        for i in 0..ids.len() {
            let id = ids.get(i).unwrap();
            let invoice: Invoice = env
                .storage()
                .persistent()
                .get(&DataKey::Invoice(id))
                .unwrap();
            result.push_back(invoice);
        }
        result
    }

    pub fn get_by_buyer(env: Env, address: Address) -> Vec<Invoice> {
        let ids: Vec<BytesN<32>> = env
            .storage()
            .persistent()
            .get(&DataKey::InvoicesByBuyer(address))
            .unwrap_or(Vec::new(&env));
        let mut result: Vec<Invoice> = Vec::new(&env);
        for i in 0..ids.len() {
            let id = ids.get(i).unwrap();
            let invoice: Invoice = env
                .storage()
                .persistent()
                .get(&DataKey::Invoice(id))
                .unwrap();
            result.push_back(invoice);
        }
        result
    }

    pub fn set_expiry_window(env: Env, window: u64) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::ExpiryWindow, &window);
        Self::extend_instance_ttl(&env);
    }

    pub fn get_expiry_window(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ExpiryWindow)
            .unwrap_or(7 * 24 * 60 * 60)
    }

    pub fn check_auth(_env: Env, address: Address) {
        address.require_auth();
    }

    pub fn expire_listing(env: Env, invoice_id: BytesN<32>) -> bool {
        let inv_key = DataKey::Invoice(invoice_id.clone());
        let mut invoice: Invoice = env
            .storage()
            .persistent()
            .get(&inv_key)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));

        if invoice.status != InvoiceStatus::Listed {
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }

        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));

        let is_issuer = env
            .try_invoke_contract::<(), soroban_sdk::Error>(
                &env.current_contract_address(),
                &Symbol::new(&env, "check_auth"),
                (invoice.issuer.clone(),).into_val(&env),
            )
            .is_ok();

        if is_issuer {
            // Already authorized by issuer
        } else {
            admin.require_auth();
        }

        let listed_at = invoice.listed_at.unwrap_or(0);
        let expiry_window = env
            .storage()
            .instance()
            .get(&DataKey::ExpiryWindow)
            .unwrap_or(7 * 24 * 60 * 60);

        let current_time = env.ledger().timestamp();
        if current_time <= listed_at + expiry_window {
            panic_with_error!(&env, InvoiceError::ListingNotExpired);
        }

        let prev_status = invoice.status.clone();
        invoice.status = InvoiceStatus::Expired;
        env.storage().persistent().set(&inv_key, &invoice);

        self::move_status_index(&env, &invoice_id, prev_status, InvoiceStatus::Expired);
        events::invoice_expired(&env, &invoice_id);
        true
    }
}

fn extend_index(env: &Env, key: &DataKey, invoice_id: &BytesN<32>) {
    let mut ids: Vec<BytesN<32>> = env.storage().persistent().get(key).unwrap_or(Vec::new(env));
    ids.push_back(invoice_id.clone());
    env.storage().persistent().set(key, &ids);
    env.storage().persistent().extend_ttl(key, 100, 2_000_000);
}

fn move_status_index(env: &Env, invoice_id: &BytesN<32>, from: InvoiceStatus, to: InvoiceStatus) {
    let from_key = DataKey::InvoicesByStatus(from as u32);
    let mut from_ids: Vec<BytesN<32>> = env
        .storage()
        .persistent()
        .get(&from_key)
        .unwrap_or(Vec::new(env));
    let mut filtered: Vec<BytesN<32>> = Vec::new(env);
    for id in from_ids.iter() {
        if id != *invoice_id {
            filtered.push_back(id);
        }
    }
    from_ids = filtered;
    env.storage().persistent().set(&from_key, &from_ids);
    env.storage()
        .persistent()
        .extend_ttl(&from_key, 100, 2_000_000);

    let to_key = DataKey::InvoicesByStatus(to as u32);
    let mut to_ids: Vec<BytesN<32>> = env
        .storage()
        .persistent()
        .get(&to_key)
        .unwrap_or(Vec::new(env));
    to_ids.push_back(invoice_id.clone());
    env.storage().persistent().set(&to_key, &to_ids);
    env.storage()
        .persistent()
        .extend_ttl(&to_key, 100, 2_000_000);
}

impl InvoiceContract {
    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }
}
