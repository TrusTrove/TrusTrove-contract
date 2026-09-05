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

/// Default maximum utilization cap (in basis points) written at
/// `initialize()` time. 8500 bps = 85%. This is the single source of truth for
/// the default: `totals()`'s fallback reads the same constant, so the two call
/// sites can never silently desync if the default is ever changed.
pub const DEFAULT_MAX_UTILIZATION_BPS: u32 = 8500;

#[contract]
pub struct PoolContract;
