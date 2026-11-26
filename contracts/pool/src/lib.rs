#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, Address, BytesN, Env, Symbol, Val, Vec,
};

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PoolError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    UnderMinimumFunding = 4,
    InvalidAmount = 5,
}

#[contracttype]
#[derive(Clone, Debug)]
pub enum DataKey {
    Admin,
    EscrowContract,
    UsdcAsset,
    TotalPrincipal,
    TotalDeposited,
    MinFundPrincipal,
}

#[contract]
pub struct PoolContract;

#[contractimpl]
impl PoolContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        escrow_contract: Address,
        usdc_asset: Address,
    ) -> Result<(), PoolError> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(PoolError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::EscrowContract, &escrow_contract);
        env.storage().instance().set(&DataKey::UsdcAsset, &usdc_asset);
        env.storage().instance().set(&DataKey::TotalPrincipal, &0u128);
        env.storage().instance().set(&DataKey::TotalDeposited, &0u128);
        env.storage().instance().set(&DataKey::MinFundPrincipal, &0u128);
        Ok(())
    }

    pub fn set_min_fund_principal(env: Env, min_amount: u128) -> Result<(), PoolError> {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).ok_or(PoolError::NotInitialized)?;
        admin.require_auth();
        env.storage().instance().set(&DataKey::MinFundPrincipal, &min_amount);
        Ok(())
    }

    pub fn get_min_fund_principal(env: Env) -> u128 {
        env.storage().instance().get(&DataKey::MinFundPrincipal).unwrap_or(0)
    }

    pub fn get_utilization_bps(env: Env) -> u32 {
        let total_deposited: u128 = env.storage().instance().get(&DataKey::TotalDeposited).unwrap_or(0);
        if total_deposited == 0 {
            return 0;
        }
        let total_principal: u128 = env.storage().instance().get(&DataKey::TotalPrincipal).unwrap_or(0);
        if total_principal >= total_deposited {
            return 10000;
        }
        ((total_principal * 10000) / total_deposited) as u32
    }

    pub fn fund_invoice(
        env: Env,
        invoice_id: BytesN<32>,
        amount: u128,
        issuer: Address,
    ) -> Result<(), PoolError> {
        let min_fund: u128 = Self::get_min_fund_principal(env.clone());
        if amount < min_fund {
            return Err(PoolError::UnderMinimumFunding);
        }

        let prev_util = Self::get_utilization_bps(env.clone());

        let total_principal: u128 = env.storage().instance().get(&DataKey::TotalPrincipal).unwrap_or(0);
        env.storage().instance().set(&DataKey::TotalPrincipal, &(total_principal + amount));

        let new_util = Self::get_utilization_bps(env.clone());
        if prev_util != new_util {
            let topics = (Symbol::new(&env, "utilization_change"),);
            env.events().publish(topics, (prev_util, new_util));
        }

        Ok(())
    }

    pub fn deposit(env: Env, amount: u128) -> Result<(), PoolError> {
        let prev_util = Self::get_utilization_bps(env.clone());
        let total_deposited: u128 = env.storage().instance().get(&DataKey::TotalDeposited).unwrap_or(0);
        env.storage().instance().set(&DataKey::TotalDeposited, &(total_deposited + amount));
        let new_util = Self::get_utilization_bps(env.clone());
        if prev_util != new_util {
            let topics = (Symbol::new(&env, "utilization_change"),);
            env.events().publish(topics, (prev_util, new_util));
        }
        Ok(())
    }

    pub fn withdraw(env: Env, amount: u128) -> Result<(), PoolError> {
        let prev_util = Self::get_utilization_bps(env.clone());
        let total_deposited: u128 = env.storage().instance().get(&DataKey::TotalDeposited).unwrap_or(0);
        if total_deposited < amount {
            return Err(PoolError::InvalidAmount);
        }
        env.storage().instance().set(&DataKey::TotalDeposited, &(total_deposited - amount));
        let new_util = Self::get_utilization_bps(env.clone());
        if prev_util != new_util {
            let topics = (Symbol::new(&env, "utilization_change"),);
            env.events().publish(topics, (prev_util, new_util));
        }
        Ok(())
    }

    pub fn repay(env: Env, amount: u128) -> Result<(), PoolError> {
        let prev_util = Self::get_utilization_bps(env.clone());
        let total_principal: u128 = env.storage().instance().get(&DataKey::TotalPrincipal).unwrap_or(0);
        if total_principal < amount {
            return Err(PoolError::InvalidAmount);
        }
        env.storage().instance().set(&DataKey::TotalPrincipal, &(total_principal - amount));
        let new_util = Self::get_utilization_bps(env.clone());
        if prev_util != new_util {
            let topics = (Symbol::new(&env, "utilization_change"),);
            env.events().publish(topics, (prev_util, new_util));
        }
        Ok(())
    }

    pub fn handle_default(env: Env, amount: u128) -> Result<(), PoolError> {
        let prev_util = Self::get_utilization_bps(env.clone());
        let total_principal: u128 = env.storage().instance().get(&DataKey::TotalPrincipal).unwrap_or(0);
        let total_deposited: u128 = env.storage().instance().get(&DataKey::TotalDeposited).unwrap_or(0);
        
        let dec_principal = if total_principal < amount { total_principal } else { amount };
        let dec_deposited = if total_deposited < amount { total_deposited } else { amount };

        env.storage().instance().set(&DataKey::TotalPrincipal, &(total_principal - dec_principal));
        env.storage().instance().set(&DataKey::TotalDeposited, &(total_deposited - dec_deposited));

        let new_util = Self::get_utilization_bps(env.clone());
        if prev_util != new_util {
            let topics = (Symbol::new(&env, "utilization_change"),);
            env.events().publish(topics, (prev_util, new_util));
        }
        Ok(())
    }
}
