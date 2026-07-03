use soroban_sdk::{contracttype, Address, Map, String};

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum Role {
    Issuer,
    Buyer,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Profile {
    pub address: Address,
    pub role: Role,
    pub verified: bool,
    pub registered_at: u64,
    pub metadata: Map<String, String>,
}

#[contracttype]
pub enum DataKey {
    Admin,
    Profile(Address),
}
