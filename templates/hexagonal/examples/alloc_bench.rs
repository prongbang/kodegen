//! Counts heap allocations per request through the full axum router
//! (routing + extractors + handlers + serde + repo; excludes hyper I/O).
//!   cargo run --release --example alloc_bench

use axum::body::Body;
use axum::http::Request;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use tower::ServiceExt;

struct Counting;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size() as u64, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn snapshot() -> (u64, u64) {
    (ALLOCS.load(Relaxed), BYTES.load(Relaxed))
}

async fn measure(app: &axum::Router, name: &str, make_req: impl Fn() -> Request<Body>, iters: u64) {
    // warmup
    for _ in 0..100 {
        let res = app.clone().oneshot(make_req()).await.unwrap();
        assert!(res.status().is_success(), "{name}: {}", res.status());
        let _ = axum::body::to_bytes(res.into_body(), 64 * 1024).await;
    }
    let (a0, b0) = snapshot();
    for _ in 0..iters {
        let res = app.clone().oneshot(make_req()).await.unwrap();
        let _ = axum::body::to_bytes(res.into_body(), 64 * 1024).await;
    }
    let (a1, b1) = snapshot();
    println!(
        "{name:<28} {:>6} allocs/req  {:>8} bytes/req",
        (a1 - a0) / iters,
        (b1 - b0) / iters
    );
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let app = hexagonal_rs::bootstrap::build_router();

    // seed one user for the GET path
    let res = app
        .clone()
        .oneshot(
            Request::post("/users")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"id":"u1","name":"Alice"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(res.status().is_success());

    let n = 10_000;
    measure(
        &app,
        "GET /health",
        || Request::get("/health").body(Body::empty()).unwrap(),
        n,
    )
    .await;
    measure(
        &app,
        "GET /users/u1",
        || Request::get("/users/u1").body(Body::empty()).unwrap(),
        n,
    )
    .await;
    measure(
        &app,
        "POST /users",
        || {
            Request::post("/users")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"id":"u1","name":"Alice"}"#))
                .unwrap()
        },
        n,
    )
    .await;
}
