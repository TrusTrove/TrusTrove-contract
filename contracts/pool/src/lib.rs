#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, token, Address, BytesN, Env, IntoVal, Symbol, Vec,
};
use trusttrove_common::persistent_set;

mod errors;
mod events;
mod test;
mod types;

pub use errors::*;
pub use types::*;

#[contract]
pub struct PoolContract;

#[contractimpl]
impl PoolContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        invoice_contract: Address,
        escrow_contract: Address,
        usdc_asset: Address,
    ) {
        // Initializes the pool contract with admin and external contract references.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `admin` - The admin address for this contract.
        // * `invoice_contract` - The invoice contract address.
        // * `escrow_contract` - The escrow contract address.
        // * `usdc_asset` - The USDC asset address.
        //
        // # Returns
        // * `()` - No value is returned.
        //
        // # Panics
        // * `AlreadyInitialized` if the contract has already been initialized.
        //
        // # Example
        // ```ignore
        // client.initialize(&admin, &invoice, &escrow, &usdc);
        // ```
        if env.storage().instance().has(&DataKey::Admin) {
            panic_with_error!(&env, PoolError::AlreadyInitialized);
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
    }

    pub fn get_usdc_asset(env: Env) -> Address {
        // Returns the USDC asset used by the pool.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        //
        // # Returns
        // * `Address` - The USDC asset address.
        //
        // # Example
        // ```ignore
        // let asset = client.get_usdc_asset();
        // ```
        env.storage().instance().get(&DataKey::UsdcAsset).unwrap()
    }

    pub fn deposit(env: Env, lp: Address, usdc_amount: u128) -> u128 {
        // Deposits USDC from an LP and issues pool shares.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `lp` - The liquidity provider address.
        // * `usdc_amount` - The amount of USDC to deposit.
        //
        // # Returns
        // * `u128` - The number of shares issued.
        //
        // # Panics
        // * `InvalidAmount` if `usdc_amount` is zero.
        //
        // # Example
        // ```ignore
        // let shares = client.deposit(&lp, 1_000);
        // ```
        lp.require_auth();
        if usdc_amount == 0 {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }

        let usdc_id: Address = env.storage().instance().get(&DataKey::UsdcAsset).unwrap();
        let usdc = token::Client::new(&env, &usdc_id);
        usdc.transfer(&lp, &env.current_contract_address(), &(usdc_amount as i128));

        let total_shares: u128 = env.storage().instance().get(&DataKey::TotalShares).unwrap();
        let total_deposits: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDeposits)
            .unwrap();

        let shares_to_issue = if total_shares == 0 || total_deposits == 0 {
            usdc_amount
        } else {
            usdc_amount
                .checked_mul(total_shares)
                .and_then(|v| v.checked_div(total_deposits))
                .unwrap_or_else(|| panic_with_error!(&env, PoolError::Overflow))
        };

        if shares_to_issue == 0 {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }

        env.storage()
            .instance()
            .set(&DataKey::TotalShares, &(total_shares + shares_to_issue));
        env.storage()
            .instance()
            .set(&DataKey::TotalDeposits, &(total_deposits + usdc_amount));

        let lp_shares_key = DataKey::LPShares(lp.clone());
        let lp_shares: u128 = env.storage().persistent().get(&lp_shares_key).unwrap_or(0);
        persistent_set(&env, &lp_shares_key, &(lp_shares + shares_to_issue));

        let lp_deposit_count_key = DataKey::LPDepositCount(lp.clone());
        let count: u32 = env
            .storage()
            .persistent()
            .get(&lp_deposit_count_key)
            .unwrap_or(0);
        persistent_set(&env, &lp_deposit_count_key, &(count + 1));

        let lp_init_key = DataKey::LPInitialDeposit(lp.clone());
        let init_dep: u128 = env.storage().persistent().get(&lp_init_key).unwrap_or(0);
        persistent_set(&env, &lp_init_key, &(init_dep + usdc_amount));

        events::lp_deposited(&env, &lp, usdc_amount, shares_to_issue);
        shares_to_issue
    }

    pub fn withdraw(env: Env, lp: Address, shares: u128) -> u128 {
        // Withdraws shares from the pool and transfers USDC to the LP.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `lp` - The liquidity provider address.
        // * `shares` - The number of shares to withdraw.
        //
        // # Returns
        // * `u128` - The amount of USDC returned.
        //
        // # Panics
        // * `InvalidAmount` if `shares` is zero.
        // * `NoShares` if the LP has no shares.
        // * `InsufficientShares` if the LP does not own enough shares.
        // * `InsufficientLiquidity` if the pool lacks enough available USDC.
        //
        // # Example
        // ```ignore
        // let returned = client.withdraw(&lp, 500);
        // ```
        lp.require_auth();
        if shares == 0 {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }

        let lp_shares_key = DataKey::LPShares(lp.clone());
        let original_lp_shares: u128 = env
            .storage()
            .persistent()
            .get(&lp_shares_key)
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::NoShares));
        if shares > original_lp_shares {
            panic_with_error!(&env, PoolError::InsufficientShares);
        }

        let total_shares: u128 = env.storage().instance().get(&DataKey::TotalShares).unwrap();
        let total_deposits: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDeposits)
            .unwrap();
        let total_funded: u128 = env.storage().instance().get(&DataKey::TotalFunded).unwrap();
        let available = total_deposits - total_funded;

        let usdc_to_return = shares
            .checked_mul(total_deposits)
            .and_then(|v| v.checked_div(total_shares))
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::Overflow));
        if usdc_to_return > available {
            panic_with_error!(&env, PoolError::InsufficientLiquidity);
        }

        let usdc_id: Address = env.storage().instance().get(&DataKey::UsdcAsset).unwrap();
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

        persistent_set(&env, &lp_shares_key, &(original_lp_shares - shares));

        let init_dep_key = DataKey::LPInitialDeposit(lp.clone());
        let init_dep: u128 = env.storage().persistent().get(&init_dep_key).unwrap_or(0);
        let principal_portion = shares
            .checked_mul(init_dep)
            .and_then(|v| v.checked_div(original_lp_shares))
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::Overflow));
        let yield_earned = usdc_to_return.saturating_sub(principal_portion);

        let yield_key = DataKey::LPYieldEarned(lp.clone());
        let prev_yield: u128 = env.storage().persistent().get(&yield_key).unwrap_or(0);
        persistent_set(&env, &yield_key, &(prev_yield + yield_earned));

        events::lp_withdrawn(&env, &lp, usdc_to_return, shares);
        usdc_to_return
    }

    pub fn fund_invoice(env: Env, invoice_id: BytesN<32>) -> bool {
        // Permissionless: any caller may trigger funding for an eligible invoice.
        // Access control is enforced entirely through eligibility checks below
        // (invoice must be in Listed status, asset must match the pool's asset,
        // and the pool must have sufficient liquidity).  There is no admin gate
        // so that capital allocation cannot be censored or selectively withheld.
        //
        // See README §"Known Centralization Risks & Roadmap" for the longer-term
        // governance design that will let LPs signal approval on funding decisions.

        let invoice_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::InvoiceContract)
            .unwrap();

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let invoice_status: u32 =
            env.invoke_contract(&invoice_contract, &Symbol::new(&env, "get_status"), args);
        if invoice_status != 1 {
            panic_with_error!(&env, PoolError::InvoiceNotListed);
        }

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let invoice_asset: Address = env.invoke_contract(
            &invoice_contract,
            &Symbol::new(&env, "get_funding_asset"),
            args,
        );
        let usdc_id: Address = env.storage().instance().get(&DataKey::UsdcAsset).unwrap();
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

        let total_deposits: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDeposits)
            .unwrap();
        let total_funded: u128 = env.storage().instance().get(&DataKey::TotalFunded).unwrap();
        let available = total_deposits - total_funded;
        if funded_amount > available {
            panic_with_error!(&env, PoolError::InsufficientLiquidity);
        }

        let max_utilization_bps: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxUtilizationBps)
            .unwrap();
        let new_total_funded = total_funded + funded_amount;
        let utilization_after = (new_total_funded * 10000)
            .checked_div(total_deposits)
            .unwrap_or(0) as u32;
        if utilization_after > max_utilization_bps {
            panic_with_error!(&env, PoolError::UtilizationCapExceeded);
        }

        let escrow_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::EscrowContract)
            .unwrap();

        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        args.push_back(funded_amount.into_val(&env));
        let _: bool = env.invoke_contract(&escrow_contract, &Symbol::new(&env, "lock"), args);

        let pool_address = env.current_contract_address();
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        args.push_back(pool_address.into_val(&env));
        args.push_back(usdc_id.clone().into_val(&env));
        args.push_back(funded_amount.into_val(&env));
        let _: bool =
            env.invoke_contract(&invoice_contract, &Symbol::new(&env, "mark_funded"), args);

        // Issue #56: fetch the actual invoice issuer and immediately release the locked
        // funds from escrow to the issuer so they receive payment at funding time.
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let issuer: Address =
            env.invoke_contract(&invoice_contract, &Symbol::new(&env, "get_issuer"), args);
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        args.push_back(issuer.into_val(&env));
        let _: bool = env.invoke_contract(
            &escrow_contract,
            &Symbol::new(&env, "release_to_issuer"),
            args,
        );

        env.storage()
            .instance()
            .set(&DataKey::TotalFunded, &(total_funded + funded_amount));
        let active_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ActiveInvoiceCount)
            .unwrap();
        env.storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &(active_count + 1));

        let funded_key = DataKey::FundedInvoice(invoice_id.clone());
        persistent_set(&env, &funded_key, &funded_amount);

        events::invoice_funded(&env, &invoice_id, funded_amount);
        true
    }

    pub fn receive_repayment(env: Env, invoice_id: BytesN<32>, amount: u128) -> bool {
        // Receives invoice repayment and updates pool liquidity metrics.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice being repaid.
        // * `amount` - The amount repaid.
        //
        // # Returns
        // * `bool` - `true` when repayment is processed.
        //
        // # Panics
        // * `InvoiceNotFound` if the invoice is not funded.
        // * `InvalidAmount` if the repayment amount is less than the funded amount.
        //
        // # Example
        // ```ignore
        // client.receive_repayment(&invoice_id, 1_050);
        // ```
        let invoice_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::InvoiceContract)
            .unwrap();
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
        let total_deposits: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDeposits)
            .unwrap();
        let total_funded: u128 = env.storage().instance().get(&DataKey::TotalFunded).unwrap();
        let total_yield: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalYieldDistributed)
            .unwrap();

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

        let active_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ActiveInvoiceCount)
            .unwrap();
        env.storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &(active_count - 1));

        env.storage().persistent().remove(&funded_key);

        events::repayment_received(&env, &invoice_id, amount, yield_amount);
        true
    }

    pub fn handle_default(env: Env, invoice_id: BytesN<32>) -> bool {
        // Forwards a defaulted invoice to escrow default handling.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The defaulted invoice.
        //
        // # Returns
        // * `bool` - `true` when default handling completes, `false` if invoice is not funded.
        //
        // # Example
        // ```ignore
        // client.handle_default(&invoice_id);
        // ```
        let invoice_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::InvoiceContract)
            .unwrap();
        invoice_contract.require_auth();

        let funded_key = DataKey::FundedInvoice(invoice_id.clone());
        if !env.storage().persistent().has(&funded_key) {
            return false;
        }
        let funded_amount: u128 = env.storage().persistent().get(&funded_key).unwrap();

        let escrow_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::EscrowContract)
            .unwrap();
        let pool_address = env.current_contract_address();
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        args.push_back(pool_address.into_val(&env));
        let _: bool =
            env.invoke_contract(&escrow_contract, &Symbol::new(&env, "handle_default"), args);

        let total_funded: u128 = env.storage().instance().get(&DataKey::TotalFunded).unwrap();

        // Issue #55: only unwind TotalFunded when a default occurs.
        // The escrow already returned the locked funds to the pool, so TotalDeposits
        // correctly reflects the pool balance and must not be deducted a second time.
        env.storage()
            .instance()
            .set(&DataKey::TotalFunded, &(total_funded - funded_amount));

        let active_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ActiveInvoiceCount)
            .unwrap();
        env.storage()
            .instance()
            .set(&DataKey::ActiveInvoiceCount, &(active_count - 1));

        env.storage().persistent().remove(&funded_key);

        events::invoice_defaulted(&env, &invoice_id, funded_amount);
        true
    }

    pub fn get_stats(env: Env) -> PoolStats {
        // Returns current pool statistics and utilization metrics.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        //
        // # Returns
        // * `PoolStats` - The current pool statistics.
        //
        // # Example
        // ```ignore
        // let stats = client.get_stats();
        // ```
        let total_deposits: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDeposits)
            .unwrap_or(0);
        let total_funded: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalFunded)
            .unwrap_or(0);
        let available = total_deposits - total_funded;
        let utilization = (total_funded * 10000)
            .checked_div(total_deposits)
            .unwrap_or(0) as u32;
        let total_yield: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalYieldDistributed)
            .unwrap_or(0);
        let active_count: u32 = env
            .storage()
            .instance()
            .get(&DataKey::ActiveInvoiceCount)
            .unwrap_or(0);
        let total_shares: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalShares)
            .unwrap_or(0);
        let max_utilization_bps: u32 = env
            .storage()
            .instance()
            .get(&DataKey::MaxUtilizationBps)
            .unwrap_or(8500);

        PoolStats {
            total_deposits,
            total_funded,
            available_liquidity: available,
            utilization_rate_bps: utilization,
            total_yield_distributed: total_yield,
            active_invoice_count: active_count,
            total_shares,
            max_utilization_bps,
        }
    }

    pub fn get_lp_position(env: Env, lp: Address) -> LPPosition {
        // Returns the LP's position, including shares, value, yield, and deposits.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `lp` - The liquidity provider address.
        //
        // # Returns
        // * `LPPosition` - The LP position details.
        //
        // # Example
        // ```ignore
        // let position = client.get_lp_position(&lp);
        // ```
        let lp_shares: u128 = env
            .storage()
            .persistent()
            .get(&DataKey::LPShares(lp.clone()))
            .unwrap_or(0);
        let total_shares: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalShares)
            .unwrap_or(0);
        let total_deposits: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDeposits)
            .unwrap_or(0);

        let usdc_value = if total_shares > 0 && lp_shares > 0 {
            lp_shares
                .checked_mul(total_deposits)
                .and_then(|v| v.checked_div(total_shares))
                .unwrap_or(0)
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

    pub fn get_utilization_rate(env: Env) -> u32 {
        // Returns the pool utilization rate as basis points.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        //
        // # Returns
        // * `u32` - The utilization rate in basis points.
        //
        // # Example
        // ```ignore
        // let utilization = client.get_utilization_rate();
        // ```
        let total_deposits: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDeposits)
            .unwrap_or(0);
        let total_funded: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalFunded)
            .unwrap_or(0);
        if total_deposits == 0 {
            return 0;
        }
        (total_funded * 10000 / total_deposits) as u32
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
            .unwrap_or_else(|| panic_with_error!(&env, PoolError::NotFound));
        admin.require_auth();
        new_admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        events::ownership_transferred(&env, &admin, &new_admin);
        Self::extend_instance_ttl(&env);
    }

    pub fn set_max_utilization(env: Env, admin: Address, new_cap_bps: u32) -> bool {
        admin.require_auth();
        if new_cap_bps > 10000 {
            panic_with_error!(&env, PoolError::InvalidAmount);
        }
        env.storage()
            .instance()
            .set(&DataKey::MaxUtilizationBps, &new_cap_bps);
        true
    }

    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }
}
