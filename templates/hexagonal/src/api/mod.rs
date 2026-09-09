pub mod hello;
pub mod user;

use crate::application::UserService;
use crate::domain::{DomainError, Greeter, Readiness};
use axum::{extract::Request, extract::State, http::StatusCode, routing::get, Json, Router};
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceBuilder;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnResponse, TraceLayer};
use tracing::Level;

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

/// Wall-clock budget for one request, enforced by the layer below: past it the
/// handler's future is dropped and the caller gets tower-http's own `408
/// Request Timeout` (kept as-is — the body is empty, and a proxy reads the
/// status, not the text). Well above anything this template does on purpose
/// (`/ready` is bounded at 2s per check) and below the 60s an idle load
/// balancer typically gives up at, so a request that hits this is a bug, not a
/// slow backend. Larger than `SHUTDOWN_TIMEOUT` (10s) in main.rs on purpose: a
/// request still running at SIGTERM+10s is cut off without a response.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub fn router(services: Services) -> Router {
    let router = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .merge(user::routes())
        .merge(hello::routes())
        .with_state(services);
    middleware(router, REQUEST_TIMEOUT)
}

/// The whole HTTP stack, outermost layer first, applied to a finished
/// `Router<()>`. One function rather than a chain inside `router()` so a test
/// can wrap a router of its own in the very stack production runs (with a
/// timeout it can actually wait out).
pub fn middleware(router: Router, timeout: Duration) -> Router {
    router.layer(
        ServiceBuilder::new()
            // outermost, and it has to be: a caller's own `x-request-id` is left
            // alone and only a request without one gets a fresh uuid, so an id
            // assigned at the edge stays the same id through every hop — and
            // the layer below copies whatever this one settled on
            .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
            // above the trace and timeout layers, so *every* response carries
            // the id — including the 408 below and anything they turn away
            .layer(PropagateRequestIdLayer::x_request_id())
            // under both, so the span can record the id the two agreed on
            .layer(
                TraceLayer::new_for_http()
                    .make_span_with(|req: &Request| {
                        tracing::info_span!(
                            "http",
                            method = %req.method(),
                            // the uri path, not the matched route: `MatchedPath`
                            // is inserted by the router, which sits *below* this
                            // layer and has not run yet
                            path = %req.uri().path(),
                            request_id = req
                                .headers()
                                .get("x-request-id")
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or(""),
                        )
                    })
                    // status + latency, at info because this is the access log;
                    // the default failure classifier keeps 5xx at error, so a 5xx
                    // gets a second ERROR line here on top of `to_http_err`'s.
                    // Neither headers nor bodies are logged — they carry credentials.
                    .on_response(DefaultOnResponse::new().level(Level::INFO)),
            )
            // `TimeoutLayer::new` is the same thing and deprecated; the status
            // is spelled out because the constructor that is not takes it. The
            // 408 only drops the handler's future — `spawn_blocking` work
            // already handed to Diesel runs to completion (see main.rs).
            .layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                timeout,
            )),
    )
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
