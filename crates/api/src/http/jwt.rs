use std::time::Duration;

use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode, errors::ErrorKind,
};
use ledger_application::Actor;
use ledger_domain::{Role, User, UserId};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use super::ApiError;

const ISSUER: &str = "ledger";

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: Uuid,
    role: String,
    name: String,
    iss: String,
    iat: i64,
    exp: i64,
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

pub struct IssuedToken {
    pub token: String,
    pub expires_in: Duration,
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

    pub fn issue(&self, user: &User) -> Result<IssuedToken, ApiError> {
        self.issue_at(user, OffsetDateTime::now_utc())
    }

    pub fn issue_at(&self, user: &User, now: OffsetDateTime) -> Result<IssuedToken, ApiError> {
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
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        Ok(IssuedToken {
            token,
            expires_in: self.ttl,
        })
    }

    pub fn verify(&self, token: &str) -> Result<Actor, ApiError> {
        let data =
            decode::<Claims>(token, &self.decoding, &self.validation).map_err(|e| {
                match e.kind() {
                    ErrorKind::ExpiredSignature => ApiError::Unauthorized("token expired"),
                    _ => ApiError::Unauthorized("invalid token"),
                }
            })?;
        let claims = data.claims;
        let role = Role::parse(&claims.role).ok_or(ApiError::Unauthorized("invalid token"))?;
        Ok(Actor {
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
        let actor = keys.verify(&issued.token).unwrap();
        assert_eq!(
            (actor.user_id, actor.role, actor.name.as_str()),
            (u.id(), Role::Admin, "Firuz")
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
        assert!(matches!(
            keys.verify(&old.token),
            Err(ApiError::Unauthorized("token expired"))
        ));

        let foreign = JwtKeys::random(Duration::from_secs(60))
            .issue(&user(Role::Admin))
            .unwrap();
        assert!(matches!(
            keys.verify(&foreign.token),
            Err(ApiError::Unauthorized("invalid token"))
        ));

        assert!(keys.verify("not.a.jwt").is_err());
        let none_alg = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiIwIn0.";
        assert!(keys.verify(none_alg).is_err(), "alg=none must be rejected");
    }
}
