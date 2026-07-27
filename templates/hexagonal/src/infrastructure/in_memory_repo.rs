use crate::domain::{DomainError, User, UserRepository};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Default)]
pub struct InMemoryUserRepository {
    // ponytail: std RwLock — no .await is held across the lock; swap for a DB pool in real use
    inner: RwLock<HashMap<String, User>>,
}

impl InMemoryUserRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl UserRepository for InMemoryUserRepository {
    async fn save(&self, user: User) -> Result<(), DomainError> {
        self.inner
            .write()
            .expect("lock poisoned")
            .insert(user.id.clone(), user);
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<User, DomainError> {
        self.inner
            .read()
            .expect("lock poisoned")
            .get(id)
            .cloned()
            .ok_or(DomainError::NotFound)
    }
}
