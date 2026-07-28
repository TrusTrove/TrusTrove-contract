#![no_std]

use soroban_sdk::{contract, contractimpl, panic_with_error, token, Address, BytesN, Env, Vec};

mod errors;
mod events;
mod test;
mod types;

pub use errors::*;
pub use types::*;

const DEFAULT_MIN_LOCK_SECONDS: u64 = 60;

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    /// Initializes the escrow contract and stores required contract references.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `admin` - The admin address for this contract.
    /// * `pool_contract` - The pool contract address.
    /// * `invoice_contract` - The invoice contract address.
    /// * `usdc_asset` - The USDC asset address.
    ///
    /// # Auth
    /// Requires authorization from `admin`.
    ///
    /// # Panics
    /// * `AlreadyInitialized` if the contract has already been initialized.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Example
    /// ```ignore
    /// client.initialize(&admin, &pool, &invoice, &usdc);
    /// ```
    /// Get a token client for the USDC asset stored in the contract.
    fn usdc_client(env: &Env) -> token::Client {
        let usdc_id: Address = env.storage().instance().get(&DataKey::UsdcAsset).unwrap();
        token::Client::new(env, &usdc_id)
    }

    pub fn initialize(
        env: Env,
        admin: Address,
        pool_contract: Address,
        invoice_contract: Address,
        usdc_asset: Address,
    ) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, EscrowError::AlreadyInitialized);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::PoolContract, &pool_contract);
        env.storage()
            .instance()
            .set(&DataKey::InvoiceContract, &invoice_contract);
        env.storage()
            .instance()
            .set(&DataKey::UsdcAsset, &usdc_asset);
        Self::extend_instance_ttl(&env);
    }

    /// Locks USDC in escrow against a funded invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice ID being locked.
    /// * `amount` - The amount to lock.
    ///
    /// # Auth
    /// Requires authorization from the configured pool contract.
    ///
    /// # Panics
    /// * `NotInitialized` if the contract has not been initialized.
    /// * `InvalidAmount` if the amount is zero.
    /// * `AlreadyLocked` if the invoice is already locked.
    ///
    /// # Returns
    /// * `bool` - `true` when the funds are locked.
    ///
    /// # Example
    /// ```ignore
    /// client.lock(&invoice_id, &amount);
    /// ```
    pub fn lock(env: Env, invoice_id: BytesN<32>, amount: u128) -> bool {
        let pool = Self::require_pool_auth(&env);

        if amount == 0 {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }

        let key = DataKey::Locked(invoice_id.clone());
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, EscrowError::AlreadyLocked);
        }

        let usdc = Self::usdc_client(&env);
        usdc.transfer(&pool, &env.current_contract_address(), &(amount as i128));

        let record = EscrowRecord {
            invoice_id: invoice_id.clone(),
            amount,
            locked_at: env.ledger().timestamp(),
        };
        env.storage().persistent().set(&key, &record);
        env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
        Self::append_history(&env, &invoice_id, EscrowAction::Locked, amount);
        Self::extend_instance_ttl(&env);
        events::funds_locked(&env, &invoice_id, amount);

        true
    }

    /// Releases escrowed funds to the issuer.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice whose escrow is released.
    /// * `issuer` - The issuer address to receive funds.
    ///
    /// # Auth
    /// Requires authorization from the configured pool contract.
    ///
    /// # Panics
    /// * `NotInitialized` if the contract has not been initialized.
    /// * `NotFound` if no escrow record exists for the invoice.
    /// * `InvalidRecipient` if issuer is escrow, pool, or invoice contract address.
    ///
    /// # Returns
    /// * `bool` - `true` when funds are released.
    ///
    /// # Example
    /// ```ignore
    /// client.release_to_issuer(&invoice_id, &issuer);
    /// ```
    pub fn release_to_issuer(env: Env, invoice_id: BytesN<32>, issuer: Address) -> bool {
        let pool = Self::require_pool_auth(&env);

        if issuer == env.current_contract_address() || issuer == pool {
            panic_with_error!(&env, EscrowError::InvalidRecipient);
        }

        if let Some(invoice_contract) = env
            .storage()
            .instance()
            .get::<_, Address>(&DataKey::InvoiceContract)
        {
            if issuer == invoice_contract {
                panic_with_error!(&env, EscrowError::InvalidRecipient);
            }
        }

        let key = DataKey::Locked(invoice_id.clone());
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotFound));

        let usdc = Self::usdc_client(&env);
        usdc.transfer(
            &env.current_contract_address(),
            &issuer,
            &(record.amount as i128),
        );

        Self::append_history(
            &env,
            &invoice_id,
            EscrowAction::ReleasedToIssuer,
            record.amount,
        );
        env.storage().persistent().remove(&key);
        Self::extend_instance_ttl(&env);
        events::released_to_issuer(&env, &invoice_id, &issuer, record.amount);
        true
    }

    /// Releases escrowed funds back to the pool as repayment.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice whose escrow is returned.
    /// * `repayment_amount` - The amount returned to the pool.
    ///
    /// # Auth
    /// Requires authorization from the configured pool contract.
    ///
    /// # Panics
    /// * `NotInitialized` if the contract has not been initialized.
    /// * `NotFound` if no escrow record exists for the invoice.
    /// * `InvalidAmount` if `repayment_amount` is zero or exceeds the locked amount.
    ///
    /// # Returns
    /// * `bool` - `true` when funds are returned.
    ///
    /// # Example
    /// ```ignore
    /// client.release_to_pool(&invoice_id, &repayment_amount);
    /// ```
    pub fn release_to_pool(env: Env, invoice_id: BytesN<32>, repayment_amount: u128) -> bool {
        let pool = Self::require_pool_auth(&env);

        if repayment_amount == 0 {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }

        let key = DataKey::Locked(invoice_id.clone());
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotFound));

        if repayment_amount > record.amount {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }

        let usdc = Self::usdc_client(&env);
        usdc.transfer(
            &env.current_contract_address(),
            &pool,
            &(repayment_amount as i128),
        );

        Self::append_history(
            &env,
            &invoice_id,
            EscrowAction::ReleasedToPool,
            repayment_amount,
        );
        env.storage().persistent().remove(&key);
        Self::extend_instance_ttl(&env);
        events::released_to_pool(&env, &invoice_id, &pool, repayment_amount);
        true
    }

    /// Handles an escrow default by returning the locked funds to the pool.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice with a defaulted escrow lock.
    /// * `caller` - The address calling this function (admin or pool contract).
    ///
    /// # Auth
    /// Requires authorization from `caller`, which must equal either the stored
    /// admin address or the configured pool contract.
    ///
    /// # Panics
    /// * `NotInitialized` if the contract has not been initialized and a lock record exists for the invoice.
    /// * `NotAuthorized` if `caller` is neither the admin nor the pool contract.
    /// * `NotAuthorized` if the record has not been locked long enough to satisfy the grace period.
    ///
    /// # Returns
    /// * `bool` - `true` if default handling completed, `false` if no lock exists.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.handle_default(&invoice_id, &caller);
    /// ```
    pub fn handle_default(env: Env, invoice_id: BytesN<32>, caller: Address) -> bool {
        let key = DataKey::Locked(invoice_id.clone());
        let Some(record) = env.storage().persistent().get::<_, EscrowRecord>(&key) else {
            return false;
        };
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized));
        let pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized));

        caller.require_auth();
        if caller != admin && caller != pool {
            panic_with_error!(&env, EscrowError::NotAuthorized);
        }

        let now = env.ledger().timestamp();
        if now - record.locked_at < DEFAULT_MIN_LOCK_SECONDS {
            panic_with_error!(&env, EscrowError::NotAuthorized);
        }

        let usdc = Self::usdc_client(&env);
        usdc.transfer(
            &env.current_contract_address(),
            &pool,
            &(record.amount as i128),
        );

        Self::append_history(
            &env,
            &invoice_id,
            EscrowAction::DefaultHandled,
            record.amount,
        );
        env.storage().persistent().remove(&key);
        Self::extend_instance_ttl(&env);
        events::default_resolved(&env, &invoice_id, &pool, record.amount);
        true
    }

    /// Returns the amount currently locked in escrow for an invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to query.
    ///
    /// # Auth
    /// None. This is a read-only view.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `u128` - The amount locked, or 0 if none exists.
    ///
    /// # Example
    /// ```ignore
    /// let locked = client.get_locked(&invoice_id);
    /// ```
    pub fn get_locked(env: Env, invoice_id: BytesN<32>) -> u128 {
        env.storage()
            .persistent()
            .get::<_, EscrowRecord>(&DataKey::Locked(invoice_id))
            .map(|r| r.amount)
            .unwrap_or(0)
    }

    /// Returns the timestamp when the escrow was locked for an invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to query.
    ///
    /// # Auth
    /// None. This is a read-only view.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `u64` - The locked-at timestamp, or 0 if no escrow record exists.
    ///
    /// # Example
    /// ```ignore
    /// let locked_at = client.get_locked_at(&invoice_id);
    /// ```
    pub fn get_locked_at(env: Env, invoice_id: BytesN<32>) -> u64 {
        env.storage()
            .persistent()
            .get::<_, EscrowRecord>(&DataKey::Locked(invoice_id))
            .map(|r| r.locked_at)
            .unwrap_or(0)
    }

    pub fn get_history(env: Env, invoice_id: BytesN<32>) -> Vec<EscrowEvent> {
        let key = DataKey::History(invoice_id);
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(&env))
    }

    fn append_history(env: &Env, invoice_id: &BytesN<32>, action: EscrowAction, amount: u128) {
        let key = DataKey::History(invoice_id.clone());
        let mut history: Vec<EscrowEvent> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(env));
        history.push_back(EscrowEvent {
            invoice_id: invoice_id.clone(),
            action,
            amount,
            timestamp: env.ledger().timestamp(),
        });
        env.storage().persistent().set(&key, &history);
        env.storage().persistent().extend_ttl(&key, 100, 2_000_000);
    }

    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }

    fn require_pool_auth(env: &Env) -> Address {
        let pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap_or_else(|| panic_with_error!(env, EscrowError::NotInitialized));
        pool.require_auth();
        pool
    }
}
