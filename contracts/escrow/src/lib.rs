#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, token, Address, BytesN, Env, Symbol, Vec,
};
use trusttrove_pause::{require_not_paused, set_paused};

mod constants;
mod errors;
mod events;
mod test;
mod types;

pub use constants::*;
pub use errors::*;
pub use types::*;

const DEFAULT_MIN_LOCK_SECONDS: u64 = 60;
/// Default `release_after_timeout` window (30 days) used until the admin sets one.
pub const DEFAULT_TIMEOUT_WINDOW_SECONDS: u64 = 30 * 24 * 60 * 60;
/// Upper bound accepted by `set_timeout_window` (365 days), matching invoice's expiry cap.
pub const MAX_TIMEOUT_WINDOW_SECONDS: u64 = 31_536_000;

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
    /// * `usdc_asset` - The USDC asset address.
    ///
    /// # Auth
    /// Requires authorization from `admin`.
    ///
    /// # Panics
    /// * `AlreadyInitialized` if the contract has already been initialized.
    /// * `InvalidConfig` if `pool_contract`, `usdc_asset`, and `admin` are not distinct addresses.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Example
    /// ```ignore
    /// client.initialize(&admin, &pool, &usdc);
    /// ```
    pub fn initialize(env: Env, admin: Address, pool_contract: Address, usdc_asset: Address) {
        require_not_paused(&env);
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, EscrowError::AlreadyInitialized);
        }
        if pool_contract == usdc_asset || pool_contract == admin || usdc_asset == admin {
            panic_with_error!(&env, EscrowError::InvalidConfig);
        }
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::PoolContract, &pool_contract);
        env.storage()
            .instance()
            .set(&DataKey::UsdcAsset, &usdc_asset);
        Self::extend_instance_ttl(&env);
        // #867: every other contract announces initialization; escrow must
        // too, so an indexer sees admin/pool/asset without reading storage.
        events::contract_initialized(&env, &admin, &pool_contract, &usdc_asset);
    }

    /// Get a token client for the USDC asset stored in the contract.
    fn usdc_client(env: &Env) -> token::Client<'_> {
        let usdc_id: Address = env
            .storage()
            .instance()
            .get(&DataKey::UsdcAsset)
            .unwrap_or_else(|| panic_with_error!(env, EscrowError::NotInitialized));
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

    /// Returns the admin address this escrow contract was initialized with.
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
    /// * `Address` - The admin address.
    ///
    /// # Example
    /// ```ignore
    /// let admin = client.get_admin();
    /// ```
    pub fn get_admin(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized))
    }

    /// Replaces this contract's Wasm with an installed Wasm using the stored admin.
    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized));
        admin.require_auth();

        env.deployer()
            .update_current_contract_wasm(new_wasm_hash.clone());
        Self::extend_instance_ttl(&env);
        events::upgraded(&env, &new_wasm_hash);
    }

    /// Returns the pool contract address this escrow contract was initialized with.
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
    /// * `Address` - The pool contract address.
    ///
    /// # Example
    /// ```ignore
    /// let pool = client.get_pool_contract();
    /// ```
    pub fn get_pool_contract(env: Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized))
    }

    /// Returns the invoice contract authorized to call `release_to_pool`.
    ///
    /// Issue #820: the principal is read from the configured pool's own
    /// `get_invoice_contract` view, so escrow and the pool can never drift on
    /// who may settle a buyer repayment.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `NotInitialized` if the pool contract address is unset.
    ///
    /// # Returns
    /// * `Address` - The invoice contract address configured on the pool.
    ///
    /// # Example
    /// ```ignore
    /// let invoice = client.get_invoice_contract();
    /// ```
    pub fn get_invoice_contract(env: Env) -> Address {
        let pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized));
        env.invoke_contract(
            &pool,
            &Symbol::new(&env, "get_invoice_contract"),
            Vec::new(&env),
        )
    }

    /// Pins the invoice contract reference used to validate `release_to_pool`
    /// (#997).
    ///
    /// Defense-in-depth on top of issue #820: `release_to_pool` resolves the
    /// invoice contract from the pool's own `get_invoice_contract` view and
    /// requires its authorization. Storing the expected reference here lets
    /// the escrow admin validate that resolution — when a pin is stored and
    /// the pool's view drifts from it, settlement is rejected with
    /// `InvalidConfig` instead of silently following the pool's
    /// configuration.
    ///
    /// Optional by design: a deployment wired before this entry point existed
    /// keeps the #820 behaviour (pool-resolved authorization) until the
    /// admin pins a reference; no re-initialization is required.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_contract` - The invoice contract address to pin.
    ///
    /// # Auth
    /// * Requires `admin.require_auth()` — only the stored admin may pin or
    ///   repin the reference.
    ///
    /// # Panics
    /// * `NotInitialized` if the contract has not been initialized.
    /// * `InvalidConfig` if `invoice_contract` collides with the admin, the
    ///   pool contract, the USDC asset, or this escrow contract — mirroring
    ///   the distinct-address rule enforced by
    ///   [`Self::initialize`](Self::initialize).
    ///
    /// # Returns
    /// * `()` - No value is returned. Emits `invoice_contract_set`.
    ///
    /// # Example
    /// ```ignore
    /// client.set_invoice_contract(&invoice);
    /// ```
    pub fn set_invoice_contract(env: Env, invoice_contract: Address) {
        require_not_paused(&env);
        let admin = Self::require_admin(&env);
        admin.require_auth();
        let pool = Self::get_pool_contract(env.clone());
        let usdc = Self::get_usdc_asset(env.clone());
        if invoice_contract == admin
            || invoice_contract == pool
            || invoice_contract == usdc
            || invoice_contract == env.current_contract_address()
        {
            panic_with_error!(&env, EscrowError::InvalidConfig);
        }
        env.storage()
            .instance()
            .set(&DataKey::InvoiceContract, &invoice_contract);
        events::invoice_contract_set(&env, &admin, &invoice_contract);
        Self::extend_instance_ttl(&env);
    }

    /// Locks USDC in escrow against a funded invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice ID being locked.
    /// * `amount` - The amount to lock.
    /// * `issuer` - The invoice issuer address authorized to receive released funds.
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
    /// client.lock(&invoice_id, &amount, &issuer);
    /// ```
    pub fn lock(env: Env, invoice_id: BytesN<32>, amount: u128, issuer: Address) -> bool {
        require_not_paused(&env);
        let pool = Self::require_pool_auth(&env);

        if amount == 0 || amount > i128::MAX as u128 {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }

        let key = DataKey::Locked(invoice_id.clone());
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, EscrowError::AlreadyLocked);
        }

        let usdc = Self::usdc_client(&env);
        usdc.transfer(&pool, env.current_contract_address(), &(amount as i128));

        let record = EscrowRecord {
            invoice_id: invoice_id.clone(),
            amount,
            locked_at: env.ledger().timestamp(),
            issuer: issuer.clone(),
        };
        env.storage().persistent().set(&key, &record);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        Self::append_history(&env, &invoice_id, EscrowAction::Locked, amount, None);
        Self::extend_instance_ttl(&env);
        events::funds_locked(&env, &invoice_id, &issuer, amount);

        true
    }

    /// Releases escrowed funds to the issuer.
    ///
    /// Wired into `PoolContract::fund_invoice` (issue #819): the pool locks
    /// the funded principal with [`Self::lock`] and immediately releases it to
    /// the issuer here, so the SME receives funds at funding time. Removing
    /// the `Locked` record also writes the [`DataKey::IssuerReleased`] marker
    /// that lets the repayment path (`release_to_pool`) and the default path
    /// (`PoolContract::handle_default`) recognize that the principal left
    /// escrow to the issuer rather than having been removed out of band.
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
    /// * `InvalidRecipient` if issuer is escrow or pool contract address.
    /// * `InvalidRecipient` if issuer does not match the stored issuer address.
    /// * `InvalidAmount` if the stored amount exceeds `i128::MAX` (checked
    ///   conversion fails before any transfer or record removal).
    ///
    /// # Returns
    /// * `bool` - `true` when funds are released.
    ///
    /// # Example
    /// ```ignore
    /// client.release_to_issuer(&invoice_id, &issuer);
    /// ```
    pub fn release_to_issuer(env: Env, invoice_id: BytesN<32>, issuer: Address) -> bool {
        require_not_paused(&env);
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

        if issuer != record.issuer {
            panic_with_error!(&env, EscrowError::InvalidRecipient);
        }

        let usdc = Self::usdc_client(&env);
        // #1001: checked conversion — a corrupted record whose stored amount
        // exceeds `i128::MAX` must surface the typed `InvalidAmount` error
        // before any transfer or record removal, instead of wrapping into an
        // invalid token amount.
        let amount = i128::try_from(record.amount)
            .unwrap_or_else(|_| panic_with_error!(&env, EscrowError::InvalidAmount));
        usdc.transfer(&env.current_contract_address(), &issuer, &amount);

        Self::append_history(
            &env,
            &invoice_id,
            EscrowAction::ReleasedToIssuer,
            record.amount,
            None,
        );
        env.storage().persistent().remove(&key);
        // Issue #819: keep durable evidence that the principal was paid out to
        // the issuer. `release_to_pool` consumes this marker on the repayment
        // path; `PoolContract::handle_default` reads it (via
        // `was_released_to_issuer`) to accept a default where escrow holds no
        // principal.
        let released_key = DataKey::IssuerReleased(invoice_id.clone());
        env.storage()
            .persistent()
            .set(&released_key, &record.amount);
        env.storage()
            .persistent()
            .extend_ttl(&released_key, TTL_THRESHOLD, TTL_EXTEND_TO);
        Self::extend_instance_ttl(&env);
        events::released_to_issuer(&env, &invoice_id, &issuer, record.amount);
        true
    }

    /// Releases escrowed funds back to the pool as repayment.
    ///
    /// Called by the invoice contract during buyer repayment flows:
    /// `buyer → invoice.repay → escrow.release_to_pool → pool`.
    ///
    /// Since issue #819 the funding path releases the locked principal to the
    /// issuer at funding time, so at repayment there is normally no
    /// [`DataKey::Locked`] record left. The replay/unknown-invoice guard
    /// therefore accepts either evidence of a prior lock for this invoice:
    /// the live `Locked` record (principal still in escrow) or the
    /// [`DataKey::IssuerReleased`] marker written by
    /// [`Self::release_to_issuer`] (principal already paid out). Whichever
    /// exists is consumed, so a full settlement can succeed at most once per
    /// funded invoice.
    ///
    /// Repayment may be partial: when a live lock holds more than
    /// `repayment_amount`, the record is kept with its `amount` reduced by the
    /// repaid portion and its original `locked_at` unchanged, so the escrow
    /// still custodies the outstanding principal. Repaying the remainder (or
    /// any amount at least equal to the residual) removes the record.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice whose escrow is being released.
    /// * `repayment_amount` - The amount transferred to the pool (the buyer's
    ///   repayment routed through escrow; the full `face_value` including
    ///   yield when the invoice settles).
    ///
    /// # Auth
    /// Requires authorization from the invoice contract configured on the
    /// pool (issue #820). A contract address only authorizes calls it makes
    /// itself, so this is satisfied exactly when the invoice contract is the
    /// direct caller — never by an unrelated address, and never by a
    /// buyer-signed transaction pretending to carry the pool's authorization.
    ///
    /// When the admin has pinned a reference with
    /// [`Self::set_invoice_contract`] (#997), that pin is validated against
    /// the pool-resolved address, so settlement additionally requires the
    /// pool's configuration to match the escrow admin's pin.
    ///
    /// # Panics
    /// * `NotInitialized` if the contract has not been initialized.
    /// * `NotFound` if neither a lock record nor an issuer-release marker
    ///   exists for the invoice (unknown invoice or replayed release).
    /// * `InvalidAmount` if `repayment_amount` is zero.
    /// * `InvalidConfig` if a pinned invoice contract (see
    ///   [`Self::set_invoice_contract`]) differs from the invoice contract
    ///   configured on the pool (#997).
    ///
    /// # Returns
    /// * `bool` - `true` when funds are returned.
    ///
    /// # Example
    /// ```ignore
    /// client.release_to_pool(&invoice_id, &repayment_amount);
    /// ```
    pub fn release_to_pool(env: Env, invoice_id: BytesN<32>, repayment_amount: u128) -> bool {
        require_not_paused(&env);
        // Issue #820: this entry point is only ever invoked by the invoice
        // contract as part of `buyer → invoice.repay → escrow.release_to_pool
        // → pool`. The previous `require_pool_auth` guard was unsatisfiable
        // from that flow: a contract address (the pool) implicitly authorizes
        // only calls it makes itself, so no buyer-signed transaction can
        // present one, and the repayment path passed in tests only under the
        // blanket `mock_all_auths*` mocks (issue #637). The correct principal
        // is the configured invoice contract — the resolution closed issue
        // #628 shipped — restored here by reading the invoice address from
        // the pool's own configuration and requiring its auth.
        let pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized));
        let invoice_contract: Address = Self::get_invoice_contract(env.clone());
        invoice_contract.require_auth();

        // Issue #997: when the admin pinned the invoice contract reference
        // via `set_invoice_contract`, the pool-resolved address must match
        // it — a pool whose configuration drifted from the pin is a
        // misconfiguration to reject, not a licence to redirect repayments.
        // Deployments that never pinned a reference keep the #820 behaviour.
        if let Some(pinned) = env
            .storage()
            .instance()
            .get::<_, Address>(&DataKey::InvoiceContract)
        {
            if pinned != invoice_contract {
                panic_with_error!(&env, EscrowError::InvalidConfig);
            }
        }

        if repayment_amount == 0 || repayment_amount > i128::MAX as u128 {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }

        let key = DataKey::Locked(invoice_id.clone());
        let released_key = DataKey::IssuerReleased(invoice_id.clone());
        // Repayment can be partial: when a live lock covers more than this
        // repayment, the residual stays locked (with its original
        // `locked_at`) so custody keeps matching the outstanding principal.
        let mut record: Option<EscrowRecord> = env.storage().persistent().get(&key);
        if record.is_none() && !env.storage().persistent().has(&released_key) {
            panic_with_error!(&env, EscrowError::NotFound);
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
            None,
        );
        if let Some(mut record) = record.take() {
            if repayment_amount < record.amount {
                record.amount -= repayment_amount;
                env.storage().persistent().set(&key, &record);
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
            } else {
                env.storage().persistent().remove(&key);
            }
        } else {
            env.storage().persistent().remove(&released_key);
        }
        Self::extend_instance_ttl(&env);
        events::released_to_pool(&env, &invoice_id, &pool, repayment_amount);
        true
    }

    /// Returns whether the locked principal for an invoice was released to
    /// the issuer and that release has not yet been consumed by
    /// `release_to_pool` (issue #819).
    ///
    /// Read by `PoolContract::handle_default`: a `false` from
    /// [`Self::handle_default`] (no lock to return) combined with a `true`
    /// here means the funding path already paid the issuer, which is the
    /// normal post-#819 default case; `false` here means the lock was
    /// removed out of band and the pool must reject the default (#21).
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to check.
    ///
    /// # Auth
    /// None. This is a read-only view.
    ///
    /// # Returns
    /// * `bool` - `true` when an unconsumed issuer-release marker exists.
    ///
    /// # Example
    /// ```ignore
    /// let paid = client.was_released_to_issuer(&invoice_id);
    /// ```
    pub fn was_released_to_issuer(env: Env, invoice_id: BytesN<32>) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey::IssuerReleased(invoice_id))
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
    /// Since issue #819 the funding path releases the principal to the issuer
    /// at funding time, which removes the `Locked` record — a default on a
    /// normally funded invoice therefore finds no record here, returns
    /// `false`, and is accounted by the pool as an unrecoverable LP capital
    /// loss (see `PoolContract::handle_default`). The grace period only gates
    /// locks that are still live in escrow at default time (e.g. a lock not
    /// (yet) paired with an issuer release). The still-open coupling noted in
    /// `contracts/pool/src/test.rs`'s
    /// `test_trigger_default_reverts_when_escrow_grace_period_not_elapsed`
    /// predates #819; that test was rewritten with the issuer payout.
    ///
    /// # Returns
    /// * `bool` - `true` if default handling completed, `false` if no lock exists.
    ///
    /// # Example
    /// ```ignore
    /// let result = client.handle_default(&invoice_id, &caller);
    /// ```
    pub fn handle_default(env: Env, invoice_id: BytesN<32>, caller: Address) -> bool {
        require_not_paused(&env);
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
        if record.amount > i128::MAX as u128 {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }
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
            Some(caller.clone()),
        );
        env.storage().persistent().remove(&key);
        Self::extend_instance_ttl(&env);
        events::default_resolved(&env, &invoice_id, &pool, &caller, record.amount);
        true
    }

    /// Sets the window (seconds after `locked_at`) after which
    /// `release_after_timeout` may return a still-locked record to the pool.
    ///
    /// Mirrors `invoice::set_expiry_window`.
    ///
    /// # Auth
    /// Requires authorization from the stored `admin`.
    ///
    /// # Panics
    /// * `InvalidTimeoutWindow` if `window` is `0` or exceeds `MAX_TIMEOUT_WINDOW_SECONDS`.
    /// * `NotInitialized` if the contract has not been initialized.
    ///
    /// # Example
    /// ```ignore
    /// client.set_timeout_window(&(14 * 24 * 60 * 60));
    /// ```
    pub fn set_timeout_window(env: Env, window: u64) {
        require_not_paused(&env);
        if window == 0 || window > MAX_TIMEOUT_WINDOW_SECONDS {
            panic_with_error!(&env, EscrowError::InvalidTimeoutWindow);
        }
        let admin = Self::require_admin(&env);
        admin.require_auth();
        env.storage()
            .instance()
            .set(&DataKey::TimeoutWindow, &window);
        events::timeout_window_set(&env, window);
        Self::extend_instance_ttl(&env);
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

        Self::append_history(
            &env,
            &invoice_id,
            EscrowAction::TimeoutReleased,
            record.amount,
            None,
        );
        env.storage().persistent().remove(&key);
        Self::extend_instance_ttl(&env);
        events::timeout_released(&env, &invoice_id, &pool, record.amount);
        true
    }

    /// Engages the emergency circuit breaker.
    ///
    /// While paused every state-changing entry point (`lock`,
    /// `release_to_issuer`, `release_to_pool`, `handle_default`) reverts with
    /// `ContractPaused`, so no funds can move in or out of escrow while the
    /// read-only views (`get_locked`, `get_locked_at`, `get_history`,
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

    /// Returns the history of escrow events for an invoice.
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
    /// * `Vec<EscrowEvent>` - The history of escrow events for the invoice, or an empty vector if none exist.
    ///
    /// # Example
    /// ```ignore
    /// let history = client.get_history(&invoice_id);
    /// ```
    pub fn get_history(env: Env, invoice_id: BytesN<32>) -> Vec<EscrowEvent> {
        let key = DataKey::History(invoice_id);
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(&env))
    }

    fn append_history(
        env: &Env,
        invoice_id: &BytesN<32>,
        action: EscrowAction,
        amount: u128,
        caller: Option<Address>,
    ) {
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
            caller,
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

    fn require_admin(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, EscrowError::NotInitialized))
    }
}
