use hexagonal_rs::{api, bootstrap};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// How long a graceful drain may take before the process exits anyway. Both
// servers stop accepting the moment the signal fires, but an established HTTP/2
// connection is only closed by its *client* — a gRPC channel is pooled and idle
// forever, so `serve_with_shutdown` alone would wait forever too. Kubernetes
// sends SIGKILL at `terminationGracePeriodSeconds` (default 30s); 10s leaves
// headroom for in-flight requests to finish and for the exit to be logged.
const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = tracing_subscriber::registry().with(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
    );
    // Same layer either way, so `RUST_LOG` keeps working identically. The
    // Dockerfile sets `LOG_FORMAT=json` so containers emit one JSON object per
    // line for the log shipper, while a local `cargo run` stays human-readable.
    if std::env::var("LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json")) {
        registry
            .with(tracing_subscriber::fmt::layer().json())
            .init();
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }

    let services = bootstrap::build_services_from_env();
    let app = api::router(services.clone());

    // One signal listener fanned out to both servers: axum and tonic each wait
    // on their own receiver, so SIGTERM drains HTTP *and* gRPC instead of
    // killing the process out from under half of it. `rx` here is never marked
    // seen, so a clone made *after* the send still observes the change — a
    // signal that arrives before the servers start cannot be lost.
    let (tx, rx) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = tx.send(());
    });

    // `try_join!` rather than a bare `spawn` for the gRPC half: a bind or serve
    // error there now propagates out of `main` (non-zero exit) instead of being
    // logged while the process limps on with gRPC dead. With `GRPC_PORT` unset
    // the branch is an immediate `Ok(())`.
    let grpc = async {
        let Ok(grpc_port) = std::env::var("GRPC_PORT") else {
            return Ok(());
        };
        let grpc_addr: std::net::SocketAddr = format!("0.0.0.0:{grpc_port}").parse()?;
        let routes = bootstrap::grpc_routes(&services);
        let mut rx = rx.clone();
        tracing::info!("starting gRPC on {grpc_addr}");
        tonic::transport::Server::builder()
            .add_routes(routes)
            .serve_with_shutdown(grpc_addr, async move {
                let _ = rx.changed().await;
            })
            .await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };

    let http = async {
        let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
        let addr = format!("0.0.0.0:{port}");
        let mut rx = rx.clone();

        let listener = tokio::net::TcpListener::bind(&addr).await?;
        tracing::info!("listening on http://{addr}");
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.changed().await;
            })
            .await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    };

    // The deadline starts at the *signal*, not at process start, and the servers
    // still win the race on the normal path (both futures resolve as soon as
    // their connections drain) and on the error path (a bind failure resolves
    // before any signal, so the timeout arm cannot swallow it).
    let mut drain = rx.clone();
    tokio::select! {
        r = async { tokio::try_join!(grpc, http) } => { r?; }
        () = async {
            let _ = drain.changed().await;
            tokio::time::sleep(SHUTDOWN_TIMEOUT).await;
        } => {
            tracing::warn!("drain timeout after {SHUTDOWN_TIMEOUT:?}, exiting with connections open");
            // Dropping the runtime waits on in-flight `spawn_blocking` work (Diesel queries; r2d2
            // checkout alone is 30s), so only `exit` keeps the 10s bound; `fmt` already flushed.
            std::process::exit(0);
        }
    }

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install ctrl_c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutting down");
}
