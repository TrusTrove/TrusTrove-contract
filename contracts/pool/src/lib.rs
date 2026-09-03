#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, token, Address, BytesN, Env, IntoVal, Symbol, Vec,
};

mod constants;
mod errors;
mod events;
mod test;
mod types;

pub use constants::*;

pub use errors::*;
pub use types::*;

/// Minimum initial deposit floor (1 USDC = 10_000_000 stroops).
/// Prevents share-price griefing by requiring the initial deposit in an empty pool
/// to be at least this floor.
pub const MIN_INITIAL_DEPOSIT: u128 = 10_000_000;

#[contract]
pub struct PoolContract;

#[derive(Clone, Copy)]
struct PoolTotals {
    shares: u128,
    deposits: u128,
    funded: u128,
    yield_distributed: u128,
    loss_realised: u128,
    active_invoices: u32,
    max_utilization_bps: u32,
}

#[contractimpl]
impl PoolContract {
    /// Initializes the pool contract with admin and external contract references.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `admin` - The admin address for this contract.
    /// * `invoice_contract` - The invoice contract address.
    /// * `escrow_contract` - The escrow contract address.
    /// * `usdc_asset` - The USDC asset address.
    /// * `registry_contract` - The registry contract address, consulted by
    ///   `fund_invoice` to re-verify the issuer and buyer are still verified
    ///   before pool capital is committed.
    ///
    /// # Auth
    /// Requires authorization from `admin`.
    ///
    /// # Wiring order
    /// `escrow_contract` must already be initialized before this call, since
    /// `initialize` cross-checks `escrow_contract.get_usdc_asset()` against
    /// its own `usdc_asset` to catch a misconfigured deploy where escrow was
    /// wired up with a different token.
    ///
    /// # Panics
    /// * `AlreadyInitialized` if the contract has already been initialized.
    /// * `InvalidConfiguration` if any two of `admin`, `invoice_contract`,
    ///   `escrow_contract`, `usdc_asset`, and `registry_contract` are the
    ///   same address.
    /// * `EscrowAssetMismatch` if `escrow_contract`'s configured USDC asset
    ///   does not match `usdc_asset`.
    ///
    /// # Returns
    /// * `()` - No value is returned.
    ///
    /// # Example
    /// ```ignore
    /// escrow_client.initialize(&admin, &pool, &invoice, &usdc); // escrow first
    /// client.initialize(&admin, &invoice, &escrow, &usdc, &registry);
    /// ```
    pub fn initialize(
        env: Env,
        admin: Address,
        invoice_contract: Address,
        escrow_contract: Address,
        usdc_asset: Address,
        registry_contract: Address,
    ) {
        if Self::admin(&env).is_some() {
            panic_with_error!(&env, PoolError::AlreadyInitialized);
        }
        if admin == invoice_contract
            || admin == escrow_contract
            || admin == usdc_asset
            || admin == registry_contract
            || invoice_contract == escrow_contract
            || invoice_contract == usdc_asset
            || invoice_contract == registry_contract
            || escrow_contract == usdc_asset
            || escrow_contract == registry_contract
            || usdc_asset == registry_contract
        {
            panic_with_error!(&env, PoolError::InvalidConfiguration);
        }

        // Cross-check that the escrow contract being wired in was itself
        // initialized with the same usdc_asset. A mismatch here would only
        // otherwise surface later as a failed token transfer inside
        // fund_invoice's escrow.lock call, since escrow.lock pulls funds
        // using escrow's own configured token client. This requires
        // escrow_contract to already be initialized at the time pool.initialize
        // is called.
        let args = Vec::new(&env);
        let escrow_usdc_asset: Address =
            env.invoke_contract(&escrow_contract, &Symbol::new(&env, "get_usdc_asset"), args);
        if escrow_usdc_asset != usdc_asset {
            panic_with_error!(&env, PoolError::EscrowAssetMismatch);
        }

        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage()
            .instance()
            .set(&DataKey::InvoiceContract, &invoice_contract);
        env.storage()
            .instance()
            .set(&DataKey::EscrowContract, &escrow_contract);
        env.storage()
            .instance()
            .set(&DataKey::UsdcAsset, &usdc_asset);
        env.storage()
            .instance()
            .set(&DataKey::RegistryContract, &registry_contract);
        env.storage().instance().set(&DataKey::TotalShares, &0u128);
        env.storage()
            .instance()
            .set(&DataKey::TotalDeposits, &0u128);
        env.storage().instance().set(&DataKey::TotalFunded, &0u128);
        env.storage()
            .instance()
            .set(&DataKey::TotalYieldDistributed, &0u128);
        env.storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &0u32);
        env.storage()
            .instance()
            .set(&DataKey::MaxUtilizationBps, &8500u32);
        env.storage()
            .instance()
            .set(&DataKey::TotalLossRealised, &0u128);
        Self::extend_instance_ttl(&env);
    }

    /// Returns the USDC asset used by the pool.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * Panics if the contract has not been initialized (missing `UsdcAsset`).
    ///
    /// # Returns
    /// * `Address` - The USDC asset address.
    ///
    /// # Example
    /// ```ignore
    /// let asset = client.get_usdc_asset();
    /// ```
    pub fn get_usdc_asset(env: Env) -> Address {
        Self::usdc(&env)
    }

    /// Deposits USDC from an LP and issues pool shares.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `lp` - The liquidity provider address.
    /// * `usdc_amount` - The amount of USDC to deposit.
    ///
    /// # Auth
    /// Requires self-authorization from `lp` (via `lp.require_auth()`).
    ///
    /// # Panics
    /// * `InvalidAmount` if `usdc_amount` is zero or if initial deposit is below `MIN_INITIAL_DEPOSIT`.
    /// * `MinimumDeposit` if the deposit is too small to mint at least 1 share
    ///   at the current share price (prevents 0-share dust deposits).
    /// * `Overflow` if `usdc_amount * total_shares` would overflow `u128`
    ///   while computing the proportional share price.
    ///
    /// # Returns
    /// * `u128` - The number of shares issued.
    ///
    /// # Example
    /// ```ignore
    /// let shares = client.deposit(&lp, 10_000_000);
    /// ```
    pub fn deposit(env: Env, lp: Address, usdc_amount: u128) -> u128 {
        Self::require_initialized(&env);
        lp.require_auth();
        if usdc_amount == 0 {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }

        let totals = Self::totals(&env);
        let total_shares = totals.shares;
        let total_deposits = totals.deposits;

        if (total_shares == 0 || total_deposits == 0) && usdc_amount < MIN_INITIAL_DEPOSIT {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }

        let shares_to_issue = if total_shares == 0 || total_deposits == 0 {
            usdc_amount
        } else {
            let scaled = usdc_amount
                .checked_mul(total_shares)
                .unwrap_or_else(|| panic_with_error!(&env, PoolError::Overflow));
            scaled / total_deposits
        };

        // Dust-attack guard: once the pool accrues yield, the share price
        // (total_deposits / total_shares) rises above 1.0, so a sufficiently
        // small deposit can round down to 0 shares while its USDC is still
        // pulled into total_deposits, silently donating the deposit to existing
        // LPs. Reject any deposit that would mint 0 shares so the caller keeps
        // their funds. This check runs before the token transfer, so no USDC
        // leaves the depositor on the rejection path.
        if shares_to_issue == 0 {
            panic_with_error!(&env, PoolError::MinimumDeposit);
        }

        let usdc_id = Self::usdc(&env);
        let usdc = token::Client::new(&env, &usdc_id);
        usdc.transfer(&lp, &env.current_contract_address(), &(usdc_amount as i128));

        env.storage()
            .instance()
            .set(&DataKey::TotalShares, &(total_shares + shares_to_issue));
        env.storage()
            .instance()
            .set(&DataKey::TotalDeposits, &(total_deposits + usdc_amount));

        let lp_shares_key = DataKey::LPShares(lp.clone());
        let lp_shares: u128 = env.storage().persistent().get(&lp_shares_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&lp_shares_key, &(lp_shares + shares_to_issue));
        env.storage()
            .persistent()
            .extend_ttl(&lp_shares_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        let lp_deposit_count_key = DataKey::LPDepositCount(lp.clone());
        let count: u32 = env
            .storage()
            .persistent()
            .get(&lp_deposit_count_key)
            .unwrap_or(0);
        env.storage()
            .persistent()
            .set(&lp_deposit_count_key, &(count + 1));
        env.storage()
            .persistent()
            .extend_ttl(&lp_deposit_count_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        let lp_init_key = DataKey::LPInitialDeposit(lp.clone());
        let init_dep: u128 = env.storage().persistent().get(&lp_init_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&lp_init_key, &(init_dep + usdc_amount));
        env.storage()
            .persistent()
            .extend_ttl(&lp_init_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        events::lp_deposited(&env, &lp, usdc_amount, shares_to_issue);
        Self::extend_instance_ttl(&env);
        shares_to_issue
    }

    /// Withdraws shares from the pool and transfers USDC to the LP.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `lp` - The liquidity provider address.
    /// * `shares` - The number of shares to withdraw.
    ///
    /// # Auth
    /// Requires self-authorization from `lp` (via `lp.require_auth()`).
    ///
    /// # Panics
    /// * `InvalidAmount` if `shares` is zero.
    /// * `NoShares` if the LP has no shares.
    /// * `InsufficientShares` if the LP does not own enough shares.
    /// * `InsufficientLiquidity` if the pool lacks enough available USDC.
    /// * `Overflow` if `shares * total_deposits` (or `shares * lp_initial_deposit`)
    ///   would overflow `u128` while computing the redemption amount.
    ///
    /// # Notes
    /// On full withdrawal (remaining shares reach zero), `LPInitialDeposit`
    /// and `LPDepositCount` are removed from storage. This ensures a
    /// subsequent re-deposit starts with a fresh initial-deposit basis
    /// and an accurate deposit count.
    ///
    /// # Returns
    /// * `u128` - The amount of USDC returned.
    ///
    /// # Example
    /// ```ignore
    /// let returned = client.withdraw(&lp, 500);
    /// ```
    pub fn withdraw(env: Env, lp: Address, shares: u128) -> u128 {
        Self::require_initialized(&env);
        lp.require_auth();
        if shares == 0 {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }

        let lp_shares_key = DataKey::LPShares(lp.clone());
        let lp_shares: u128 = env
            .storage()
            .persistent()
            .get(&lp_shares_key)
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::NoShares));
        if shares > lp_shares {
            panic_with_error!(&env, PoolError::InsufficientShares);
        }

        let totals = Self::totals(&env);
        let total_shares = totals.shares;
        let total_deposits = totals.deposits;
        let total_funded = totals.funded;
        let available = total_deposits - total_funded;

        let scaled = shares
            .checked_mul(total_deposits)
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::Overflow));
        let usdc_to_return = scaled / total_shares;
        if usdc_to_return > available {
            panic_with_error!(&env, PoolError::InsufficientLiquidity);
        }

        let usdc_id = Self::usdc(&env);
        let usdc = token::Client::new(&env, &usdc_id);
        usdc.transfer(
            &env.current_contract_address(),
            &lp,
            &(usdc_to_return as i128),
        );

        env.storage()
            .instance()
            .set(&DataKey::TotalShares, &(total_shares - shares));
        env.storage()
            .instance()
            .set(&DataKey::TotalDeposits, &(total_deposits - usdc_to_return));

        let remaining_shares = lp_shares - shares;
        env.storage()
            .persistent()
            .set(&lp_shares_key, &remaining_shares);
        env.storage()
            .persistent()
            .extend_ttl(&lp_shares_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        if remaining_shares == 0 {
            // Full withdrawal: reset LP-scoped storage to prevent stale state
            // on re-deposit. LPInitialDeposit is zeroed below via the
            // principal_portion calculation; LPDepositCount must be removed too.
            let dep_count_key = DataKey::LPDepositCount(lp.clone());
            env.storage().persistent().remove(&dep_count_key);
        }

        let init_dep_key = DataKey::LPInitialDeposit(lp.clone());
        let init_dep: u128 = env.storage().persistent().get(&init_dep_key).unwrap_or(0);
        let principal_scaled = shares
            .checked_mul(init_dep)
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::Overflow));
        let principal_portion = principal_scaled / (lp_shares);
        let yield_earned = usdc_to_return.saturating_sub(principal_portion);

        let new_init_dep = init_dep.saturating_sub(principal_portion);
        if new_init_dep > 0 {
            env.storage().persistent().set(&init_dep_key, &new_init_dep);
            env.storage()
                .persistent()
                .extend_ttl(&init_dep_key, TTL_THRESHOLD, TTL_EXTEND_TO);
        } else {
            env.storage().persistent().remove(&init_dep_key);
        }

        let yield_key = DataKey::LPYieldEarned(lp.clone());
        let prev_yield: u128 = env.storage().persistent().get(&yield_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&yield_key, &(prev_yield + yield_earned));
        env.storage()
            .persistent()
            .extend_ttl(&yield_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        events::lp_withdrawn(&env, &lp, usdc_to_return, shares);
        Self::extend_instance_ttl(&env);
        usdc_to_return
    }

    /// Funds a listed invoice by moving USDC through escrow and invoice contracts.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice to fund.
    ///
    /// # Auth
    /// **Permissionless.** Any caller can trigger funding for an invoice, provided
    /// the invoice passes all on-chain eligibility checks:
    /// 1. Invoice status must be `Listed` (status 1)
    /// 2. Invoice funding asset must match the pool's asset (USDC)
    /// 3. Pool must have sufficient available liquidity
    /// 4. Funding would not cause pool utilization to exceed the `max_utilization_bps` cap
    ///
    /// See README §"Known Centralization Risks & Roadmap" for the longer-term
    /// governance design that will let LPs signal approval on funding decisions.
    ///
    /// # Registry re-verification
    /// The issuer and buyer's registry verification is re-checked here, in
    /// addition to the checks already performed by `invoice.create()` and
    /// `invoice.list_for_financing()`. This is the point where pool capital
    /// is actually committed, so a revocation that happened after listing
    /// must still block new funding. Once funding succeeds, verification is
    /// **not** re-checked again at any later step (`mark_shipped`,
    /// `confirm_delivery`, `repay`, `trigger_default`) — see
    /// `InvoiceContract::list_for_financing` for the rationale.
    ///
    /// # Panics
    /// * `InvoiceNotListed` if the invoice is not in listed status.
    /// * `AlreadyFunded` if a `FundedInvoice` entry already exists for this invoice id.
    /// * `IssuerNotVerified` if the invoice issuer's registry verification has
    ///   since been revoked.
    /// * `BuyerNotVerified` if the invoice buyer's registry verification has
    ///   since been revoked.
    /// * `AssetMismatch` if the invoice funding asset does not match pool USDC.
    /// * `InvalidAmount` if the computed funded amount is zero.
    /// * `InsufficientLiquidity` if the pool does not have enough funds.
    /// * `UtilizationCapExceeded` if funding would push utilization above the cap.
    /// * `Overflow` if the resulting utilization calculation overflows `u128`.
    ///
    /// # Returns
    /// * `bool` - `true` when the invoice is funded.
    ///
    /// # Example
    /// ```ignore
    /// client.fund_invoice(&invoice_id);
    /// ```
    pub fn fund_invoice(env: Env, invoice_id: BytesN<32>) -> bool {
        Self::require_initialized(&env);
        let invoice_contract = Self::invoice_contract(&env);

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let invoice_status: u32 =
            env.invoke_contract(&invoice_contract, &Symbol::new(&env, "get_status"), args);
        if invoice_status != 1 {
            panic_with_error!(&env, PoolError::InvoiceNotListed);
        }

        let funded_key = DataKey::FundedInvoice(invoice_id.clone());
        if env.storage().persistent().has(&funded_key) {
            panic_with_error!(&env, PoolError::AlreadyFunded);
        }

        let registry_id = Self::registry_contract(&env);
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let issuer: Address =
            env.invoke_contract(&invoice_contract, &Symbol::new(&env, "get_issuer"), args);
        let mut args = Vec::new(&env);
        args.push_back(issuer.into_val(&env));
        let issuer_verified: bool =
            env.invoke_contract(&registry_id, &Symbol::new(&env, "is_verified"), args);
        if !issuer_verified {
            panic_with_error!(&env, PoolError::IssuerNotVerified);
        }

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let buyer: Address =
            env.invoke_contract(&invoice_contract, &Symbol::new(&env, "get_buyer"), args);
        let mut args = Vec::new(&env);
        args.push_back(buyer.into_val(&env));
        let buyer_verified: bool =
            env.invoke_contract(&registry_id, &Symbol::new(&env, "is_verified"), args);
        if !buyer_verified {
            panic_with_error!(&env, PoolError::BuyerNotVerified);
        }

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let invoice_asset: Address = env.invoke_contract(
            &invoice_contract,
            &Symbol::new(&env, "get_funding_asset"),
            args,
        );
        let usdc_id = Self::usdc(&env);
        if invoice_asset != usdc_id {
            panic_with_error!(&env, PoolError::AssetMismatch);
        }

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let face_value: u128 = env.invoke_contract(
            &invoice_contract,
            &Symbol::new(&env, "get_face_value"),
            args,
        );
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let discount_bps: u32 = env.invoke_contract(
            &invoice_contract,
            &Symbol::new(&env, "get_discount_bps"),
            args,
        );

        let funded_amount = face_value * (10000 - discount_bps as u128) / 10000;
        if funded_amount == 0 {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }

        let totals = Self::totals(&env);
        let total_deposits = totals.deposits;
        let total_funded = totals.funded;
        let available = total_deposits - total_funded;
        if funded_amount > available {
            panic_with_error!(&env, PoolError::InsufficientLiquidity);
        }

        let max_utilization_bps = totals.max_utilization_bps;
        let new_total_funded = total_funded + funded_amount;
        let utilization_after =
            Self::utilization_bps_or_panic(&env, new_total_funded, total_deposits);
        if utilization_after > max_utilization_bps {
            panic_with_error!(&env, PoolError::UtilizationCapExceeded);
        }

        // --- Checks-effects-interactions: commit pool state BEFORE any
        // cross-contract calls so a reentrant callback into this contract
        // always sees the updated TotalFunded / ActiveInvoiceCount /
        // FundedInvoice, preventing double-funding via stale state.
        env.storage()
            .instance()
            .set(&DataKey::TotalFunded, &(total_funded + funded_amount));
        let active_count = totals.active_invoices;
        env.storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &(active_count + 1));

        env.storage().persistent().set(&funded_key, &funded_amount);
        env.storage()
            .persistent()
            .extend_ttl(&funded_key, TTL_THRESHOLD, TTL_EXTEND_TO);

        // --- Interactions: cross-contract calls after pool state is committed.
        let escrow_contract = Self::escrow_contract(&env);

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        args.push_back(funded_amount.into_val(&env));
        args.push_back(issuer.into_val(&env));
        let _: bool = env.invoke_contract(&escrow_contract, &Symbol::new(&env, "lock"), args);

        let pool_address = env.current_contract_address();
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        args.push_back(pool_address.into_val(&env));
        args.push_back(usdc_id.into_val(&env));
        args.push_back(funded_amount.into_val(&env));
        let _: bool =
            env.invoke_contract(&invoice_contract, &Symbol::new(&env, "mark_funded"), args);

        events::invoice_funded(&env, &invoice_id, funded_amount);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Receives invoice repayment and updates pool liquidity metrics.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice being repaid.
    /// * `amount` - The amount repaid.
    ///
    /// # Auth
    /// Requires authorization from the configured `invoice_contract`
    /// (via `invoice_contract.require_auth()`); only the invoice contract may
    /// invoke this entry point.
    ///
    /// # Panics
    /// * `InvoiceNotFound` if the invoice is not funded.
    /// * `InvalidAmount` if the repayment amount is less than the funded amount.
    /// * `ActiveCountUnderflow` if the active-invoice counter would underflow
    ///   (e.g. a mismatched repayment for an invoice that was never funded
    ///   through this pool).
    ///
    /// # Returns
    /// * `bool` - `true` when repayment is processed.
    ///
    /// # Example
    /// ```ignore
    /// client.receive_repayment(&invoice_id, 1_050);
    /// ```
    pub fn receive_repayment(env: Env, invoice_id: BytesN<32>, amount: u128) -> bool {
        let invoice_contract = Self::invoice_contract(&env);
        invoice_contract.require_auth();

        let funded_key = DataKey::FundedInvoice(invoice_id.clone());
        let funded_amount: u128 = env
            .storage()
            .persistent()
            .get(&funded_key)
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::InvoiceNotFound));
        if amount < funded_amount {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }

        let yield_amount = amount - funded_amount;
        let totals = Self::totals(&env);
        let total_deposits = totals.deposits;
        let total_funded = totals.funded;
        let total_yield = totals.yield_distributed;

        env.storage()
            .instance()
            .set(&DataKey::TotalDeposits, &(total_deposits + yield_amount));
        env.storage().instance().set(
            &DataKey::TotalYieldDistributed,
            &(total_yield + yield_amount),
        );
        env.storage()
            .instance()
            .set(&DataKey::TotalFunded, &(total_funded - funded_amount));

        let active_count = totals.active_invoices;
        let new_active_count = active_count
            .checked_sub(1)
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::ActiveCountUnderflow));
        env.storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &new_active_count);

        env.storage().persistent().remove(&funded_key);

        events::repayment_received(&env, &invoice_id, amount, yield_amount);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Receives invoice repayment with a partial refund to the buyer and updates
    /// pool liquidity metrics.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The invoice being repaid.
    /// * `amount` - The amount repaid.
    /// * `refund` - The amount to refund to the buyer.
    /// * `buyer` - The buyer receiving the refund.
    ///
    /// # Auth
    /// Requires authorization from the configured `invoice_contract`
    /// (via `invoice_contract.require_auth()`); only the invoice contract may
    /// invoke this entry point.
    ///
    /// # Trust boundary: the discount/refund split is invoice-computed
    /// `invoice.repay()` / `invoice.repay_early()` independently compute
    /// `earned_by_pool` / `refund_to_buyer` from the invoice's discount,
    /// elapsed time, and term, and pass the resulting `refund` here. Pool has
    /// no visibility into `funded_at`, `due_date`, or elapsed/term at all —
    /// it only bounds `refund` to `[0, amount - funded_amount]` (the maximum
    /// possible discount) via `InvalidAmount`. Pool does **not** independently
    /// verify that `refund` is proportional to time elapsed; any `refund`
    /// invoice passes within that bound is accepted unconditionally, and the
    /// remainder is credited to LPs as yield. Correctness of the time-based
    /// split is entirely `invoice_contract`'s responsibility.
    ///
    /// # Panics
    /// * `InvoiceNotFound` if the invoice is not funded.
    /// * `InvalidAmount` if the repayment amount is less than the funded amount,
    ///   or if the refund exceeds the maximum allowed.
    /// * `ActiveCountUnderflow` if the active-invoice counter would underflow
    ///   (e.g. a mismatched repayment for an invoice that was never funded
    ///   through this pool).
    ///
    /// # Returns
    /// * `bool` - `true` when repayment is processed.
    ///
    /// # Example
    /// ```ignore
    /// client.receive_repayment_with_refund(&invoice_id, 1_050, 50, &buyer);
    /// ```
    pub fn receive_repayment_with_refund(
        env: Env,
        invoice_id: BytesN<32>,
        amount: u128,
        refund: u128,
        buyer: Address,
    ) -> bool {
        let invoice_contract = Self::invoice_contract(&env);
        invoice_contract.require_auth();

        let funded_key = DataKey::FundedInvoice(invoice_id.clone());
        let funded_amount: u128 = env
            .storage()
            .persistent()
            .get(&funded_key)
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::InvoiceNotFound));
        if amount < funded_amount {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }

        let max_refund = amount.saturating_sub(funded_amount);
        if refund > max_refund {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }

        let yield_amount = amount - funded_amount - refund;
        let totals = Self::totals(&env);
        let total_deposits = totals.deposits;
        let total_funded = totals.funded;
        let total_yield = totals.yield_distributed;

        env.storage()
            .instance()
            .set(&DataKey::TotalDeposits, &(total_deposits + yield_amount));
        env.storage().instance().set(
            &DataKey::TotalYieldDistributed,
            &(total_yield + yield_amount),
        );
        env.storage()
            .instance()
            .set(&DataKey::TotalFunded, &(total_funded - funded_amount));

        let active_count = totals.active_invoices;
        let new_active_count = active_count
            .checked_sub(1)
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::ActiveCountUnderflow));
        env.storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &new_active_count);

        env.storage().persistent().remove(&funded_key);

        // transfer refund back to buyer from pool's USDC balance
        let usdc_id = Self::usdc(&env);
        let usdc = token::Client::new(&env, &usdc_id);
        if refund > 0 {
            usdc.transfer(&env.current_contract_address(), &buyer, &(refund as i128));
        }

        events::repayment_received(&env, &invoice_id, amount, yield_amount);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Forwards a defaulted invoice to escrow default handling and updates
    /// the invoice status on the invoice contract.
    ///
    /// This function performs the following cross-contract sequence:
    /// 1. Calls `escrow.handle_default()` to release escrowed funds back
    ///    to the pool.
    /// 2. Calls `invoice.mark_defaulted()` to persist the `Defaulted` status
    ///    on the invoice record, update the status index, and emit the
    ///    `invoice_defaulted` event.
    /// 3. Updates the pool's local accounting (TotalFunded, TotalDeposits,
    ///    TotalLossRealised, ActiveInvoiceCount) and removes the funded
    ///    invoice entry.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `invoice_id` - The defaulted invoice.
    ///
    /// # Auth
    /// Requires authorization from the configured `invoice_contract`
    /// (via `invoice_contract.require_auth()`); only the invoice contract may
    /// invoke this entry point.
    ///
    /// # Panics
    /// * `InvoiceNotFound` if no funded invoice entry exists for `invoice_id`.
    /// * `EscrowDefaultNotReleased` if `escrow.handle_default()` returns `false`
    ///   (e.g. no lock record exists in escrow for this invoice, so no tokens
    ///   were actually transferred back to the pool). Without this check, pool
    ///   accounting would otherwise proceed to record a loss and free up
    ///   utilization as if funds had been recovered, even though escrow moved
    ///   nothing.
    /// * `ActiveCountUnderflow` if the active-invoice counter would underflow
    ///   (e.g. double-default of the same invoice).
    ///
    /// # Returns
    /// * `bool` - `true` when default handling completes.
    ///
    /// # Example
    /// ```ignore
    /// client.handle_default(&invoice_id);
    /// ```
    pub fn handle_default(env: Env, invoice_id: BytesN<32>) -> bool {
        let invoice_contract = Self::invoice_contract(&env);
        invoice_contract.require_auth();

        let funded_key = DataKey::FundedInvoice(invoice_id.clone());
        if !env.storage().persistent().has(&funded_key) {
            panic_with_error!(&env, PoolError::InvoiceNotFound);
        }
        let funded_amount: u128 = env.storage().persistent().get(&funded_key).unwrap();

        let escrow_contract = Self::escrow_contract(&env);
        let pool_address = env.current_contract_address();
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        args.push_back(pool_address.into_val(&env));
        let escrow_released: bool =
            env.invoke_contract(&escrow_contract, &Symbol::new(&env, "handle_default"), args);
        if !escrow_released {
            panic_with_error!(&env, PoolError::EscrowDefaultNotReleased);
        }

        let totals = Self::totals(&env);
        let total_funded = totals.funded;
        let total_deposits = totals.deposits;
        let total_loss_realised = totals.loss_realised;

        env.storage()
            .instance()
            .set(&DataKey::TotalFunded, &(total_funded - funded_amount));
        env.storage()
            .instance()
            .set(&DataKey::TotalDeposits, &(total_deposits - funded_amount));
        env.storage().instance().set(
            &DataKey::TotalLossRealised,
            &(total_loss_realised + funded_amount),
        );

        let active_count = totals.active_invoices;
        let new_active_count = active_count
            .checked_sub(1)
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::ActiveCountUnderflow));
        env.storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &new_active_count);

        // Persist the Defaulted status on the invoice contract (step 2 of the
        // documented cross-contract sequence). This runs after the active-count
        // underflow check so a mismatched default still surfaces
        // ActiveCountUnderflow (#17) rather than an invoice lookup error from
        // mark_defaulted. mark_defaulted is idempotent: when
        // invoice.trigger_default already transitioned the status to Defaulted
        // before invoking this pool entry point, the call is a no-op.
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let _: bool = env.invoke_contract(
            &invoice_contract,
            &Symbol::new(&env, "mark_defaulted"),
            args,
        );

        env.storage().persistent().remove(&funded_key);

        events::invoice_defaulted(&env, &invoice_id, funded_amount);
        Self::extend_instance_ttl(&env);
        true
    }

    /// Returns current pool statistics and utilization metrics.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `NotInitialized` if the pool contract has not been initialized.
    /// * `Overflow` if scaling `total_funded` into basis points would overflow.
    ///
    /// # Returns
    /// * `PoolStats` - The current pool statistics.
    ///
    /// # Example
    /// ```ignore
    /// let stats = client.get_stats();
    /// ```
    pub fn get_stats(env: Env) -> PoolStats {
        if Self::admin(&env).is_none() {
            panic_with_error!(&env, PoolError::NotInitialized);
        }
        let totals = Self::totals(&env);
        let total_deposits = totals.deposits;
        let total_funded = totals.funded;
        let available = total_deposits - total_funded;
        let utilization = Self::utilization_bps_or_panic(&env, total_funded, total_deposits);

        PoolStats {
            total_deposits,
            total_funded,
            available_liquidity: available,
            utilization_rate_bps: utilization,
            total_yield_distributed: totals.yield_distributed,
            total_loss_realised: totals.loss_realised,
            active_invoice_count: totals.active_invoices,
            total_shares: totals.shares,
            max_utilization_bps: totals.max_utilization_bps,
        }
    }

    /// Returns the LP's position, including shares, value, yield, and deposits.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `lp` - The liquidity provider address.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `Overflow` if `lp_shares * total_deposits` would overflow `u128`
    ///   while computing the position's USDC value.
    ///
    /// All storage reads default to `0` when the LP has no recorded position,
    /// so an LP without a position simply reports zeros.
    ///
    /// # Returns
    /// * `LPPosition` - The LP position details.
    ///
    /// # Example
    /// ```ignore
    /// let position = client.get_lp_position(&lp);
    /// ```
    pub fn get_lp_position(env: Env, lp: Address) -> LPPosition {
        let lp_shares: u128 = env
            .storage()
            .persistent()
            .get(&DataKey::LPShares(lp.clone()))
            .unwrap_or(0);
        let totals = Self::totals(&env);
        let total_shares = totals.shares;
        let total_deposits = totals.deposits;

        let usdc_value = if total_shares > 0 && lp_shares > 0 {
            let scaled = lp_shares
                .checked_mul(total_deposits)
                .unwrap_or_else(|| panic_with_error!(&env, PoolError::Overflow));
            scaled / total_shares
        } else {
            0
        };

        let yield_earned: u128 = env
            .storage()
            .persistent()
            .get(&DataKey::LPYieldEarned(lp.clone()))
            .unwrap_or(0);
        let deposit_count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::LPDepositCount(lp.clone()))
            .unwrap_or(0);

        LPPosition {
            shares: lp_shares,
            usdc_value,
            yield_earned,
            deposit_count,
        }
    }

    /// Returns the pool utilization rate as basis points.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    ///
    /// # Auth
    /// No authorization is required.
    ///
    /// # Panics
    /// * `Overflow` if scaling `total_funded` into basis points would overflow.
    ///
    /// # Returns
    /// * `u32` - The utilization rate in basis points, or `0` when
    ///   `total_deposits` is zero.
    ///
    /// # Example
    /// ```ignore
    /// let utilization = client.get_utilization_rate();
    /// ```
    pub fn get_utilization_rate(env: Env) -> u32 {
        let totals = Self::totals(&env);
        Self::utilization_bps_or_panic(&env, totals.funded, totals.deposits)
    }

    /// Updates the pool's maximum utilization cap.
    ///
    /// The cap bounds the utilization (in basis points) that `fund_invoice`
    /// may drive the pool to: funding is rejected with
    /// `UtilizationCapExceeded` when the post-funding utilization would
    /// exceed it. The current cap is reported in
    /// `PoolStats::max_utilization_bps`.
    ///
    /// Emits a `max_utilization_updated` event carrying the old and new cap
    /// values so off-chain indexers can observe risk-parameter changes
    /// without polling `get_stats()`.
    ///
    /// # Arguments
    /// * `env` - The Soroban environment.
    /// * `admin` - The admin address for this contract.
    /// * `new_cap_bps` - The new utilization cap, in basis points
    ///   (`10_000` = 100%).
    ///
    /// # Auth
    /// Requires authorization from `admin` (via `admin.require_auth()`).
    ///
    /// # Panics
    /// * `InvalidAmount` if `new_cap_bps` exceeds `10_000`.
    ///
    /// # Returns
    /// * `bool` - `true` when the cap is updated.
    ///
    /// # Example
    /// ```ignore
    /// client.set_max_utilization(&admin, &9000);
    /// ```
    pub fn set_max_utilization(env: Env, admin: Address, new_cap_bps: u32) -> bool {
        admin.require_auth();
        if new_cap_bps > 10000 {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }
        let old_cap_bps = Self::totals(&env).max_utilization_bps;
        env.storage()
            .instance()
            .set(&DataKey::MaxUtilizationBps, &new_cap_bps);
        events::max_utilization_updated(&env, old_cap_bps, new_cap_bps);
        Self::extend_instance_ttl(&env);
        true
    }

    fn utilization_bps_or_panic(env: &Env, total_funded: u128, total_deposits: u128) -> u32 {
        if total_deposits == 0 {
            return 0;
        }

        let scaled_funded = total_funded
            .checked_mul(10_000)
            .unwrap_or_else(|| panic_with_error!(env, PoolError::Overflow));

        scaled_funded.checked_div(total_deposits).unwrap_or(0) as u32
    }

    fn require_initialized(env: &Env) {
        if !env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(env, PoolError::NotInitialized);
        }
    }

    fn admin(env: &Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::Admin)
    }

    fn invoice_contract(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::InvoiceContract)
            .expect("pool is not initialized: invoice contract missing")
    }

    fn escrow_contract(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::EscrowContract)
            .expect("pool is not initialized: escrow contract missing")
    }

    fn usdc(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::UsdcAsset)
            .expect("pool is not initialized: USDC asset missing")
    }

    fn registry_contract(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::RegistryContract)
            .expect("pool is not initialized: registry contract missing")
    }

    fn totals(env: &Env) -> PoolTotals {
        PoolTotals {
            shares: env
                .storage()
                .instance()
                .get(&DataKey::TotalShares)
                .unwrap_or(0),
            deposits: env
                .storage()
                .instance()
                .get(&DataKey::TotalDeposits)
                .unwrap_or(0),
            funded: env
                .storage()
                .instance()
                .get(&DataKey::TotalFunded)
                .unwrap_or(0),
            yield_distributed: env
                .storage()
                .instance()
                .get(&DataKey::TotalYieldDistributed)
                .unwrap_or(0),
            loss_realised: env
                .storage()
                .instance()
                .get(&DataKey::TotalLossRealised)
                .unwrap_or(0),
            active_invoices: env
                .storage()
                .instance()
                .get(&DataKey::ActiveInvoiceCount)
                .unwrap_or(0),
            max_utilization_bps: env
                .storage()
                .instance()
                .get(&DataKey::MaxUtilizationBps)
                .unwrap_or(8500),
        }
    }

    fn extend_instance_ttl(env: &Env) {
        env.storage()
            .instance()
            .extend_ttl(TTL_THRESHOLD, TTL_EXTEND_TO);
    }
}
