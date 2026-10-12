use ledger_domain::{Account, Role, User, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub user_id: UserId,
    pub role: Role,
    pub name: String,
}

impl Actor {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }

    pub fn can_view(&self, account: &Account) -> bool {
        self.is_admin() || account.is_held_by(self.user_id)
    }
}

impl From<&User> for Actor {
    fn from(user: &User) -> Self {
        Self {
            user_id: user.id(),
            role: user.role(),
            name: user.name().to_owned(),
        }
    }
}
