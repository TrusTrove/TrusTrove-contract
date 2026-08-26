#![no_std]

use soroban_sdk::{contract, contractimpl, panic_with_error, token, Address, BytesN, Env, Vec};

mod constants;
mod errors;
mod events;
mod test;
mod types;

pub use constants::*;
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
    /// * `invoice_contract` - The invoice contract address authorized to call
    ///   `release_to_pool`.
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

    /// Get a token client for the USDC asset stored in the contract.
    fn usdc_client(env: &Env) -> token::Client<'_> {
        let usdc_id: Address = env.storage().instance().get(&DataKey::UsdcAsset).unwrap();
        token::Client::new(env, &usdc_id)
    }

    /// Returns the USDC asset this escrow contract was initialized with.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// None. This is a read-only view.
    ///
    /// # Panics
    /// * `NotInitialized` if the contract has not been initialized.
    ///
    /// # Returns
    /// * `Address` - The USDC asset address.
    ///
    /// # Example
    /// ```ignore
    /// let asset = client.get_usdc_asset();
    /// ```
    pub fn get_usdc_asset(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::UsdcAsset)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized))
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
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
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
    /// Called by the invoice contract during buyer repayment flows:
    /// `buyer → invoice.repay → escrow.release_to_pool → pool`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice whose escrow is being released.
    /// * `repayment_amount` - The amount transferred to the pool (may exceed the
    ///   originally locked amount when the buyer repays the full face value including yield).
    ///
    /// # Auth
    /// Requires authorization from the configured invoice contract (via
    /// `invoice_contract.require_auth()`), so only `invoice.repay()` /
    /// `invoice.repay_early()` can trigger this release.
    ///
    /// # Panics
    /// * `NotInitialized` if the contract has not been initialized.
    /// * `NotFound` if no escrow record exists for the invoice.
    /// * `InvalidAmount` if `repayment_amount` is zero.
    ///
    /// # Returns
    /// * `bool` - `true` when funds are returned.
    ///
    /// # Example
    /// ```ignore
    /// client.release_to_pool(&invoice_id, &repayment_amount);
    /// ```
    pub fn release_to_pool(env: Env, invoice_id: BytesN<32>, repayment_amount: u128) -> bool {
        // This function is called by the invoice contract during buyer repay flows
        // (buyer → invoice → escrow → pool). Only the configured invoice contract
        // may trigger it.
        let invoice_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::InvoiceContract)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized));
        invoice_contract.require_auth();

        let pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized));

        if repayment_amount == 0 {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }

        let key = DataKey::Locked(invoice_id.clone());
        let _record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotFound));

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
    /// # Coupling with `invoice.trigger_default`
    /// This grace period (`DEFAULT_MIN_LOCK_SECONDS`, measured from
    /// [`EscrowRecord::locked_at`]) is independent of, and not known to,
    /// `invoice.trigger_default`'s own due-date gate (`now >= due_date`).
    /// `invoice.trigger_default` sets the invoice to `Defaulted` and then calls
    /// this function transitively via `pool.handle_default`; if an invoice's
    /// `due_date` is reached less than `DEFAULT_MIN_LOCK_SECONDS` after it was
    /// funded (i.e. after `locked_at`), this call panics with `NotAuthorized`
    /// and the whole transaction (including the invoice's status change)
    /// reverts. There is currently no mechanism for `invoice` to read or
    /// respect this window ahead of time; callers of very-short-duration
    /// invoices should expect `trigger_default` to revert until `locked_at +
    /// DEFAULT_MIN_LOCK_SECONDS` has elapsed. See
    /// `contracts/invoice/src/test.rs`'s
    /// `test_trigger_default_reverts_when_escrow_grace_period_not_elapsed` for
    /// a pinned repro of this behavior.
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
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    fn extend_instance_ttl(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
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
