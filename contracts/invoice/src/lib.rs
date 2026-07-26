#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, token, xdr::ToXdr, Address, Bytes, BytesN, Env,
    IntoVal, Map, String, Symbol, Vec,
};

mod errors;
mod events;
mod test;
mod types;

pub use errors::*;
pub use types::*;

/// Upper bound on `Invoice::face_value`, in USDC stroops.
///
/// Chosen so that `face_value * 10_000` (the scaling factor used by
/// downstream discount/utilization math in the pool contract, e.g.
/// `face_value * (10_000 - discount_bps) / 10_000`) can never overflow
/// `u128`, preventing arithmetic overflow in the pool when it consumes
/// this value.
pub const MAX_FACE_VALUE: u128 = u128::MAX / 10_000;

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
    /// # Auth
    /// Requires authorization from `admin`.
    ///
    /// # Panics
    /// * `InvoiceError::AlreadyInitialized` if the contract has already been initialized.
    ///
    /// # Returns
    /// * `()` - No value is returned.
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
        Self::extend_instance_ttl(&env);
    }

    /// Sets the pool contract address used by this invoice contract.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `pool_contract` - The pool contract address.
    ///
    /// # Auth
    /// Requires authorization from the stored admin address.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the admin is not initialized.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Example
    /// ```ignore
    /// client.set_pool_contract(&pool_address);
    /// ```
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
        events::pool_contract_set(&env, &pool_contract);
    }

    /// Creates a new invoice with the given issuer, buyer, and terms.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `issuer` - The issuer address creating the invoice.
    /// * `buyer` - The buyer address receiving the invoice.
    /// * `face_value` - The full invoice value.
    /// * `due_date` - The invoice due date timestamp.
    /// * `funding_asset` - The asset to be used for financing.
    ///
    /// # Auth
    /// Requires authorization from `issuer`.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the contract has not been initialized.
    /// * `InvoiceError::IssuerNotVerified` if the issuer is not verified in the registry.
    /// * `InvoiceError::BuyerNotVerified` if the buyer is not verified in the registry.
    /// * `InvoiceError::InvalidFaceValue` if `face_value` is zero.
    /// * `InvoiceError::InvalidAmount` if `face_value` exceeds [`MAX_FACE_VALUE`].
    /// * `InvoiceError::InvalidDueDate` if `due_date` is not in the future.
    /// * `InvoiceError::CounterOverflow` if the internal invoice counter overflows.
    ///
    /// # Returns
    /// * `BytesN<32>` - The generated invoice ID.
    ///
    /// # Example
    /// ```ignore
    /// let invoice_id = client.create(&issuer, &buyer, 1_000, 1_000_000, &asset);
    /// ```
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
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));

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
        if face_value > MAX_FACE_VALUE {
            panic_with_error!(&env, InvoiceError::InvalidAmount);
        }
        if due_date <= env.ledger().timestamp() {
            panic_with_error!(&env, InvoiceError::InvalidDueDate);
        }

        let counter: u64 = env
            .storage()
            .instance()
            .get(&DataKey::Counter)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        let next_counter = counter
            .checked_add(1)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::CounterOverflow));
        env.storage()
            .instance()
            .set(&DataKey::Counter, &next_counter);

        let now = env.ledger().timestamp();
        let mut hash_input = Bytes::new(&env);
        let issuer_xdr = issuer.clone().to_xdr(&env);
        let buyer_xdr = buyer.clone().to_xdr(&env);

        // Safely append all XDR bytes without assuming a fixed length
        for b in issuer_xdr.iter() {
            hash_input.push_back(b);
        }
        for b in buyer_xdr.iter() {
            hash_input.push_back(b);
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
            for b in asset_xdr.iter() {
                hash_input.push_back(b);
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

        self::extend_issuer_index(&env, &issuer, &invoice_id);
        self::extend_buyer_index(&env, &buyer, &invoice_id);
        self::extend_status_index(&env, InvoiceStatus::Created, &invoice_id);
        increment_status_count(&env, InvoiceStatus::Created);
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

    /// Lists a created invoice for financing with a discount.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to list.
    /// * `discount_bps` - The discount rate in basis points.
    ///
    /// # Auth
    /// Requires authorization from the invoice's issuer.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice does not exist.
    /// * `InvoiceError::InvalidStatusTransition` if invoice status is not `Created`.
    /// * `InvoiceError::DiscountTooHigh` if `discount_bps` is greater than 5000.
    ///
    /// # Returns
    /// * `bool` - `true` when listing succeeds.
    ///
    /// # Example
    /// ```ignore
    /// client.list_for_financing(&invoice_id, 250);
    /// ```
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

        move_status_index(
            &env,
            &invoice_id,
            InvoiceStatus::Created,
            InvoiceStatus::Listed,
        );
        events::invoice_listed(&env, &invoice_id, discount_bps);
        true
    }

    /// Marks a listed invoice as funded by a pool.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice being funded.
    /// * `pool_address` - The pool address authorizing funding.
    /// * `asset_address` - The asset used to fund the invoice.
    /// * `funded_amount` - The amount funded.
    ///
    /// # Auth
    /// Requires authorization from `pool_address`.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found.
    /// * `InvoiceError::InvalidStatusTransition` if invoice status is not `Listed`.
    /// * `InvoiceError::UnsupportedAsset` if the asset does not match the invoice funding asset.
    ///
    /// # Returns
    /// * `bool` - `true` when funding is recorded.
    ///
    /// # Example
    /// ```ignore
    /// client.mark_funded(&invoice_id, &pool, &asset, 950);
    /// ```
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

        move_status_index(
            &env,
            &invoice_id,
            InvoiceStatus::Listed,
            InvoiceStatus::Funded,
        );
        events::invoice_funded(&env, &invoice_id, funded_amount);
        true
    }

    /// Marks a funded invoice as shipped.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to mark as shipped.
    ///
    /// # Auth
    /// Requires authorization from the invoice's issuer.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found.
    /// * `InvoiceError::InvalidStatusTransition` if invoice status is not `Funded`.
    ///
    /// # Returns
    /// * `bool` - `true` when shipment is recorded.
    ///
    /// # Example
    /// ```ignore
    /// client.mark_shipped(&invoice_id);
    /// ```
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

        move_status_index(
            &env,
            &invoice_id,
            InvoiceStatus::Funded,
            InvoiceStatus::Active,
        );
        events::invoice_shipped(&env, &invoice_id);
        true
    }

    /// Confirms delivery for an active invoice by issuer or buyer.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice being confirmed.
    /// * `confirmer` - The address confirming delivery.
    ///
    /// # Auth
    /// Requires authorization from `confirmer`.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found.
    /// * `InvoiceError::InvalidStatusTransition` if invoice status is not `Active`.
    /// * `InvoiceError::NotAuthorized` if the confirmer is neither issuer nor buyer.
    /// * `InvoiceError::AlreadyConfirmed` if the confirmer already confirmed.
    ///
    /// # Returns
    /// * `bool` - `true` when confirmation is processed.
    ///
    /// # Example
    /// ```ignore
    /// client.confirm_delivery(&invoice_id, &buyer);
    /// ```
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
            move_status_index(
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

    /// Repays a confirmed invoice, transferring funds to the pool.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice being repaid.
    ///
    /// # Auth
    /// Requires authorization from the invoice's buyer.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found, or if the invoice has no
    ///   recorded funding pool or funding timestamp.
    /// * `InvoiceError::InvalidStatusTransition` if invoice status is not `Confirmed`.
    ///
    /// # Returns
    /// * `bool` - `true` when repayment is completed.
    ///
    /// # Example
    /// ```ignore
    /// client.repay(&invoice_id);
    /// ```
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
        let funded_amount = invoice.funded_amount;
        let funding_asset = invoice.funding_asset.clone();

        let now = env.ledger().timestamp();
        let funded_at = invoice
            .funded_at
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        let discount = face_value.saturating_sub(funded_amount);
        let term = invoice.due_date.saturating_sub(funded_at);
        let elapsed = now.saturating_sub(funded_at);
        let earned_by_pool = if term == 0 {
            discount
        } else {
            discount * (elapsed as u128) / (term as u128)
        };
        let refund_to_buyer = discount.saturating_sub(earned_by_pool);

        let buyer = invoice.buyer.clone();

        let token = token::Client::new(&env, &funding_asset);
        token.transfer(&buyer, &pool, &(face_value as i128));

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        args.push_back(face_value.into_val(&env));
        args.push_back(refund_to_buyer.into_val(&env));
        args.push_back(buyer.into_val(&env));
        let _: bool = env.invoke_contract(
            &pool,
            &Symbol::new(&env, "receive_repayment_with_refund"),
            args,
        );

        let mut updated = invoice;
        updated.status = InvoiceStatus::Repaid;
        updated.repaid_at = Some(env.ledger().timestamp());
        env.storage().persistent().set(&inv_key, &updated);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        Self::extend_instance_ttl(&env);

        move_status_index(
            &env,
            &invoice_id,
            InvoiceStatus::Confirmed,
            InvoiceStatus::Repaid,
        );
        events::invoice_repaid(&env, &invoice_id, updated.face_value);
        true
    }

    pub fn repay_early(env: Env, invoice_id: BytesN<32>) -> bool {
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
        let discount_bps = invoice.discount_bps as u128;
        let funded_amount = face_value * (10000u128 - discount_bps) / 10000u128;
        let discount = face_value.saturating_sub(funded_amount);

        let funded_at = invoice
            .funded_at
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        let now = env.ledger().timestamp();
        if now >= invoice.due_date {
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }

        let term = invoice.due_date.saturating_sub(funded_at);
        let elapsed = now.saturating_sub(funded_at);

        let earned_by_pool = if term == 0 {
            discount
        } else {
            discount * (elapsed as u128) / (term as u128)
        };
        let refund_to_buyer = discount.saturating_sub(earned_by_pool);

        let buyer = invoice.buyer.clone();
        let funding_asset = invoice.funding_asset.clone();

        let token = token::Client::new(&env, &funding_asset);
        token.transfer(&buyer, &pool, &(face_value as i128));

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        args.push_back(face_value.into_val(&env));
        args.push_back(refund_to_buyer.into_val(&env));
        args.push_back(buyer.into_val(&env));
        let _: bool = env.invoke_contract(
            &pool,
            &Symbol::new(&env, "receive_repayment_with_refund"),
            args,
        );

        let mut updated = invoice;
        updated.status = InvoiceStatus::Repaid;
        updated.repaid_at = Some(now);
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

    /// Triggers default on a past-due invoice.
    ///
    /// Default is permitted once `now >= due_date` — the due date has been
    /// reached or passed. This is consistent with the `create` check that
    /// rejects `due_date <= now` (due dates must be in the future).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to default.
    ///
    /// # Auth
    /// Requires authorization from the stored admin address.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the admin, invoice, or funding pool cannot be found.
    /// * `InvoiceError::InvalidStatusTransition` if invoice is not `Funded`, `Active`, or `Confirmed`.
    /// * `InvoiceError::DueDateNotPassed` if `now < due_date` — the due date
    ///   has not yet been reached.
    ///
    /// # Returns
    /// * `bool` - `true` when default processing succeeds.
    ///
    /// # Example
    /// ```ignore
    /// client.trigger_default(&invoice_id);
    /// ```
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
        if env.ledger().timestamp() < invoice.due_date {
            panic_with_error!(&env, InvoiceError::DueDateNotPassed);
        }

        let prev_status = invoice.status;
        invoice.status = InvoiceStatus::Defaulted;
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        Self::extend_instance_ttl(&env);

        move_status_index(&env, &invoice_id, prev_status, InvoiceStatus::Defaulted);

        let pool: Address = invoice
            .funding_pool
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let _: bool = env.invoke_contract(&pool, &Symbol::new(&env, "handle_default"), args);
        events::invoice_defaulted(&env, &invoice_id);
        true
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
        Self::extend_instance_ttl(&env);
    }

    /// Returns the current listing expiry window in seconds.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `u64` - The expiry window in seconds. Defaults to 7 days (`7 * 24 * 60 * 60`) if unset.
    ///
    /// # Example
    /// ```ignore
    /// let window = client.get_expiry_window();
    /// ```
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

        if !is_issuer {
            admin.require_auth();
        }

        let listed_at = invoice.listed_at.unwrap_or(0);
        let expiry_window = env
            .storage()
            .instance()
            .get(&DataKey::ExpiryWindow)
            .unwrap_or(7 * 24 * 60 * 60);
        let current_time = env.ledger().timestamp();
        let deadline = listed_at
            .checked_add(expiry_window)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::MathOverflow));

        if current_time < deadline {
            panic_with_error!(&env, InvoiceError::ListingNotExpired);
        }

        let prev_status = invoice.status;
        invoice.status = InvoiceStatus::Expired;
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        Self::extend_instance_ttl(&env);

        move_status_index(&env, &invoice_id, prev_status, InvoiceStatus::Expired);
        events::invoice_expired(&env, &invoice_id);
        true
    }

    /// Returns the status code of an invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to query.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found.
    ///
    /// # Returns
    /// * `u32` - The invoice status as a numeric code.
    ///
    /// # Example
    /// ```ignore
    /// let status = client.get_status(&invoice_id);
    /// ```
    pub fn get_status(env: Env, invoice_id: BytesN<32>) -> u32 {
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.status as u32
    }

    /// Returns the face value of an invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to query.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found.
    ///
    /// # Returns
    /// * `u128` - The invoice face value.
    ///
    /// # Example
    /// ```ignore
    /// let face_value = client.get_face_value(&invoice_id);
    /// ```
    pub fn get_face_value(env: Env, invoice_id: BytesN<32>) -> u128 {
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.face_value
    }

    /// Returns the discount basis points for an invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to query.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found.
    ///
    /// # Returns
    /// * `u32` - The discount rate in basis points.
    ///
    /// # Example
    /// ```ignore
    /// let discount = client.get_discount_bps(&invoice_id);
    /// ```
    pub fn get_discount_bps(env: Env, invoice_id: BytesN<32>) -> u32 {
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.discount_bps
    }

    /// Returns the funding asset for an invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to query.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found.
    ///
    /// # Returns
    /// * `Address` - The funding asset address.
    ///
    /// # Example
    /// ```ignore
    /// let asset = client.get_funding_asset(&invoice_id);
    /// ```
    pub fn get_funding_asset(env: Env, invoice_id: BytesN<32>) -> Address {
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.funding_asset
    }

    /// Retrieves the full invoice record by ID.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to retrieve.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found.
    ///
    /// # Returns
    /// * `Invoice` - The full invoice object.
    ///
    /// # Example
    /// ```ignore
    /// let invoice = client.get(&invoice_id);
    /// ```
    pub fn get(env: Env, invoice_id: BytesN<32>) -> Invoice {
        env.storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound))
    }

    /// Lists invoices for a given status.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `status` - The invoice status filter.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `Vec<Invoice>` - The invoices matching the status.
    ///
    /// # Example
    /// ```ignore
    /// let invoices = client.get_by_status(InvoiceStatus::Created);
    /// ```
    pub fn get_by_status(env: Env, status: InvoiceStatus) -> Vec<Invoice> {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::StatusIndexCount(status as u32))
            .unwrap_or(0);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for i in 0..count {
            let id: BytesN<32> = env
                .storage()
                .persistent()
                .get(&DataKey::StatusIndexEntry(status as u32, i))
                .unwrap();
            let invoice: Invoice = env
                .storage()
                .persistent()
                .get(&DataKey::Invoice(id))
                .unwrap();
            if invoice.status == status {
                result.push_back(invoice);
            }
        }
        result
    }

    /// Lists invoices created by a given issuer.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The issuer address.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `Vec<Invoice>` - The invoices for the issuer.
    ///
    /// # Example
    /// ```ignore
    /// let invoices = client.get_by_issuer(&issuer);
    /// ```
    pub fn get_by_issuer(env: Env, address: Address) -> Vec<Invoice> {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::IssuerIndexCount(address.clone()))
            .unwrap_or(0);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for i in 0..count {
            let id: BytesN<32> = env
                .storage()
                .persistent()
                .get(&DataKey::IssuerIndexEntry(address.clone(), i))
                .unwrap();
            let invoice: Invoice = env
                .storage()
                .persistent()
                .get(&DataKey::Invoice(id))
                .unwrap();
            result.push_back(invoice);
        }
        result
    }

    /// Lists invoices associated with a given buyer.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `address` - The buyer address.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `Vec<Invoice>` - The invoices for the buyer.
    ///
    /// # Example
    /// ```ignore
    /// let invoices = client.get_by_buyer(&buyer);
    /// ```
    pub fn get_by_buyer(env: Env, address: Address) -> Vec<Invoice> {
        let count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::BuyerIndexCount(address.clone()))
            .unwrap_or(0);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for i in 0..count {
            let id: BytesN<32> = env
                .storage()
                .persistent()
                .get(&DataKey::BuyerIndexEntry(address.clone(), i))
                .unwrap();
            let invoice: Invoice = env
                .storage()
                .persistent()
                .get(&DataKey::Invoice(id))
                .unwrap();
            result.push_back(invoice);
        }
        result
    }

    /// Returns a map of invoice counts keyed by status name.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `Map<String, u64>` - Counts for each `InvoiceStatus` variant.
    ///
    /// # Example
    /// ```ignore
    /// let counts = client.get_counts();
    /// ```
    pub fn get_counts(env: Env) -> Map<String, u64> {
        let mut counts: Map<String, u64> = Map::new(&env);
        let statuses = [
            InvoiceStatus::Created,
            InvoiceStatus::Listed,
            InvoiceStatus::Funded,
            InvoiceStatus::Active,
            InvoiceStatus::Confirmed,
            InvoiceStatus::Repaid,
            InvoiceStatus::Defaulted,
            InvoiceStatus::Expired,
        ];
        for status in statuses {
            let key = String::from_str(&env, status.as_str());
            let value = read_status_count(&env, status);
            counts.set(key, value);
        }
        counts
    }

    /// Returns the issuer address for an invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to query.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found.
    ///
    /// # Returns
    /// * `Address` - The issuer address.
    ///
    /// # Example
    /// ```ignore
    /// let issuer = client.get_issuer(&invoice_id);
    /// ```
    pub fn get_issuer(env: Env, invoice_id: BytesN<32>) -> Address {
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        invoice.issuer
    }

    pub fn transfer_ownership(env: Env, new_admin: Address) {
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

    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }
}

fn extend_issuer_index(env: &Env, issuer: &Address, invoice_id: &BytesN<32>) {
    let count_key = DataKey::IssuerIndexCount(issuer.clone());
    let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
    let entry_key = DataKey::IssuerIndexEntry(issuer.clone(), count);
    env.storage().persistent().set(&entry_key, invoice_id);
    env.storage().persistent().set(&count_key, &(count + 1));
    env.storage()
        .persistent()
        .extend_ttl(&entry_key, 100, 2_000_000);
    env.storage()
        .persistent()
        .extend_ttl(&count_key, 100, 2_000_000);
}

fn extend_buyer_index(env: &Env, buyer: &Address, invoice_id: &BytesN<32>) {
    let count_key = DataKey::BuyerIndexCount(buyer.clone());
    let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
    let entry_key = DataKey::BuyerIndexEntry(buyer.clone(), count);
    env.storage().persistent().set(&entry_key, invoice_id);
    env.storage().persistent().set(&count_key, &(count + 1));
    env.storage()
        .persistent()
        .extend_ttl(&entry_key, 100, 2_000_000);
    env.storage()
        .persistent()
        .extend_ttl(&count_key, 100, 2_000_000);
}

fn extend_status_index(env: &Env, status: InvoiceStatus, invoice_id: &BytesN<32>) {
    let status_u32 = status as u32;
    let count_key = DataKey::StatusIndexCount(status_u32);
    let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
    let entry_key = DataKey::StatusIndexEntry(status_u32, count);
    env.storage().persistent().set(&entry_key, invoice_id);
    env.storage().persistent().set(&count_key, &(count + 1));
    env.storage()
        .persistent()
        .extend_ttl(&entry_key, 100, 2_000_000);
    env.storage()
        .persistent()
        .extend_ttl(&count_key, 100, 2_000_000);
}

fn move_status_index(env: &Env, invoice_id: &BytesN<32>, from: InvoiceStatus, to: InvoiceStatus) {
    decrement_status_count(env, from);
    increment_status_count(env, to);
    extend_status_index(env, to, invoice_id);
}

fn increment_status_count(env: &Env, status: InvoiceStatus) {
    let key = DataKey::StatusCount(status as u32);
    let current: u64 = env.storage().persistent().get(&key).unwrap_or(0u64);
    env.storage().persistent().set(&key, &(current + 1));
}

fn decrement_status_count(env: &Env, status: InvoiceStatus) {
    let key = DataKey::StatusCount(status as u32);
    let current: u64 = env.storage().persistent().get(&key).unwrap_or(0u64);
    let next = current
        .checked_sub(1)
        .unwrap_or_else(|| panic_with_error!(env, InvoiceError::InvalidStatusTransition));
    env.storage().persistent().set(&key, &next);
}

fn read_status_count(env: &Env, status: InvoiceStatus) -> u64 {
    env.storage()
        .persistent()
        .get(&DataKey::StatusCount(status as u32))
        .unwrap_or(0u64)
}
