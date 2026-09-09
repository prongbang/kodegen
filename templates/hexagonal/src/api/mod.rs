pub mod hello;
pub mod user;

use crate::application::UserService;
use crate::domain::{DomainError, Greeter, Readiness};
use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;

/// One field per feature; adding a feature = a field here, a `FromRef` +
/// `routes()` in its api/<feature>.rs, and one `.merge()` in `router()`.
#[derive(Clone)]
pub struct Services {
    pub user: Arc<dyn UserService>,
    pub greeter: Arc<dyn Greeter>,
    /// One check per real backend the composition root built; the in-memory
    /// defaults register nothing, so `/ready` is 200 with no server around.
    pub readiness: Vec<Arc<dyn Readiness>>,
}

#[derive(Serialize)]
pub struct ErrorRes {
    pub error: String,
}

/// `{"status":"ok"}`, or `{"status":"unavailable","checks":{"<name>":"ok"|"<why>"}}`.
#[derive(Serialize)]
pub struct ReadyRes {
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<BTreeMap<&'static str, String>>,
}

/// Per-check budget for `/ready`. A datastore that accepts the connection and
/// then says nothing must not hang the probe: past its own `timeoutSeconds`
/// Kubernetes counts the probe as failed anyway, but without a body that names
/// the backend. Set the probe's timeout above this.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

pub fn router(services: Services) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .merge(user::routes())
        .merge(hello::routes())
        .with_state(services)
}

async fn health() -> &'static str {
    "ok"
}

/// Readiness, as opposed to `/health` above, which is liveness and never looks
/// at a backend: a pod whose database is down should stop receiving traffic,
/// not be restarted into the same state.
async fn ready(State(services): State<Services>) -> (StatusCode, Json<ReadyRes>) {
    match readiness(&services.readiness).await {
        Ok(()) => (
            StatusCode::OK,
            Json(ReadyRes {
                status: "ok",
                checks: None,
            }),
        ),
        Err(checks) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyRes {
                status: "unavailable",
                checks: Some(checks),
            }),
        ),
    }
}

/// Every check at once, each under `READY_TIMEOUT`. `Err` carries all of them
/// ("ok" or why not) so the caller can say which backend is down. Shared with
/// the gRPC health service (`bootstrap::grpc_routes`).
pub async fn readiness(
    checks: &[Arc<dyn Readiness>],
) -> Result<(), BTreeMap<&'static str, String>> {
    // spawned rather than awaited in turn: N slow backends cost one deadline, not N
    let probes: Vec<_> = checks
        .iter()
        .cloned()
        .map(|check| (check.name(), tokio::spawn(probe(check))))
        .collect();
    let mut out = BTreeMap::new();
    for (name, probe) in probes {
        let outcome = match probe.await {
            Ok(Ok(())) => "ok".to_string(),
            Ok(Err(why)) => why,
            Err(e) => {
                tracing::warn!(check = name, error = %e, "readiness check panicked");
                "internal error".into()
            }
        };
        out.insert(name, outcome);
    }
    if out.values().all(|v| v == "ok") {
        Ok(())
    } else {
        Err(out)
    }
}

async fn probe(check: Arc<dyn Readiness>) -> Result<(), String> {
    let name = check.name();
    match tokio::time::timeout(READY_TIMEOUT, check.ready()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            tracing::warn!(check = name, error = %e, "readiness check failed");
            // the same rule as `to_http_err`: `Other` is the adapter's own text
            // and can carry a connection string, so the operator gets it (above)
            // and the caller does not
            Err(match e {
                DomainError::Other(_) => "internal error".into(),
                e => e.to_string(),
            })
        }
        Err(_) => {
            tracing::warn!(
                check = name,
                "readiness check timed out after {READY_TIMEOUT:?}"
            );
            Err(format!("timed out after {READY_TIMEOUT:?}"))
        }
    }
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
