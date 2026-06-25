#![no_std]

use soroban_sdk::{
    contract, contractimpl, panic_with_error, token, Address, BytesN, Env, IntoVal, Symbol, Vec,
};

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
        Self::extend_instance_ttl(&env);
    }

    pub fn get_usdc_asset(env: Env) -> Address {
        env.storage().instance().get(&DataKey::UsdcAsset).unwrap()
    }

    pub fn deposit(env: Env, lp: Address, usdc_amount: u128) -> u128 {
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
            usdc_amount * total_shares / total_deposits
        };

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
            .extend_ttl(&lp_shares_key, 100, 2_000_000);

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
            .extend_ttl(&lp_deposit_count_key, 100, 2_000_000);

        let lp_init_key = DataKey::LPInitialDeposit(lp.clone());
        let init_dep: u128 = env.storage().persistent().get(&lp_init_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&lp_init_key, &(init_dep + usdc_amount));
        env.storage()
            .persistent()
            .extend_ttl(&lp_init_key, 100, 2_000_000);

        events::lp_deposited(&env, &lp, usdc_amount, shares_to_issue);
        Self::extend_instance_ttl(&env);
        shares_to_issue
    }

    pub fn withdraw(env: Env, lp: Address, shares: u128) -> u128 {
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

        let total_shares: u128 = env.storage().instance().get(&DataKey::TotalShares).unwrap();
        let total_deposits: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDeposits)
            .unwrap();
        let total_funded: u128 = env.storage().instance().get(&DataKey::TotalFunded).unwrap();
        let available = total_deposits - total_funded;

        let usdc_to_return = shares * total_deposits / total_shares;
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

        env.storage()
            .persistent()
            .set(&lp_shares_key, &(lp_shares - shares));
        env.storage()
            .persistent()
            .extend_ttl(&lp_shares_key, 100, 2_000_000);

        let init_dep_key = DataKey::LPInitialDeposit(lp.clone());
        let init_dep: u128 = env.storage().persistent().get(&init_dep_key).unwrap_or(0);
        let principal_portion = shares * init_dep / (lp_shares);
        let yield_earned = usdc_to_return.saturating_sub(principal_portion);

        let yield_key = DataKey::LPYieldEarned(lp.clone());
        let prev_yield: u128 = env.storage().persistent().get(&yield_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&yield_key, &(prev_yield + yield_earned));
        env.storage()
            .persistent()
            .extend_ttl(&yield_key, 100, 2_000_000);

        events::lp_withdrawn(&env, &lp, usdc_to_return, shares);
        Self::extend_instance_ttl(&env);
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
        args.push_back(usdc_id.into_val(&env));
        args.push_back(funded_amount.into_val(&env));
        let _: bool =
            env.invoke_contract(&invoice_contract, &Symbol::new(&env, "mark_funded"), args);

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
        env.storage().persistent().set(&funded_key, &funded_amount);
        env.storage()
            .persistent()
            .extend_ttl(&funded_key, 100, 2_000_000);

        events::invoice_funded(&env, &invoice_id, funded_amount);
        Self::extend_instance_ttl(&env);
        true
    }

    pub fn receive_repayment(env: Env, invoice_id: BytesN<32>, amount: u128) -> bool {
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
        Self::extend_instance_ttl(&env);
        true
    }

    pub fn handle_default(env: Env, invoice_id: BytesN<32>) -> bool {
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
        let total_deposits: u128 = env
            .storage()
            .instance()
            .get(&DataKey::TotalDeposits)
            .unwrap();

        env.storage()
            .instance()
            .set(&DataKey::TotalFunded, &(total_funded - funded_amount));
        env.storage()
            .instance()
            .set(&DataKey::TotalDeposits, &(total_deposits - funded_amount));

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
        Self::extend_instance_ttl(&env);
        true
    }

    pub fn get_stats(env: Env) -> PoolStats {
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

        PoolStats {
            total_deposits,
            total_funded,
            available_liquidity: available,
            utilization_rate_bps: utilization,
            total_yield_distributed: total_yield,
            active_invoice_count: active_count,
            total_shares,
        }
    }

    pub fn get_lp_position(env: Env, lp: Address) -> LPPosition {
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
            lp_shares * total_deposits / total_shares
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

    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }
}
