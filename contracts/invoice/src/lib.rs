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
        Self::extend_instance_ttl(&env);
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
        let asset_xdr = funding_asset.clone().to_xdr(&env);
        // Use full XDR bytes — prior code only took first 32 bytes which risks
        // collisions when addresses share an XDR prefix (#65).
        for i in 0..issuer_xdr.len() {
            hash_input.push_back(issuer_xdr.get(i).unwrap());
        }
        for i in 0..buyer_xdr.len() {
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
        for i in 0..asset_xdr.len() {
            hash_input.push_back(asset_xdr.get(i).unwrap());
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

        // Per-field keys for gas-efficient cross-contract reads (#62)
        write_field_status(&env, &invoice_id, InvoiceStatus::Created);
        write_field_face_value(&env, &invoice_id, face_value);
        write_field_discount_bps(&env, &invoice_id, 0u32);
        write_field_funding_asset(&env, &invoice_id, &funding_asset);

        self::extend_issuer_index(&env, &issuer, &invoice_id);
        self::extend_buyer_index(&env, &buyer, &invoice_id);
        self::extend_status_index(&env, InvoiceStatus::Created, &invoice_id);
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
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        write_field_status(&env, &invoice_id, InvoiceStatus::Listed);
        write_field_discount_bps(&env, &invoice_id, discount_bps);
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
        write_field_status(&env, &invoice_id, InvoiceStatus::Funded);
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
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        write_field_status(&env, &invoice_id, InvoiceStatus::Active);
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
            write_field_status(&env, &invoice_id, InvoiceStatus::Confirmed);
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

    pub fn unconfirm_delivery(env: Env, invoice_id: BytesN<32>) -> bool {
        // Reverts a Confirmed invoice back to Active, clearing both confirmation flags.
        // Requires authorisation from BOTH the issuer and the buyer, OR from the admin.
        // This guards against either party unilaterally undoing the other's confirmation.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to revert.
        //
        // # Returns
        // * `bool` - `true` when the reversion succeeds.
        //
        // # Panics
        // * `NotFound` if the invoice or admin cannot be found.
        // * `InvalidStatusTransition` if invoice status is not `Confirmed`.
        // * `NotAuthorized` if neither the dual-party nor admin authorisation is satisfied.
        //
        // # Example
        // ```ignore
        // client.unconfirm_delivery(&invoice_id);
        // ```
        let inv_key = DataKey::Invoice(invoice_id.clone());
        let mut invoice: Invoice = env
            .storage()
            .persistent()
            .get(&inv_key)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));

        if invoice.status != InvoiceStatus::Confirmed {
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }

        // Accept dual-party auth (both issuer AND buyer must sign) or admin auth.
        // We use try_invoke_contract to test issuer+buyer auth without aborting on failure,
        // then fall back to the admin key as an emergency escape hatch.
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));

        let issuer_auth_ok = env
            .try_invoke_contract::<(), soroban_sdk::Error>(
                &env.current_contract_address(),
                &Symbol::new(&env, "check_auth"),
                (invoice.issuer.clone(),).into_val(&env),
            )
            .is_ok();
        let buyer_auth_ok = env
            .try_invoke_contract::<(), soroban_sdk::Error>(
                &env.current_contract_address(),
                &Symbol::new(&env, "check_auth"),
                (invoice.buyer.clone(),).into_val(&env),
            )
            .is_ok();

        if issuer_auth_ok && buyer_auth_ok {
            // Both parties consented — no additional require_auth needed.
        } else {
            // Fall back to admin auth.
            admin.require_auth();
        }

        invoice.issuer_confirmed = false;
        invoice.buyer_confirmed = false;
        invoice.status = InvoiceStatus::Active;

        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        write_field_status(&env, &invoice_id, InvoiceStatus::Active);

        self::move_status_index(
            &env,
            &invoice_id,
            InvoiceStatus::Confirmed,
            InvoiceStatus::Active,
        );

        Self::extend_instance_ttl(&env);
        events::delivery_unconfirmed(&env, &invoice_id);
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
        let _: bool = env.invoke_contract(&pool, &Symbol::new(&env, "receive_repayment"), args);

        let mut updated = invoice;
        updated.status = InvoiceStatus::Repaid;
        updated.repaid_at = Some(env.ledger().timestamp());
        env.storage().persistent().set(&inv_key, &updated);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        write_field_status(&env, &invoice_id, InvoiceStatus::Repaid);
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
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        write_field_status(&env, &invoice_id, InvoiceStatus::Defaulted);
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
        // Returns the status code of an invoice from its dedicated per-field key (#62).
        env.storage()
            .persistent()
            .get::<_, InvoiceStatus>(&DataKey::FieldStatus(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound))
            as u32
    }

    pub fn get_face_value(env: Env, invoice_id: BytesN<32>) -> u128 {
        // Returns the face value of an invoice from its dedicated per-field key (#62).
        env.storage()
            .persistent()
            .get::<_, u128>(&DataKey::FieldFaceValue(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound))
    }

    pub fn get_discount_bps(env: Env, invoice_id: BytesN<32>) -> u32 {
        // Returns the discount basis points from its dedicated per-field key (#62).
        env.storage()
            .persistent()
            .get::<_, u32>(&DataKey::FieldDiscountBps(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound))
    }

    pub fn get_funding_asset(env: Env, invoice_id: BytesN<32>) -> Address {
        // Returns the funding asset address from its dedicated per-field key (#62).
        env.storage()
            .persistent()
            .get::<_, Address>(&DataKey::FieldFundingAsset(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound))
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

    pub fn get_expiry_window(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::ExpiryWindow)
            .unwrap_or(7 * 24 * 60 * 60)
    }

    pub fn check_auth(_env: Env, address: Address) {
        address.require_auth();
    }

    // `caller` must be either the invoice issuer or the contract admin (#63).
    // Replaces the fragile try_invoke_contract pattern that silently swallowed
    // non-auth errors and exposed `check_auth` as an unintended public entry point.
    pub fn expire_listing(env: Env, invoice_id: BytesN<32>, caller: Address) -> bool {
        caller.require_auth();

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

        if caller != invoice.issuer && caller != admin {
            panic_with_error!(&env, InvoiceError::NotAuthorized);
        }

        let listed_at = invoice.listed_at.unwrap_or(0);
        let expiry_window: u64 = env
            .storage()
            .instance()
            .get(&DataKey::ExpiryWindow)
            .unwrap_or(7 * 24 * 60 * 60);

        if env.ledger().timestamp() <= listed_at + expiry_window {
            panic_with_error!(&env, InvoiceError::ListingNotExpired);
        }

        invoice.status = InvoiceStatus::Expired;
        env.storage().persistent().set(&inv_key, &invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, 100, 2_000_000);
        write_field_status(&env, &invoice_id, InvoiceStatus::Expired);
        Self::extend_instance_ttl(&env);

        self::move_status_index(&env, &invoice_id, InvoiceStatus::Listed, InvoiceStatus::Expired);
        events::invoice_expired(&env, &invoice_id);
        true
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

fn move_status_index(env: &Env, invoice_id: &BytesN<32>, _from: InvoiceStatus, to: InvoiceStatus) {
    extend_status_index(env, to, invoice_id);
}

// Per-field write helpers — keep field keys in sync with the main Invoice struct (#62).
fn write_field_status(env: &Env, invoice_id: &BytesN<32>, status: InvoiceStatus) {
    let key = DataKey::FieldStatus(invoice_id.clone());
    env.storage().persistent().set(&key, &status);
    env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
}

fn write_field_face_value(env: &Env, invoice_id: &BytesN<32>, value: u128) {
    let key = DataKey::FieldFaceValue(invoice_id.clone());
    env.storage().persistent().set(&key, &value);
    env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
}

fn write_field_discount_bps(env: &Env, invoice_id: &BytesN<32>, bps: u32) {
    let key = DataKey::FieldDiscountBps(invoice_id.clone());
    env.storage().persistent().set(&key, &bps);
    env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
}

fn write_field_funding_asset(env: &Env, invoice_id: &BytesN<32>, asset: &Address) {
    let key = DataKey::FieldFundingAsset(invoice_id.clone());
    env.storage().persistent().set(&key, asset);
    env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
}

impl InvoiceContract {
    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }
}
