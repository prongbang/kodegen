use crate::{
    api,
    application::{UserService, UserServiceImpl},
    domain::{Greeter, Readiness, UserRepository},
    infrastructure::{
        circuit_breaker::default_breaker,
        diesel_db::{build_pool, Db},
        diesel_repo::DieselUserRepository,
        grpc_greeter::GrpcGreeter,
        in_memory_repo::InMemoryUserRepository,
    },
};
use std::sync::Arc;

pub fn build_router() -> axum::Router {
    api::router(build_services_from_env())
}

pub fn build_services_from_env() -> api::Services {
    // shared resources are resolved once; each feature builder picks its adapter.
    // Every real backend registers its readiness check as it is resolved; the
    // in-memory defaults register nothing (see `api::ready`).
    let mut readiness: Vec<Arc<dyn Readiness>> = Vec::new();
    let db = std::env::var("DATABASE_URL")
        .ok()
        .map(|url| Db::new(build_pool(&url).expect("diesel pool init failed")));
    if let Some(db) = &db {
        readiness.push(Arc::new(db.clone()));
    }
    // kodegen:resources

    api::Services {
        user: build_user_service(db.as_ref()),
        greeter: build_greeter(),
        readiness,
        // kodegen:services:env
    }
}

/// How often the gRPC health status is re-evaluated. Kubernetes polls a probe
/// at `periodSeconds` (10s by default), so a shorter loop would only run the
/// checks for nobody.
const GRPC_HEALTH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Inbound gRPC services, one `.add_service(...)` line per feature
/// (`kodegen feature <name> --transport grpc` inserts them here).
/// Served by main.rs when `GRPC_PORT` is set.
///
/// Always carries `grpc.health.v1`, answering for the whole server (service
/// name `""`) with the same checks `GET /ready` runs — a gRPC-only deployment
/// needs a probe too. The status is pushed, not pulled: the protocol has no
/// per-request hook, so a task re-evaluates on `GRPC_HEALTH_INTERVAL`, which
/// is also why this must be called from inside the runtime.
pub fn grpc_routes(services: &api::Services) -> tonic::service::Routes {
    let mut routes = tonic::service::RoutesBuilder::default();
    let (reporter, health) = tonic_health::server::health_reporter();
    let checks = services.readiness.clone();
    tokio::spawn(async move {
        // tonic-health starts every service as SERVING; the first pass through
        // the checks can take up to the probe timeout, and a probe landing in
        // that window must not be told yes
        reporter
            .set_service_status("", tonic_health::ServingStatus::NotServing)
            .await;
        loop {
            let status = match api::readiness(&checks).await {
                Ok(()) => tonic_health::ServingStatus::Serving,
                Err(_) => tonic_health::ServingStatus::NotServing,
            };
            reporter.set_service_status("", status).await;
            tokio::time::sleep(GRPC_HEALTH_INTERVAL).await;
        }
    });
    routes.add_service(health);
    routes.routes()
}

/// In-memory defaults; tests build on this and swap in fake ports.
pub fn build_services() -> api::Services {
    api::Services {
        user: build_user_service(None),
        greeter: build_greeter(),
        readiness: Vec::new(),
        // kodegen:services:default
    }
}

fn build_user_service(db: Option<&Db>) -> Arc<dyn UserService> {
    let repo: Arc<dyn UserRepository> = match db {
        Some(db) => Arc::new(DieselUserRepository::new(db.clone())),
        None => Arc::new(InMemoryUserRepository::new()),
    };
    Arc::new(UserServiceImpl::new(repo))
}

fn build_greeter() -> Arc<dyn Greeter> {
    let greeter_addr =
        std::env::var("GREETER_ADDR").unwrap_or_else(|_| "http://localhost:50051".into());
    Arc::new(
        GrpcGreeter::connect_lazy(&greeter_addr, default_breaker()).expect("invalid GREETER_ADDR"),
    )
}
