use super::{errors::DomainError, model::User};
use async_trait::async_trait;

#[async_trait]
pub trait UserRepository: Send + Sync {
    /// Upsert: saving an existing `id` overwrites it and returns `Ok(())`.
    /// Never returns `NotFound`. Infrastructure failures map to `Other`.
    async fn save(&self, user: User) -> Result<(), DomainError>;
    /// Returns `NotFound` when `id` is absent — not `Ok` with a default.
    async fn get(&self, id: &str) -> Result<User, DomainError>;
}

/// Outbound port: greeting another service.
#[async_trait]
pub trait Greeter: Send + Sync {
    async fn say_hello(&self, name: String) -> Result<String, DomainError>;
}
