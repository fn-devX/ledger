use std::time::Duration;

use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode, errors::ErrorKind,
};
use ledger_domain::{Role, User, UserId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

const ISSUER: &str = "ledger";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("token expired")]
    Expired,
    #[error("invalid token")]
    Invalid,
    #[error("cannot sign token: {0}")]
    Signing(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenSubject {
    pub user_id: UserId,
    pub role: Role,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: Uuid,
    role: String,
    name: String,
    iss: String,
    iat: i64,
    exp: i64,
}

pub struct IssuedToken {
    pub token: String,
    pub expires_in: Duration,
}

pub struct JwtKeys {
    encoding: EncodingKey,
    decoding: DecodingKey,
    validation: Validation,
    ttl: Duration,
}

impl std::fmt::Debug for JwtKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtKeys")
            .field("ttl", &self.ttl)
            .finish_non_exhaustive()
    }
}

impl JwtKeys {
    pub fn new(secret: &[u8], ttl: Duration) -> Self {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[ISSUER]);
        validation.set_required_spec_claims(&["exp", "iss", "sub"]);
        validation.leeway = 30;
        Self {
            encoding: EncodingKey::from_secret(secret),
            decoding: DecodingKey::from_secret(secret),
            validation,
            ttl,
        }
    }

    pub fn random(ttl: Duration) -> Self {
        let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        Self::new(secret.as_bytes(), ttl)
    }

    pub fn issue(&self, user: &User) -> Result<IssuedToken, TokenError> {
        self.issue_at(user, OffsetDateTime::now_utc())
    }

    pub fn issue_at(&self, user: &User, now: OffsetDateTime) -> Result<IssuedToken, TokenError> {
        let ttl = i64::try_from(self.ttl.as_secs()).unwrap_or(i64::MAX);
        let claims = Claims {
            sub: user.id().as_uuid(),
            role: user.role().as_str().to_owned(),
            name: user.name().to_owned(),
            iss: ISSUER.to_owned(),
            iat: now.unix_timestamp(),
            exp: now.unix_timestamp().saturating_add(ttl),
        };
        let token = encode(&Header::new(Algorithm::HS256), &claims, &self.encoding)
            .map_err(|e| TokenError::Signing(e.to_string()))?;
        Ok(IssuedToken {
            token,
            expires_in: self.ttl,
        })
    }

    pub fn verify(&self, token: &str) -> Result<TokenSubject, TokenError> {
        let data =
            decode::<Claims>(token, &self.decoding, &self.validation).map_err(|e| {
                match e.kind() {
                    ErrorKind::ExpiredSignature => TokenError::Expired,
                    _ => TokenError::Invalid,
                }
            })?;
        let claims = data.claims;
        let role = Role::parse(&claims.role).ok_or(TokenError::Invalid)?;
        Ok(TokenSubject {
            user_id: UserId::from_uuid(claims.sub),
            role,
            name: claims.name,
        })
    }
}

#[cfg(test)]
mod tests {
    use ledger_domain::Email;

    use super::*;

    fn user(role: Role) -> User {
        User::restore(
            UserId::from_uuid(Uuid::now_v7()),
            Email::parse("a@b.tj").unwrap(),
            "Firuz".into(),
            role,
            "hash".into(),
        )
    }

    #[test]
    fn round_trip() {
        let keys = JwtKeys::random(Duration::from_secs(60));
        let u = user(Role::Admin);
        let issued = keys.issue(&u).unwrap();
        let subject = keys.verify(&issued.token).unwrap();
        assert_eq!(
            subject,
            TokenSubject {
                user_id: u.id(),
                role: Role::Admin,
                name: "Firuz".into()
            }
        );
    }

    #[test]
    fn rejects_expired_foreign_and_garbage_tokens() {
        let keys = JwtKeys::random(Duration::from_secs(60));
        let old = keys
            .issue_at(
                &user(Role::Customer),
                OffsetDateTime::now_utc() - time::Duration::hours(2),
            )
            .unwrap();
        assert_eq!(keys.verify(&old.token), Err(TokenError::Expired));

        let foreign = JwtKeys::random(Duration::from_secs(60))
            .issue(&user(Role::Admin))
            .unwrap();
        assert_eq!(keys.verify(&foreign.token), Err(TokenError::Invalid));

        assert_eq!(keys.verify("not.a.jwt"), Err(TokenError::Invalid));
        let none_alg = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiIwIn0.";
        assert_eq!(
            keys.verify(none_alg),
            Err(TokenError::Invalid),
            "alg=none must be rejected"
        );
    }
}
