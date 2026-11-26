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
