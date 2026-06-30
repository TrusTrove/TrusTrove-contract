#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, token, xdr::ToXdr, Address, Bytes, BytesN, Env,
    IntoVal, Symbol, Vec,
};
use trusttrove_common::persistent_set;

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
    /// Initializes the invoice contract with admin and registry references.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `admin` - The admin address for this contract.
    /// * `registry_contract` - The deployed registry contract address.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Panics
    /// * `AlreadyInitialized` if the contract has already been initialized.
    ///
    /// # Example
    /// ```ignore
    /// client.initialize(&admin, &registry_address);
    /// ```
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
    }

    pub fn set_pool_contract(env: Env, pool_contract: Address) {
        // Sets the pool contract address used by this invoice contract.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `pool_contract` - The pool contract address.
        //
        // # Returns
        // * `()` - No value is returned.
        //
        // # Panics
        // * `NotFound` if the admin is not initialized.
        //
        // # Example
        // ```ignore
        // client.set_pool_contract(&pool_address);
        // ```
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::PoolContract, &pool_contract);
        events::pool_contract_set(&env, &pool_contract);
    }

    pub fn add_supported_asset(env: Env, asset: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        admin.require_auth();

        let key = DataKey::SupportedAsset(asset.clone());
        if env.storage().persistent().has(&key) {
            return;
        }

        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::SupportedAssetCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::SupportedAssetCount, &(count + 1));
        env.storage().persistent().set(&key, &true);
    }

    pub fn remove_supported_asset(env: Env, asset: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        admin.require_auth();

        let key = DataKey::SupportedAsset(asset.clone());
        if !env.storage().persistent().has(&key) {
            return;
        }

        let count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::SupportedAssetCount)
            .unwrap_or(0);
        env.storage()
            .instance()
            .set(&DataKey::SupportedAssetCount, &(count - 1));
        env.storage().persistent().remove(&key);
    }

    pub fn is_supported_asset(env: Env, asset: Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::SupportedAsset(asset))
    }

    pub fn get_supported_asset_count(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::SupportedAssetCount)
            .unwrap_or(0)
    }

    pub fn create(
        env: Env,
        issuer: Address,
        buyer: Address,
        face_value: u128,
        due_date: u64,
        funding_asset: Address,
    ) -> BytesN<32> {
        // Creates a new invoice with the given issuer, buyer, and terms.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `issuer` - The issuer address creating the invoice.
        // * `buyer` - The buyer address receiving the invoice.
        // * `face_value` - The full invoice value.
        // * `due_date` - The invoice due date timestamp.
        // * `funding_asset` - The asset to be used for financing.
        //
        // # Returns
        // * `BytesN<32>` - The generated invoice ID.
        //
        // # Panics
        // * `IssuerNotVerified` if the issuer is not verified in the registry.
        // * `BuyerNotVerified` if the buyer is not verified in the registry.
        // * `InvalidFaceValue` if `face_value` is zero.
        // * `InvalidDueDate` if `due_date` is not in the future.
        //
        // # Example
        // ```ignore
        // let invoice_id = client.create(&issuer, &buyer, 1_000, 1_000_000, &asset);
        // ```
        issuer.require_auth();
        if issuer == buyer {
            panic_with_error!(&env, InvoiceError::IssuerIsBuyer);
        }

        let registry_id: Address = env
            .storage()
            .instance()
            .get(&DataKey::RegistryContract)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotInitialized));

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

        if !env
            .storage()
            .persistent()
            .has(&DataKey::SupportedAsset(funding_asset.clone()))
        {
            panic_with_error!(&env, InvoiceError::UnsupportedAsset);
        }

        if face_value == 0 {
            panic_with_error!(&env, InvoiceError::InvalidFaceValue);
        }
        if due_date <= env.ledger().timestamp() {
            panic_with_error!(&env, InvoiceError::InvalidDueDate);
        }

        let counter: u64 = env
            .storage()
            .instance()
            .get(&DataKey::Counter)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotInitialized));
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
        persistent_set(&env, &inv_key, &invoice);

        self::extend_issuer_index(&env, &issuer, &invoice_id);
        self::extend_buyer_index(&env, &buyer, &invoice_id);
        self::extend_status_index(&env, InvoiceStatus::Created, &invoice_id);

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
        // Lists a created invoice for financing with a discount.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to list.
        // * `discount_bps` - The discount rate in basis points.
        //
        // # Returns
        // * `bool` - `true` when listing succeeds.
        //
        // # Panics
        // * `NotFound` if the invoice does not exist.
        // * `InvalidStatusTransition` if invoice status is not `Created`.
        // * `DiscountTooHigh` if `discount_bps` is greater than 5000.
        //
        // # Example
        // ```ignore
        // client.list_for_financing(&invoice_id, 250);
        // ```
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
        persistent_set(&env, &inv_key, &invoice);

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
        // Marks a listed invoice as funded by a pool.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice being funded.
        // * `pool_address` - The pool address authorizing funding.
        // * `asset_address` - The asset used to fund the invoice.
        // * `funded_amount` - The amount funded.
        //
        // # Returns
        // * `bool` - `true` when funding is recorded.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        // * `InvalidStatusTransition` if invoice status is not `Listed`.
        // * `UnsupportedAsset` if the asset does not match the invoice funding asset.
        //
        // # Example
        // ```ignore
        // client.mark_funded(&invoice_id, &pool, &asset, 950);
        // ```
        pool_address.require_auth();

        let expected_pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        if pool_address != expected_pool {
            panic_with_error!(&env, InvoiceError::UnauthorizedPool);
        }

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
        persistent_set(&env, &inv_key, &invoice);

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
        // Marks a funded invoice as shipped.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to mark as shipped.
        //
        // # Returns
        // * `bool` - `true` when shipment is recorded.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        // * `InvalidStatusTransition` if invoice status is not `Funded`.
        //
        // # Example
        // ```ignore
        // client.mark_shipped(&invoice_id);
        // ```
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
        persistent_set(&env, &inv_key, &invoice);

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
        // Confirms delivery for an active invoice by issuer or buyer.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice being confirmed.
        // * `confirmer` - The address confirming delivery.
        //
        // # Returns
        // * `bool` - `true` when confirmation is processed.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        // * `InvalidStatusTransition` if invoice status is not `Active`.
        // * `NotAuthorized` if the confirmer is neither issuer nor buyer.
        // * `AlreadyConfirmed` if the confirmer already confirmed.
        //
        // # Example
        // ```ignore
        // client.confirm_delivery(&invoice_id, &buyer);
        // ```
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

        persistent_set(&env, &inv_key, &invoice);
        events::delivery_confirmed(&env, &invoice_id, &confirmer);
        true
    }

    pub fn repay(env: Env, invoice_id: BytesN<32>) -> bool {
        // Repays a confirmed invoice, transferring funds to the pool.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice being repaid.
        //
        // # Returns
        // * `bool` - `true` when repayment is completed.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        // * `InvalidStatusTransition` if invoice status is not `Confirmed`.
        //
        // # Example
        // ```ignore
        // client.repay(&invoice_id);
        // ```
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
        let repayment_ok: bool =
            env.invoke_contract(&pool, &Symbol::new(&env, "receive_repayment"), args);
        if !repayment_ok {
            panic_with_error!(&env, InvoiceError::RepaymentFailed);
        }

        let mut updated = invoice;
        updated.status = InvoiceStatus::Repaid;
        updated.repaid_at = Some(env.ledger().timestamp());
        persistent_set(&env, &inv_key, &updated);

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
        // Triggers default on a past-due invoice.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to default.
        //
        // # Returns
        // * `bool` - `true` when default processing succeeds.
        //
        // # Panics
        // * `NotFound` if the admin or invoice cannot be found.
        // * `InvalidStatusTransition` if invoice is not Funded, Active, or Confirmed.
        // * `DueDateNotPassed` if the invoice due date has not yet passed.
        //
        // # Example
        // ```ignore
        // client.trigger_default(&invoice_id);
        // ```
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
        persistent_set(&env, &inv_key, &invoice);

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
        // Returns the status code of an invoice.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to query.
        //
        // # Returns
        // * `u32` - The invoice status as a numeric code.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        //
        // # Example
        // ```ignore
        // let status = client.get_status(&invoice_id);
        // ```
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.status as u32
    }

    pub fn get_face_value(env: Env, invoice_id: BytesN<32>) -> u128 {
        // Returns the face value of an invoice.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to query.
        //
        // # Returns
        // * `u128` - The invoice face value.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        //
        // # Example
        // ```ignore
        // let face_value = client.get_face_value(&invoice_id);
        // ```
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.face_value
    }

    pub fn get_discount_bps(env: Env, invoice_id: BytesN<32>) -> u32 {
        // Returns the discount basis points for an invoice.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to query.
        //
        // # Returns
        // * `u32` - The discount rate in basis points.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        //
        // # Example
        // ```ignore
        // let discount = client.get_discount_bps(&invoice_id);
        // ```
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.discount_bps
    }

    pub fn get_funding_asset(env: Env, invoice_id: BytesN<32>) -> Address {
        // Returns the funding asset for an invoice.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to query.
        //
        // # Returns
        // * `Address` - The funding asset address.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        //
        // # Example
        // ```ignore
        // let asset = client.get_funding_asset(&invoice_id);
        // ```
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.funding_asset
    }

    pub fn get(env: Env, invoice_id: BytesN<32>) -> Invoice {
        // Retrieves the full invoice record by ID.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to retrieve.
        //
        // # Returns
        // * `Invoice` - The full invoice object.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        //
        // # Example
        // ```ignore
        // let invoice = client.get(&invoice_id);
        // ```
        env.storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound))
    }

    pub fn get_by_status(env: Env, status: InvoiceStatus, offset: u32, limit: u32) -> Vec<Invoice> {
        let status_u32 = status as u32;
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::StatusIndexCount(status_u32))
            .unwrap_or(0);
        let mut result: Vec<Invoice> = Vec::new(&env);
        let end = core::cmp::min(offset.saturating_add(limit), count);
        for i in offset..end {
            let id: BytesN<32> = env
                .storage()
                .persistent()
                .get(&DataKey::StatusIndexEntry(status_u32, i))
                .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
            // O(1) membership check instead of loading full invoice
            let is_member: bool = env
                .storage()
                .persistent()
                .get(&DataKey::StatusMembership(status_u32, id.clone()))
                .unwrap_or(false);
            if is_member {
                let invoice: Invoice = env
                    .storage()
                    .persistent()
                    .get(&DataKey::Invoice(id))
                    .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
                if invoice.status == status {
                    result.push_back(invoice);
                }
            }
        }
        result
    }

    pub fn get_by_issuer(env: Env, address: Address, offset: u32, limit: u32) -> Vec<Invoice> {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::IssuerIndexCount(address.clone()))
            .unwrap_or(0);
        let mut result: Vec<Invoice> = Vec::new(&env);
        let end = core::cmp::min(offset.saturating_add(limit), count);
        for i in offset..end {
            let id: BytesN<32> = env
                .storage()
                .persistent()
                .get(&DataKey::IssuerIndexEntry(address.clone(), i))
                .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
            let invoice: Invoice = env
                .storage()
                .persistent()
                .get(&DataKey::Invoice(id))
                .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
            result.push_back(invoice);
        }
        result
    }

    pub fn get_by_buyer(env: Env, address: Address, offset: u32, limit: u32) -> Vec<Invoice> {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::BuyerIndexCount(address.clone()))
            .unwrap_or(0);
        let mut result: Vec<Invoice> = Vec::new(&env);
        let end = core::cmp::min(offset.saturating_add(limit), count);
        for i in offset..end {
            let id: BytesN<32> = env
                .storage()
                .persistent()
                .get(&DataKey::BuyerIndexEntry(address.clone(), i))
                .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
            let invoice: Invoice = env
                .storage()
                .persistent()
                .get(&DataKey::Invoice(id))
                .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
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
        events::expiry_window_set(&env, window);
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

    pub fn get_issuer(env: Env, invoice_id: BytesN<32>) -> Address {
        // Returns the issuer address for an invoice.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to query.
        //
        // # Returns
        // * `Address` - The issuer address.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        //
        // # Example
        // ```ignore
        // let issuer = client.get_issuer(&invoice_id);
        // ```
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.issuer
    }

    pub fn transfer_ownership(env: Env, new_admin: Address) {
        // Transfers admin ownership to a new address.
        //
        // Requires authentication from BOTH the current admin and the incoming
        // new admin, preventing accidental transfers to wrong addresses.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `new_admin` - The address that will become the new admin.
        //
        // # Panics
        // * `NotFound` if the admin is not set.
        //
        // # Example
        // ```ignore
        // client.transfer_ownership(&new_admin);
        // ```
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        admin.require_auth();
        new_admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        events::ownership_transferred(&env, &admin, &new_admin);
        Self::extend_instance_ttl(&env);
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

        let prev_status = invoice.status;
        invoice.status = InvoiceStatus::Expired;
        env.storage().persistent().set(&inv_key, &invoice);

        self::move_status_index(&env, &invoice_id, prev_status, InvoiceStatus::Expired);
        events::invoice_expired(&env, &invoice_id);
        true
    }

    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }
}

fn extend_issuer_index(env: &Env, issuer: &Address, invoice_id: &BytesN<32>) {
    let count_key = DataKey::IssuerIndexCount(issuer.clone());
    let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
    let entry_key = DataKey::IssuerIndexEntry(issuer.clone(), count);
    persistent_set(env, &entry_key, invoice_id);
    persistent_set(env, &count_key, &(count + 1));
}

fn extend_buyer_index(env: &Env, buyer: &Address, invoice_id: &BytesN<32>) {
    let count_key = DataKey::BuyerIndexCount(buyer.clone());
    let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
    let entry_key = DataKey::BuyerIndexEntry(buyer.clone(), count);
    persistent_set(env, &entry_key, invoice_id);
    persistent_set(env, &count_key, &(count + 1));
}

fn extend_status_index(env: &Env, status: InvoiceStatus, invoice_id: &BytesN<32>) {
    let status_u32 = status as u32;

    // Set membership marker for O(1) lookups
    let membership_key = DataKey::StatusMembership(status_u32, invoice_id.clone());
    env.storage().persistent().set(&membership_key, &true);
    env.storage()
        .persistent()
        .extend_ttl(&membership_key, 100, 2_000_000);

    // Increment count
    let count_key = DataKey::StatusIndexCount(status_u32);
    let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
    let entry_key = DataKey::StatusIndexEntry(status_u32, count);
    env.storage().persistent().set(&entry_key, invoice_id);
    env.storage()
        .persistent()
        .extend_ttl(&entry_key, 100, 2_000_000);
    env.storage().persistent().set(&count_key, &(count + 1));
    env.storage()
        .persistent()
        .extend_ttl(&count_key, 100, 2_000_000);
}

fn move_status_index(env: &Env, invoice_id: &BytesN<32>, from: InvoiceStatus, to: InvoiceStatus) {
    let from_u32 = from as u32;

    // Remove from old status - O(1) operation
    let membership_key = DataKey::StatusMembership(from_u32, invoice_id.clone());
    env.storage().persistent().remove(&membership_key);

    // Add to new status - O(1) operation
    extend_status_index(env, to, invoice_id);
}
