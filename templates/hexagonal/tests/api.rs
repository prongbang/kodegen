use axum::body::Body;
use axum::http::{Request, StatusCode};
use hexagonal_rs::bootstrap;
use hexagonal_rs::domain::{DomainError, Greeter};
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
