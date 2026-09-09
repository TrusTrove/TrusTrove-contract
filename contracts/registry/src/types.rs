use soroban_sdk::{contracttype, Address, Map, String};

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum Role {
    Issuer,
    Buyer,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub enum VerificationStatus {
    Unregistered,
    Pending,
    Verified,
    Revoked,
}

#[contracttype]
#[derive(Clone, Debug)]
pub struct Profile {
    pub packed_flags: u32,
    pub registered_at: u64,
    pub metadata: Map<String, String>,
}

impl Profile {
    pub fn new(
        role: Role,
        verified: bool,
        registered_at: u64,
        metadata: Map<String, String>,
    ) -> Self {
        let mut packed_flags = 0u32;
        if role == Role::Buyer {
            packed_flags |= 1;
        }
        if verified {
            packed_flags |= 2;
        }
        Profile {
            packed_flags,
            registered_at,
            metadata,
        }
    }

    pub fn role(&self) -> Role {
        if (self.packed_flags & 1) != 0 {
            Role::Buyer
        } else {
            Role::Issuer
        }
    }

    pub fn verified(&self) -> bool {
        (self.packed_flags & 2) != 0
    }

    pub fn set_verified(&mut self, verified: bool) {
        if verified {
            self.packed_flags |= 2;
        } else {
            self.packed_flags &= !2;
        }
    }

    pub fn revoked(&self) -> bool {
        (self.packed_flags & 4) != 0
    }

    pub fn set_revoked(&mut self, revoked: bool) {
        if revoked {
            self.packed_flags |= 4;
        } else {
            self.packed_flags &= !4;
        }
    }
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileView {
    pub role: Role,
    pub verified: bool,
    pub revoked: bool,
    pub registered_at: u64,
    pub metadata: Map<String, String>,
}

impl ProfileView {
    pub fn from_profile(profile: &Profile) -> Self {
        ProfileView {
            role: profile.role(),
            verified: profile.verified(),
            revoked: profile.revoked(),
            registered_at: profile.registered_at,
            metadata: profile.metadata.clone(),
        }
    }

    pub fn role(&self) -> Role {
        self.role.clone()
    }
}

#[contracttype]
pub enum DataKey {
    Admin,
    Profile(Address),
}
