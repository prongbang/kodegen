# Hexagonal/Clean Architecture Overview


This project follows a **Hexagonal/Clean Architecture** with a clear composition root and one–way dependencies.

```
src/
 ├─ main.rs            # entrypoint (HTTP; also serves gRPC when GRPC_PORT is set)
 ├─ bootstrap.rs       # composition root (one builder per feature + grpc_routes)
 ├─ api/               # delivery layer: one module per feature —
 │                     #   <feature>.rs (axum routes) or <feature>_grpc.rs (tonic adapter)
 ├─ application/       # use-cases, one module per feature
 ├─ domain/            # core business + ports (traits)
 └─ infrastructure/    # adapters (DB, cache, clients, etc.)
proto/                 # protobuf definitions — every file here is compiled by build.rs
```

## Quick Start

### Prerequisites

- Rust (stable, edition 2021).
- `protoc` — required for local builds because `build.rs` compiles every `.proto` in `proto/` via `tonic-prost-build`.
  - macOS: `brew install protobuf`
  - Debian/Ubuntu: `apt-get install protobuf-compiler`

### Run

```bash
cargo run                       # HTTP on 0.0.0.0:$PORT (default 8080)
GRPC_PORT=50052 cargo run       # + inbound gRPC features on 0.0.0.0:50052
```

### Test

```bash
cargo test
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` on every push/PR to `main` (after installing `protoc`).

### Examples

```bash
# local Greeter gRPC server for testing GET /hello/{name} (honors GREETER_PORT, default 50051)
cargo run --example greeter_server

# allocation-per-request measurement through the full router (routing + extractors + handlers + serde + repo)
cargo run --release --example alloc_bench
```

### Docker

```bash
docker build -t hexagonal-rs .
docker run -p 8080:8080 hexagonal-rs
```

The build stage installs `protobuf-compiler` itself, so no local `protoc` is needed just to build the image.

## HTTP Endpoints

| Method | Path | Description | Success | Failure |
|---|---|---|---|---|
| GET | `/health` | Liveness check | `200` `ok` | — |
| POST | `/users` | Create a user. Body: `{"id": "...", "name": "..."}` | `201` `{"ok": true}` | `400` on empty/too-long `id` or `name` |
| GET | `/users/{id}` | Fetch a user | `200` `User` JSON (`{"id", "name"}`) | `404` if not found |
| GET | `/hello/{name}` | Greet via the outbound gRPC Greeter service | `200` `{"message": "..."}` | `503` if the circuit breaker is open or the upstream is unavailable |

```bash
curl -X POST localhost:8080/users -H 'content-type: application/json' -d '{"id":"u1","name":"Alice"}'
curl localhost:8080/users/u1
curl localhost:8080/hello/world
```

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `PORT` | `8080` | HTTP listen port |
| `GREETER_ADDR` | `http://localhost:50051` | gRPC address of the upstream Greeter service |
| `GRPC_PORT` | unset | Optional. When set, inbound gRPC features (`bootstrap::grpc_routes`) are served on this port alongside HTTP. |
| `RUST_LOG` | `info` | Tracing log filter (`EnvFilter`) |
| `DATABASE_URL` | unset (in-memory repo) | Optional. SQLite file path (e.g. `app.db` or `/data/app.db`) for the Diesel-backed `UserRepository`. Unset falls back to the in-memory adapter. |

## Outbound gRPC + Circuit Breaker

- The `Greeter` port (`src/domain/ports.rs`) is implemented by `GrpcGreeter` (`src/infrastructure/grpc_greeter.rs`), which wraps a generated `GreeterClient` (from `proto/greeter.proto`, compiled by `build.rs`).
- The underlying tonic `Channel` is wrapped with a tower `BreakerLayer` (`src/infrastructure/circuit_breaker.rs`), backed by [`recloser`](https://docs.rs/recloser). The default breaker (`default_breaker()`) opens once ≥50% of the last 100 calls fail, and after a 30s cooldown allows a half-open trial call through.
- The tonic `Endpoint` is configured with a 2s connect timeout and a 5s request timeout (`GrpcGreeter::connect_lazy`).
- When the circuit is open, calls are rejected before hitting the network and mapped to `DomainError::Unavailable`; the same mapping applies to genuine gRPC `Unavailable`/`DeadlineExceeded` statuses. The `api` layer turns `DomainError::Unavailable` into HTTP `503`.
- Swapping the greeter implementation or tuning the breaker/timeouts only touches `bootstrap.rs` (`build_greeter()`) — `domain` and `api` stay unaware of gRPC/recloser.

## Benchmarks

Reference numbers from one machine (Apple M4 Pro, release build, `wrk -t4 -c64`, localhost) — not a guarantee for other hardware or workloads. Reproduce throughput with `wrk` against `cargo run --release`, and allocation counts with `cargo run --release --example alloc_bench`.

| Backend | GET /users/{id} | POST /users |
|---|---|---|
| In-memory | ~175k req/s, p99 ~0.5ms | ~175k req/s, p99 ~0.5ms |
| Diesel/SQLite (WAL) | ~41k req/s | ~51k req/s |

Allocations per request (`alloc_bench`):

| Endpoint | In-memory | Diesel |
|---|---|---|
| `GET /health` | 15 | 15 |
| `GET /users/{id}` | 25 | 32 |
| `POST /users` | 35 | 46 |

## Responsibilities

### `main.rs` — Entrypoint
- Starts the runtime (logging/tracing/Tokio).
- Binds the TCP listener and calls `axum::serve`; when `GRPC_PORT` is set it also spawns a tonic server with `bootstrap::grpc_routes(&services)`.
- Contains no business logic or wiring details.

### `bootstrap.rs` — Composition Root
- Builds concrete adapters (e.g., Diesel/SQLite vs. in-memory), services, and the HTTP router/state.
- `build_services_from_env()` resolves shared resources once (e.g. the `Db` pool from `DATABASE_URL`), then one `build_<feature>_service(db)` builder per feature picks its adapter — adding a feature adds one builder and one `Services` field, nothing multiplies.
- `grpc_routes(&services)` collects inbound tonic services (one `.add_service(...)` per gRPC feature); `main.rs` serves it when `GRPC_PORT` is set.
- The only module allowed to import **everything** (api + application + domain + infra).
- Great place to switch implementations by config/env/feature flags.
- Tests build on top of it: call `bootstrap::build_services()` to get real defaults (in-memory repo, `GrpcGreeter`), then swap in a fake port implementation before passing the services to `api::router()`. See `tests/api.rs` (`FakeGreeter`) for the pattern. One test also asserts the response JSON's exact key set (`["id", "name"]`) as a trip-wire: if the domain `User` gains a field, that assertion fails instead of silently widening the public API — the fix is a response DTO, not loosening the test.

### `api/` — Delivery Layer
- One module per feature: `<feature>.rs` holds that feature's axum `routes()` + handlers; `<feature>_grpc.rs` holds a tonic adapter registered in `bootstrap::grpc_routes`. `mod.rs` just owns `Services` and assembles `router()` from `.merge(<feature>::routes())` lines.
- Handlers pull only the dependencies they need via `State<…>` (often with `FromRef`).
- Maps domain/application errors into HTTP status codes (`to_http_err`) or gRPC `Status` (`to_status` in each `*_grpc.rs`).
- No database or business rules here.

### `application/` — Use‑Cases
- Orchestrates flows across domain objects and ports (e.g., `CreateUser`, `GetUser`).
- Depends on **domain ports (traits)** only; unaware of specific databases/HTTP.
- A natural place for transaction boundaries and cross-entity workflows.

### `domain/` — Core
- Entities, value objects, domain services, and domain errors.
- **Ports (traits)** such as `UserRepository` are defined here, each documenting a behavioral contract — e.g. `UserRepository::save` is an upsert and never returns `NotFound`; `get` returns `NotFound` when the id is absent, not an empty/default value. Adapters must honor the documented contract so they stay substitutable.
- Pure business rules; framework‑agnostic; minimal dependencies.

### `infrastructure/` — Adapters
- Implements domain ports: DB repositories, caches, MQ, external clients (gRPC), logging, config, migrations.
- Swappable without touching `domain`/`application` code.
- `UserRepository` ships two adapters: `in_memory_repo::InMemoryUserRepository` (default) and `diesel_repo::DieselUserRepository`, selected at startup by setting `DATABASE_URL` (see `bootstrap::build_router`).
- Diesel/SQLite plumbing shared by every repository lives in `diesel_db.rs`, not in the repository itself: `build_pool(url)` runs the embedded migrations and, on a single setup connection, switches the database file to `WAL` mode once; the resulting r2d2 pool is wrapped in a cheap-to-clone `Db` handle whose `run(closure)` does the `spawn_blocking` + connection checkout + error mapping every repository needs. `DieselUserRepository::new(db: Db)` just takes that handle, so a future repository shares the same pool by cloning `Db` rather than re-deriving pool/pragma logic.
- Every pooled connection also sets `PRAGMA busy_timeout` and `PRAGMA synchronous = NORMAL` on acquire: SQLite allows only one writer at a time, so without WAL + `busy_timeout` concurrent writers fail immediately with `SQLITE_BUSY` instead of waiting their turn.
- The composition root (`bootstrap.rs`) is the only place that builds the pool and constructs `DieselUserRepository`; repositories never see `DATABASE_URL` or the pool directly.
- Moving to Postgres later means swapping the `diesel` `sqlite` feature for `postgres` and pointing `DATABASE_URL` at a Postgres connection string — the port and call sites don't change.

## Dependency Direction

```
api  ─▶  application  ─▶  domain
                       ▲
                       └── infrastructure (implements domain ports)
bootstrap depends on all to assemble the app
```

- **Only the composition root** (`bootstrap.rs`) wires concrete implementations together.
- **api** depends on **application**, **application** depends on **domain** (ports), and **infrastructure** implements those ports.

## Typical Request Flow

`POST /users` → `api` handler → `application` use‑case → domain validation → call `UserRepository` (port) → `infrastructure` repository hits DB → result mapped back to HTTP.

A gRPC feature is the same flow with a different first hop: `Create` RPC → `api/<feature>_grpc.rs` adapter → the **same** `application` use-case; errors map to `tonic::Status` instead of HTTP codes.

## Adding a Feature

If this project was generated with [kodegen](https://github.com/prongbang/kodegen), the whole
checklist below is automated: `kodegen feature orders` creates every file, wires it up, and
leaves working `POST /orders` + `GET /orders/{id}` routes plus an integration test.
`kodegen feature orders --transport grpc` scaffolds the same domain/application/infrastructure
layers but delivers them as a tonic service instead: a `proto/orders.proto`, an
`api/orders_grpc.rs` adapter registered in `bootstrap::grpc_routes`, and a client-based
integration test. Serve gRPC features with `GRPC_PORT=50052 cargo run`.

Manually, follow the dependency direction, inside out. Example: `orders`.

1. **`domain/`** — add the entity to `model.rs`, the port trait (e.g. `OrderRepository`) to `ports.rs`, re-export both in `domain/mod.rs`. Add a `DomainError` variant only if none fits.
2. **`application/orders.rs`** — define `OrderService` (trait) + `OrderServiceImpl` (impl over the port). Register in `application/mod.rs`: `pub mod orders;` + re-export.
3. **`infrastructure/`** — implement the port: `orders_repo.rs` (start in-memory; a Diesel one reuses `Db` by cloning it). Register in `infrastructure/mod.rs`.
4. **`api/orders.rs`** — handlers + a `OrderSvc(Arc<dyn OrderService>)` wrapper with `FromRef<Services>` + its own `routes()`. Then in `api/mod.rs`: add `pub mod orders;`, one `order` field on `Services`, and one `.merge(orders::routes())` in `router()` (or `.nest("/orders", …)` for a prefix).
5. **`bootstrap.rs`** — add a `build_order_service(db: Option<&Db>)` builder (picks the adapter, reuses the pool via `db.clone()`) and set the new field in `build_services_from_env()`/`build_services()`. For a gRPC feature, also add `.add_service(...)` in `grpc_routes()`.
6. **`tests/`** — hit the new routes through `bootstrap::build_services()`, swapping in a fake port impl where needed (see `FakeGreeter` in `tests/api.rs`).

Each step touches one file per layer; nothing existing changes shape — `Services` gains a field, `router()` gains routes, everything else is new files.

## Production Notes

- **Testability:** domain/application can be unit‑tested with mocks; API/infra with integration tests.
- **Swappable adapters:** change DB/clients in `infrastructure/`, adjust wiring only in `bootstrap.rs`.
- **Clear boundaries:** each layer has a single responsibility; accidental cross‑layer imports are easy to spot.
- **`dyn` services, measured:** `Services` holds `Arc<dyn UserService>` / `Arc<dyn Greeter>` so adding a feature never grows generic signatures. The vtable cost is not measurable here: allocations per request are identical to a fully-generic variant (`#[async_trait]` already boxes every call), and `wrk` throughput differs by less than run-to-run noise (±2%). If you strip `async_trait` and need every nanosecond, making `Services` generic again is a local change to `api/mod.rs` + `bootstrap.rs`.
- **State cloning:** prefer storing types that are cheap to clone (e.g., `diesel::r2d2::Pool` and `tonic::transport::Channel` are already cheap/Arc-backed — see `Db` in `diesel_db.rs`). Wrap custom non‑clone resources in `Arc<T>` only when necessary.

## Getting Started (wiring, simplified)

The real wiring lives in `src/bootstrap.rs` and `src/main.rs`; this is the shape of it,
trimmed for illustration (see those files for the actual code, including the
greeter/circuit-breaker wiring, `PORT`/`GREETER_ADDR` env handling, and graceful shutdown):

```rust
// bootstrap.rs
pub fn build_router() -> axum::Router {
    api::router(build_services_from_env())
}

pub fn build_services_from_env() -> api::Services {
    // shared resources are resolved once; each feature builder picks its adapter
    let db = std::env::var("DATABASE_URL")
        .ok()
        .map(|url| Db::new(build_pool(&url).expect("diesel pool init failed")));

    api::Services {
        user: build_user_service(db.as_ref()),
        greeter: build_greeter(),
    }
}

// one builder per feature: picks Diesel when a Db is available, in-memory otherwise
fn build_user_service(db: Option<&Db>) -> Arc<dyn UserService> {
    let repo: Arc<dyn UserRepository> = match db {
        Some(db) => Arc::new(DieselUserRepository::new(db.clone())),
        None => Arc::new(InMemoryUserRepository::new()),
    };
    Arc::new(UserServiceImpl::new(repo))
}
```

```rust
// main.rs
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = crate::bootstrap::build_router();
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app).await?;
    Ok(())
}
```

## Tips

- Use `FromRef` (or small newtype wrappers + `FromRef`) so each handler requests only the dependency it needs.
- Put environment/config parsing in `bootstrap.rs`; keep `api/application/domain` framework‑agnostic.
- For multiple binaries (API, worker, migrator), create `src/bin/<name>.rs` and share `bootstrap` helpers.

## 🙏 Acknowledgments

- Built with Rust 🦀
- IDE Support by [RustRover](https://www.jetbrains.com/rust/)

![RustRover](https://resources.jetbrains.com/help/img/idea/2024.3/RustRover_icon.svg)

---
