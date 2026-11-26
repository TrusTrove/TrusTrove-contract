#![no_std]

mod types;

use soroban_sdk::{
    contract, contracterror, contractimpl, Address, BytesN, Env, IntoVal, Symbol, Val, Vec,
};

pub use types::*;

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EscrowError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    AlreadyLocked = 4,
    NotLocked = 5,
    InsufficientBalance = 6,
    InvalidAmount = 7,
    TimeoutNotReached = 8,
    ContractPaused = 9,
    InvalidConfig = 10,
}

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        pool_contract: Address,
        usdc_asset: Address,
        timeout_window: u64,
    ) -> Result<(), EscrowError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(EscrowError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::PoolContract, &pool_contract);
        env.storage()
            .instance()
            .set(&DataKey::UsdcAsset, &usdc_asset);
        env.storage()
            .instance()
            .set(&DataKey::TimeoutWindow, &timeout_window);
        Ok(())
    }

    pub fn lock(
        env: Env,
        invoice_id: BytesN<32>,
        amount: u128,
        issuer: Address,
    ) -> Result<(), EscrowError> {
        let pool_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .ok_or(EscrowError::NotInitialized)?;
        pool_contract.require_auth();

        if amount == 0 {
            return Err(EscrowError::InvalidAmount);
        }

        if env.storage().persistent().has(&DataKey::Locked(invoice_id.clone())) {
            return Err(EscrowError::AlreadyLocked);
        }

        let usdc_asset: Address = env
            .storage()
            .instance()
            .get(&DataKey::UsdcAsset)
            .ok_or(EscrowError::NotInitialized)?;

        let token_client = soroban_sdk::token::Client::new(&env, &usdc_asset);
        token_client.transfer(
            &pool_contract,
            &env.current_contract_address(),
            &(amount as i128),
        );

        let record = EscrowRecord {
            invoice_id: invoice_id.clone(),
            amount,
            locked_at: env.ledger().timestamp(),
            issuer,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Locked(invoice_id.clone()), &record);
        env.storage().persistent().extend_ttl(
            &DataKey::Locked(invoice_id.clone()),
            17280,
            345600,
        );

        let event = EscrowEvent {
            invoice_id: invoice_id.clone(),
            action: EscrowAction::Locked,
            amount,
            timestamp: env.ledger().timestamp(),
            caller: None,
        };
        Self::append_history(&env, &invoice_id, &event);

        Ok(())
    }

    /// Returns the current `release_after_timeout` window in seconds.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Returns
    /// * `u64` - The window in seconds. Defaults to `DEFAULT_TIMEOUT_WINDOW_SECONDS` (30 days) if unset.
    pub fn get_timeout_window(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::TimeoutWindow)
            .unwrap_or(DEFAULT_TIMEOUT_WINDOW_SECONDS)
    }

    /// Sets the minimum amount, in USDC stroops, that future [`Self::lock`]
    /// calls must meet (issue #1297).
    ///
    /// The default is `0`, which preserves the previous behaviour of accepting
    /// any non-zero amount. The policy only applies to future locks: records
    /// created before it was set remain unchanged and releasable through
    /// [`Self::release_to_issuer`], [`Self::release_to_pool`],
    /// [`Self::handle_default`], and [`Self::release_after_timeout`].
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `min_amount` - The minimum accepted by subsequent `lock` calls. `0`
    ///   disables the minimum.
    ///
    /// # Auth
    /// Requires authorization from the stored `admin`.
    ///
    /// # Panics
    /// * `NotInitialized` if the contract has not been initialized.
    ///
    /// # Returns
    /// * `()` - No value is returned. Emits `min_lock_amount_set`.
    ///
    /// # Example
    /// ```ignore
    /// client.set_min_lock_amount(&1_000_000); // 0.1 USDC
    /// ```
    pub fn set_min_lock_amount(env: Env, min_amount: u128) {
        require_not_paused(&env);
        let admin = Self::require_admin(&env);
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::MinLockAmount, &min_amount);
        events::min_lock_amount_set(&env, &admin, min_amount);
        Self::extend_instance_ttl(&env);
    }

    /// Returns the minimum amount accepted by future [`Self::lock`] calls.
    ///
    /// # Auth
    /// No authorization is required. This is a read-only view.
    ///
    /// # Returns
    /// * `u128` - The configured minimum in USDC stroops, or `0` when unset.
    ///
    /// # Example
    /// ```ignore
    /// let minimum = client.get_min_lock_amount();
    /// ```
    pub fn get_min_lock_amount(env: Env) -> u128 {
        Self::min_lock_amount(&env)
    }

    /// Returns the recipient of the most recent successful release or default
    /// for an invoice (issue #1296).
    ///
    /// The recipient is recorded after the token transfer succeeds by
    /// [`Self::release_to_issuer`] (the issuer),
    /// [`Self::release_to_pool`] (the pool),
    /// [`Self::handle_default`] (the pool), and
    /// [`Self::release_after_timeout`] (the pool). A failed transfer reverts
    /// the whole call, so it never overwrites the previously recorded
    /// recipient.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to query.
    ///
    /// # Auth
    /// None. This is a read-only view, callable while the contract is paused.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `Option<Address>` - The last recipient, or `None` when no successful
    ///   release/default has been recorded for the invoice.
    ///
    /// # Example
    /// ```ignore
    /// let recipient = client.get_last_release_recipient(&invoice_id);
    /// ```
    pub fn get_last_release_recipient(env: Env, invoice_id: BytesN<32>) -> Option<Address> {
        let key = DataKey::LastReleaseRecipient(invoice_id);
        match env.storage().persistent().get::<_, Address>(&key) {
            Some(recipient) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                Some(recipient)
            }
            None => None,
        }
    }

    /// Returns a custody coverage preview for one invoice (issue #1298).
    ///
    /// Reports the amount tracked by the invoice's active [`DataKey::Locked`]
    /// record, the escrow contract's actual balance of the configured asset,
    /// and whether that balance covers the tracked amount. A caller can use it
    /// to decide whether a proposed release can succeed before submitting a
    /// transaction that would fail.
    ///
    /// The comparison is against the tracked lock amount, not a caller-supplied
    /// amount. `can_release` is `false` when no lock exists, when the tracked
    /// amount cannot be represented as a token amount, or when the balance is
    /// short. A tracked amount of `0` therefore previews as not releasable.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to inspect.
    ///
    /// # Auth
    /// None. This is a read-only view, callable while the contract is paused.
    ///
    /// # Storage
    /// Read-only: no transfer, no storage write and no TTL extension. The only
    /// external call is the asset's `balance` view.
    ///
    /// # Panics
    /// * `NotInitialized` if the USDC asset address is unset.
    ///
    /// # Returns
    /// * `CustodyCoverage` - Tracked amount, token balance, and coverage flag.
    ///
    /// # Example
    /// ```ignore
    /// let coverage = client.get_custody_coverage(&invoice_id);
    /// if coverage.can_release {
    ///     // safe to submit the release
    /// }
    /// ```
    pub fn get_custody_coverage(env: Env, invoice_id: BytesN<32>) -> CustodyCoverage {
        let tracked_amount = env
            .storage()
            .persistent()
            .get::<_, EscrowRecord>(&DataKey::Locked(invoice_id))
            .map(|record| record.amount)
            .unwrap_or(0);
        let token_balance = Self::usdc_client(&env).balance(&env.current_contract_address());
        let can_release = tracked_amount > 0
            && tracked_amount <= i128::MAX as u128
            && token_balance >= tracked_amount as i128;
        CustodyCoverage {
            tracked_amount,
            token_balance,
            can_release,
        }
    }

    /// Permissionlessly returns a stale lock to the pool once
    /// `now >= locked_at + timeout_window`.
    ///
    /// Provides an exit path for locked funds when neither the pool nor the
    /// admin ever acts. Funds go to the pool, the same safe default recipient
    /// as `handle_default`. Mirrors `invoice::expire_listing`'s timeout pattern.
    ///
    /// # Auth
    /// No authorization is required; anyone may call.
    ///
    /// # Panics
    /// * `NotFound` if no lock exists for `invoice_id`.
    /// * `TimeoutNotElapsed` if `now < locked_at + timeout_window`.
    /// * `MathOverflow` if `locked_at + timeout_window` overflows.
    /// * `NotInitialized` if the contract has not been initialized.
    ///
    /// # Returns
    /// * `bool` - `true` when the funds are returned to the pool.
    ///
    /// # Example
    /// ```ignore
    /// client.release_after_timeout(&invoice_id);
    /// ```
    pub fn release_after_timeout(env: Env, invoice_id: BytesN<32>) -> bool {
        require_not_paused(&env);
        let key = DataKey::Locked(invoice_id.clone());
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotFound));

        let deadline = record
            .locked_at
            .checked_add(Self::get_timeout_window(env.clone()))
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::MathOverflow));
        if env.ledger().timestamp() < deadline {
            panic_with_error!(&env, EscrowError::TimeoutNotElapsed);
        }

        let pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized));
        if record.amount > i128::MAX as u128 {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }
        Self::usdc_client(&env).transfer(
            &env.current_contract_address(),
            &pool,
            &(record.amount as i128),
        );
        Self::record_last_release_recipient(&env, &invoice_id, &pool);

        Self::append_history(
            &env,
            &invoice_id,
            EscrowAction::TimeoutReleased,
            record.amount,
            None,
        );
        env.storage().persistent().remove(&key);
        env.storage()
            .persistent()
            .remove(&DataKey::LockedLedger(invoice_id.clone()));
        Self::extend_instance_ttl(&env);
        events::timeout_released(&env, &invoice_id, &pool, record.amount);
        true
    }

    /// Engages the emergency circuit breaker.
    ///
    /// While paused every state-changing entry point (`lock`,
    /// `release_to_issuer`, `release_to_pool`, `handle_default`) reverts with
    /// `ContractPaused`, so no funds can move in or out of escrow while the
    /// read-only views (`get_locked`, `get_locked_at`, `get_locked_ledger`,
    /// `get_total_returned_to_pool`, `get_history`,
    /// `get_usdc_asset`, `get_admin`, `get_pool_contract`) stay callable. Only
    /// the stored admin may pause, and [`Self::unpause`] is intentionally never
    /// guarded so paused escrow can always be resumed. Emits `paused`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// Requires authorization from the stored `admin`.
    ///
    /// # Panics
    /// * `EscrowError::NotInitialized` if the contract has not been
    ///   initialized.
    ///
    /// # Example
    /// ```ignore
    /// client.pause();
    /// ```
    pub fn pause(env: Env) {
        let admin = Self::require_admin(&env);
        admin.require_auth();
        set_paused(&env, true);
        events::paused(&env, &admin);
        Self::extend_instance_ttl(&env);
    }

    /// Disengages the emergency circuit breaker, restoring fund movement.
    ///
    /// Deliberately *not* guarded by `require_not_paused`: otherwise paused
    /// escrow could never be resumed. Emits `unpaused`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// Requires authorization from the stored `admin`.
    ///
    /// # Panics
    /// * `EscrowError::NotInitialized` if the contract has not been
    ///   initialized.
    ///
    /// # Example
    /// ```ignore
    /// client.unpause();
    /// ```
    pub fn unpause(env: Env) {
        let admin = Self::require_admin(&env);
        admin.require_auth();
        set_paused(&env, false);
        events::unpaused(&env, &admin);
        Self::extend_instance_ttl(&env);
    }

    /// Recovers a token that was sent to this escrow by mistake.
    ///
    /// Anyone can transfer an unrelated token to a contract address, and with
    /// no exit path it would be stranded forever. This lets the admin sweep
    /// such a token to `to`.
    ///
    /// The configured funding asset is **never** recoverable here. Every
    /// active lock is collateral held in that asset, and a stray deposit of it
    /// cannot be told apart from collateral without cumulative accounting, so
    /// recovery of the funding asset is rejected outright. Because no other
    /// token is ever recorded as collateral, active locks are never touched:
    /// no `Locked`, `History` or `IssuerReleased` entry is read or written.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `token` - The stray token contract to recover. Must not be the funding asset.
    /// * `to` - The recipient. Must not be this contract.
    /// * `amount` - The amount to move, in the token's smallest unit.
    ///
    /// # Auth
    /// Requires authorization from the stored `admin`.
    ///
    /// # Panics
    /// * `ContractPaused` if the emergency pause is engaged.
    /// * `NotInitialized` if the contract has not been initialized.
    /// * `InvalidAmount` if `amount` is zero or exceeds `i128::MAX`.
    /// * `InvalidToken` if `token` is the configured funding asset.
    /// * `InvalidRecipient` if `to` is this contract.
    ///
    /// # Events
    /// Emits `token_recovered` with topics `(admin, token, to)` and the
    /// amount as data.
    ///
    /// # Returns
    /// * `bool` - Always `true` on success.
    ///
    /// # Example
    /// ```ignore
    /// client.recover_token(&stray_token, &treasury, &5_000_000);
    /// ```
    pub fn recover_token(env: Env, token: Address, to: Address, amount: u128) -> bool {
        require_not_paused(&env);
        let admin = Self::require_admin(&env);
        admin.require_auth();
        if amount == 0 || amount > i128::MAX as u128 {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }
        let usdc: Address = env
            .storage()
            .instance()
            .get(&DataKey::UsdcAsset)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized));
        if token == usdc {
            panic_with_error!(&env, EscrowError::InvalidToken);
        }
        if to == env.current_contract_address() {
            panic_with_error!(&env, EscrowError::InvalidRecipient);
        }
        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &to,
            &(amount as i128),
        );
        events::token_recovered(&env, &admin, &token, &to, amount);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Returns the active escrow lock record for an invoice.
    ///
    /// This view reads the existing [`DataKey::Locked`] entry and returns the
    /// stored [`EscrowRecord`] without changing the storage schema. It is a
    /// convenient single-call alternative to [`Self::get_locked`] and
    /// [`Self::get_locked_at`] for clients that need both values.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice ID to query.
    ///
    /// # Auth
    /// None. This is a read-only view, callable while the contract is paused.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `Option<EscrowRecord>` - The active lock record, or `None` if no lock
    ///   exists. A missing read does not create a persistent entry.
    ///
    /// # Example
    /// ```ignore
    /// let lock = client.get_lock_record(&invoice_id);
    /// ```
    pub fn get_lock_record(env: Env, invoice_id: BytesN<32>) -> Option<EscrowRecord> {
        let key = DataKey::Locked(invoice_id);
        match env.storage().persistent().get::<_, EscrowRecord>(&key) {
            Some(record) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                Some(record)
            }
            None => None,
        }
    }

    /// Returns whether an escrow lock record exists for an invoice.
    ///
    /// This checks the persistent record itself, not its amount. It therefore
    /// distinguishes a missing lock from a malformed record whose amount is zero.
    /// Missing keys are not created by this read-only view.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice ID to query.
    ///
    /// # Auth
    /// None. This is a read-only view, callable while the contract is paused.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `bool` - `true` if a lock record exists, even if its stored amount is zero.
    ///
    /// # Example
    /// ```ignore
    /// let exists = client.has_lock(&invoice_id);
    /// ```
    pub fn has_lock(env: Env, invoice_id: BytesN<32>) -> bool {
        let key = DataKey::Locked(invoice_id);
        if !env.storage().persistent().has(&key) {
            return false;
        }
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        true
    }
    /// Returns the amount of USDC currently locked against an invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice ID to query.
    ///
    /// # Auth
    /// None. This is a read-only view, callable while the contract is paused.
    ///
    /// # Panics
    /// Does not panic.
    ///
    /// # Returns
    /// * `u128` - The locked amount, or `0` if no record exists.
    ///
    /// # Example
    /// ```ignore
    /// let locked = client.get_locked(&invoice_id);
    /// ```
    pub fn get_locked(env: Env, invoice_id: BytesN<32>) -> u128 {
        let record: Option<EscrowRecord> = env
            .storage()
            .persistent()
            .get(&DataKey::Locked(invoice_id));
        match record {
            Some(r) => r.amount,
            None => 0,
        }
    }

    /// Returns amount, locked_at, and presence for each supplied ID in input order.
    /// Missing IDs do not fail the batch.
    pub fn get_locked_batch(env: Env, invoice_ids: Vec<BytesN<32>>) -> Vec<Val> {
        let mut result = Vec::new(&env);
        for id in invoice_ids.iter() {
            let record: Option<EscrowRecord> = env
                .storage()
                .persistent()
                .get(&DataKey::Locked(id));
            match record {
                Some(r) => {
                    result.push_back(r.amount.into_val(&env));
                    result.push_back(r.locked_at.into_val(&env));
                    result.push_back(true.into_val(&env));
                }
                None => {
                    result.push_back(0u128.into_val(&env));
                    result.push_back(0u64.into_val(&env));
                    result.push_back(false.into_val(&env));
                }
            }
        }
        result
    }

    fn append_history(env: &Env, invoice_id: &BytesN<32>, event: &EscrowEvent) {
        let key = DataKey::History(invoice_id.clone());
        let mut history: Vec<EscrowEvent> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        history.push_back(event.clone());
        env.storage().persistent().set(&key, &history);
        env.storage()
            .persistent()
            .extend_ttl(&key, 17280, 345600);
    }
}
