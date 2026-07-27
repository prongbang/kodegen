use crate::domain::{DomainError, User, UserRepository};
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait UserService: Send + Sync {
    async fn create_user(&self, id: String, name: String) -> Result<(), DomainError>;
    async fn get_user(&self, id: String) -> Result<User, DomainError>;
}

pub struct UserServiceImpl {
    repo: Arc<dyn UserRepository>,
}

impl UserServiceImpl {
    pub fn new(repo: Arc<dyn UserRepository>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl UserService for UserServiceImpl {
    async fn create_user(&self, id: String, name: String) -> Result<(), DomainError> {
        let user = User::new(id, name)?;
        self.repo.save(user).await
    }

    async fn get_user(&self, id: String) -> Result<User, DomainError> {
        self.repo.get(&id).await
    }
}
