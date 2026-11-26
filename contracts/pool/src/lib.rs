//! # TrusTrove Pool Contract
//!
//! Manages liquidity provider deposits, redemptions, invoice funding, repayments,
//! asset balance reconciliation, donations, and max loss threshold limits.

#![no_std]

mod errors;
mod events;
mod types;

use soroban_sdk::{contract, contractimpl, Address, BytesN, Env, Symbol, Vec};

pub use crate::errors::PoolError;
pub use crate::types::*;

/// Basis point denominator (`10_000` = 100%).
pub const BPS_DENOMINATOR: u32 = 10_000;

#[contract]
pub struct PoolContract;

#[contractimpl]
impl PoolContract {
    /// Set the maximum loss threshold in basis points (issue #1289).
    pub fn set_max_loss_threshold(env: Env, threshold_bps: Option<u32>) -> Result<(), PoolError> {
        let admin: Address = env.storage().instance().get(&DataKey::Admin).ok_or(PoolError::NotInitialized)?;
        admin.require_auth();
        if let Some(bps) = threshold_bps {
            if bps > BPS_DENOMINATOR {
                return Err(PoolError::InvalidConfiguration);
            }
        }
        let old_threshold = env.storage().instance().get(&DataKey::MaxLossThresholdBps);
        if let Some(bps) = threshold_bps {
            env.storage().instance().set(&DataKey::MaxLossThresholdBps, &bps);
        } else {
            env.storage().instance().remove(&DataKey::MaxLossThresholdBps);
        }
        events::max_loss_threshold_updated(&env, old_threshold, threshold_bps);
        Ok(())
    }

    /// Reconcile asset donation with explicit donation semantics (issue #1291).
    pub fn donate_assets(env: Env, donor: Address, amount: u128) -> Result<(), PoolError> {
        donor.require_auth();
        if amount == 0 {
            return Err(PoolError::InvalidAmount);
        }
        let funding_asset: Address = env.storage().instance().get(&DataKey::FundingAsset).ok_or(PoolError::NotInitialized)?;
        let token_client = soroban_sdk::token::TokenClient::new(&env, &funding_asset);
        token_client.transfer(&donor, &env.current_contract_address(), &(amount as i128));

        // Increase total_deposits so available liquidity and reserves correctly reflect the donation without minting LP shares.
        let total_deposits: u128 = env.storage().instance().get(&DataKey::TotalDeposits).unwrap_or(0);
        env.storage().instance().set(&DataKey::TotalDeposits, &(total_deposits + amount));
        Ok(())
    }

    /// Read-only asset balance reconciliation view (issue #1288).
    pub fn get_asset_balance_reconciliation(env: Env) -> Result<AssetBalanceReconciliation, PoolError> {
        if !env.storage().instance().has(&DataKey::Admin) {
            return Err(PoolError::NotInitialized);
        }
        let total_deposits: u128 = env.storage().instance().get(&DataKey::TotalDeposits).unwrap_or(0);
        let total_funded: u128 = env.storage().instance().get(&DataKey::TotalFunded).unwrap_or(0);
        let recorded_available_liquidity = total_deposits.saturating_sub(total_funded);

        let funding_asset: Address = env.storage().instance().get(&DataKey::FundingAsset).ok_or(PoolError::NotInitialized)?;
        let token_client = soroban_sdk::token::TokenClient::new(&env, &funding_asset);
        let actual_token_balance = token_client.balance(&env.current_contract_address()) as u128;

        let signed_difference = (actual_token_balance as i128) - (recorded_available_liquidity as i128);

        Ok(AssetBalanceReconciliation {
            recorded_available_liquidity,
            actual_token_balance,
            signed_difference,
        })
    }

    /// Helper to check max loss threshold before new funding (issue #1289).
    pub fn check_max_loss_threshold(env: &Env) -> Result<(), PoolError> {
        let threshold_opt: Option<u32> = env.storage().instance().get(&DataKey::MaxLossThresholdBps);
        if let Some(threshold_bps) = threshold_opt {
            let total_loss_realised: u128 = env.storage().instance().get(&DataKey::TotalLossRealised).unwrap_or(0);
            let cumulative_funded: u128 = env.storage().instance().get(&DataKey::CumulativeFunded).unwrap_or(0);
            if cumulative_funded > 0 {
                // loss_bps = total_loss_realised * 10_000 / cumulative_funded
                let loss_bps = (total_loss_realised * (BPS_DENOMINATOR as u128)) / cumulative_funded;
                if loss_bps > (threshold_bps as u128) {
                    return Err(PoolError::MaxLossExceeded);
                }
            }
        }
        Ok(())
    }

    /// Checkpoint helper for deposit and withdraw (issue #1292).
    pub fn emit_lp_checkpoint(env: &Env, lp: &Address) {
        let shares: u128 = env.storage().persistent().get(&DataKey::LPShares(lp.clone())).unwrap_or(0);
        let principal: u128 = env.storage().persistent().get(&DataKey::LPInitialDeposit(lp.clone())).unwrap_or(0);
        let ledger_time = env.ledger().timestamp();
        events::lp_position_checkpoint(env, lp, shares, principal, ledger_time);
    }
}
