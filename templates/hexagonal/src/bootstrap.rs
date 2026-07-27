use crate::{
    api,
    application::{UserService, UserServiceImpl},
    domain::{Greeter, UserRepository},
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
    // shared resources are resolved once; each feature builder picks its adapter
    let db = std::env::var("DATABASE_URL")
        .ok()
        .map(|url| Db::new(build_pool(&url).expect("diesel pool init failed")));

    api::router(api::Services {
        user: build_user_service(db.as_ref()),
        greeter: build_greeter(),
    })
}

/// In-memory defaults; tests build on this and swap in fake ports.
pub fn build_services() -> api::Services {
    api::Services {
        user: build_user_service(None),
        greeter: build_greeter(),
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
