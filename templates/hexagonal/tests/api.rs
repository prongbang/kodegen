use axum::body::Body;
use axum::http::{Request, StatusCode};
use hexagonal_rs::bootstrap;
use hexagonal_rs::domain::{DomainError, Greeter, Readiness};
use std::sync::Arc;
use tower::ServiceExt;

fn json_req(method: &str, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn health_ok() {
    let app = bootstrap::build_router();
    let res = app
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

/// No backend built means no check registered — a fresh project is ready.
#[tokio::test]
async fn ready_ok_with_in_memory_defaults() {
    let app = bootstrap::build_router();
    let res = app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v, serde_json::json!({"status": "ok"}));
}

/// One failing check is a 503 that names it, and an adapter's own error text
/// (which can carry a connection string) never reaches the caller.
#[tokio::test]
async fn ready_is_503_naming_the_failed_check() {
    struct Fake(&'static str, fn() -> Result<(), DomainError>);
    #[async_trait::async_trait]
    impl Readiness for Fake {
        fn name(&self) -> &'static str {
            self.0
        }
        async fn ready(&self) -> Result<(), DomainError> {
            (self.1)()
        }
    }

    let mut services = bootstrap::build_services();
    services.readiness = vec![
        Arc::new(Fake("cache", || Ok(()))),
        Arc::new(Fake("database", || Err(DomainError::Unavailable))),
        Arc::new(Fake("sink", || {
            Err(DomainError::Other("postgres://app:s3cret@db/app".into()))
        })),
    ];
    let app = hexagonal_rs::api::router(services);

    let res = app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        v,
        serde_json::json!({
            "status": "unavailable",
            "checks": {
                "cache": "ok",
                "database": "service unavailable",
                "sink": "internal error",
            }
        })
    );
    assert!(!body.windows(6).any(|w| w == b"s3cret"), "{body:?}");
}

/// A check that never answers is a 503 as well, on the probe's own budget
/// rather than the caller's patience.
#[tokio::test]
async fn ready_is_503_when_a_check_outlives_the_budget() {
    struct Slow;
    #[async_trait::async_trait]
    impl Readiness for Slow {
        fn name(&self) -> &'static str {
            "slow"
        }
        async fn ready(&self) -> Result<(), DomainError> {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            Ok(())
        }
    }

    let mut services = bootstrap::build_services();
    services.readiness = vec![Arc::new(Slow)];
    let app = hexagonal_rs::api::router(services);

    let res = app
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["checks"]["slow"], "timed out after 2s");
}

#[tokio::test]
async fn create_then_get_user() {
    let app = bootstrap::build_router();

    let res = app
        .clone()
        .oneshot(json_req("POST", "/users", r#"{"id":"u1","name":"Alice"}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let res = app
        .oneshot(Request::get("/users/u1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let user: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(user["name"], "Alice");

    // trip-wire: the domain `User` currently IS the public API shape.
    // The day this fails, add a response DTO instead of widening the contract.
    let keys: Vec<_> = user.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["id", "name"]);
}

#[tokio::test]
async fn get_missing_user_is_404() {
    let app = bootstrap::build_router();
    let res = app
        .oneshot(Request::get("/users/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn hello_uses_greeter_port() {
    struct FakeGreeter;
    #[async_trait::async_trait]
    impl Greeter for FakeGreeter {
        async fn say_hello(&self, name: String) -> Result<String, DomainError> {
            Ok(format!("Hello {name}!"))
        }
    }

    let mut services = bootstrap::build_services();
    services.greeter = std::sync::Arc::new(FakeGreeter);
    let app = hexagonal_rs::api::router(services);

    let res = app
        .oneshot(Request::get("/hello/world").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["message"], "Hello world!");
}

#[tokio::test]
async fn create_user_empty_name_is_400() {
    let app = bootstrap::build_router();
    let res = app
        .oneshot(json_req("POST", "/users", r#"{"id":"u2","name":"  "}"#))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

/// Every response carries an id, whether or not the caller brought one — the
/// two cases together are the whole contract of the request-id pair.
#[tokio::test]
async fn request_id_is_generated_or_propagated() {
    let app = bootstrap::build_router();
    let res = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(!res.headers()["x-request-id"].is_empty());

    let res = app
        .oneshot(
            Request::get("/health")
                .header("x-request-id", "abc")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.headers()["x-request-id"], "abc");
}

/// The production stack around a handler that outlives its budget: 408, not a
/// hung connection. Wrapped by hand only to shorten the 30s wait.
#[tokio::test]
async fn slow_handler_times_out() {
    let router = axum::Router::new().route(
        "/slow",
        axum::routing::get(|| async {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }),
    );
    let app = hexagonal_rs::api::middleware(router, std::time::Duration::from_millis(100));
    let res = app
        .oneshot(Request::get("/slow").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::REQUEST_TIMEOUT);
    // the id layer is outside the timeout, so even the response the handler
    // never produced is traceable
    assert!(!res.headers()["x-request-id"].is_empty());
}
