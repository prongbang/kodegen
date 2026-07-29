pub mod hello;
pub mod user;

use crate::application::UserService;
use crate::domain::{DomainError, Greeter};
use axum::{http::StatusCode, routing::get, Json, Router};
use serde::Serialize;
use std::sync::Arc;

/// One field per feature; adding a feature = a field here, a `FromRef` +
/// `routes()` in its api/<feature>.rs, and one `.merge()` in `router()`.
#[derive(Clone)]
pub struct Services {
    pub user: Arc<dyn UserService>,
    pub greeter: Arc<dyn Greeter>,
}

#[derive(Serialize)]
pub struct ErrorRes {
    pub error: String,
}

pub fn router(services: Services) -> Router {
    Router::new()
        .route("/health", get(health))
        .merge(user::routes())
        .merge(hello::routes())
        .with_state(services)
}

async fn health() -> &'static str {
    "ok"
}

fn to_http_err(e: DomainError) -> (StatusCode, Json<ErrorRes>) {
    let (code, msg) = match e {
        DomainError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
        DomainError::Validation(m) => (StatusCode::BAD_REQUEST, m),
        DomainError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "service unavailable".into(),
        ),
        // logged here because this is the only arm whose cause is not already in
        // the response: the other three say what went wrong, this one deliberately
        // does not (an adapter's error text can carry a connection string, a
        // query, or a row). Dropping it entirely, though, is how a 500 becomes
        // unexplainable — so the operator gets it and the caller does not.
        DomainError::Other(e) => {
            tracing::error!(error = %e, "internal error");
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error".into())
        }
    };
    (code, Json(ErrorRes { error: msg }))
}
