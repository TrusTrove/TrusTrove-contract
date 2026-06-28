use soroban_sdk::{contracttype, Address, BytesN};

#[contracttype]
#[derive(Clone, Debug)]
pub struct PoolStats {
    pub total_deposits: u128,
    pub total_funded: u128,
    pub available_liquidity: u128,
    pub utilization_rate_bps: u32,
    pub total_yield_distributed: u128,
    pub active_invoice_count: u32,
    pub total_shares: u128,
    pub max_utilization_bps: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct LPPosition {
    pub shares: u128,
    pub usdc_value: u128,
    pub yield_earned: u128,
    pub deposit_count: u32,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct FundedInvoiceData {
    pub remaining_funded: u128,
    pub remaining_face_value: u128,
}

#[contracttype]
pub enum DataKey {
    Admin,
    InvoiceContract,
    EscrowContract,
    UsdcAsset,
    TotalShares,
    TotalDeposits,
    TotalFunded,
    TotalYieldDistributed,
    ActiveInvoiceCount,
    LPShares(Address),
    LPDepositCount(Address),
    LPYieldEarned(Address),
    LPInitialDeposit(Address),
    FundedInvoice(BytesN<32>),
    MaxUtilizationBps,
}
