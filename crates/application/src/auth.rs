use ledger_domain::{Email, Password, Role, User, UserId};

use crate::{
    AppError,
    ports::{IdGenerator, PasswordHasher, RepoError, UserRepository},
};

#[derive(Debug, Clone)]
pub struct Register {
    pub email: String,
    pub name: String,
    pub password: String,
}

#[derive(Debug)]
pub struct AuthService<U, H, G> {
    users: U,
    hasher: H,
    ids: G,
}

impl<U: UserRepository, H: PasswordHasher, G: IdGenerator> AuthService<U, H, G> {
    pub fn new(users: U, hasher: H, ids: G) -> Self {
        Self { users, hasher, ids }
    }

    #[tracing::instrument(skip_all, fields(email = %cmd.email), err(level = "warn"))]
    pub async fn register(&self, cmd: Register) -> Result<User, AppError> {
        self.create(&cmd.email, &cmd.name, &cmd.password, Role::Customer)
            .await
    }

    #[tracing::instrument(skip_all, fields(email = %email), err(level = "warn"))]
    pub async fn login(&self, email: &str, password: &str) -> Result<User, AppError> {
        let user = match Email::parse(email) {
            Ok(email) => self.users.find_user_by_email(&email).await?,
            Err(_) => None,
        };
        let hash = user
            .as_ref()
            .map_or_else(|| self.hasher.decoy_hash(), User::password_hash);
        let matches = self
            .hasher
            .verify(password, hash)
            .await
            .map_err(RepoError::Storage)?;
        match user {
            Some(user) if matches => Ok(user),
            _ => Err(AppError::InvalidCredentials),
        }
    }

    pub async fn ensure_admin(
        &self,
        email: &str,
        name: &str,
        password: &str,
    ) -> Result<User, AppError> {
        let parsed = Email::parse(email)?;
        if let Some(existing) = self.users.find_user_by_email(&parsed).await? {
            return Ok(existing);
        }
        match self.create(email, name, password, Role::Admin).await {
            Err(AppError::EmailTaken) => self
                .users
                .find_user_by_email(&parsed)
                .await?
                .ok_or(AppError::EmailTaken),
            other => other,
        }
    }

    async fn create(
        &self,
        email: &str,
        name: &str,
        password: &str,
        role: Role,
    ) -> Result<User, AppError> {
        let email = Email::parse(email)?;
        let password = Password::new(password)?;
        let hash = self
            .hasher
            .hash(&password)
            .await
            .map_err(RepoError::Storage)?;
        let user = User::register(
            UserId::from_uuid(self.ids.generate()),
            email,
            name,
            role,
            hash,
        )?;
        match self.users.insert_user(&user).await {
            Ok(()) => {
                tracing::info!(user_id = %user.id(), role = role.as_str(), "user registered");
                Ok(user)
            }
            Err(RepoError::DuplicateEmail) => Err(AppError::EmailTaken),
            Err(e) => Err(e.into()),
        }
    }
}
