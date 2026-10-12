use std::sync::Arc;

use argon2::{
    Argon2,
    password_hash::{Error as HashError, PasswordHasher as _, PasswordVerifier as _},
};
use ledger_application::ports::{BoxError, PasswordHasher};
use ledger_domain::Password;

#[derive(Debug, Clone)]
pub struct Argon2Hasher {
    decoy: Arc<str>,
}

impl Argon2Hasher {
    pub fn new() -> Result<Self, BoxError> {
        let decoy = hash_blocking(&uuid::Uuid::now_v7().to_string())?;
        Ok(Self {
            decoy: decoy.into(),
        })
    }
}

fn hash_blocking(password: &str) -> Result<String, BoxError> {
    Ok(Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(|e| e.to_string())?
        .to_string())
}

fn verify_blocking(password: &str, hash: &str) -> Result<bool, BoxError> {
    match Argon2::default().verify_password(password.as_bytes(), hash) {
        Ok(()) => Ok(true),
        Err(HashError::PasswordInvalid) => Ok(false),
        Err(e) => Err(e.to_string().into()),
    }
}

async fn off_runtime<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, BoxError> + Send + 'static,
) -> Result<T, BoxError> {
    tokio::task::spawn_blocking(job).await?
}

impl PasswordHasher for Argon2Hasher {
    #[tracing::instrument(name = "argon2.hash", skip_all)]
    async fn hash(&self, password: &Password) -> Result<String, BoxError> {
        let password = password.expose().to_owned();
        off_runtime(move || hash_blocking(&password)).await
    }

    #[tracing::instrument(name = "argon2.verify", skip_all)]
    async fn verify(&self, password: &str, hash: &str) -> Result<bool, BoxError> {
        let (password, hash) = (password.to_owned(), hash.to_owned());
        off_runtime(move || verify_blocking(&password, &hash)).await
    }

    fn decoy_hash(&self) -> &str {
        &self.decoy
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hashes_and_verifies() {
        let hasher = Argon2Hasher::new().unwrap();
        let password = Password::new("correct horse battery").unwrap();
        let hash = hasher.hash(&password).await.unwrap();
        assert!(hash.starts_with("$argon2id$"));
        assert_ne!(
            hash,
            hasher.hash(&password).await.unwrap(),
            "salt must differ"
        );
        assert!(hasher.verify("correct horse battery", &hash).await.unwrap());
        assert!(!hasher.verify("wrong", &hash).await.unwrap());
        assert!(
            !hasher
                .verify("anything", hasher.decoy_hash())
                .await
                .unwrap()
        );
        assert!(hasher.verify("x", "not-a-hash").await.is_err());
    }
}
