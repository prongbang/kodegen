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

/// Readiness probe: one per real backend, run by `GET /ready` and the gRPC
/// health service. `ready` is the cheapest round-trip the backend answers
/// (`SELECT 1`, `PING`, `HEAD bucket`), and for a lazily-dialed adapter it is
/// what dials — a datastore nobody has talked to yet is not ready.
///
/// Infrastructure failures map the way the adapter's other methods do
/// (`Unavailable` for a dead server, `Other` for anything it answered); the
/// probe never returns `Other`'s text to the caller.
#[async_trait]
pub trait Readiness: Send + Sync {
    /// What `/ready` names this check by — the backend, not the port.
    fn name(&self) -> &'static str;
    async fn ready(&self) -> Result<(), DomainError>;
}
