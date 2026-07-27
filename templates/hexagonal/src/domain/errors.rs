use thiserror::Error;

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("not found")]
    NotFound,
    #[error("validation: {0}")]
    Validation(String),
    #[error("service unavailable")]
    Unavailable,
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}
