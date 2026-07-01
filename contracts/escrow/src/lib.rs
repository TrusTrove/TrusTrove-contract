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
pub struct EscrowContract;

#[contractimpl]
impl EscrowContract {
    pub fn initialize(
        env: Env,
        admin: Address,
        pool_contract: Address,
        invoice_contract: Address,
        usdc_asset: Address,
    ) {
        // Initializes the escrow contract and stores required contract references.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `admin` - The admin address for this contract.
        // * `pool_contract` - The pool contract address.
        // * `invoice_contract` - The invoice contract address.
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
        // client.initialize(&admin, &pool, &invoice, &usdc);
        // ```
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
    }

    pub fn lock(env: Env, invoice_id: BytesN<32>, amount: u128) -> bool {
        // Locks USDC in escrow against a funded invoice.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice ID being locked.
        // * `amount` - The amount to lock.
        //
        // # Returns
        // * `bool` - `true` when the funds are locked.
        //
        // # Panics
        // * `InvalidAmount` if the amount is zero.
        // * `AlreadyLocked` if the invoice is already locked.
        //
        // # Example
        // ```ignore
        // client.lock(&invoice_id, &amount);
        // ```
        let pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotInitialized));
        pool.require_auth();

        if amount == 0 {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }

        let key = DataKey::Locked(invoice_id.clone());
        if env.storage().persistent().has(&key) {
            panic_with_error!(&env, EscrowError::AlreadyLocked);
        }

        let usdc_id: Address = env.storage().instance().get(&DataKey::UsdcAsset).unwrap();
        let usdc = token::Client::new(&env, &usdc_id);
        usdc.transfer(&pool, &env.current_contract_address(), &(amount as i128));

        let record = EscrowRecord {
            invoice_id: invoice_id.clone(),
            amount,
            locked_at: env.ledger().timestamp(),
        };
        persistent_set(&env, &key, &record);
        Self::append_history(&env, &invoice_id, EscrowAction::Locked, amount);
        events::funds_locked(&env, &invoice_id, amount);

        true
    }

    pub fn release_to_issuer(env: Env, invoice_id: BytesN<32>, issuer: Address) -> bool {
        // Releases escrowed funds to the issuer.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice whose escrow is released.
        // * `issuer` - The issuer address to receive funds.
        //
        // # Returns
        // * `bool` - `true` when funds are released.
        //
        // # Panics
        // * `NotFound` if no escrow record exists for the invoice.
        //
        // # Example
        // ```ignore
        // client.release_to_issuer(&invoice_id, &issuer);
        // ```
        let pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap();
        pool.require_auth();

        let key = DataKey::Locked(invoice_id.clone());
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotFound));

        // Issue #57: validate the recipient against the actual invoice issuer to prevent
        // funds from being redirected to an arbitrary address.
        let invoice_contract: Address = env
            .storage()
            .instance()
            .get(&DataKey::InvoiceContract)
            .unwrap();
        let mut args = Vec::new(&env);
        args.push_back(invoice_id.clone().into_val(&env));
        let actual_issuer: Address =
            env.invoke_contract(&invoice_contract, &Symbol::new(&env, "get_issuer"), args);
        if issuer != actual_issuer {
            panic_with_error!(&env, EscrowError::NotAuthorized);
        }

        let usdc_id: Address = env.storage().instance().get(&DataKey::UsdcAsset).unwrap();
        let usdc = token::Client::new(&env, &usdc_id);
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
        events::released_to_issuer(&env, &invoice_id, &issuer, record.amount);
        true
    }

    pub fn release_to_pool(env: Env, invoice_id: BytesN<32>, repayment_amount: u128) -> bool {
        // Releases escrowed funds back to the pool as repayment.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice whose escrow is returned.
        // * `repayment_amount` - The amount returned to the pool.
        //
        // # Returns
        // * `bool` - `true` when funds are returned.
        //
        // # Panics
        // * `NotFound` if no escrow record exists for the invoice.
        //
        // # Example
        // ```ignore
        // client.release_to_pool(&invoice_id, &repayment_amount);
        // ```
        let pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap();
        pool.require_auth();

        let key = DataKey::Locked(invoice_id.clone());
        let record: EscrowRecord = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic_with_error!(&env, EscrowError::NotFound));

        if repayment_amount == 0 || repayment_amount > record.amount {
            panic_with_error!(&env, EscrowError::InvalidAmount);
        }

        let usdc_id: Address = env.storage().instance().get(&DataKey::UsdcAsset).unwrap();
        let usdc = token::Client::new(&env, &usdc_id);
        usdc.transfer(
            &env.current_contract_address(),
            &pool,
            &(repayment_amount as i128),
        );

        let remaining_amount = record.amount - repayment_amount;
        let mut updated_record = record.clone();
        if remaining_amount == 0 {
            env.storage().persistent().remove(&key);
        } else {
            updated_record.amount = remaining_amount;
            persistent_set(&env, &key, &updated_record);
        }

        Self::append_history(
            &env,
            &invoice_id,
            EscrowAction::ReleasedToPool,
            repayment_amount,
        );
        events::released_to_pool(&env, &invoice_id, &pool, repayment_amount);
        true
    }

    pub fn handle_default(env: Env, invoice_id: BytesN<32>, caller: Address) -> bool {
        let key = DataKey::Locked(invoice_id.clone());
        if !env.storage().persistent().has(&key) {
            return false;
        }

        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        let pool: Address = env
            .storage()
            .instance()
            .get(&DataKey::PoolContract)
            .unwrap();

        // Require the caller to authenticate themselves, then verify they are
        // either the admin (emergency/recovery path) or the pool contract
        // (normal operational path).  Using an explicit `caller` parameter is
        // the idiomatic Soroban pattern for "one of N authorised parties".
        caller.require_auth();
        if caller != admin && caller != pool {
            panic_with_error!(&env, EscrowError::NotAuthorized);
        }

        let record: EscrowRecord = env.storage().persistent().get(&key).unwrap();
        let usdc_id: Address = env.storage().instance().get(&DataKey::UsdcAsset).unwrap();
        let usdc = token::Client::new(&env, &usdc_id);
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
        events::default_resolved(&env, &invoice_id, &pool, record.amount);
        true
    }

    pub fn get_locked(env: Env, invoice_id: BytesN<32>) -> u128 {
        // Returns the amount currently locked in escrow for an invoice.
        //
        // # Arguments
        // * `env` - The Soroban environment.
        // * `invoice_id` - The invoice to query.
        //
        // # Returns
        // * `u128` - The amount locked, or 0 if none exists.
        //
        // # Example
        // ```ignore
        // let locked = client.get_locked(&invoice_id);
        // ```
        env.storage()
            .persistent()
            .get::<_, EscrowRecord>(&DataKey::Locked(invoice_id))
            .map(|r| r.amount)
            .unwrap_or(0)
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
        // * `AlreadyInitialized` used internally; panics `NotAuthorized` if auth fails.
        //
        // # Example
        // ```ignore
        // client.transfer_ownership(&new_admin);
        // ```
        let admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        admin.require_auth();
        new_admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        events::ownership_transferred(&env, &admin, &new_admin);
        Self::extend_instance_ttl(&env);
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
        persistent_set(env, &key, &history);
    }

    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(100, 2_000_000);
    }
}
