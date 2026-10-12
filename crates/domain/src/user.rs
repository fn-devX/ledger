use std::fmt;

use uuid::Uuid;

use crate::DomainError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(Uuid);

impl UserId {
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Customer,
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Customer => "customer",
            Self::Admin => "admin",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "customer" => Some(Self::Customer),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Email(String);

impl Email {
    pub fn parse(raw: &str) -> Result<Self, DomainError> {
        let email = raw.trim().to_lowercase();
        let valid = email.len() <= 254
            && !email.chars().any(char::is_whitespace)
            && email.split_once('@').is_some_and(|(local, domain)| {
                !local.is_empty()
                    && !domain.contains('@')
                    && domain.contains('.')
                    && !domain.starts_with('.')
                    && !domain.ends_with('.')
            });
        if valid {
            Ok(Self(email))
        } else {
            Err(DomainError::InvalidEmail)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Password(String);

impl Password {
    pub const MIN_LEN: usize = 8;
    pub const MAX_LEN: usize = 128;

    pub fn new(raw: &str) -> Result<Self, DomainError> {
        let len = raw.chars().count();
        if (Self::MIN_LEN..=Self::MAX_LEN).contains(&len) {
            Ok(Self(raw.to_owned()))
        } else {
            Err(DomainError::WeakPassword {
                min: Self::MIN_LEN,
                max: Self::MAX_LEN,
            })
        }
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(***)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    id: UserId,
    email: Email,
    name: String,
    role: Role,
    password_hash: String,
}

impl User {
    pub fn register(
        id: UserId,
        email: Email,
        name: &str,
        role: Role,
        password_hash: String,
    ) -> Result<Self, DomainError> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(DomainError::InvalidOwner);
        }
        Ok(Self::restore(
            id,
            email,
            name.to_owned(),
            role,
            password_hash,
        ))
    }

    pub fn restore(
        id: UserId,
        email: Email,
        name: String,
        role: Role,
        password_hash: String,
    ) -> Self {
        Self {
            id,
            email,
            name,
            role,
            password_hash,
        }
    }

    pub fn id(&self) -> UserId {
        self.id
    }
    pub fn email(&self) -> &Email {
        &self.email
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn role(&self) -> Role {
        self.role
    }
    pub fn password_hash(&self) -> &str {
        &self.password_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_is_normalized_and_validated() {
        assert_eq!(
            Email::parse("  Firuz@Example.TJ ").unwrap().as_str(),
            "firuz@example.tj"
        );
        for bad in [
            "", "no-at", "@x.tj", "a@", "a@b", "a@.tj", "a@tj.", "a b@x.tj", "a@b@x.tj",
        ] {
            assert!(Email::parse(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn password_policy_and_redacted_debug() {
        assert!(Password::new("short").is_err());
        assert!(Password::new(&"x".repeat(129)).is_err());
        let p = Password::new("correct horse").unwrap();
        assert_eq!(format!("{p:?}"), "Password(***)");
        assert_eq!(p.expose(), "correct horse");
    }

    #[test]
    fn role_round_trips() {
        for role in [Role::Customer, Role::Admin] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
        assert_eq!(Role::parse("root"), None);
    }
}
