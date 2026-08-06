#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, token, xdr::ToXdr, Address, Bytes, BytesN, Env,
    IntoVal, Map, String, Symbol, Vec,
};

mod constants;
mod errors;
mod events;
mod test;
mod types;

pub use constants::*;
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

/// Maximum allowed invoice lifetime in seconds.
///
/// This caps `due_date` to `now + MAX_INVOICE_LIFETIME_SECONDS` to reject
/// far-future due dates that are effectively garbage data (e.g., centuries
/// in the future). The value is ~10 years (10 * 365 * 24 * 60 * 60).
pub const MAX_INVOICE_LIFETIME_SECONDS: u64 = 10 * 365 * 24 * 60 * 60;

#[contract]
pub struct InvoiceContract;

#[contractimpl]
impl InvoiceContract {
    fn save_invoice(env: &Env, inv_key: DataKey, invoice: &Invoice) {
        env.storage().persistent().set(&inv_key, invoice);
        env.storage()
            .persistent()
            .extend_ttl(&inv_key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }

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
        events::contract_initialized(&env, &admin, &registry_contract);
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
        let old_pool: Option<Address> = env.storage().instance().get(&DataKey::PoolContract);
        env.storage()
            .instance()
            .set(&DataKey::PoolContract, &pool_contract);
        if let Some(old) = old_pool {
            events::pool_contract_updated(&env, &old, &pool_contract);
        } else {
            events::pool_contract_updated(&env, &pool_contract, &pool_contract);
        }
    }

    /// Sets the escrow contract address used by this invoice contract.
    ///
    /// The escrow address is required so that `repay` and `repay_early` can
    /// route buyer funds through escrow (buyer → escrow → pool) instead of
    /// transferring directly to the pool, which would bypass escrow security.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `escrow_contract` - The escrow contract address.
    ///
    /// # Auth
    /// Requires authorization from the stored admin address.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the admin is not initialized.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    pub fn set_escrow_contract(env: Env, escrow_contract: Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::EscrowContract, &escrow_contract);
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
    /// * `InvoiceError::InvalidDueDate` if `due_date` is not strictly in the
    ///   future. Requires `due_date > now`; the boundary comparator is `<=`,
    ///   so `due_date == now` is rejected. Pinning tests:
    ///   `test_create_fails_when_due_date_equals_now` and
    ///   `test_create_succeeds_when_due_date_one_second_in_future`.
    /// * `InvoiceError::InvalidDueDateTooFar` if `due_date` exceeds
    ///   `now + MAX_INVOICE_LIFETIME_SECONDS` (~10 years).
    /// * `InvoiceError::CounterOverflow` if the internal invoice counter overflows.
    /// * `InvoiceError::InvalidParticipants` if `issuer` and `buyer` are the same address.
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

        if issuer == buyer {
            panic_with_error!(&env, InvoiceError::InvalidParticipants);
        }

        let registry_id: Address = env
            .storage()
            .instance()
            .get(&DataKey::RegistryContract)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));

        require_verified(&env, &registry_id, &issuer, InvoiceError::IssuerNotVerified);
        require_verified(&env, &registry_id, &buyer, InvoiceError::BuyerNotVerified);

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
        if face_value > MAX_FACE_VALUE {
            panic_with_error!(&env, InvoiceError::InvalidAmount);
        }
        let now = env.ledger().timestamp();
        if due_date <= now {
            panic_with_error!(&env, InvoiceError::InvalidDueDate);
        }
        let max_due_date = now
            .checked_add(MAX_INVOICE_LIFETIME_SECONDS)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::MathOverflow));
        if due_date > max_due_date {
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
        Self::save_invoice(&env, inv_key, &invoice);

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
    /// * `InvoiceError::InvalidDiscount` if `discount_bps` is zero (a 0% discount is
    ///   nonsensical — the pool would fund at face value with zero yield).
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
        if discount_bps == 0 {
            panic_with_error!(&env, InvoiceError::InvalidDiscount);
        }
        if discount_bps > 5000 {
            panic_with_error!(&env, InvoiceError::DiscountTooHigh);
        }
        invoice.status = InvoiceStatus::Listed;
        invoice.discount_bps = discount_bps;
        invoice.listed_at = Some(env.ledger().timestamp());
        Self::save_invoice(&env, inv_key, &invoice);
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
    /// * `InvoiceError::InvalidAmount` if `funded_amount` is zero.
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

        if funded_amount == 0 {
            panic_with_error!(&env, InvoiceError::InvalidAmount);
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
        Self::save_invoice(&env, inv_key, &invoice);
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
        Self::save_invoice(&env, inv_key, &invoice);
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
    /// # Events
    /// All events are published after the invoice record is persisted and its
    /// TTL extended, so any event observer that reacts to an event is
    /// guaranteed to see the fully-updated invoice if it reads storage in
    /// response. When both parties have confirmed, `both_confirmed` is
    /// published first, followed by `delivery_confirmed`; otherwise only
    /// `delivery_confirmed` is published.
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

        let both_confirmed = invoice.issuer_confirmed && invoice.buyer_confirmed;
        if both_confirmed {
            invoice.status = InvoiceStatus::Confirmed;
            move_status_index(
                &env,
                &invoice_id,
                InvoiceStatus::Active,
                InvoiceStatus::Confirmed,
            );
        }

        Self::save_invoice(&env, inv_key, &invoice);
        Self::extend_instance_ttl(&env);

        // Emit events only after all state (invoice record, status index,
        // TTLs) has been persisted, so event ordering never depends on which
        // branch was taken above.
        if both_confirmed {
            events::both_confirmed(&env, &invoice_id);
        }
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
        // Repays an invoice from Funded, Active, or Confirmed state,
        // transferring the face value to the pool.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice being repaid.
        //
        // # Returns
        // * `bool` - `true` when repayment is completed.
        //
        // # Auth
        // * `buyer` - The buyer must authorize the repayment.
        //
        // # Panics
        // * `NotFound` if the invoice cannot be found.
        // * `InvalidStatusTransition` if invoice status is not `Funded`, `Active`, or `Confirmed`.
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
        if invoice.status != InvoiceStatus::Funded
            && invoice.status != InvoiceStatus::Active
            && invoice.status != InvoiceStatus::Confirmed
        {
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }
        let prev_status = invoice.status;

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

        // Route repayment through escrow so escrow remains the secure
        // intermediary for all fund movements (fixes issue #59).
        // Flow: buyer → escrow → pool (via escrow::release_to_pool)
        let escrow: Address = env
            .storage()
            .instance()
            .get(&DataKey::EscrowContract)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));

        let token = token::Client::new(&env, &funding_asset);
        // Step 1: buyer transfers face_value into escrow
        token.transfer(&buyer, &escrow, &(face_value as i128));

        // Step 2: escrow releases face_value back to pool
        let mut escrow_args = Vec::new(&env);
        escrow_args.push_back(invoice_id.clone().into_val(&env));
        escrow_args.push_back(face_value.into_val(&env));
        let _: bool =
            env.invoke_contract(&escrow, &Symbol::new(&env, "release_to_pool"), escrow_args);

        // Step 3: notify pool to update its internal accounting
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
        Self::save_invoice(&env, inv_key, &updated);
        Self::extend_instance_ttl(&env);

        move_status_index(&env, &invoice_id, prev_status, InvoiceStatus::Repaid);
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
        let _prev_status = invoice.status;
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

        let _prev_status = invoice.status;
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

        // Route repayment through escrow so escrow remains the secure
        // intermediary for all fund movements (fixes issue #59).
        // Flow: buyer → escrow → pool (via escrow::release_to_pool)
        let escrow: Address = env
            .storage()
            .instance()
            .get(&DataKey::EscrowContract)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));

        let token = token::Client::new(&env, &funding_asset);
        // Step 1: buyer transfers face_value into escrow
        token.transfer(&buyer, &escrow, &(face_value as i128));

        // Step 2: escrow releases face_value back to pool
        let mut escrow_args = Vec::new(&env);
        escrow_args.push_back(invoice_id.clone().into_val(&env));
        escrow_args.push_back(face_value.into_val(&env));
        let _: bool =
            env.invoke_contract(&escrow, &Symbol::new(&env, "release_to_pool"), escrow_args);

        // Step 3: notify pool to update its internal accounting
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
        Self::save_invoice(&env, inv_key, &updated);
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
    /// No authorization is required. Anyone may trigger a default once the due
    /// date has passed. This removes the single-point-of-failure risk of an
    /// admin-only gate and ensures LP loss recognition is timely.
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice or funding pool cannot be found.
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
        Self::save_invoice(&env, inv_key, &invoice);
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

    /// Marks an invoice as defaulted, called by the pool contract during
    /// default processing.
    ///
    /// This function is invoked as a cross-contract call from
    /// `pool.handle_default()` and is responsible for persisting the
    /// `Defaulted` status on the invoice record, updating the status index,
    /// and emitting the `invoice_defaulted` event.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to mark as defaulted.
    ///
    /// # Auth
    /// Requires authorization from the invoice's funding pool contract
    /// (retrieved from the invoice record's `funding_pool` field).
    ///
    /// # Panics
    /// * `InvoiceError::NotFound` if the invoice cannot be found or has no
    ///   recorded funding pool.
    /// * `InvoiceError::InvalidStatusTransition` if the invoice status is not
    ///   `Funded`, `Active`, or `Confirmed` (or already `Defaulted`, which is
    ///   accepted as a no-op for idempotency).
    ///
    /// # Returns
    /// * `bool` - `true` when the invoice is marked as defaulted.
    ///
    /// # Example
    /// ```ignore
    /// client.mark_defaulted(&invoice_id);
    /// ```
    pub fn mark_defaulted(env: Env, invoice_id: BytesN<32>) -> bool {
        let inv_key = DataKey::Invoice(invoice_id.clone());
        let mut invoice: Invoice = env
            .storage()
            .persistent()
            .get(&inv_key)
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));

        let pool: Address = invoice
            .funding_pool
            .clone()
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        pool.require_auth();

        // Allow transition from Funded, Active, or Confirmed to Defaulted.
        // If already Defaulted, treat as a no-op for idempotency (e.g., when
        // trigger_default already performed the transition before calling
        // pool.handle_default).
        let valid_transition = invoice.status == InvoiceStatus::Funded
            || invoice.status == InvoiceStatus::Active
            || invoice.status == InvoiceStatus::Confirmed;
        if !valid_transition {
            if invoice.status == InvoiceStatus::Defaulted {
                return true;
            }
            panic_with_error!(&env, InvoiceError::InvalidStatusTransition);
        }

        let prev_status = invoice.status;
        invoice.status = InvoiceStatus::Defaulted;
        Self::save_invoice(&env, inv_key, &invoice);
        Self::extend_instance_ttl(&env);

        move_status_index(&env, &invoice_id, prev_status, InvoiceStatus::Defaulted);
        events::invoice_defaulted(&env, &invoice_id);
        true
    }

    pub fn set_expiry_window(env: Env, window: u64) {
        if window > 31_536_000u64 {
            panic_with_error!(&env, InvoiceError::InvalidExpiryWindow);
        }
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

    /// Helper to check authorization for a given address.
    /// This is invoked dynamically via `try_invoke_contract` in `expire_listing`.
    /// Rust's dead-code analysis can't see the dynamic dispatch via `Symbol`, so
    /// the `#[allow(dead_code)]` keeps it in the WASM dispatch table.
    #[allow(dead_code)]
    fn check_auth(_env: Env, address: Address) {
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
        Self::save_invoice(&env, inv_key, &invoice);
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
    /// * `InvoiceError::NotInitialized` if the contract has not been initialized.
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
        Self::get_invoice(&env, invoice_id).status as u32
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
    /// * `InvoiceError::NotInitialized` if the contract has not been initialized.
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
        Self::get_invoice(&env, invoice_id).face_value
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
    /// * `InvoiceError::NotInitialized` if the contract has not been initialized.
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
        Self::get_invoice(&env, invoice_id).discount_bps
    }

    /// Returns (status, face_value, discount_bps) in a single cross-contract call.
    ///
    /// # Arguments
    /// - `invoice_id` - The invoice to query.
    ///
    /// # Auth
    /// - None.
    ///
    /// # Panics
    /// - If the invoice does not exist (`InvoiceError::NotFound`).
    ///
    /// # Returns
    /// A tuple of `(invoice_status as u32, face_value, discount_bps)`.
    pub fn get_funding_terms(env: Env, invoice_id: BytesN<32>) -> (u32, u128, u32) {
        let invoice: Invoice = env
            .storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(&env, InvoiceError::NotFound));
        (invoice.status as u32, invoice.face_value, invoice.discount_bps)
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
    /// * `InvoiceError::NotInitialized` if the contract has not been initialized.
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
        Self::get_invoice(&env, invoice_id).funding_asset
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
    /// * `InvoiceError::NotInitialized` if the contract has not been initialized.
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
        Self::get_invoice(&env, invoice_id)
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
        let mut ids: Vec<BytesN<32>> = Vec::new(&env);
        for i in 0..count {
            let id: BytesN<32> = env
                .storage()
                .persistent()
                .get(&DataKey::StatusIndexEntry(status as u32, i))
                .unwrap();
            ids.push_back(id);
        }
        let invoices = hydrate_ids(&env, ids);
        let mut result: Vec<Invoice> = Vec::new(&env);
        for i in 0..invoices.len() {
            let invoice = invoices.get(i).unwrap();
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
        let mut ids: Vec<BytesN<32>> = Vec::new(&env);
        for i in 0..count {
            let id: BytesN<32> = env
                .storage()
                .persistent()
                .get(&DataKey::IssuerIndexEntry(address.clone(), i))
                .unwrap();
            ids.push_back(id);
        }
        hydrate_ids(&env, ids)
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
        let mut ids: Vec<BytesN<32>> = Vec::new(&env);
        for i in 0..count {
            let id: BytesN<32> = env
                .storage()
                .persistent()
                .get(&DataKey::BuyerIndexEntry(address.clone(), i))
                .unwrap();
            ids.push_back(id);
        }
        hydrate_ids(&env, ids)
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
    /// * `InvoiceError::NotInitialized` if the contract has not been initialized.
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
        Self::get_invoice(&env, invoice_id).issuer
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

    fn require_initialized(env: &Env) {
        if !env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, InvoiceError::NotInitialized);
        }
    }

    fn get_invoice(env: &Env, invoice_id: BytesN<32>) -> Invoice {
        Self::require_initialized(env);
        env.storage()
            .persistent()
            .get(&DataKey::Invoice(invoice_id))
            .unwrap_or_else(|| panic_with_error!(env, InvoiceError::NotFound))
    }

    fn extend_instance_ttl(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
    }
}

/// Checks that an address is verified in the registry, panicking with the
/// provided error if not.
fn require_verified(env: &Env, registry_id: &Address, addr: &Address, err: InvoiceError) {
    let mut args = Vec::new(env);
    args.push_back(addr.clone().into_val(env));
    let verified: bool = env.invoke_contract(registry_id, &Symbol::new(env, "is_verified"), args);
    if !verified {
        panic_with_error!(env, err);
    }
}

/// Adds an invoice ID to the issuer's index if not already present.
///
/// # Arguments
/// * `env` - The Soroban environment.
/// * `issuer` - The issuer address.
/// * `invoice_id` - The invoice ID to add.
///
/// # Panics
/// Does not panic.
///
/// # Returns
/// * `()` - No value is returned.
fn extend_issuer_index(env: &Env, issuer: &Address, invoice_id: &BytesN<32>) {
    let count_key = DataKey::IssuerIndexCount(issuer.clone());
    let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);

    // Check if invoice_id already exists in this issuer index
    for i in 0..count {
        let entry_key = DataKey::IssuerIndexEntry(issuer.clone(), i);
        let existing_id: BytesN<32> = env.storage().persistent().get(&entry_key).unwrap();
        if existing_id == *invoice_id {
            return; // Already exists, skip duplicate
        }
    }

    let entry_key = DataKey::IssuerIndexEntry(issuer.clone(), count);
    env.storage().persistent().set(&entry_key, invoice_id);
    env.storage().persistent().set(&count_key, &(count + 1));
    env.storage()
        .persistent()
        .extend_ttl(&entry_key, TTL_THRESHOLD, TTL_EXTEND_TO);
    env.storage()
        .persistent()
        .extend_ttl(&count_key, TTL_THRESHOLD, TTL_EXTEND_TO);
}

/// Adds an invoice ID to the buyer's index if not already present.
///
/// # Arguments
/// * `env` - The Soroban environment.
/// * `buyer` - The buyer address.
/// * `invoice_id` - The invoice ID to add.
///
/// # Panics
/// Does not panic.
///
/// # Returns
/// * `()` - No value is returned.
fn extend_buyer_index(env: &Env, buyer: &Address, invoice_id: &BytesN<32>) {
    let count_key = DataKey::BuyerIndexCount(buyer.clone());
    let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);

    // Check if invoice_id already exists in this buyer index
    for i in 0..count {
        let entry_key = DataKey::BuyerIndexEntry(buyer.clone(), i);
        let existing_id: BytesN<32> = env.storage().persistent().get(&entry_key).unwrap();
        if existing_id == *invoice_id {
            return; // Already exists, skip duplicate
        }
    }

    let entry_key = DataKey::BuyerIndexEntry(buyer.clone(), count);
    env.storage().persistent().set(&entry_key, invoice_id);
    env.storage().persistent().set(&count_key, &(count + 1));
    env.storage()
        .persistent()
        .extend_ttl(&entry_key, TTL_THRESHOLD, TTL_EXTEND_TO);
    env.storage()
        .persistent()
        .extend_ttl(&count_key, TTL_THRESHOLD, TTL_EXTEND_TO);
}

/// Adds an invoice ID to the status index if not already present.
///
/// # Arguments
/// * `env` - The Soroban environment.
/// * `status` - The invoice status.
/// * `invoice_id` - The invoice ID to add.
///
/// # Panics
/// Does not panic.
///
/// # Returns
/// * `()` - No value is returned.
fn extend_status_index(env: &Env, status: InvoiceStatus, invoice_id: &BytesN<32>) {
    let status_u32 = status as u32;
    let count_key = DataKey::StatusIndexCount(status_u32);
    let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);

    // Check if invoice_id already exists in this status index
    for i in 0..count {
        let entry_key = DataKey::StatusIndexEntry(status_u32, i);
        let existing_id: BytesN<32> = env.storage().persistent().get(&entry_key).unwrap();
        if existing_id == *invoice_id {
            return; // Already exists, skip duplicate
        }
    }

    let entry_key = DataKey::StatusIndexEntry(status_u32, count);
    env.storage().persistent().set(&entry_key, invoice_id);
    env.storage().persistent().set(&count_key, &(count + 1));
    env.storage()
        .persistent()
        .extend_ttl(&entry_key, TTL_THRESHOLD, TTL_EXTEND_TO);
    env.storage()
        .persistent()
        .extend_ttl(&count_key, TTL_THRESHOLD, TTL_EXTEND_TO);
}

/// Moves an invoice ID from one status index to another, with idempotency for replayed transitions.
///
/// This function checks if the invoice is already in the target status index before performing
/// any operations. If already present, it returns early without modifying counts or indexes,
/// making replayed transitions a no-op.
///
/// # Arguments
/// * `env` - The Soroban environment.
/// * `invoice_id` - The invoice ID to move.
/// * `from` - The source status.
/// * `to` - The target status.
///
/// # Panics
/// * `InvoiceError::InvalidStatusTransition` if the source status count underflows.
///
/// # Returns
/// * `()` - No value is returned.
fn move_status_index(env: &Env, invoice_id: &BytesN<32>, from: InvoiceStatus, to: InvoiceStatus) {
    // Check if invoice is already in the target status index (idempotency for replayed transitions)
    let to_u32 = to as u32;
    let count_key = DataKey::StatusIndexCount(to_u32);
    let count: u32 = env.storage().persistent().get(&count_key).unwrap_or(0);
    for i in 0..count {
        let entry_key = DataKey::StatusIndexEntry(to_u32, i);
        let existing_id: BytesN<32> = env.storage().persistent().get(&entry_key).unwrap();
        if existing_id == *invoice_id {
            return; // Already in target index, skip all operations
        }
    }

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

fn hydrate_ids(env: &Env, ids: Vec<BytesN<32>>) -> Vec<Invoice> {
    let mut result: Vec<Invoice> = Vec::new(env);
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
