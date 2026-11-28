#![no_std]

use soroban_sdk::xdr::ToXdr;
use soroban_sdk::{
    contract, contractimpl, panic_with_error, token, Address, Bytes, BytesN, Env, Symbol, Vec,
};
use trusttrove_pause::{is_paused, require_not_paused, set_paused};

mod constants;
mod errors;
mod events;
mod test;
mod types;

pub use constants::*;
pub use errors::*;
pub use types::*;

const DEFAULT_MIN_LOCK_SECONDS: u64 = 60;
/// Default minimum lock amount. `0` preserves the pre-#1297 behaviour where
/// any non-zero amount was accepted.
const DEFAULT_MIN_LOCK_AMOUNT: u128 = 0;
const MAX_HISTORY_PAGE_SIZE: u32 = 100;
/// Default `release_after_timeout` window (30 days) used until the admin sets one.
pub const DEFAULT_TIMEOUT_WINDOW_SECONDS: u64 = 30 * 24 * 60 * 60;
/// Upper bound accepted by `set_timeout_window` (365 days), matching invoice's expiry cap.
pub const MAX_TIMEOUT_WINDOW_SECONDS: u64 = 31_536_000;

#[contract]
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    /// Constructs the escrow contract and stores required contract references.
    ///
    /// Runs atomically at deploy time (Soroban protocol constructor, #827):
    /// there is no window in which the instance exists unconfigured, so no
    /// one can front-run initialization.
    ///
    /// Escrow configuration follows the required two-phase initialization path:
    /// 1. Construction: `__constructor(admin, pool_contract, usdc_asset)` atomically
    ///    initializes the contract instance on deployment.
    /// 2. Invoice reference configuration: `set_invoice_contract(invoice_contract)`
    ///    is invoked by the admin to pin the invoice contract ([`DataKey::InvoiceContract`])
    ///    and enforce distinctness across all configured references.
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
    /// * `InvalidConfig` if `pool_contract`, `usdc_asset`, and `admin` are not distinct addresses,
    ///   or if any of them alias this contract's own address.
    ///   A panic here aborts the deployment transaction, so no misconfigured
    ///   instance is ever created.
    ///   `AlreadyInitialized` is unreachable from this entry point (the
    ///   protocol invokes the constructor exactly once) and is kept only for
    ///   error-code stability.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Example
    /// ```ignore
    /// // Deploy-and-construct in a single operation:
    /// // env.register(EscrowContract, (admin, pool, usdc))
    /// ```
    pub fn __constructor(env: Env, admin: Address, pool_contract: Address, usdc_asset: Address) {
        require_not_paused(&env);
        if pool_contract == usdc_asset
            || pool_contract == admin
            || usdc_asset == admin
            || pool_contract == env.current_contract_address()
            || usdc_asset == env.current_contract_address()
            || admin == env.current_contract_address()
        {
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
        env.storage()
            .instance()
            .set(&DataKey::MinLockSeconds, &DEFAULT_MIN_LOCK_SECONDS);
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
    /// Returns the invoice contract address configured on escrow or pool.
    ///
    /// Returns the stored [`DataKey::InvoiceContract`] reference if configured
    /// (via [`Self::set_invoice_contract`]). If unconfigured,
    /// falls back to reading from the configured pool's own `get_invoice_contract`
    /// view (issue #820).
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `NotInitialized` if neither invoice contract nor pool contract address is set.
    ///
    /// # Returns
    /// * `Address` - The invoice contract address.
    ///
    /// # Example
    /// ```ignore
    /// let invoice = client.get_invoice_contract();
    /// ```
    pub fn get_invoice_contract(env: Env) -> Address {
        if let Some(invoice) = env
            .storage()
            .instance()
            .get::<_, Address>(&DataKey::InvoiceContract)
        {
            return invoice;
        }
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
    /// (#997) and `release_to_issuer`.
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
    ///   the distinct-address rule enforced at deploy time via
    ///   [`Self::__constructor`](Self::__constructor).
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
    /// * `InvalidAmount` if the amount is zero or exceeds `i128::MAX`.
    /// * `AmountBelowMinimum` if the amount is below the admin-configured
    ///   minimum lock amount (#1297). The default minimum is `0`, so existing
    ///   deployments keep accepting any non-zero amount until the admin sets
    ///   one.
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
        // #1297: future locks must meet the admin-configured minimum. Existing
        // locks are untouched and stay releasable.
        if amount < Self::min_lock_amount(&env) {
            panic_with_error!(&env, EscrowError::AmountBelowMinimum);
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
        // #1196: record the ledger sequence alongside the lock. This lives in
        // a separate key so pre-existing `Locked` records keep decoding.
        let ledger_key = DataKey::LockedLedger(invoice_id.clone());
        env.storage()
            .persistent()
            .set(&ledger_key, &(env.ledger().sequence() as u64));
        env.storage()
            .persistent()
            .extend_ttl(&ledger_key, TTL_THRESHOLD, TTL_EXTEND_TO);
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
    /// * `InvalidRecipient` if issuer is escrow, pool, invoice contract, or token asset address.
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

        let usdc = Self::get_usdc_asset(env.clone());
        if issuer == env.current_contract_address() || issuer == pool || issuer == usdc {
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

        let recorded_issuer = record.issuer.clone();
        if issuer != recorded_issuer {
            panic_with_error!(&env, EscrowError::InvalidRecipient);
        }

        let usdc = Self::usdc_client(&env);
        // #1001: checked conversion — a corrupted record whose stored amount
        // exceeds `i128::MAX` must surface the typed `InvalidAmount` error
        // before any transfer or record removal, instead of wrapping into an
        // invalid token amount.
        let amount = i128::try_from(record.amount)
            .unwrap_or_else(|_| panic_with_error!(&env, EscrowError::InvalidAmount));
        // Pay from the locked invoice record, never from the caller-supplied
        // address after it has merely passed validation.
        usdc.transfer(&env.current_contract_address(), &recorded_issuer, &amount);
        // #1296: record the recipient only after the transfer succeeds. A
        // failed transfer reverts this transaction, so the previous recipient
        // is preserved.
        Self::record_last_release_recipient(&env, &invoice_id, &recorded_issuer);

        Self::append_history(
            &env,
            &invoice_id,
            EscrowAction::ReleasedToIssuer,
            record.amount,
            None,
        );
        env.storage().persistent().remove(&key);
        env.storage()
            .persistent()
            .remove(&DataKey::LockedLedger(invoice_id.clone()));
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
        events::released_to_issuer(&env, &invoice_id, &recorded_issuer, record.amount);
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
    /// * `InvalidAmount` if `repayment_amount` is zero or exceeds `i128::MAX`.
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
        let pool_resolved: Address = env.invoke_contract(
            &pool,
            &Symbol::new(&env, "get_invoice_contract"),
            Vec::new(&env),
        );

        // Issue #997: when the admin pinned the invoice contract reference
        // via `set_invoice_contract`, the pool-resolved address
        // must match it — a pool whose configuration drifted from the stored
        // reference is a misconfiguration to reject, not a licence to redirect repayments.
        // Deployments that never pinned a reference keep the #820 behaviour.
        let invoice_contract: Address = if let Some(pinned) = env
            .storage()
            .instance()
            .get::<_, Address>(&DataKey::InvoiceContract)
        {
            if pinned != pool_resolved {
                panic_with_error!(&env, EscrowError::InvalidConfig);
            }
            pinned
        } else {
            pool_resolved
        };
        invoice_contract.require_auth();

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
        Self::record_last_release_recipient(&env, &invoice_id, &pool);

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
                env.storage()
                    .persistent()
                    .remove(&DataKey::LockedLedger(invoice_id.clone()));
            }
        } else {
            env.storage().persistent().remove(&released_key);
            env.storage()
                .persistent()
                .remove(&DataKey::LockedLedger(invoice_id.clone()));
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
        let key = DataKey::IssuerReleased(invoice_id);
        if env.storage().persistent().has(&key) {
            env.storage()
                .persistent()
                .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
            true
        } else {
            false
        }
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
    /// This grace period (measured from
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
        // A timestamp rollback (or a malformed record seeded with a future
        // `locked_at`) must fail the grace-period check as a typed
        // authorization error, not underflow `now - locked_at`.
        if now < record.locked_at || now - record.locked_at < Self::min_lock_seconds(&env) {
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
        Self::record_last_release_recipient(&env, &invoice_id, &pool);

        Self::append_history(
            &env,
            &invoice_id,
            EscrowAction::DefaultHandled,
            record.amount,
            Some(caller.clone()),
        );
        env.storage().persistent().remove(&key);
        env.storage()
            .persistent()
            .remove(&DataKey::LockedLedger(invoice_id.clone()));
        Self::extend_instance_ttl(&env);
        let lock_age = now.saturating_sub(record.locked_at);
        events::default_resolved(&env, &invoice_id, &pool, &caller, record.amount, lock_age);
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
        let key = DataKey::Locked(invoice_id);
        match env.storage().persistent().get::<_, EscrowRecord>(&key) {
            Some(r) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                r.amount
            }
            None => 0,
        }
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
        let key = DataKey::Locked(invoice_id);
        match env.storage().persistent().get::<_, EscrowRecord>(&key) {
            Some(r) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                r.locked_at
            }
            None => 0,
        }
    }

    fn min_lock_seconds(env: &Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::MinLockSeconds)
            .unwrap_or(DEFAULT_MIN_LOCK_SECONDS)
    }

    fn min_lock_amount(env: &Env) -> u128 {
        env.storage()
            .instance()
            .get(&DataKey::MinLockAmount)
            .unwrap_or(DEFAULT_MIN_LOCK_AMOUNT)
    }

    /// Persists the recipient of a successful release/default (#1296). Called
    /// only after the token transfer succeeds, so a reverted transfer cannot
    /// change the stored recipient.
    fn record_last_release_recipient(env: &Env, invoice_id: &BytesN<32>, recipient: &Address) {
        let key = DataKey::LastReleaseRecipient(invoice_id.clone());
        env.storage().persistent().set(&key, recipient);
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    /// Returns the first timestamp at which the minimum lock no longer blocks
    /// default handling. Returns `None` when there is no active escrow.
    /// Fails with `MathOverflow` if the expiration cannot fit in a `u64`.
    pub fn get_lock_expiration(env: Env, invoice_id: BytesN<32>) -> Option<u64> {
        let key = DataKey::Locked(invoice_id);
        let storage = env.storage().persistent();
        let record: EscrowRecord = storage.get(&key)?;
        let expiration = record
            .locked_at
            .checked_add(Self::min_lock_seconds(&env))
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::MathOverflow));
        storage.extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        Some(expiration)
    }

    /// Returns the ledger sequence when the escrow was locked for an invoice.
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
    /// * `u64` - The lock-creation ledger sequence, or `0` when the record is
    ///   absent or was created before ledger tracking existed (unknown).
    ///
    /// # Example
    /// ```ignore
    /// let ledger = client.get_locked_ledger(&invoice_id);
    /// ```
    pub fn get_locked_ledger(env: Env, invoice_id: BytesN<32>) -> u64 {
        let key = DataKey::Locked(invoice_id.clone());
        if !env.storage().persistent().has(&key) {
            return 0;
        }
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
        let ledger_key = DataKey::LockedLedger(invoice_id);
        match env.storage().persistent().get::<_, u64>(&ledger_key) {
            Some(ledger) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&ledger_key, TTL_THRESHOLD, TTL_EXTEND_TO);
                ledger
            }
            // Legacy records created before #1196 have no ledger key.
            None => 0,
        }
    }

    /// Returns the cumulative amount returned to the pool for an invoice.
    ///
    /// Sums successful [`EscrowAction::ReleasedToPool`] and
    /// [`EscrowAction::DefaultHandled`] history entries with checked
    /// arithmetic. Releases to the issuer and timeout releases are excluded:
    /// they are not repayments reconciled against the invoice.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to query.
    ///
    /// # Auth
    /// None. This is a read-only view.
    ///
    /// # Panics
    /// Panics with [`EscrowError::MathOverflow`] if the total cannot fit in
    /// `u128`.
    ///
    /// # Returns
    /// * `u128` - The cumulative returned amount, or `0` when no matching
    ///   history exists.
    ///
    /// # Example
    /// ```ignore
    /// let total = client.get_total_returned_to_pool(&invoice_id);
    /// ```
    pub fn get_total_returned_to_pool(env: Env, invoice_id: BytesN<32>) -> u128 {
        let history = Self::get_history(env.clone(), invoice_id);
        let mut total: u128 = 0;
        for event in history.iter() {
            match &event.action {
                EscrowAction::ReleasedToPool | EscrowAction::DefaultHandled => {
                    total = total
                        .checked_add(event.amount)
                        .unwrap_or_else(|| panic_with_error!(&env, EscrowError::MathOverflow));
                }
                _ => {}
            }
        }
        total
    }

    /// Returns the configured asset's custody token balance held by the escrow contract.
    ///
    /// # Invariants and Reconciliation
    /// This view inspects the balance of `usdc_asset` at `env.current_contract_address()`.
    /// The returned custody balance may differ from the sum of active [`Self::get_locked`]
    /// records because:
    /// - Direct or unsolicited token transfers to the escrow contract address increase
    ///   the token custody balance without creating or updating an escrow lock record.
    /// - Residual balances or dust following partial repayments or fee reconciliations
    ///   may remain held in escrow.
    /// - Inter-transaction states between funding lock and release may temporarily exhibit
    ///   non-matching totals.
    ///
    /// # Safety
    /// This method is purely a read-only query. It performs no transfers, burns, or
    /// balance modifications, and does not alter contract storage.
    pub fn get_custody_balance(env: Env) -> i128 {
        let usdc = Self::usdc_client(&env);
        usdc.balance(&env.current_contract_address())
    }

    /// Returns a deterministic digest (SHA-256) committing to the active lock's
    /// invoice ID, amount, timestamp, and recipient.
    ///
    /// Returns `None` if no active lock exists for the specified invoice.
    /// When a partial repayment or release occurs via [`Self::release_to_pool`],
    /// the recorded amount decreases in place, deterministically producing a
    /// different checksum.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - 32-byte invoice identifier to query.
    pub fn get_lock_checksum(env: Env, invoice_id: BytesN<32>) -> Option<BytesN<32>> {
        let key = DataKey::Locked(invoice_id);
        let record = env.storage().persistent().get::<_, EscrowRecord>(&key)?;
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);

        let mut payload = Bytes::new(&env);
        payload.append(&record.invoice_id.into());
        payload.append(&Bytes::from_slice(&env, &record.amount.to_be_bytes()));
        payload.append(&Bytes::from_slice(&env, &record.locked_at.to_be_bytes()));
        payload.append(&record.issuer.to_xdr(&env));
        Some(env.crypto().sha256(&payload).into())
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
        let key = DataKey::History(invoice_id.clone());
        match env.storage().persistent().get::<_, Vec<EscrowEvent>>(&key) {
            Some(history) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                let ledger_key = DataKey::HistoryLedgers(invoice_id);
                if env.storage().persistent().has(&ledger_key) {
                    env.storage().persistent().extend_ttl(
                        &ledger_key,
                        TTL_THRESHOLD,
                        TTL_EXTEND_TO,
                    );
                }
                history
            }
            None => Vec::new(&env),
        }
    }

    /// Returns the versioned history of escrow events for an invoice, including
    /// the ledger sequence at which each entry was recorded (#1295).
    ///
    /// Entries written before ledger sequence tracking was deployed decode
    /// cleanly with `ledger_sequence == 0`. Missing history returns an empty
    /// vector without materializing any storage keys.
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
    /// * `Vec<EscrowEventV2>` - Append-ordered history entries with ledger
    ///   sequences, or an empty vector if none exist.
    ///
    /// # Example
    /// ```ignore
    /// let history = client.get_history_v2(&invoice_id);
    /// ```
    pub fn get_history_v2(env: Env, invoice_id: BytesN<32>) -> Vec<EscrowEventV2> {
        let (history, ledgers) = Self::load_history_with_ledgers(&env, &invoice_id);
        let mut entries = Vec::new(&env);
        for (idx, event) in history.iter().enumerate() {
            let ledger_sequence = ledgers.get(idx as u32).unwrap_or(0);
            entries.push_back(Self::into_history_v2(event, ledger_sequence));
        }
        entries
    }

    /// Returns the number of escrow history events recorded for an invoice.
    ///
    /// This view allows consumers to plan paginated reads without loading the
    /// entire event vector. Returns `0` if no history exists for the invoice.
    /// The count is preserved even after the active lock is released or removed.
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
    /// * `u32` - The number of history events recorded for the invoice, or `0`
    ///   if no history entry exists.
    ///
    /// # Example
    /// ```ignore
    /// let count = client.get_history_count(&invoice_id);
    /// ```
    pub fn get_history_count(env: Env, invoice_id: BytesN<32>) -> u32 {
        let key = DataKey::History(invoice_id.clone());
        match env.storage().persistent().get::<_, Vec<EscrowEvent>>(&key) {
            Some(history) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);
                let ledger_key = DataKey::HistoryLedgers(invoice_id);
                if env.storage().persistent().has(&ledger_key) {
                    env.storage().persistent().extend_ttl(
                        &ledger_key,
                        TTL_THRESHOLD,
                        TTL_EXTEND_TO,
                    );
                }
                history.len()
            }
            None => 0,
        }
    }

    /// Reads up to 100 escrow events in append order, starting at `offset`.
    /// Returns the matching events and the total number of stored events.
    /// An offset at or beyond the end returns an empty page with the same total.
    /// A limit outside 1–100 fails with `InvalidConfig`.
    pub fn get_history_page(
        env: Env,
        invoice_id: BytesN<32>,
        offset: u32,
        limit: u32,
    ) -> (Vec<EscrowEvent>, u32) {
        if !(1..=MAX_HISTORY_PAGE_SIZE).contains(&limit) {
            panic_with_error!(&env, EscrowError::InvalidConfig);
        }

        let history = Self::get_history(env, invoice_id);
        let total = history.len();
        let start = offset.min(total);
        let page_len = limit.min(total - start);
        (history.slice(start..start + page_len), total)
    }

    /// Reads up to 100 versioned escrow history entries (including each entry's
    /// `ledger_sequence`) in append order, starting at `offset` (#1295).
    ///
    /// Returns `(entries, total)` where `total` is the total number of stored
    /// history events for the invoice. An `offset` at or beyond `total` returns
    /// an empty page with the same `total`. Legacy entries written before
    /// ledger sequence tracking decode with `ledger_sequence == 0`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to query.
    /// * `offset` - Zero-based index of the first entry to return.
    /// * `limit` - Maximum number of entries to return (`1..=100`).
    ///
    /// # Auth
    /// None. This is a read-only view, callable while the contract is paused.
    ///
    /// # Panics
    /// * `InvalidConfig` if `limit` is outside `1..=100`.
    ///
    /// # Returns
    /// * `(Vec<EscrowEventV2>, u32)` - The requested page and total entry count.
    pub fn get_history_page_v2(
        env: Env,
        invoice_id: BytesN<32>,
        offset: u32,
        limit: u32,
    ) -> (Vec<EscrowEventV2>, u32) {
        if !(1..=MAX_HISTORY_PAGE_SIZE).contains(&limit) {
            panic_with_error!(&env, EscrowError::InvalidConfig);
        }

        let (history, ledgers) = Self::load_history_with_ledgers(&env, &invoice_id);
        let total = history.len();
        let start = offset.min(total);
        let page_len = limit.min(total - start);
        let end = start + page_len;
        let mut page = Vec::new(&env);
        for idx in start..end {
            if let Some(event) = history.get(idx) {
                let ledger_sequence = ledgers.get(idx).unwrap_or(0);
                page.push_back(Self::into_history_v2(event, ledger_sequence));
            }
        }
        (page, total)
    }

    fn load_history_with_ledgers(
        env: &Env,
        invoice_id: &BytesN<32>,
    ) -> (Vec<EscrowEvent>, Vec<u32>) {
        let key = DataKey::History(invoice_id.clone());
        let Some(history) = env.storage().persistent().get::<_, Vec<EscrowEvent>>(&key) else {
            return (Vec::new(env), Vec::new(env));
        };
        env.storage()
            .persistent()
            .extend_ttl(&key, TTL_THRESHOLD, TTL_EXTEND_TO);

        let ledger_key = DataKey::HistoryLedgers(invoice_id.clone());
        let ledgers = match env.storage().persistent().get::<_, Vec<u32>>(&ledger_key) {
            Some(stored) => {
                env.storage()
                    .persistent()
                    .extend_ttl(&ledger_key, TTL_THRESHOLD, TTL_EXTEND_TO);
                stored
            }
            None => Vec::new(env),
        };
        (history, ledgers)
    }

    fn into_history_v2(event: EscrowEvent, ledger_sequence: u32) -> EscrowEventV2 {
        EscrowEventV2 {
            invoice_id: event.invoice_id,
            action: event.action,
            amount: event.amount,
            timestamp: event.timestamp,
            ledger_sequence,
            caller: event.caller,
        }
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
        let prior_len = history.len();
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

        let ledger_key = DataKey::HistoryLedgers(invoice_id.clone());
        let mut ledgers: Vec<u32> = env
            .storage()
            .persistent()
            .get(&ledger_key)
            .unwrap_or(Vec::new(env));
        while ledgers.len() < prior_len {
            ledgers.push_back(0u32);
        }
        ledgers.push_back(env.ledger().sequence());
        env.storage().persistent().set(&ledger_key, &ledgers);
        env.storage()
            .persistent()
            .extend_ttl(&ledger_key, TTL_THRESHOLD, TTL_EXTEND_TO);
    }

    /// Reports which escrow write entry points `caller` may invoke under the
    /// current configuration, so an operator can check before submitting a
    /// transaction (issue #1198).
    ///
    /// Caller rules mirrored from the write paths:
    /// * `lock`, `release_to_issuer`: the configured pool contract.
    /// * `release_to_pool`: the invoice contract the pool's
    ///   `get_invoice_contract` view reports, which must also equal the
    ///   reference pinned with [`Self::set_invoice_contract`] when one is set,
    ///   exactly as `release_to_pool` resolves it. If the pool cannot answer,
    ///   or the pin disagrees with it, no address may call it and the flag is
    ///   `false`.
    /// * `handle_default`: the stored admin or the configured pool contract.
    /// * `release_after_timeout`: anyone.
    ///
    /// While the contract is paused every one of these reverts with
    /// `ContractPaused`, so all action flags are `false` and `paused` is
    /// `true`.
    ///
    /// Per-invoice conditions are not evaluated (lock or issuer-release marker
    /// present, recorded issuer, default grace period, timeout deadline). The
    /// result is a preview only and never substitutes for authorization: each
    /// write keeps its own `require_auth` and checks.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `caller` - The address to check.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Storage
    /// Read-only: nothing is written and no TTL is extended. The only external
    /// call is the pool's read-only `get_invoice_contract` view, made when the
    /// contract is not paused.
    ///
    /// # Panics
    /// * `NotInitialized` if the admin or pool contract address is unset.
    ///
    /// # Returns
    /// * `ReleaseAuthorization` - One flag per action, plus the pause state.
    ///
    /// # Example
    /// ```ignore
    /// let allowed = client.get_release_authorization(&operator);
    /// if allowed.handle_default {
    ///     client.handle_default(&invoice_id, &operator);
    /// }
    /// ```
    pub fn get_release_authorization(env: Env, caller: Address) -> ReleaseAuthorization {
        let admin = Self::require_admin(&env);
        let pool = Self::get_pool_contract(env.clone());
        let paused = is_paused(&env);
        let live = !paused;
        let is_pool = caller == pool;

        ReleaseAuthorization {
            paused,
            lock: live && is_pool,
            release_to_issuer: live && is_pool,
            release_to_pool: live && Self::is_release_to_pool_caller(&env, &pool, &caller),
            handle_default: live && (is_pool || caller == admin),
            release_after_timeout: live,
        }
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

    /// Whether `caller` is the principal `release_to_pool` would authorize:
    /// the invoice contract the pool reports, provided it matches the pinned
    /// reference when one is stored. Read-only counterpart of the resolution
    /// at the top of [`Self::release_to_pool`]; a pool that cannot answer
    /// means nobody can currently call it.
    fn is_release_to_pool_caller(env: &Env, pool: &Address, caller: &Address) -> bool {
        let resolved = env.try_invoke_contract::<Address, soroban_sdk::Error>(
            pool,
            &Symbol::new(env, "get_invoice_contract"),
            Vec::new(env),
        );
        let Ok(Ok(resolved)) = resolved else {
            return false;
        };
        if let Some(pinned) = env
            .storage()
            .instance()
            .get::<_, Address>(&DataKey::InvoiceContract)
        {
            if pinned != resolved {
                return false;
            }
        }
        *caller == resolved
    }

    fn require_admin(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic_with_error!(env, EscrowError::NotInitialized))
    }
}
