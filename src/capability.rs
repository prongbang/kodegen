//! `kodegen add <capability>`: give a generated project a piece of
//! infrastructure — a relational backend, a cache, or object storage.
//!
//! Same shape as `scaffold::add_feature` — the whole change is computed in
//! memory (which is what validates every anchor and signature) and written in
//! one phase at the end, so `--dry-run` is the same code path minus the writes.
//!
//! The two paths differ in kind, which is why they are two functions:
//!
//! - **relational** (`add_relational`) *replaces* an adapter behind a port the
//!   template already has. `infrastructure/diesel_db.rs` is written against one
//!   connection type from top to bottom, so it is rewritten, not edited — and
//!   anything rewritten is first compared against what `kodegen new` actually
//!   wrote (the embedded template with the project's name substituted in), so a
//!   file the user has touched is refused with manual instructions rather than
//!   clobbered. Only one may be on at a time.
//! - **mongodb** (`add_mongodb`) replaces the same adapter with one that is not
//!   Diesel at all, so it takes the same slot as a relational backend and is
//!   refused next to one. It writes a new file rather than rewriting
//!   `diesel_db.rs`, and patches the three blocks of `bootstrap.rs` that name the
//!   Diesel handle — the same verbatim-signature check, one file over.
//! - **redis** (`add_redis`) and **s3** (`add_s3`) *add a port* the template does
//!   not ship, together with both of its adapters and the wiring. Purely
//!   additive, so they compose with any datastore and with each other, and
//!   neither has an existing file to refuse.
//! - **clickhouse**, **influxdb** and **greptimedb** (`add_event_sink`) add the
//!   `EventSink` port the same way — but the three of them share one *slot*, the
//!   way the datastores do: one port, one adapter, so a project has an event sink
//!   or the other, never both. (`greptimedb` is `influxdb` with a different image
//!   and two different variables, the way `mariadb` is `mysql` — one adapter, one
//!   cargo feature.) Everything else about them is additive, so any of them sits
//!   next to any datastore, the cache and the blob store.
//! - **scalar** (`add_scalar`) adds no port at all: `GET /docs` and
//!   `GET /openapi.json` are a route, not an adapter behind a trait, so there is
//!   nothing for a composition root to build and nothing to refuse a second
//!   occupant of. What it does add is a `#[cfg_attr(feature = "apidocs", ...)]`
//!   above every handler and DTO the template ships, plus one new file
//!   (`src/api/openapi.rs`) whose `paths(...)` list names them. `kodegen feature`
//!   carries the same attributes on every handler it scaffolds regardless of
//!   whether this capability is on, which is what lets the two commands run in
//!   either order — see the comment on `add_scalar`.
//!
//! `Slot` is what makes "these exclude each other" one rule rather than one per
//! pair: a capability names the slot it fills, the slot names the cargo features
//! that fill it, and `add` refuses a second occupant before it reads a file.

use crate::scaffold::{
    edit, insert_after_line, insert_at_marker, insert_before_line, insert_pub_mod, require_files,
    write_plan,
};
use std::{error::Error, fs, path::Path};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Capability {
    Postgres,
    Mysql,
    Mariadb,
    Timescaledb,
    Mongodb,
    Mssql,
    Redis,
    S3,
    Clickhouse,
    Influxdb,
    Greptimedb,
    Scalar,
}

/// Which ORM a relational capability is built on. Orthogonal to `Capability`
/// the way `--transport rest|grpc` is orthogonal to a feature name, which is why
/// it is a flag rather than four more capabilities: `mariadb` and `timescaledb`
/// are already aliases, and pairing every one of them with an ORM would make
/// them aliases squared.
///
/// Only the relational capabilities take one — `redis` has no ORM, and saying
/// so is an error rather than a silently ignored flag (see `check_orm`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum Orm {
    /// The default: a C driver, a blocking pool, and versioned migrations under
    /// `migrations/`.
    Diesel,
    /// Pure Rust (sqlx + rustls), async all the way down, and the schema applied
    /// at startup instead of a migration harness — see `add_seaorm`.
    Seaorm,
}

/// The axis a capability sits on: datastores replace each other, event sinks
/// replace each other, a cache and a blob store are additive. Every kind needs
/// its own `add_*` function, its own env vars and its own feature, which is
/// exactly what the matches on this enforce.
///
/// MongoDB is its own kind rather than a fourth `Relational`: it shares the
/// *slot* with them, not the driver, and everything `Relational` answers
/// (connection type, `on_conflict` target, dialect `CREATE TABLE`, which apt
/// packages the bundled C driver needs) has no meaning for it. Splitting them
/// keeps both enums total — `Relational` never has to invent an answer, and a
/// new kind is a compile error at every match below rather than a capability
/// quietly taking the Diesel path.
///
/// SQL Server is its own kind for the *same* reason, and it is the sharper case:
/// it really is a relational database, and `Relational` still has no answer for
/// it. There is no Diesel backend (diesel#2201, open since 2019), no open-source
/// SeaORM backend (SQL Server lives in the commercial SeaORM X), so no
/// `feature(orm)`, no `DieselConnection`, no `migrations/`, and no `ON CONFLICT`
/// of any spelling — `Mssql` in that enum would make every method on it start
/// inventing. Named for the vendor rather than the wire protocol (`Tds`) because
/// there is exactly one: Azure SQL Database and Azure SQL Managed Instance are
/// the same server at the other end of the same driver, the way Valkey is redis.
enum Kind {
    Relational(Relational),
    Document,
    Mssql,
    Cache,
    Blob,
    Events(EventBackend),
    /// `GET /docs` + `GET /openapi.json`. No slot for the same reason `Cache`
    /// and `Blob` have none — see the `slot()` doc — but it is not a third
    /// example of the same shape: it has no port, no trait, no adapter the
    /// composition root builds, so it needs no env either (`env()` returns
    /// `&[]`, which is what `main.rs` has to not index into — see the
    /// `env[0]` fix next to `Cmd::Add`).
    ApiDocs,
}

impl Kind {
    /// The slot this kind fills, if it is exclusive. `Db`/`Mongo` is built once
    /// in the composition root and `UserRepository` gets one adapter, so postgres,
    /// mysql, mariadb, timescaledb and mongodb all exclude each other; the
    /// `EventSink` port has exactly one adapter for the same reason, so clickhouse,
    /// influxdb and greptimedb exclude each other. A cache and a blob store fill no
    /// slot and sit alongside whatever is on.
    fn slot(&self) -> Option<Slot> {
        match self {
            Kind::Relational(_) | Kind::Document | Kind::Mssql => Some(Slot::Datastore),
            Kind::Events(_) => Some(Slot::Events),
            Kind::Cache | Kind::Blob | Kind::ApiDocs => None,
        }
    }
}

/// A place in a generated project that holds exactly one adapter. Two
/// capabilities in the same slot are alternatives, not additions — which is one
/// rule with one error message rather than a rule per pair.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Datastore,
    Events,
}

impl Slot {
    /// Every slot, for the test that pins these lists to `Kind::slot()`. A new
    /// variant is a compile error in the two matches below; this is the only line
    /// that has to be remembered.
    const ALL: [Slot; 2] = [Slot::Datastore, Slot::Events];

    /// The cargo features that fill this slot. `default = [...]` is the only
    /// state there is, so "is the slot taken?" is answered by looking for one of
    /// these names in it — which is also why they have to stay in step with
    /// `Capability::feature()` (see the test at the bottom).
    fn features(self) -> &'static [&'static str] {
        match self {
            // the SeaORM names are in this list and not in a second one: one ORM
            // per project is the *same* rule as one datastore per project, so
            // `add postgres` on a SeaORM project is refused by the code that
            // already refuses it on a Mongo one — no new rule, no new if
            Slot::Datastore => &[
                "sqlite",
                "postgres",
                "mysql",
                "document",
                "mssql",
                "seaorm-postgres",
                "seaorm-mysql",
            ],
            Slot::Events => &["analytics", "timeseries"],
        }
    }

    /// The occupant the template ships with, which `add` replaces silently
    /// instead of refusing. The event sink has none: a fresh project has no
    /// `EventSink` port at all, so the first `add` is always the only one.
    fn shipped(self) -> Option<&'static str> {
        match self {
            Slot::Datastore => Some("sqlite"),
            Slot::Events => None,
        }
    }

    fn what(self) -> &'static str {
        match self {
            Slot::Datastore => "datastore",
            Slot::Events => "event sink",
        }
    }
}

/// The relational subset of `Capability`. Split out so that everything about a
/// dialect — connection type, upsert, migration, compose image — stays total:
/// `add redis` has no answer to "which `DieselConnection`?" and should not have
/// to invent one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Relational {
    Postgres,
    Mysql,
    Mariadb,
    Timescaledb,
}

/// The two backends behind the one `EventSink` port. Split out for the same
/// reason `Relational` is: they share every line of the port, the default
/// adapter and the wiring, and differ only in the adapter file, the resource the
/// composition root builds, the env that selects it and the compose service.
///
/// `Influxdb` and `Greptimedb` are two *names* for one of them, the way mariadb
/// and mysql are: one adapter, because the write path is one HTTP POST of line
/// protocol on both — only the image and the env differ.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EventBackend {
    Clickhouse,
    /// InfluxDB v2 and v3.
    Influxdb,
    /// The same line protocol, under `/v1/influxdb`.
    Greptimedb,
}

/// Package name of the built-in template, needed to undo the rename
/// `kodegen new` applies before comparing a project's file to its origin.
const TEMPLATE_PKG: &str = "hexagonal-rs";

const DIESEL_DB: &str = "src/infrastructure/diesel_db.rs";
const DIESEL_REPO: &str = "src/infrastructure/diesel_repo.rs";
const COMPOSE: &str = "docker-compose.dev.yml";
const APT_ANCHOR: &str = "protobuf-compiler";

const BOOTSTRAP: &str = "src/bootstrap.rs";
const INFRA_MOD: &str = "src/infrastructure/mod.rs";
const MONGO_REPO: &str = "src/infrastructure/mongo_repo.rs";
/// Default port, and the same `127.0.0.1` rule as every other URL here.
/// `MONGODB_DB` has a default (`app`) in the generated builder, so it is
/// informational — `MONGODB_URL` is the switch.
const MONGO_URL: &str = "mongodb://127.0.0.1:27017";
/// Appended to `infrastructure/mod.rs`. The `cfg` is the whole point: with it,
/// undoing this capability is one line in Cargo.toml plus `bootstrap.rs`, and
/// the adapter can stay on disk uncompiled — exactly the deal the Diesel modules
/// get from `relational`.
const MONGO_MOD: &str = "#[cfg(feature = \"document\")]\npub mod mongo_repo;\n";

const SEA_DB: &str = "src/infrastructure/sea_db.rs";
const SEA_REPO: &str = "src/infrastructure/sea_repo.rs";
/// The SeaORM half of `MONGO_MOD`, and gated for the same reason: with `seaorm`
/// off these two files stay on disk and stop compiling, so undoing the
/// capability is one line in Cargo.toml plus `bootstrap.rs` — never a deletion.
const SEA_MOD: &str = "#[cfg(feature = \"seaorm\")]\npub mod sea_db;\n#[cfg(feature = \"seaorm\")]\npub mod sea_repo;\n";

const MSSQL_DB: &str = "src/infrastructure/mssql_db.rs";
const MSSQL_REPO: &str = "src/infrastructure/mssql_repo.rs";
/// The MSSQL half of `SEA_MOD`, gated for the same reason: with `mssql` off
/// these two files stay on disk and stop compiling, so undoing the capability is
/// one line in Cargo.toml plus `bootstrap.rs` — never a deletion.
const MSSQL_MOD: &str = "#[cfg(feature = \"mssql\")]\npub mod mssql_db;\n#[cfg(feature = \"mssql\")]\npub mod mssql_repo;\n";
/// `DATABASE_URL` is not a URL here: `tiberius::Config` parses an **ADO.NET**
/// connection string and nothing else (`from_jdbc_string` exists too, but one
/// shape is enough and this is the one Azure portal, SSMS and every Microsoft
/// document print). The variable keeps its name because it is the same switch for
/// the same slot — a `postgres://` left over from another backend gets a parse
/// error on the first request, which is loud rather than silent.
///
/// `TrustServerCertificate=true` is **dev only**: the image serves a self-signed
/// certificate. Against a real server (Azure SQL, or anything with a CA-issued
/// cert) take it out — see the `SSL_CERT_FILE` note in the Dockerfile.
const MSSQL_URL: &str = "server=tcp:127.0.0.1,1433;user id=sa;password=kodeGen!2026;database=app;encrypt=true;TrustServerCertificate=true";
const PORTS: &str = "src/domain/ports.rs";
const IN_MEMORY_CACHE: &str = "src/infrastructure/in_memory_cache.rs";
const REDIS_CACHE: &str = "src/infrastructure/redis_cache.rs";
const CACHE_TEST: &str = "tests/cache.rs";
const RESOURCES_MARKER: &str = "// kodegen:resources";
const ASYNC_TRAIT_USE: &str = "use async_trait::async_trait;";

/// Default port, and `127.0.0.1` for the same reason the database URLs use it:
/// a literal address is the one thing that behaves identically on every host.
const REDIS_URL: &str = "redis://127.0.0.1:6379";
const PG_URL: &str = "postgres://app:app@127.0.0.1:5432/app";
const MYSQL_URL: &str = "mysql://app:app@127.0.0.1:3306/app";

const FS_BLOBS: &str = "src/infrastructure/fs_blobs.rs";
const S3_BLOBS: &str = "src/infrastructure/s3_blobs.rs";
const BLOB_TEST: &str = "tests/blob.rs";

const API_MOD: &str = "src/api/mod.rs";
const USER_API: &str = "src/api/user.rs";
const HELLO_API: &str = "src/api/hello.rs";
const DOMAIN_MODEL: &str = "src/domain/model.rs";
const OPENAPI_RS: &str = "src/api/openapi.rs";
const APIDOCS_TEST: &str = "tests/apidocs.rs";
/// Where a REST `kodegen feature` inserts its own two paths (`add_feature` in
/// scaffold.rs), and the line `OPENAPI_TPL` seeds `openapi.rs` with so there is
/// always somewhere to insert them. One real constant rather than the same
/// string literal in two files, so a typo in either place is a compile error
/// instead of a silently-missed anchor.
pub(crate) const OPENAPI_PATHS_MARKER: &str = "// kodegen:openapi:paths";

const LOG_EVENTS: &str = "src/infrastructure/log_events.rs";
const CLICKHOUSE_EVENTS: &str = "src/infrastructure/clickhouse_events.rs";
const LINE_PROTOCOL_EVENTS: &str = "src/infrastructure/line_protocol_events.rs";
const EVENTS_TEST: &str = "tests/events.rs";

/// ClickHouse's HTTP port, and `127.0.0.1` for the same reason as everywhere
/// else here. `CLICKHOUSE_DB` has a default (`default`, the server's own) in the
/// generated builder, so the URL is the switch — the other three are what the
/// generated `docker-compose.dev.yml` boots with.
const CLICKHOUSE_ENV: &[(&str, &str)] = &[
    // first, and the one the composition root gates on: no URL, no sink
    ("CLICKHOUSE_URL", "http://127.0.0.1:8123"),
    ("CLICKHOUSE_DB", "app"),
    // the server images have required a password for the `default` user since
    // 25.x, so the dev service creates a user instead of running open
    ("CLICKHOUSE_USER", "app"),
    ("CLICKHOUSE_PASSWORD", "app"),
];

/// What the generated `docker-compose.dev.yml` boots. `INFLUX_URL` is the base
/// address, not the write endpoint — the composition root appends
/// `/api/v2/write`, which is what lets the same three variables address InfluxDB
/// v2, InfluxDB v3 and GreptimeDB (see the README).
const INFLUX_ENV: &[(&str, &str)] = &[
    // first, and the one the composition root gates on: no URL, no sink
    ("INFLUX_URL", "http://127.0.0.1:8086"),
    ("INFLUX_BUCKET", "app"),
    ("INFLUX_ORG", "app"),
    ("INFLUX_TOKEN", "kodegen-dev-token"),
];

/// GreptimeDB's version of the above: the same three-variable shape with the
/// compat prefix in the URL, and two variables rather than four because the
/// other two have nothing to select.
///
/// - `/v1/influxdb` is where the line-protocol endpoint lives, so it is part of
///   the *base* address — which is exactly why `INFLUX_URL` is a base and not a
///   write URL.
/// - `bucket=` names the database (verified against a real server, not only the
///   docs), and it has to be one that already exists: Greptime creates a *table*
///   on write but never a database, so `app` would answer `400 Failed to find
///   schema`. `public` is the one every standalone server boots with.
/// - no token: standalone runs with authentication off. Turn it on and the value
///   is `<user>:<password>`, not a bare token (see the README).
const GREPTIME_ENV: &[(&str, &str)] = &[
    // first, and the one the composition root gates on: no URL, no sink
    ("INFLUX_URL", "http://127.0.0.1:4000/v1/influxdb"),
    ("INFLUX_BUCKET", "public"),
];

/// What the generated `docker-compose.dev.yml` boots, and the credentials it
/// boots it with — dev-only defaults, printed together because an S3 client with
/// three of the four is not a client.
const S3_ENV: &[(&str, &str)] = &[
    // first, and the one the composition root gates on: no bucket, no S3
    ("S3_BUCKET", "app"),
    ("S3_ENDPOINT", "http://127.0.0.1:9000"),
    ("AWS_ACCESS_KEY_ID", "rustfsadmin"),
    ("AWS_SECRET_ACCESS_KEY", "rustfsadmin"),
];

impl Capability {
    pub fn name(self) -> &'static str {
        match self {
            Capability::Postgres => "postgres",
            Capability::Mysql => "mysql",
            Capability::Mariadb => "mariadb",
            Capability::Timescaledb => "timescaledb",
            Capability::Mongodb => "mongodb",
            Capability::Mssql => "mssql",
            Capability::Redis => "redis",
            Capability::S3 => "s3",
            Capability::Clickhouse => "clickhouse",
            Capability::Influxdb => "influxdb",
            Capability::Greptimedb => "greptimedb",
            Capability::Scalar => "scalar",
        }
    }

    /// What kind of thing this capability is. Every decision that varies by kind
    /// goes through this, so adding a capability is a compile error in `kind()`
    /// and adding a *kind* is a compile error at each of the three matches on
    /// it — rather than a new capability quietly taking the redis path.
    fn kind(self) -> Kind {
        match self {
            Capability::Postgres => Kind::Relational(Relational::Postgres),
            Capability::Mysql => Kind::Relational(Relational::Mysql),
            Capability::Mariadb => Kind::Relational(Relational::Mariadb),
            // a PostgreSQL extension, so it is the same driver, the same
            // connection type and the same cargo feature — what differs is the
            // image and one migration
            Capability::Timescaledb => Kind::Relational(Relational::Timescaledb),
            Capability::Mongodb => Kind::Document,
            Capability::Mssql => Kind::Mssql,
            Capability::Redis => Kind::Cache,
            Capability::S3 => Kind::Blob,
            Capability::Clickhouse => Kind::Events(EventBackend::Clickhouse),
            Capability::Influxdb => Kind::Events(EventBackend::Influxdb),
            // the same line protocol on a different path, so it is the same
            // adapter, the same cargo feature and the same generated code —
            // what differs is the image and the two variables (see the README),
            // exactly the relationship mariadb has with mysql
            Capability::Greptimedb => Kind::Events(EventBackend::Greptimedb),
            Capability::Scalar => Kind::ApiDocs,
        }
    }

    /// Whether `--orm` means anything here. Only the relational capabilities
    /// have an ORM to choose; everything else is refused rather than ignored.
    pub fn takes_orm(self) -> bool {
        matches!(self.kind(), Kind::Relational(_))
    }

    /// The cargo feature this capability turns on — also how "is it already
    /// added?" is answered, since `default = [...]` is the only state there is.
    ///
    /// The ORM is part of the answer for a relational capability and irrelevant
    /// to every other one, which is why it is a parameter rather than a second
    /// method: `add`, `check_combination` and the slot rule all ask this one
    /// question, and they must all get the ORM's answer.
    fn feature(self, orm: Orm) -> &'static str {
        match self.kind() {
            Kind::Relational(rel) => rel.feature(orm),
            // named for the shape, not the vendor, and deliberately not
            // "mongodb": it is the datastore slot's document option, the way
            // `sqlite`/`postgres` are its relational ones
            Kind::Document => "document",
            // named for the vendor, unlike its two slot-mates, because there is
            // one: `mssql` is SQL Server, Azure SQL Database and Azure SQL MI,
            // which are one server behind one driver
            Kind::Mssql => "mssql",
            // Redis and Valkey speak the same protocol, so one feature covers
            // both: the capability is the cache, not the vendor
            Kind::Cache => "cache",
            // RustFS, MinIO, Ceph and AWS are one adapter over one API: the
            // capability is object storage, not the vendor
            Kind::Blob => "blob",
            Kind::Events(backend) => backend.feature(),
            // the compile-time kill switch: off, `cfg_attr(feature = "apidocs",
            // ...)` disappears from every handler `kodegen feature` writes and
            // neither utoipa crate is even compiled — see the README
            Kind::ApiDocs => "apidocs",
        }
    }

    /// How to bring this capability's dev services up, as lines to print after
    /// `add`. One line for everything except SQL Server, whose compose file has a
    /// second, one-shot service that creates the database — and `--wait` over the
    /// whole file exits **1** when that service finishes, because a container
    /// that is no longer running reads to it as a failed dependency. Naming the
    /// long-running service and then running the init one is the form that
    /// reports what actually happened, and it is what the README and CI use.
    pub fn compose_up(self) -> &'static [&'static str] {
        match self.kind() {
            Kind::Mssql => &[
                "docker compose -f docker-compose.dev.yml up -d --wait mssql",
                "docker compose -f docker-compose.dev.yml run --rm mssql-init",
            ],
            _ => &["docker compose -f docker-compose.dev.yml up -d"],
        }
    }

    /// The env vars that select this capability's adapter, and values matching
    /// what the generated `docker-compose.dev.yml` boots. The **first** one is
    /// the switch: leave it unset and the default, server-less adapter is what
    /// runs.
    pub fn env(self) -> &'static [(&'static str, &'static str)] {
        match self.kind() {
            Kind::Relational(rel) => rel.database_env(),
            // `MONGODB_DB` only to say what the default is: the builder falls
            // back to `app`, so the URL alone is enough to switch adapters
            Kind::Document => &[("MONGODB_URL", MONGO_URL), ("MONGODB_DB", "app")],
            // the same variable the relational capabilities use, because it is
            // the same slot's switch — the *value* is an ADO.NET string, not a
            // URL (see `MSSQL_URL`)
            Kind::Mssql => &[("DATABASE_URL", MSSQL_URL)],
            Kind::Cache => &[("REDIS_URL", REDIS_URL)],
            Kind::Blob => S3_ENV,
            Kind::Events(backend) => backend.env(),
            // no server, so no switch: `/docs` and `/openapi.json` are on
            // whenever `apidocs` is compiled in, which `main.rs` has to treat
            // as "nothing to print an env line or a compose_up for" rather
            // than index `env[0]` into a panic
            Kind::ApiDocs => &[],
        }
    }
}

impl EventBackend {
    /// Named for the shape rather than the vendor, like `cache` and `blob`:
    /// `timeseries` is InfluxDB v2, InfluxDB v3 and GreptimeDB, which are one
    /// adapter because they take the same line protocol on the same kind of
    /// endpoint.
    fn feature(self) -> &'static str {
        match self {
            EventBackend::Clickhouse => "analytics",
            EventBackend::Influxdb | EventBackend::Greptimedb => "timeseries",
        }
    }

    fn env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            EventBackend::Clickhouse => CLICKHOUSE_ENV,
            EventBackend::Influxdb => INFLUX_ENV,
            EventBackend::Greptimedb => GREPTIME_ENV,
        }
    }

    /// The adapter this backend writes, next to the log sink both of them share.
    fn adapter(self) -> (&'static str, &'static str) {
        match self {
            EventBackend::Clickhouse => (CLICKHOUSE_EVENTS, CLICKHOUSE_EVENTS_TPL),
            EventBackend::Influxdb | EventBackend::Greptimedb => {
                (LINE_PROTOCOL_EVENTS, LINE_PROTOCOL_EVENTS_TPL)
            }
        }
    }

    fn module(self) -> &'static str {
        match self {
            EventBackend::Clickhouse => "clickhouse_events",
            EventBackend::Influxdb | EventBackend::Greptimedb => "line_protocol_events",
        }
    }

    /// The `kodegen add` name that got here — which is not `module()`: two
    /// capabilities share that file, and neither of them is a command anyone can
    /// type. An error message that names a command must name a real one.
    fn capability_name(self) -> &'static str {
        match self {
            EventBackend::Clickhouse => "clickhouse",
            EventBackend::Influxdb => "influxdb",
            EventBackend::Greptimedb => "greptimedb",
        }
    }

    /// The name of the resource the composition root resolves — the binding the
    /// two `api::Services` literals hand to `build_event_sink`.
    fn resource_name(self) -> &'static str {
        match self {
            EventBackend::Clickhouse => "clickhouse",
            EventBackend::Influxdb | EventBackend::Greptimedb => "influx",
        }
    }

    /// Inserted above `// kodegen:resources`: reads this backend's env once, and
    /// hands the adapter something already built — an adapter never reads env.
    fn resource(self) -> &'static str {
        match self {
            EventBackend::Clickhouse => CLICKHOUSE_RESOURCE,
            EventBackend::Influxdb | EventBackend::Greptimedb => INFLUX_RESOURCE,
        }
    }

    /// Appended to `bootstrap.rs`: the same env-picks-the-adapter shape as
    /// `build_cache`, with the log sink as the default.
    fn builder(self) -> &'static str {
        match self {
            EventBackend::Clickhouse => BUILD_EVENT_SINK_CLICKHOUSE,
            EventBackend::Influxdb | EventBackend::Greptimedb => BUILD_EVENT_SINK_INFLUX,
        }
    }

    /// The `tests/events.rs` half that needs a server, filled into the shared
    /// contract test.
    fn server_test(self) -> &'static str {
        match self {
            EventBackend::Clickhouse => CLICKHOUSE_TEST,
            EventBackend::Influxdb | EventBackend::Greptimedb => INFLUX_TEST,
        }
    }

    /// The compose service name (the image's own name) and its block.
    fn compose_service(self) -> (&'static str, &'static str) {
        match self {
            EventBackend::Clickhouse => ("clickhouse", CLICKHOUSE_COMPOSE),
            EventBackend::Influxdb => ("influxdb", INFLUX_COMPOSE),
            EventBackend::Greptimedb => ("greptimedb", GREPTIME_COMPOSE),
        }
    }
}

impl Relational {
    fn name(self) -> &'static str {
        match self {
            Relational::Postgres => "postgres",
            Relational::Mysql => "mysql",
            Relational::Mariadb => "mariadb",
            Relational::Timescaledb => "timescaledb",
        }
    }

    /// MariaDB speaks the MySQL wire protocol, so it is the same driver, the
    /// same adapter and the same cargo feature — only the image and the docs
    /// differ. TimescaleDB is a PostgreSQL *extension*, so the same is true of it
    /// one column over: same driver, same connection type, same feature, and one
    /// extra migration.
    ///
    /// The SeaORM names mirror them one axis over — same backend, same slot,
    /// different ORM — which is what makes "one datastore per project" cover
    /// "one ORM per project" without a rule of its own.
    fn feature(self, orm: Orm) -> &'static str {
        match (orm, self) {
            (Orm::Diesel, Relational::Postgres | Relational::Timescaledb) => "postgres",
            (Orm::Diesel, Relational::Mysql | Relational::Mariadb) => "mysql",
            (Orm::Seaorm, Relational::Postgres | Relational::Timescaledb) => "seaorm-postgres",
            (Orm::Seaorm, Relational::Mysql | Relational::Mariadb) => "seaorm-mysql",
        }
    }

    fn dialect(self) -> &'static str {
        match self {
            Relational::Postgres => "PostgreSQL",
            Relational::Mysql => "MySQL",
            Relational::Mariadb => "MariaDB",
            Relational::Timescaledb => "TimescaleDB",
        }
    }

    fn connection(self) -> &'static str {
        match self {
            Relational::Postgres | Relational::Timescaledb => "PgConnection",
            Relational::Mysql | Relational::Mariadb => "MysqlConnection",
        }
    }

    /// 127.0.0.1, never `localhost`: libmysqlclient reads `localhost` as "use
    /// the unix socket" and ignores the port entirely, so a compose service
    /// published on 3306 fails with a socket error naming a path nobody
    /// configured. The literal address forces TCP on every backend.
    fn database_url(self) -> &'static str {
        match self {
            Relational::Postgres | Relational::Timescaledb => PG_URL,
            Relational::Mysql | Relational::Mariadb => MYSQL_URL,
        }
    }

    fn database_env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Relational::Postgres | Relational::Timescaledb => &[("DATABASE_URL", PG_URL)],
            Relational::Mysql | Relational::Mariadb => &[("DATABASE_URL", MYSQL_URL)],
        }
    }

    /// MySQL cannot index a `TEXT` column without a prefix length, so
    /// `TEXT PRIMARY KEY` is rejected at migration time — hence `VARCHAR(64)`,
    /// which matches the 64-char id limit the domain already enforces.
    fn up_sql(self) -> &'static str {
        match self {
            Relational::Postgres => "CREATE TABLE users (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL);\n",
            Relational::Mysql | Relational::Mariadb => {
                "CREATE TABLE users (id VARCHAR(64) PRIMARY KEY NOT NULL, name TEXT NOT NULL);\n"
            }
            Relational::Timescaledb => TIMESCALE_UP_SQL,
        }
    }

    /// Only TimescaleDB has one of its own: it is the one backend whose `up.sql`
    /// creates a second table, and a `down.sql` that does not drop it leaves a
    /// revert-then-migrate loop failing on `CREATE TABLE events`.
    fn down_sql(self) -> &'static str {
        match self {
            Relational::Postgres | Relational::Mysql | Relational::Mariadb => USERS_DOWN_SQL,
            Relational::Timescaledb => TIMESCALE_DOWN_SQL,
        }
    }

    /// What the SeaORM path applies on its first connection, in place of the
    /// versioned migration the Diesel path runs at startup (see `add_seaorm`).
    ///
    /// Deliberately **not** `up_sql()` with `IF NOT EXISTS` bolted on: that one
    /// is a versioned migration pinned by `KNOWN_SEEDS`, and hiding a conflict
    /// inside it would hide a bug. These are three separate constants that owe
    /// the other path nothing.
    ///
    /// One statement per entry rather than one string with semicolons: a
    /// multi-statement round trip is not enabled by default on MySQL, and one
    /// statement at a time is what behaves the same on every backend.
    fn startup_ddl(self) -> &'static [&'static str] {
        match self {
            Relational::Postgres => &[USERS_DDL_PG],
            Relational::Mysql | Relational::Mariadb => &[USERS_DDL_MYSQL],
            // every statement idempotent, including the hypertable: a
            // `create_hypertable` without `if_not_exists` fails on the *second*
            // boot, not the first, which is the kind of bug that ships
            Relational::Timescaledb => &[
                "CREATE EXTENSION IF NOT EXISTS timescaledb",
                USERS_DDL_PG,
                "CREATE TABLE IF NOT EXISTS events (recorded_at TIMESTAMPTZ NOT NULL, name TEXT NOT NULL, value DOUBLE PRECISION NOT NULL)",
                "SELECT create_hypertable('events', by_range('recorded_at'), if_not_exists => TRUE)",
            ],
        }
    }

    /// `startup_ddl()` as the body of the generated `SCHEMA` array. `{:?}` is
    /// the escaping: these are Rust string literals, and the SQL above contains
    /// the single quotes `create_hypertable` needs.
    fn schema_literal(self) -> String {
        self.startup_ddl()
            .iter()
            .map(|stmt| format!("    {stmt:?},\n"))
            .collect()
    }

    /// What `.on_conflict(...)` takes. Both dialects get a real upsert, so
    /// `save`'s documented "saving an existing id overwrites it" means the same
    /// thing whichever backend is on.
    fn conflict_target(self) -> &'static str {
        match self {
            Relational::Postgres | Relational::Timescaledb => "users::id",
            Relational::Mysql | Relational::Mariadb => "diesel::dsl::DuplicatedKeys",
        }
    }

    fn upsert_note(self) -> &'static str {
        match self {
            Relational::Postgres | Relational::Timescaledb => "// upsert: `save` is documented as one, so an existing id is updated",
            // the trapdoor this avoids stays invisible until something
            // references users(id) — exactly when it is expensive to discover
            Relational::Mysql | Relational::Mariadb => {
                "// ON DUPLICATE KEY UPDATE, not REPLACE INTO: MySQL's REPLACE is a\n                \
                 // DELETE followed by an INSERT, so it fires ON DELETE CASCADE on\n                \
                 // every row referencing this one. An upsert must not delete."
            }
        }
    }

    /// Extra Debian packages the build stage needs: both bundled backends build
    /// their C driver from source, and `openssl-sys{vendored}` needs perl.
    fn apt_packages(self) -> &'static str {
        match self {
            Relational::Postgres | Relational::Timescaledb => "perl make",
            // mysqlclient-src drives its build with cmake
            Relational::Mysql | Relational::Mariadb => "perl make cmake",
        }
    }

    fn compose_service(self) -> &'static str {
        match self {
            Relational::Postgres => POSTGRES_COMPOSE,
            Relational::Mysql => MYSQL_COMPOSE,
            Relational::Mariadb => MARIADB_COMPOSE,
            Relational::Timescaledb => TIMESCALE_COMPOSE,
        }
    }

    fn render(self, tpl: &str) -> String {
        tpl.replace("{{Conn}}", self.connection())
            .replace("{{Dialect}}", self.dialect())
            .replace("{{capability}}", self.name())
            .replace("{{conflict_target}}", self.conflict_target())
            .replace("{{upsert_note}}", self.upsert_note())
            .replace("{{schema}}", &self.schema_literal())
    }
}

/// `kodegen add --list`: what is supported, and what this project already has on.
pub fn list(project: &Path) -> Result<()> {
    println!("Supported capabilities:");
    println!("  postgres     relational — PostgreSQL (--orm diesel [default] | seaorm)");
    println!("  mysql        relational — MySQL (--orm diesel [default] | seaorm)");
    println!(
        "  mariadb      relational — MariaDB (MySQL protocol: same driver, same adapter) (--orm \
         diesel [default] | seaorm)"
    );
    println!("  timescaledb  relational — TimescaleDB (a PostgreSQL extension: same driver, plus a hypertable) (--orm diesel [default] | seaorm)");
    println!("  mongodb      document — MongoDB behind the same `UserRepository`, in place of Diesel");
    println!(
        "  mssql        relational — SQL Server / Azure SQL, raw SQL over tiberius (no ORM: \
         --orm is an error here)"
    );
    println!("  redis        cache — the `Cache` port + a Redis adapter (Valkey speaks the same protocol)");
    println!("  s3           object storage — the `BlobStore` port + an S3 adapter (RustFS, MinIO, AWS)");
    println!("  clickhouse   event sink — the `EventSink` port + a ClickHouse adapter");
    println!("  influxdb     event sink — the `EventSink` port + a line-protocol adapter (InfluxDB v2/v3)");
    println!("  greptimedb   event sink — the same line-protocol adapter, GreptimeDB image + docs");
    println!(
        "  scalar       API docs — GET /docs (Scalar) + GET /openapi.json, derived from the code \
         (no --orm, no slot, sits next to anything)"
    );
    println!();
    println!(
        "Only one datastore (postgres, mysql, mariadb, timescaledb, mongodb, mssql) and one event \
         sink (clickhouse, influxdb, greptimedb) can be on at a time; redis and s3 sit alongside \
         them."
    );
    println!(
        "Relational capabilities take --orm: `diesel` links a C driver and gives you versioned \
         migrations under migrations/; `seaorm` is pure Rust (no perl/cmake in the build image) \
         and applies its schema at startup instead, so there are no versioned migrations on that \
         path. One ORM per project. `mssql` takes neither — see below."
    );
    println!();
    // said here, before the choice, rather than in a file read afterwards: this
    // is the one capability whose driver is not actively developed, and a user
    // is entitled to know that while picking rather than while debugging
    println!("`mssql` is the thinnest capability in this list, and deliberately so:");
    println!(
        "  - tiberius 0.12.3 was released 2024-07 and has had no release since. It is also the \
         only usable TDS implementation in Rust — there is no second option to fall back to, and \
         no ORM on either side of it (diesel#2201 open since 2019; SeaORM's SQL Server backend is \
         in the commercial SeaORM X). So `add mssql` writes raw SQL, and the build image gets \
         perl/make back for vendored OpenSSL."
    );
    println!(
        "  - It is watched, not assumed: `cargo audit` runs over the template's lock in CI on \
         every change, and the generated project ships the same job."
    );
    println!(
        "  - The withdrawal trigger is stated up front: a RUSTSEC advisory on tiberius, or on a \
         dependency it pins that a lock bump cannot move off, deprecates this capability in the \
         next release. It does not get an `ignore` entry in .cargo/audit.toml."
    );
    match fs::read_to_string(project.join("Cargo.toml")).ok().as_deref().and_then(default_line) {
        // `default = [...]` is the only state there is — the features it lists
        // are exactly the dependencies that get compiled
        Some(line) => println!("Enabled in {}: {}", project.display(), parse_features(line).join(", ")),
        None => println!("No [features] default found in {} — not a generated project?", project.display()),
    }
    Ok(())
}

/// `--orm` on a capability that has no ORM at all — refused *before* anything is
/// read, because an ignored flag is how a project ends up on the wrong ORM
/// silently.
///
/// Every relational backend answers to both ORMs now that MySQL/MariaDB have
/// shipped on SeaORM, so there is nothing left to refuse per backend. A third
/// ORM would change that: it would arrive supporting some subset, and this is
/// where that subset gets written down.
///
/// Three arms rather than two, because `mssql` is the case the general sentence
/// gets wrong: it *is* a relational capability, so "--orm applies only to
/// relational capabilities" would read as a contradiction to somebody who just
/// typed it. What is missing there is an ORM to choose, not relationality, and
/// the message has to say which.
fn check_orm(cap: Capability, orm: Option<Orm>) -> Result<()> {
    if orm.is_none() {
        return Ok(());
    }
    match cap.kind() {
        Kind::Relational(_) => Ok(()),
        Kind::Mssql => Err(
            "--orm does not apply to 'mssql': neither Diesel nor SeaORM supports SQL Server in \
             open source (diesel#2201 has been open since 2019, and SeaORM's SQL Server backend is \
             in the commercial SeaORM X), so `kodegen add mssql` writes raw SQL over tiberius. \
             That is the trade — see `kodegen add --list`."
                .into(),
        ),
        Kind::Document | Kind::Cache | Kind::Blob | Kind::Events(_) | Kind::ApiDocs => Err(format!(
            "--orm applies only to relational capabilities (postgres, mysql, mariadb, timescaledb) \
             — '{}' has no ORM",
            cap.name()
        )
        .into()),
    }
}

/// Rejects a `--with` list that cannot be applied, *before* `new` renders
/// anything — half a project left on disk is worse than no project.
pub fn check_combination(with: &[Capability], orm: Option<Orm>) -> Result<()> {
    // `--orm` on `new` selects the ORM of the one relational entry in the list
    // (the slot rule below guarantees there is at most one), so a list with none
    // is a flag that would do nothing at all
    if orm.is_some() && !with.iter().any(|c| c.takes_orm()) {
        // ...but if the reason there is no ORM to pick is that the list holds
        // `mssql`, that capability has a better answer than this one: "nothing
        // here has an ORM" is true and useless next to a SQL database the user
        // just named. `check_orm` is where that sentence lives, so ask it rather
        // than copying it.
        if let Some(cap) = with.iter().find(|c| matches!(c.kind(), Kind::Mssql)) {
            check_orm(*cap, orm)?;
        }
        return Err(
            "--orm needs a relational capability in --with (postgres, mysql, mariadb, \
                    timescaledb) — nothing in this list has an ORM"
                .into(),
        );
    }
    // no per-entry `check_orm` here: the only thing it refuses is `--orm` on a
    // capability that has no ORM, and on this path the flag belongs to the
    // relational entry — the check above is what makes sure there is one
    // by feature, not by variant: `--with mysql,mariadb` is two names for one
    // cargo feature, and the second `add` would die with "already added" halfway
    // through a `new` that has already written the project
    let feature = |c: &Capability| c.feature(orm.filter(|_| c.takes_orm()).unwrap_or(Orm::Diesel));
    for (i, cap) in with.iter().enumerate() {
        if let Some(first) = with[..i]
            .iter()
            .find(|earlier| feature(earlier) == feature(cap))
        {
            return Err(format!(
                "--with lists '{}' and '{}', which are the same capability (both enable the `{}` \
                 cargo feature)",
                first.name(),
                cap.name(),
                feature(cap)
            )
            .into());
        }
    }
    for slot in Slot::ALL {
        let taken: Vec<&str> =
            with.iter().filter(|c| c.kind().slot() == Some(slot)).map(|c| c.name()).collect();
        if taken.len() > 1 {
            return Err(format!(
                "--with takes at most one {}: {} all fill that slot, and a project can only have one \
                 (they are adapters of the same port, and the handle behind it is built once)",
                slot.what(),
                taken.join(", ")
            )
            .into());
        }
    }
    Ok(())
}

/// Returns the plan as (files created, files edited) for the caller to report.
///
/// `orm` is `None` when the flag was not given, which is not the same as
/// `Some(Orm::Diesel)`: only the first of those may appear next to `redis`.
pub fn add(
    project: &Path,
    cap: Capability,
    orm: Option<Orm>,
    dry_run: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    // before any file is read, including the manifest: a flag that does not
    // apply is a mistake about the command, not about the project
    check_orm(cap, orm)?;
    let orm = orm.unwrap_or(Orm::Diesel);
    require_files(project, &["Cargo.toml"])?;
    let manifest = fs::read_to_string(project.join("Cargo.toml"))?;
    let enabled = parse_features(default_line(&manifest).ok_or_else(|| default_anchor_err(&manifest))?);
    if enabled.iter().any(|f| f == cap.feature(orm)) {
        return Err(format!("capability '{}' already added", cap.name()).into());
    }
    // one occupant per slot, checked before any file is read: `add mongodb` on a
    // Postgres project and `add postgres` on a Mongo one are the same mistake, so
    // they get the same answer — and so do `add influxdb` on a ClickHouse project
    // and the reverse
    if let Some(slot) = cap.kind().slot()
        && let Some(other) = enabled
            .iter()
            .find(|f| slot.features().contains(&f.as_str()) && Some(f.as_str()) != slot.shipped())
    {
        return Err(slot_conflict(slot, other, cap, orm).into());
    }
    // on the pair, not a guard on one of them: a third ORM must be a missing
    // arm here — a `Kind::Relational(rel) if orm == Seaorm` guard would send it
    // down the Diesel path without a word
    match (cap.kind(), orm) {
        (Kind::Relational(rel), Orm::Seaorm) => add_seaorm(project, rel, dry_run),
        (Kind::Relational(rel), Orm::Diesel) => add_relational(project, rel, dry_run),
        (Kind::Document, _) => add_mongodb(project, dry_run),
        // `_` on the ORM and not a missed case: `check_orm` above turned every
        // `Some(_)` into an error before a file was read, so the only value that
        // reaches here is the `Orm::Diesel` `add` defaults to when the flag was
        // absent — and this path never asks
        (Kind::Mssql, _) => add_mssql(project, dry_run),
        (Kind::Cache, _) => add_redis(project, dry_run),
        (Kind::Blob, _) => add_s3(project, dry_run),
        (Kind::Events(backend), _) => add_event_sink(project, backend, dry_run),
        (Kind::ApiDocs, _) => add_scalar(project, dry_run),
    }
}

/// A slot is taken and is being given a second occupant. Refused rather than
/// half-applied: the handle behind the port is built once in the composition root
/// and the port gets one adapter, so a second would need a second handle, a
/// second adapter and a rule for which one wins — out of scope (see the plan's
/// "not doing" list), and the undo path is three steps the user can run.
///
/// Only the middle step differs by slot, so only it is matched on: what to put
/// back is a property of what the first `add` wrote.
fn slot_conflict(slot: Slot, other: &str, cap: Capability, orm: Orm) -> String {
    match slot {
        Slot::Datastore => datastore_conflict(other, cap, orm),
        // an event sink is additive, so switching backends is undoing the whole
        // capability rather than swapping a file: the second `add` writes the port
        // and the wiring too, and would refuse every one of them as already there
        Slot::Events => format!(
            "the `{other}` event sink ({}) already fills this project's `EventSink`, and there can \
             only be one (the port has a single adapter, and the client behind it is built once \
             in bootstrap.rs).\n\
             To switch to '{}', undo '{other}' first — every file below was written by that one \
             command, so `git revert` of its commit does all of it:\n  \
             1. Cargo.toml: drop `\"{other}\"` from `default = [...]`\n  \
             2. delete src/infrastructure/{}.rs, src/infrastructure/log_events.rs and \
             tests/events.rs, and their two `pub mod` lines in src/infrastructure/mod.rs\n  \
             3. drop the `EventSink`/`Event`/`check_event` block from src/domain/ports.rs and its \
             `pub use` from src/domain/mod.rs, `pub events:` from src/api/mod.rs, and — in \
             src/bootstrap.rs — the resource block above `// kodegen:resources`, both \
             `events: build_event_sink(...)` lines and `fn build_event_sink`\n  \
             4. rerun `kodegen add {}`",
            match other {
                "analytics" => "kodegen add clickhouse",
                _ => "kodegen add influxdb / greptimedb",
            },
            cap.name(),
            match other {
                "analytics" => EventBackend::Clickhouse.module(),
                _ => EventBackend::Influxdb.module(),
            },
            cap.name()
        ),
    }
}

/// The datastore slot's version of the above: `Db`/`Mongo` is built once and
/// `UserRepository` gets one adapter.
/// Three occupants, three restores — but only two shapes: mongodb and SeaORM
/// both took the slot by *swapping blocks* in `bootstrap.rs` and leaving their
/// adapter on disk behind a `cfg`, so they share the sentence and differ in what
/// to name. A Diesel occupant is the odd one out because it is the state the
/// template ships in, so its restore is a file copy.
fn datastore_conflict(other: &str, cap: Capability, orm: Orm) -> String {
    // the blocks are named individually because a project with scaffolded
    // features cannot take a wholesale copy of a fresh bootstrap.rs: that would
    // drop their wiring
    let blocks = |adapters: &str, builders: &str, feature: &str| {
        format!(
            "src/bootstrap.rs: put the three Diesel blocks back from a fresh `kodegen new` (the \
             infrastructure import list, the DATABASE_URL block, and fn build_user_service){builders}. \
             {adapters} can stay — gated on the `{feature}` feature step 1 just turned off"
        )
    };
    let restore = match other {
        "document" => blocks(
            "src/infrastructure/mongo_repo.rs",
            ", and change any `_db: Option<&Mongo>` in a scaffolded builder back to `Option<&Db>`",
            "document",
        ),
        // both ORMs call the handle `Db`, so a scaffolded builder needs no edit
        // in either direction — the only thing to put back is the three blocks
        o if o.starts_with("seaorm-") => blocks(
            "src/infrastructure/sea_db.rs + sea_repo.rs",
            " — scaffolded builders need no change, since both ORMs call the handle `Db`",
            "seaorm",
        ),
        // and the third occupant that calls its handle `Db` for the same reason
        "mssql" => blocks(
            "src/infrastructure/mssql_db.rs + mssql_repo.rs",
            " — scaffolded builders need no change, since the SQL Server handle is called `Db` as \
             well",
            "mssql",
        ),
        _ => "src/infrastructure/diesel_db.rs + diesel_repo.rs: restore the versions from a fresh \
              `kodegen new`"
            .to_string(),
    };
    // the command to rerun has to be the one that was actually typed, or the
    // last step of the undo puts the project on the other ORM
    let (flag, orm_name) = match orm {
        Orm::Diesel => ("", "diesel"),
        Orm::Seaorm => (" --orm seaorm", "seaorm"),
    };
    // an ORM switch is a conflict between two spellings of one backend, and
    // "'postgres' … to switch to 'postgres'" reads like a no-op — so each side
    // says which ORM it is. `other` is a cargo feature: the `seaorm-` ones are
    // the second ORM, the rest are Diesel (`document` has no ORM, and `sqlite`
    // never reaches here — it is the slot's shipped occupant).
    let occupant = match other.strip_prefix("seaorm-") {
        Some(backend) => format!("{backend} (seaorm)"),
        // neither of these has an ORM, so neither takes the "(diesel)" suffix
        // that tells two spellings of one backend apart
        None if other == "document" || other == "mssql" => other.to_string(),
        None => format!("{other} (diesel)"),
    };
    let target = match cap.takes_orm() {
        true => format!("{} ({orm_name})", cap.name()),
        false => cap.name().to_string(),
    };
    format!(
        "capability '{occupant}' is already this project's datastore, and there can only be one \
         (the `UserRepository` has a single adapter, and the handle behind it is built once in \
         bootstrap.rs).\n\
         To switch to '{target}', undo '{occupant}' first:\n  \
         1. Cargo.toml: put `default = [\"sqlite\"]` back\n  \
         2. {restore}\n  \
         3. rerun `kodegen add {}{flag}`",
        cap.name()
    )
}

/// Switches the project onto `rel`. Not additive: `diesel_db.rs` is written
/// against one connection type from top to bottom, so it is replaced.
fn add_relational(
    project: &Path,
    cap: Relational,
    dry_run: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    require_files(project, &["Dockerfile", DIESEL_DB, DIESEL_REPO])?;
    let pkg = crate::package_name(project)
        .ok_or("Cargo.toml with a package name not found — run inside a generated project")?;

    let mut creates: Vec<(String, String)> = Vec::new();
    let mut edits = vec![
        // the whole point of the optional-dependency layout: `add` flips one
        // list of feature names and never touches [dependencies], so Cargo.lock
        // stays valid and `--locked` keeps working
        edit(project, "Cargo.toml", |c| {
            enable_feature(&c, cap.feature(Orm::Diesel), Some(Slot::Datastore))
        })?,
        // whole-file comparison rather than a marker line: once the project name
        // is substituted back this file has no project-specific content, so
        // "identical to what `kodegen new` wrote" is both the strictest check
        // available and the shortest one to write
        edit(project, DIESEL_DB, |c| {
            if c != generated(DIESEL_DB, &pkg)? {
                return Err(refusal(DIESEL_DB, "the file exactly as `kodegen new` wrote it", cap).into());
            }
            Ok(cap.render(DIESEL_DB_TPL))
        })?,
        edit(project, DIESEL_REPO, |c| rewrite_repo(&c, cap, &pkg))?,
        // the runtime image is distroless/cc, so the backend C driver has to be
        // built from source and statically linked — which needs more than protoc
        edit(project, "Dockerfile", |c| {
            if !c.contains(APT_ANCHOR) {
                return Err(format!(
                    "Dockerfile has no `{APT_ANCHOR}` in its build stage, so there is nothing to hang the \
                     extra packages off — add what the bundled {} driver needs to its `apt-get install` \
                     line yourself: {}",
                    cap.dialect(),
                    cap.apt_packages()
                )
                .into());
            }
            Ok(c.replacen(APT_ANCHOR, &format!("{APT_ANCHOR} {}", cap.apt_packages()), 1))
        })?,
    ];

    edits.extend(migrations(project, cap));

    // dev-only services, separate from the production Dockerfile: the same file
    // boots the DB for a local run, for the env-gated adapter tests, and for CI
    if project.join(COMPOSE).is_file() {
        edits.push(edit(project, COMPOSE, |c| add_compose_service(&c, cap.name(), cap.compose_service()))?);
    } else {
        creates.push((COMPOSE.into(), format!("{COMPOSE_HEADER}{}", cap.compose_service())));
    }

    if let Some((rel, _)) = creates.iter().find(|(rel, _)| project.join(rel).exists()) {
        return Err(format!("{rel} already exists").into());
    }

    let paths = |v: &[(String, String)]| v.iter().map(|(rel, _)| rel.clone()).collect();

    // everything above is pure, so this is the only side effect
    if !dry_run {
        write_plan(project, &creates, &edits)?;
    }
    Ok((paths(&creates), paths(&edits)))
}

/// `kodegen add <relational> --orm seaorm` — the same backend as
/// `add_relational`, behind the other ORM.
///
/// Structurally this is `add_mongodb`, not `add_relational`: it takes the
/// datastore slot with a driver that is not Diesel, writes its own adapter next
/// to the ones already on disk, and swaps the three blocks of `bootstrap.rs`
/// rather than rewriting a file. Four files, and none of them overwritten —
/// which is why `--orm seaorm` cannot destroy anything a user wrote, while
/// `--orm diesel` replaces `diesel_db.rs` wholesale and has to compare it first.
///
/// What is *not* here is the point of the capability:
/// - no `Dockerfile` edit. sqlx and rustls are pure Rust, so the build stage
///   needs no perl/make/cmake and the distroless/cc runtime image needs nothing.
/// - no `migrations/`. The schema is `startup_ddl()`, applied idempotently on
///   the first connection (see `sea_db.rs`), which is the same deal
///   `add mongodb`, `add clickhouse` and `add s3` already make. Whatever is left
///   under `migrations/` is not deleted — it is not ours to delete, and it is
///   exactly what a later `--orm diesel` would need back.
fn add_seaorm(
    project: &Path,
    cap: Relational,
    dry_run: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    require_files(project, &[INFRA_MOD, BOOTSTRAP])?;

    let mut creates = vec![
        (SEA_DB.to_string(), cap.render(SEA_DB_TPL)),
        (SEA_REPO.to_string(), cap.render(SEA_REPO_TPL)),
    ];
    if let Some((rel, _)) = creates.iter().find(|(rel, _)| project.join(rel).exists()) {
        return Err(
            format!("{rel} already exists — remove it first, or keep the one you have").into(),
        );
    }

    let mut edits = vec![
        // the SeaORM feature replaces the relational one rather than joining it:
        // still one list of feature names, still never [dependencies]
        edit(project, "Cargo.toml", |c| {
            enable_feature(&c, cap.feature(Orm::Seaorm), Some(Slot::Datastore))
        })?,
        // gated and appended for the same reason `add mongodb` does it: with
        // `seaorm` off these files stop compiling instead of failing to
        edit(project, INFRA_MOD, |c| Ok(format!("{c}\n{SEA_MOD}")))?,
        edit(project, BOOTSTRAP, |c| rewrite_bootstrap(&c, &SEA_SWAP))?,
    ];

    // the same compose service the Diesel path writes, from the same constant:
    // it is the same server, and `DATABASE_URL` does not change either
    if project.join(COMPOSE).is_file() {
        edits.push(edit(project, COMPOSE, |c| {
            add_compose_service(&c, cap.name(), cap.compose_service())
        })?);
    } else {
        creates.push((
            COMPOSE.to_string(),
            format!("{COMPOSE_HEADER}{}", cap.compose_service()),
        ));
    }

    // said once, here, rather than in the file nobody reads afterwards: this
    // path never runs a migration, so a `migrations/` left over from `kodegen
    // new` is dead weight that looks alive
    if project.join("migrations").is_dir() {
        eprintln!(
            "warning: migrations/ is not used on the SeaORM path (the schema lives in {SEA_DB} and \
             is applied on the first connection) — delete it, or wire up sea-orm-migration when \
             your schema outgrows the startup DDL"
        );
    }

    let paths = |v: &[(String, String)]| v.iter().map(|(rel, _)| rel.clone()).collect();

    // everything above is pure, so this is the only side effect
    if !dry_run {
        write_plan(project, &creates, &edits)?;
    }
    Ok((paths(&creates), paths(&edits)))
}

/// `kodegen add mongodb` — the datastore slot again, with a driver that is not
/// Diesel.
///
/// No new port and no new `Services` field: MongoDB is a second adapter of the
/// `UserRepository` the template already has, so `tests/api.rs`, the application
/// service and every route above it are unchanged, and the port's contract
/// (`save` is an upsert, `get` on a miss is `NotFound`) is what the adapter has
/// to honor. That is the whole reason this capability is cheap.
///
/// Two consequences of taking the slot rather than adding one:
/// - `default` loses its relational feature, which is what gates the Diesel
///   modules *and their dependencies* — a Mongo project does not build the ORM
///   at all. The adapter written here is gated the same way (on `document`), so
///   the slot is symmetric: whichever side is off is simply not compiled, and
///   neither side's files ever have to be deleted — undoing this is putting
///   `default = ["sqlite"]` back plus the three blocks in the next bullet.
/// - `bootstrap.rs` has three blocks that name the Diesel handle, and they are
///   patched (verbatim match, else refuse) rather than the file being replaced:
///   the wiring `kodegen feature` and `add redis`/`add s3` put there has to
///   survive.
fn add_mongodb(project: &Path, dry_run: bool) -> Result<(Vec<String>, Vec<String>)> {
    require_files(project, &[INFRA_MOD, BOOTSTRAP])?;

    let mut creates = vec![(MONGO_REPO.to_string(), MONGO_REPO_TPL.to_string())];
    if let Some((rel, _)) = creates.iter().find(|(rel, _)| project.join(rel).exists()) {
        return Err(format!("{rel} already exists — remove it first, or keep the one you have").into());
    }

    let mut edits = vec![
        // `document` replaces the relational feature rather than joining it:
        // still one list of feature names, still never [dependencies]
        edit(project, "Cargo.toml", |c| enable_feature(&c, "document", Some(Slot::Datastore)))?,
        // gated, symmetrically with the Diesel modules the template already ships
        // behind `relational`, and appended rather than sorted into the plain
        // block for the same reason those are: an unconditional `pub mod
        // mongo_repo;` would make undo (`default = ["sqlite"]`) fail to compile
        // until the file was also deleted, which is not what this prints or what
        // the README promises.
        edit(project, INFRA_MOD, |c| Ok(format!("{c}\n{MONGO_MOD}")))?,
        edit(project, BOOTSTRAP, |c| rewrite_bootstrap(&c, &MONGO_SWAP))?,
    ];

    // whichever `add` runs first creates the compose file, the next one appends.
    // The service is `mongo` (the image's name), the capability is `mongodb`.
    if project.join(COMPOSE).is_file() {
        edits.push(edit(project, COMPOSE, |c| add_compose_service(&c, "mongo", MONGO_COMPOSE))?);
    } else {
        creates.push((COMPOSE.to_string(), format!("{COMPOSE_HEADER}{MONGO_COMPOSE}")));
    }

    let paths = |v: &[(String, String)]| v.iter().map(|(rel, _)| rel.clone()).collect();

    // everything above is pure, so this is the only side effect
    if !dry_run {
        write_plan(project, &creates, &edits)?;
    }
    Ok((paths(&creates), paths(&edits)))
}

/// `kodegen add mssql` — the datastore slot a third time, with a driver that has
/// no ORM at all.
///
/// Structurally this is `add_seaorm`: it writes its own two files next to the
/// ones already on disk, swaps the three `bootstrap.rs` blocks rather than
/// rewriting a file, calls its handle `Db` so scaffolded builders keep compiling,
/// and applies its schema at startup instead of through `migrations/`. Nothing is
/// overwritten, so this command cannot destroy anything a user wrote.
///
/// The one thing it does that `add_seaorm` does not is touch the `Dockerfile`,
/// and that is the price of the TLS decision (see the template's Cargo.toml):
/// `native-tls` means `openssl-sys`, and the distroless/cc runtime image has no
/// libssl to link against — so `vendored` is on, which needs perl and make in the
/// build stage, and `SSL_CERT_FILE` in the runtime stage so that a statically
/// linked OpenSSL can still find a CA store.
fn add_mssql(project: &Path, dry_run: bool) -> Result<(Vec<String>, Vec<String>)> {
    require_files(project, &[INFRA_MOD, BOOTSTRAP, "Dockerfile"])?;

    let mut creates = vec![
        (MSSQL_DB.to_string(), MSSQL_DB_TPL.to_string()),
        (MSSQL_REPO.to_string(), MSSQL_REPO_TPL.to_string()),
    ];
    if let Some((rel, _)) = creates.iter().find(|(rel, _)| project.join(rel).exists()) {
        return Err(
            format!("{rel} already exists — remove it first, or keep the one you have").into(),
        );
    }

    let mut edits = vec![
        // the slot again: `mssql` replaces whatever filled it, and [dependencies]
        // is never touched, so Cargo.lock stays valid and `--locked` keeps working
        edit(project, "Cargo.toml", |c| {
            enable_feature(&c, "mssql", Some(Slot::Datastore))
        })?,
        // gated and appended, symmetrically with the Mongo and SeaORM modules
        edit(project, INFRA_MOD, |c| Ok(format!("{c}\n{MSSQL_MOD}")))?,
        edit(project, BOOTSTRAP, |c| rewrite_bootstrap(&c, &MSSQL_SWAP))?,
        edit(project, "Dockerfile", |c| rewrite_dockerfile_for_mssql(&c))?,
    ];

    // two services, not one, and the second is the reason: unlike postgres and
    // mysql, this image has no `POSTGRES_DB` equivalent — it boots with master,
    // tempdb, model and msdb and nothing else, so `database=app` in the
    // connection string would never resolve
    if project.join(COMPOSE).is_file() {
        edits.push(edit(project, COMPOSE, |c| {
            let c = add_compose_service(&c, "mssql", MSSQL_COMPOSE)?;
            add_compose_service(&c, "mssql-init", MSSQL_INIT_COMPOSE)
        })?);
    } else {
        creates.push((
            COMPOSE.to_string(),
            format!("{COMPOSE_HEADER}{MSSQL_COMPOSE}{MSSQL_INIT_COMPOSE}"),
        ));
    }

    // said once, here, rather than in the file nobody reads afterwards — the same
    // sentence `add_seaorm` prints, one backend over
    if project.join("migrations").is_dir() {
        eprintln!(
            "warning: migrations/ is not used on the SQL Server path (the schema lives in \
             {MSSQL_DB} and is applied on the first connection) — delete it, or wire up a \
             migration tool of your own when your schema outgrows the startup DDL"
        );
    }

    let paths = |v: &[(String, String)]| v.iter().map(|(rel, _)| rel.clone()).collect();

    // everything above is pure, so this is the only side effect
    if !dry_run {
        write_plan(project, &creates, &edits)?;
    }
    Ok((paths(&creates), paths(&edits)))
}

/// Two edits with two different failure modes, so they are checked separately.
///
/// The `apt-get` line is the same anchor and the same code path `add postgres`
/// uses. `SSL_CERT_FILE` is new and belongs to this capability alone: vendored
/// OpenSSL compiles in an `OPENSSLDIR` pointing at the build directory, which
/// does not exist in the runtime image, so without this variable certificate
/// verification fails against **any server with a real certificate** — Azure SQL
/// first among them — while dev and CI stay green forever on
/// `TrustServerCertificate=true`. The file itself is in `distroless/cc-debian12`
/// (216 KB, 142 certificates — verified in the image, not assumed).
fn rewrite_dockerfile_for_mssql(content: &str) -> Result<String> {
    if !content.contains(APT_ANCHOR) {
        return Err(format!(
            "Dockerfile has no `{APT_ANCHOR}` in its build stage, so there is nothing to hang the \
             extra packages off — add what vendored OpenSSL needs to its `apt-get install` line \
             yourself: {MSSQL_APT}"
        )
        .into());
    }
    if !content.contains(RUNTIME_ANCHOR) {
        return Err(format!(
            "Dockerfile has no `{RUNTIME_ANCHOR}` line, so there is no runtime stage to put the CA \
             bundle path in — add `ENV {SSL_CERT_FILE}` to whatever your runtime stage is, or TLS \
             against a server with a real certificate will fail with a verification error while \
             dev and CI stay green on TrustServerCertificate=true"
        )
        .into());
    }
    let out = content.replacen(APT_ANCHOR, &format!("{APT_ANCHOR} {MSSQL_APT}"), 1);
    insert_after_line(&out, RUNTIME_ANCHOR, MSSQL_SSL_ENV)
}

/// What vendored OpenSSL's build needs, and nothing more: no cmake here, because
/// there is no C client library to build — tiberius speaks TDS itself.
const MSSQL_APT: &str = "perl make";
const RUNTIME_ANCHOR: &str = "FROM gcr.io/distroless/cc-debian12";
const SSL_CERT_FILE: &str = "SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt";
const MSSQL_SSL_ENV: &str = r##"
# Where a *statically linked* OpenSSL looks for CA certificates. `openssl-sys`
# with `vendored` compiles in an OPENSSLDIR that points at the build directory,
# which does not exist here — so without this line certificate verification fails
# against every server with a real certificate (Azure SQL first among them),
# while dev and CI stay green forever because the compose file sets
# TrustServerCertificate=true. This image ships the file (Debian's ca-certificates).
ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt"##;

/// What one datastore's `bootstrap.rs` swap replaces, and what must be gone
/// afterwards. Two capabilities share this function because they do the same
/// thing — take the datastore slot with a driver that is not Diesel — and
/// differ only in the three blocks and in what "the Diesel wiring survived"
/// looks like for each of them.
struct Swap {
    use_block: &'static str,
    db_block: &'static str,
    build_user: &'static str,
    /// The handle type a `kodegen feature` builder asks for, if it changes.
    /// `None` for SeaORM: its handle is *also* called `Db` (deliberately — see
    /// `scaffold.rs`), so every scaffolded builder keeps compiling untouched.
    builder_handle: Option<&'static str>,
    /// Strings that must not survive the swap, and the sentence naming them.
    /// The type the file used to name is gone once the old feature is off, so
    /// anything still mentioning it would not compile.
    leftovers: &'static [&'static str],
    leftover_what: &'static str,
    /// How this capability says "I am refusing" — the manual steps differ.
    refusal: fn(&str) -> String,
}

/// Swaps the three blocks of `bootstrap.rs` that name the Diesel handle for
/// `swap`'s, and leaves everything else — including whatever `kodegen feature`
/// and the additive capabilities wired in — exactly where it is.
///
/// Each block is matched verbatim against what `kodegen new` wrote. None of them
/// contains the project name (the same property `diesel_db.rs` has), so a
/// project nobody edited always matches however long its crate name is, and one
/// that was edited is refused rather than silently half-rewritten.
///
/// Both replacements keep the binding name `db`, which is why the two
/// `api::Services` literals need no edit at all: the slot is the same slot.
fn rewrite_bootstrap(content: &str, swap: &Swap) -> Result<String> {
    let mut out = content.to_string();
    for (what, from, to) in [
        (
            "the generated `infrastructure::{ ... }` import list",
            BOOTSTRAP_USE,
            swap.use_block,
        ),
        (
            "the generated `DATABASE_URL` block in build_services_from_env()",
            BOOTSTRAP_DB,
            swap.db_block,
        ),
        (
            "the generated `fn build_user_service`",
            BOOTSTRAP_BUILD_USER,
            swap.build_user,
        ),
    ] {
        if !out.contains(from) {
            return Err((swap.refusal)(what).into());
        }
        out = out.replacen(from, to, 1);
    }
    // every builder `kodegen feature` scaffolded takes the handle it does not use
    // yet, typed as the datastore's — if that type is about to stop existing,
    // they move with it. `_db` (not `db`) is what makes this the scaffolded shape
    // and not somebody's real adapter.
    if let Some(handle) = swap.builder_handle {
        out = out.replace(FEATURE_BUILDER_DB, handle);
    }
    // and anything still naming what the old ORM owned is code kodegen did not
    // write: it stops compiling the moment the old feature goes off, so refusing
    // beats handing back a broken tree.
    if swap.leftovers.iter().any(|left| out.contains(left)) {
        return Err((swap.refusal)(swap.leftover_what).into());
    }
    Ok(out)
}

const MONGO_SWAP: Swap = Swap {
    use_block: BOOTSTRAP_USE_MONGO,
    db_block: BOOTSTRAP_DB_MONGO,
    build_user: BOOTSTRAP_BUILD_USER_MONGO,
    builder_handle: Some(FEATURE_BUILDER_MONGO),
    leftovers: &["Db"],
    leftover_what: "no mention of the Diesel `Db` handle outside the blocks kodegen generated \
                    (yours is still there, and `Db` stops existing when the relational feature \
                    goes off)",
    refusal: mongo_refusal,
};

/// The SeaORM handle is a `Db` too, so "did the swap take?" cannot be answered
/// by looking for that name — it is `diesel`/`Diesel` that must be gone. Which
/// is the stricter check of the two: the type keeps its name but none of its
/// API, so a call to `db.run(...)` kodegen did not write would compile-error
/// after the swap rather than disappear with the type.
const SEA_SWAP: Swap = Swap {
    use_block: BOOTSTRAP_USE_SEA,
    db_block: BOOTSTRAP_DB_SEA,
    build_user: BOOTSTRAP_BUILD_USER_SEA,
    builder_handle: None,
    leftovers: &["diesel", "Diesel"],
    leftover_what: "no mention of Diesel outside the blocks kodegen generated (yours is still \
                    there — the SeaORM handle is called `Db` as well, but it has none of the same \
                    API, and the Diesel modules stop compiling when the relational feature goes \
                    off)",
    refusal: sea_refusal,
};

/// The SQL Server handle is a `Db` too, so — exactly like `SEA_SWAP` — "did the
/// swap take?" is answered by looking for `diesel`/`Diesel`, not for `Db`. Same
/// reason, and it applies harder here: the type keeps its name and shares not one
/// method with the old one (no `run`, no pool of `PgConnection`), so a call
/// kodegen did not write compile-errors after the swap rather than disappearing
/// with the type.
const MSSQL_SWAP: Swap = Swap {
    use_block: BOOTSTRAP_USE_MSSQL,
    db_block: BOOTSTRAP_DB_MSSQL,
    build_user: BOOTSTRAP_BUILD_USER_MSSQL,
    builder_handle: None,
    leftovers: &["diesel", "Diesel"],
    leftover_what: "no mention of Diesel outside the blocks kodegen generated (yours is still \
                    there — the SQL Server handle is called `Db` as well, but it has none of the \
                    same API, and the Diesel modules stop compiling when the relational feature \
                    goes off)",
    refusal: mssql_refusal,
};

fn mongo_refusal(what: &str) -> String {
    format!(
        "no longer matches what kodegen generated (expected {what}) — refusing to overwrite your \
         changes.\nSwitch to MongoDB by hand instead:\n  \
         1. Cargo.toml: `default = [\"document\"]` — dropping the relational feature is what stops \
         the Diesel modules compiling\n  \
         2. src/infrastructure/mongo_repo.rs: the adapter, plus `#[cfg(feature = \"document\")] pub \
         mod mongo_repo;` in infrastructure/mod.rs (generate a throwaway `kodegen new tmp --with \
         mongodb` and copy the file across)\n  \
         3. src/bootstrap.rs: build a `Mongo` from MONGODB_URL/MONGODB_DB where the `Db` is built, \
         and hand it to build_user_service instead\n  \
         4. run with MONGODB_URL={MONGO_URL}"
    )
}

/// The same, one ORM over. Step 3 is shorter than Mongo's because the handle
/// keeps its name and its binding: what changes is what is behind it.
fn sea_refusal(what: &str) -> String {
    format!(
        "no longer matches what kodegen generated (expected {what}) — refusing to overwrite your \
         changes.\nSwitch to SeaORM by hand instead:\n  \
         1. Cargo.toml: `default = [\"seaorm-postgres\"]` — dropping the relational feature is what \
         stops the Diesel modules compiling, and `src/lib.rs` refuses to build both ORMs at once\n  \
         2. src/infrastructure/sea_db.rs + sea_repo.rs: the handle and the adapter, plus \
         `#[cfg(feature = \"seaorm\")] pub mod sea_db;` and the same for `sea_repo` in \
         infrastructure/mod.rs (generate a throwaway `kodegen new tmp --with postgres --orm seaorm` \
         and copy the files across)\n  \
         3. src/bootstrap.rs: build the handle with `Db::new(url)` where the pool was built — no \
         pool, no migration step — and hand it to build_user_service, which now constructs a \
         `SeaUserRepository`\n  \
         4. run with DATABASE_URL={PG_URL}"
    )
}

/// The same again, one driver over. Step 3 names `Db::new` because the handle
/// keeps its name here too; step 5 is the one no other capability has, and
/// leaving it out is a TLS failure that only shows up against a server with a
/// real certificate.
fn mssql_refusal(what: &str) -> String {
    format!(
        "no longer matches what kodegen generated (expected {what}) — refusing to overwrite your \
         changes.\nSwitch to SQL Server by hand instead:\n  \
         1. Cargo.toml: `default = [\"mssql\"]` — dropping the relational feature is what stops \
         the Diesel modules compiling\n  \
         2. src/infrastructure/mssql_db.rs + mssql_repo.rs: the handle and the adapter, plus \
         `#[cfg(feature = \"mssql\")] pub mod mssql_db;` and the same for `mssql_repo` in \
         infrastructure/mod.rs (generate a throwaway `kodegen new tmp --with mssql` and copy the \
         files across)\n  \
         3. src/bootstrap.rs: build the handle with `Db::new(ado)` where the pool was built — no \
         pool to build here and no migration step — and hand it to build_user_service, which now \
         constructs an `MssqlUserRepository`\n  \
         4. Dockerfile: add `{MSSQL_APT}` to the build stage's apt line (vendored OpenSSL needs \
         them) and `ENV {SSL_CERT_FILE}` to the runtime stage\n  \
         5. run with DATABASE_URL={MSSQL_URL}"
    )
}

/// `kodegen add redis` (Valkey included — same protocol, same client).
///
/// Purely additive, unlike the relational path: nothing in the template knows
/// about caching yet, so this writes the `Cache` port, both adapters and the
/// wiring, and touches no existing line except to append to it. That is also why
/// it can be combined with any relational backend.
fn add_redis(project: &Path, dry_run: bool) -> Result<(Vec<String>, Vec<String>)> {
    require_files(
        project,
        &[PORTS, "src/domain/mod.rs", "src/infrastructure/mod.rs", "src/api/mod.rs", "src/bootstrap.rs"],
    )?;
    let crate_name = crate::package_name(project)
        .ok_or("Cargo.toml with a package name not found — run inside a generated project")?
        .replace('-', "_");

    let mut creates = vec![
        (IN_MEMORY_CACHE.to_string(), IN_MEMORY_CACHE_TPL.to_string()),
        (REDIS_CACHE.to_string(), REDIS_CACHE_TPL.to_string()),
        (CACHE_TEST.to_string(), CACHE_TEST_TPL.replace("{{crate_name}}", &crate_name)),
    ];
    if let Some((rel, _)) = creates.iter().find(|(rel, _)| project.join(rel).exists()) {
        return Err(format!("{rel} already exists — remove it first, or keep the one you have").into());
    }

    let mut edits = vec![
        // one list of feature names, never [dependencies] — that is what keeps
        // Cargo.lock valid and `--locked` working across an `add`
        edit(project, "Cargo.toml", |c| enable_feature(&c, "cache", None))?,
        // ports.rs and the two mod.rs files are append-only, so no anchor can go
        // missing and nothing the user wrote above can be disturbed — but
        // "append" still has two ways to produce a file that will not compile
        edit(project, PORTS, |c| {
            if c.contains("trait Cache") {
                return Err("already declares a `Cache` trait — appending a second one would not \
                            compile. Remove yours, or keep it and skip `kodegen add redis`."
                    .into());
            }
            if !c.contains(ASYNC_TRAIT_USE) {
                return Err(format!(
                    "has no `{ASYNC_TRAIT_USE}` — the appended `Cache` port is `#[async_trait]`, \
                     so restore that line at the top of the file and rerun"
                )
                .into());
            }
            Ok(format!("{c}\n{CACHE_PORT_TPL}"))
        })?,
        edit(project, "src/domain/mod.rs", |c| Ok(format!("{c}pub use ports::Cache;\n")))?,
        edit(project, "src/infrastructure/mod.rs", |c| {
            let c = insert_pub_mod(&c, "in_memory_cache")?;
            insert_pub_mod(&c, "redis_cache")
        })?,
        edit(project, "src/api/mod.rs", |c| {
            insert_after_line(&c, "pub struct Services {", "    pub cache: Arc<dyn crate::domain::Cache>,")
        })?,
        edit(project, "src/bootstrap.rs", |c| {
            let c = insert_before_line(&c, RESOURCES_MARKER, REDIS_RESOURCE).map_err(|_| {
                format!(
                    "marker '{RESOURCES_MARKER}' is missing — put the line `    {RESOURCES_MARKER}` \
                     back just above the `api::Services {{ ... }}` literal in \
                     `build_services_from_env()`, then rerun (or wire the cache manually: resolve \
                     REDIS_URL there and pass it to a `build_cache` builder)"
                )
            })?;
            let c = insert_at_marker(
                &c,
                "// kodegen:services:env",
                "build_services_from_env",
                "user: build_user_service(db.as_ref()),",
                "        cache: build_cache(redis.as_ref()),",
            )?;
            let c = insert_at_marker(
                &c,
                "// kodegen:services:default",
                "build_services",
                "user: build_user_service(None),",
                "        cache: build_cache(None),",
            )?;
            Ok(format!("{c}\n{BUILD_CACHE_TPL}"))
        })?,
    ];

    // the relational path writes this file too: whichever `add` runs first
    // creates it with the header, the next one appends under `services:`
    if project.join(COMPOSE).is_file() {
        edits.push(edit(project, COMPOSE, |c| add_compose_service(&c, "redis", REDIS_COMPOSE))?);
    } else {
        creates.push((COMPOSE.to_string(), format!("{COMPOSE_HEADER}{REDIS_COMPOSE}")));
    }

    let paths = |v: &[(String, String)]| v.iter().map(|(rel, _)| rel.clone()).collect();

    // everything above is pure, so this is the only side effect
    if !dry_run {
        write_plan(project, &creates, &edits)?;
    }
    Ok((paths(&creates), paths(&edits)))
}

/// `kodegen add s3` — RustFS, MinIO, Ceph and AWS S3, which are one adapter
/// because they are one API.
///
/// The same purely additive shape as `add_redis`: a port the template does not
/// ship, both of its adapters, and the wiring. The default adapter here writes
/// files under a directory rather than keeping bytes in memory, because a blob
/// store is a system of record and "it was there a moment ago" is the one thing
/// it must not do.
fn add_s3(project: &Path, dry_run: bool) -> Result<(Vec<String>, Vec<String>)> {
    require_files(
        project,
        &[PORTS, "src/domain/mod.rs", "src/infrastructure/mod.rs", "src/api/mod.rs", "src/bootstrap.rs"],
    )?;
    let crate_name = crate::package_name(project)
        .ok_or("Cargo.toml with a package name not found — run inside a generated project")?
        .replace('-', "_");

    let mut creates = vec![
        (FS_BLOBS.to_string(), FS_BLOBS_TPL.to_string()),
        (S3_BLOBS.to_string(), S3_BLOBS_TPL.to_string()),
        (BLOB_TEST.to_string(), BLOB_TEST_TPL.replace("{{crate_name}}", &crate_name)),
    ];
    if let Some((rel, _)) = creates.iter().find(|(rel, _)| project.join(rel).exists()) {
        return Err(format!("{rel} already exists — remove it first, or keep the one you have").into());
    }

    let mut edits = vec![
        // one list of feature names, never [dependencies] — that is what keeps
        // Cargo.lock valid and `--locked` working across an `add`
        edit(project, "Cargo.toml", |c| enable_feature(&c, "blob", None))?,
        // append-only, so no anchor can go missing — but appending still has
        // three ways to produce a file that will not compile
        edit(project, PORTS, |c| {
            if c.contains("trait BlobStore") {
                return Err("already declares a `BlobStore` trait — appending a second one would not \
                            compile. Remove yours, or keep it and skip `kodegen add s3`."
                    .into());
            }
            if c.contains("fn check_blob_key") {
                return Err("already declares `check_blob_key` — appending a second one would not \
                            compile. Remove yours, or keep it and skip `kodegen add s3`."
                    .into());
            }
            if !c.contains(ASYNC_TRAIT_USE) {
                return Err(format!(
                    "has no `{ASYNC_TRAIT_USE}` — the appended `BlobStore` port is `#[async_trait]`, \
                     so restore that line at the top of the file and rerun"
                )
                .into());
            }
            Ok(format!("{c}\n{BLOB_PORT_TPL}"))
        })?,
        edit(project, "src/domain/mod.rs", |c| {
            Ok(format!("{c}pub use ports::{{check_blob_key, BlobStore}};\n"))
        })?,
        edit(project, "src/infrastructure/mod.rs", |c| {
            let c = insert_pub_mod(&c, "fs_blobs")?;
            insert_pub_mod(&c, "s3_blobs")
        })?,
        edit(project, "src/api/mod.rs", |c| {
            insert_after_line(&c, "pub struct Services {", "    pub blobs: Arc<dyn crate::domain::BlobStore>,")
        })?,
        edit(project, "src/bootstrap.rs", |c| {
            let c = insert_before_line(&c, RESOURCES_MARKER, S3_RESOURCE).map_err(|_| {
                format!(
                    "marker '{RESOURCES_MARKER}' is missing — put the line `    {RESOURCES_MARKER}` \
                     back just above the `api::Services {{ ... }}` literal in \
                     `build_services_from_env()`, then rerun (or wire the blob store manually: build \
                     the S3 client there and pass it to a `build_blob_store` builder)"
                )
            })?;
            let c = insert_at_marker(
                &c,
                "// kodegen:services:env",
                "build_services_from_env",
                "user: build_user_service(db.as_ref()),",
                "        blobs: build_blob_store(s3.as_ref()),",
            )?;
            let c = insert_at_marker(
                &c,
                "// kodegen:services:default",
                "build_services",
                "user: build_user_service(None),",
                "        blobs: build_blob_store(None),",
            )?;
            Ok(format!("{c}\n{BUILD_BLOB_STORE_TPL}"))
        })?,
    ];

    // whichever `add` runs first creates the compose file, the next one appends
    if project.join(COMPOSE).is_file() {
        edits.push(edit(project, COMPOSE, |c| add_compose_service(&c, "s3", S3_COMPOSE))?);
    } else {
        creates.push((COMPOSE.to_string(), format!("{COMPOSE_HEADER}{S3_COMPOSE}")));
    }

    let paths = |v: &[(String, String)]| v.iter().map(|(rel, _)| rel.clone()).collect();

    // everything above is pure, so this is the only side effect
    if !dry_run {
        write_plan(project, &creates, &edits)?;
    }
    Ok((paths(&creates), paths(&edits)))
}

/// `kodegen add clickhouse` / `influxdb` / `greptimedb` — the `EventSink` port,
/// its log-only default adapter, and one of the two backends behind it (three
/// commands, two adapters: `greptimedb` is `influxdb`'s with another image).
///
/// Additive like `add redis`/`add s3`, with one difference that is the reason
/// `Slot` exists: the backends are alternatives, so the second one is refused
/// in `add` before this function runs. Everything below is therefore written
/// exactly once per project, and the only thing that varies is `backend`.
///
/// Write path only, by design: ClickHouse and a time-series database are both
/// append-only from the application's side, and an analytical *query* port would
/// have a different shape for each of them (see the plan's "not doing" list).
fn add_event_sink(
    project: &Path,
    backend: EventBackend,
    dry_run: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    require_files(
        project,
        &[PORTS, "src/domain/mod.rs", "src/infrastructure/mod.rs", "src/api/mod.rs", "src/bootstrap.rs"],
    )?;
    let crate_name = crate::package_name(project)
        .ok_or("Cargo.toml with a package name not found — run inside a generated project")?
        .replace('-', "_");
    let (adapter_rel, adapter_tpl) = backend.adapter();
    let resource = backend.resource_name();

    let mut creates = vec![
        (LOG_EVENTS.to_string(), LOG_EVENTS_TPL.to_string()),
        (adapter_rel.to_string(), adapter_tpl.to_string()),
        (
            EVENTS_TEST.to_string(),
            EVENTS_TEST_TPL
                .replace("{{server_test}}", backend.server_test())
                .replace("{{adapter_module}}", backend.module())
                .replace("{{crate_name}}", &crate_name),
        ),
    ];
    if let Some((rel, _)) = creates.iter().find(|(rel, _)| project.join(rel).exists()) {
        return Err(format!("{rel} already exists — remove it first, or keep the one you have").into());
    }

    let mut edits = vec![
        // one list of feature names, never [dependencies] — that is what keeps
        // Cargo.lock valid and `--locked` working across an `add`
        edit(project, "Cargo.toml", |c| enable_feature(&c, backend.feature(), Some(Slot::Events)))?,
        // append-only, so no anchor can go missing — but appending still has
        // four ways to produce a file that will not compile
        edit(project, PORTS, |c| {
            for (item, what) in [
                ("trait EventSink", "an `EventSink` trait"),
                ("struct Event", "an `Event` type"),
                ("fn check_event", "`check_event`"),
            ] {
                if c.contains(item) {
                    return Err(format!(
                        "already declares {what} — appending a second one would not compile. \
                         Remove yours, or keep it and skip `kodegen add {}`.",
                        backend.capability_name()
                    )
                    .into());
                }
            }
            if !c.contains(ASYNC_TRAIT_USE) {
                return Err(format!(
                    "has no `{ASYNC_TRAIT_USE}` — the appended `EventSink` port is `#[async_trait]`, \
                     so restore that line at the top of the file and rerun"
                )
                .into());
            }
            Ok(format!("{c}\n{EVENT_PORT_TPL}"))
        })?,
        edit(project, "src/domain/mod.rs", |c| {
            Ok(format!("{c}pub use ports::{{check_event, Event, EventSink}};\n"))
        })?,
        edit(project, "src/infrastructure/mod.rs", |c| {
            let c = insert_pub_mod(&c, "log_events")?;
            insert_pub_mod(&c, backend.module())
        })?,
        edit(project, "src/api/mod.rs", |c| {
            insert_after_line(&c, "pub struct Services {", "    pub events: Arc<dyn crate::domain::EventSink>,")
        })?,
        edit(project, "src/bootstrap.rs", |c| {
            let c = insert_before_line(&c, RESOURCES_MARKER, backend.resource()).map_err(|_| {
                format!(
                    "marker '{RESOURCES_MARKER}' is missing — put the line `    {RESOURCES_MARKER}` \
                     back just above the `api::Services {{ ... }}` literal in \
                     `build_services_from_env()`, then rerun (or wire the event sink manually: \
                     resolve {} there and pass it to a `build_event_sink` builder)",
                    backend.env()[0].0
                )
            })?;
            let c = insert_at_marker(
                &c,
                "// kodegen:services:env",
                "build_services_from_env",
                "user: build_user_service(db.as_ref()),",
                &format!("        events: build_event_sink({resource}.as_ref()),"),
            )?;
            let c = insert_at_marker(
                &c,
                "// kodegen:services:default",
                "build_services",
                "user: build_user_service(None),",
                "        events: build_event_sink(None),",
            )?;
            Ok(format!("{c}\n{}", backend.builder()))
        })?,
    ];

    // whichever `add` runs first creates the compose file, the next one appends
    let (service, block) = backend.compose_service();
    if project.join(COMPOSE).is_file() {
        edits.push(edit(project, COMPOSE, |c| add_compose_service(&c, service, block))?);
    } else {
        creates.push((COMPOSE.to_string(), format!("{COMPOSE_HEADER}{block}")));
    }

    let paths = |v: &[(String, String)]| v.iter().map(|(rel, _)| rel.clone()).collect();

    // everything above is pure, so this is the only side effect
    if !dry_run {
        write_plan(project, &creates, &edits)?;
    }
    Ok((paths(&creates), paths(&edits)))
}

/// `kodegen add scalar` — no port, no adapter, no slot (see `Kind::ApiDocs`), so
/// there is nothing here for a second `add` to conflict with and nothing to
/// refuse: this is `insert_before_line` over five files that already exist,
/// plus one new one.
///
/// Four handlers are annotated unconditionally, because they ship with the
/// template regardless of whether this capability is ever added:
/// `health`/`ErrorRes` in `api/mod.rs`, `create_user`/`get_user`/`CreateUserReq`
/// in `api/user.rs`, `say_hello` in `api/hello.rs`, and `User` in
/// `domain/model.rs`. Anything `kodegen feature` scaffolded *before* this ran is
/// picked up the same way `kodegen feature` itself would recognize it later —
/// `scanned_openapi_paths` finds the `#[cfg_attr(feature = "apidocs",
/// utoipa::path(...))]` that template already carries (see `API_TPL`) and lists
/// it in `paths(...)` here. A feature scaffolded *after* this runs takes the
/// other path: `scaffold::add_feature` sees `src/api/openapi.rs` on disk and
/// inserts its two paths above `OPENAPI_PATHS_MARKER`, which this leaves behind
/// for exactly that.
fn add_scalar(project: &Path, dry_run: bool) -> Result<(Vec<String>, Vec<String>)> {
    require_files(project, &[API_MOD, USER_API, HELLO_API, DOMAIN_MODEL])?;
    let crate_name = crate::package_name(project)
        .ok_or("Cargo.toml with a package name not found — run inside a generated project")?
        .replace('-', "_");

    let creates = vec![
        (
            OPENAPI_RS.to_string(),
            OPENAPI_TPL
                .replace("{{extra_paths}}", &scanned_openapi_paths(project)?)
                .replace("{{marker}}", OPENAPI_PATHS_MARKER),
        ),
        (APIDOCS_TEST.to_string(), APIDOCS_TEST_TPL.replace("{{crate_name}}", &crate_name)),
    ];
    if let Some((rel, _)) = creates.iter().find(|(rel, _)| project.join(rel).exists()) {
        return Err(format!("{rel} already exists — remove it first, or keep the one you have").into());
    }

    let edits = vec![
        // one list of feature names, never [dependencies] — the two utoipa
        // crates are already in the template's Cargo.toml behind `optional =
        // true`, pinned in Cargo.lock whether or not this feature is on
        edit(project, "Cargo.toml", |c| enable_feature(&c, "apidocs", None))?,
        edit(project, API_MOD, |c| {
            let c = insert_pub_mod(&c, "openapi")?;
            let c = insert_before_line(&c, ".with_state(services)", "        .merge(openapi::routes())")?;
            let c = insert_before_line(&c, "pub struct ErrorRes {", TO_SCHEMA_ATTR)?;
            let c = insert_before_line(&c, "pub struct ReadyRes {", TO_SCHEMA_ATTR)?;
            let c = insert_before_line(&c, "async fn health() -> &'static str {", HEALTH_PATH_ATTR)?;
            insert_before_line(&c, "async fn ready(", READY_PATH_ATTR)
        })?,
        edit(project, USER_API, |c| {
            let c = insert_before_line(&c, "pub struct CreateUserReq {", TO_SCHEMA_ATTR)?;
            let c = insert_before_line(&c, "pub async fn create_user(", CREATE_USER_PATH_ATTR)?;
            insert_before_line(&c, "pub async fn get_user(", GET_USER_PATH_ATTR)
        })?,
        edit(project, HELLO_API, |c| {
            insert_before_line(&c, "pub async fn say_hello(", SAY_HELLO_PATH_ATTR)
        })?,
        edit(project, DOMAIN_MODEL, |c| insert_before_line(&c, "pub struct User {", TO_SCHEMA_ATTR))?,
    ];

    let paths = |v: &[(String, String)]| v.iter().map(|(rel, _)| rel.clone()).collect();

    // everything above is pure, so this is the only side effect
    if !dry_run {
        write_plan(project, &creates, &edits)?;
    }
    Ok((paths(&creates), paths(&edits)))
}

/// The paths a REST `kodegen feature` already annotated before this ran — the
/// "feature first" direction described on `add_scalar`.
///
/// `src/api/*.rs` minus the four files this function's caller annotates by hand
/// (`mod.rs`, `user.rs`, `hello.rs` — already covered) and minus `*_grpc.rs`
/// (OpenAPI does not describe gRPC, and there is no HTTP route in those files to
/// find). What is left is exactly the REST delivery files `kodegen feature`
/// writes, and every one of them carries `API_TPL`'s annotations unconditionally
/// — on `apidocs` or off — so there is always something here to find once a
/// feature has been scaffolded.
///
/// No `syn`: the shape being searched for is one this crate itself writes —
/// `utoipa::path(` followed by the next `async fn <name>(` — and being exact
/// about anything more general is not this function's job. What is annotated is
/// what ends up in the spec, by definition: a hand-written route that skipped
/// the attribute is documented in the README, not caught here.
///
/// ponytail: two known ceilings from that shape, both cheap to leave as is —
/// - a hand-edited file where the attribute is not immediately followed by the
///   `async fn` it was meant for (a helper function moved between them) gets
///   the *wrong* name here, but that fails loudly: `crate::api::<module>::<wrong
///   name>` either does not exist or has no `__path_*` for utoipa's macro to
///   find, so it is a compile error in `openapi.rs`, not a silently wrong spec.
/// - a REST feature whose file happens to be named `*_grpc.rs` (nobody does
///   this; `kodegen feature --transport grpc` is what produces that suffix) is
///   skipped along with the real gRPC files — the exclusion is by filename, not
///   by transport, because there is nothing else here to tell them apart from.
fn scanned_openapi_paths(project: &Path) -> Result<String> {
    let api_dir = project.join("src/api");
    let mut modules: Vec<String> = fs::read_dir(&api_dir)
        .map_err(|e| format!("src/api: {e}"))?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let stem = name.strip_suffix(".rs")?.to_string();
            (!matches!(stem.as_str(), "mod" | "user" | "hello") && !stem.ends_with("_grpc")).then_some(stem)
        })
        .collect();
    // read_dir order is not guaranteed; the paths this writes must be stable
    modules.sort();

    let mut out = String::new();
    for module in modules {
        let content = fs::read_to_string(api_dir.join(format!("{module}.rs")))
            .map_err(|e| format!("src/api/{module}.rs: {e}"))?;
        let mut rest = content.as_str();
        while let Some(at) = rest.find("utoipa::path(") {
            rest = &rest[at..];
            let Some(fn_at) = rest.find("async fn ") else { break };
            let name_start = fn_at + "async fn ".len();
            let Some(paren) = rest[name_start..].find('(') else { break };
            let name = rest[name_start..name_start + paren].trim();
            out.push_str(&format!("    crate::api::{module}::{name},\n"));
            rest = &rest[name_start + paren..];
        }
    }
    Ok(out)
}

/// Shared by every DTO `add_scalar` annotates: a second, independent `derive`
/// attribute is legal next to whatever the struct already has (`Serialize`,
/// `Deserialize`, ...) — attribute order carries no meaning in Rust — and
/// `cfg_attr` is what makes it vanish along with the `apidocs` feature rather
/// than sit there unused.
const TO_SCHEMA_ATTR: &str = "#[cfg_attr(feature = \"apidocs\", derive(utoipa::ToSchema))]";

const HEALTH_PATH_ATTR: &str = "#[cfg_attr(feature = \"apidocs\", utoipa::path(\n    get, path = \"/health\", tag = \"health\",\n    responses((status = 200, description = \"ok\", body = String)),\n))]";

const READY_PATH_ATTR: &str = "#[cfg_attr(feature = \"apidocs\", utoipa::path(\n    get, path = \"/ready\", tag = \"health\",\n    responses(\n        (status = 200, description = \"every backend answered\", body = ReadyRes),\n        (status = 503, description = \"a backend did not\", body = ReadyRes),\n    ),\n))]";

const CREATE_USER_PATH_ATTR: &str = "#[cfg_attr(feature = \"apidocs\", utoipa::path(\n    post, path = \"/users\", tag = \"users\",\n    request_body = CreateUserReq,\n    responses(\n        (status = 201, description = \"created\"),\n        (status = 400, description = \"validation failed\", body = ErrorRes),\n    ),\n))]";

const GET_USER_PATH_ATTR: &str = "#[cfg_attr(feature = \"apidocs\", utoipa::path(\n    get, path = \"/users/{id}\", tag = \"users\",\n    params((\"id\" = String, Path)),\n    responses(\n        (status = 200, description = \"found\", body = User),\n        (status = 404, description = \"not found\", body = ErrorRes),\n    ),\n))]";

const SAY_HELLO_PATH_ATTR: &str = "#[cfg_attr(feature = \"apidocs\", utoipa::path(\n    get, path = \"/hello/{name}\", tag = \"hello\",\n    params((\"name\" = String, Path)),\n    responses((status = 200, description = \"greeted\")),\n))]";

/// The OpenAPI document + the two routes that serve it. Written once by
/// `add_scalar`; every path after the four fixed ones is inserted above
/// `OPENAPI_PATHS_MARKER`, either by `scanned_openapi_paths` at write time or by
/// `scaffold::add_feature` on every `kodegen feature` that runs afterward.
///
/// `cfg` rather than `cfg_attr` on `ApiDoc` and inside `routes()`: with
/// `apidocs` off, the struct and the whole derive macro invocation disappear —
/// there is nothing partial to gate attribute-by-attribute the way a single
/// `#[derive(...)]` line is elsewhere in this file.
const OPENAPI_TPL: &str = r##"//! The OpenAPI document, and the two routes that serve it — written by
//! `kodegen add scalar`. Empty with `apidocs` off, which is how a production
//! build drops both routes (and every `utoipa`/`utoipa-scalar` line) without
//! touching `api/mod.rs` — see the README.

use axum::Router;

#[cfg(feature = "apidocs")]
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    crate::api::health,
    crate::api::ready,
    crate::api::user::create_user,
    crate::api::user::get_user,
    crate::api::hello::say_hello,
{{extra_paths}}    {{marker}}
))]
pub struct ApiDoc;

/// `GET /docs` (Scalar, needs a network for its JS — see the README) and
/// `GET /openapi.json` (fully offline). Both empty when `apidocs` is off.
pub fn routes() -> Router<super::Services> {
    #[cfg(feature = "apidocs")]
    {
        use utoipa::OpenApi;
        use utoipa_scalar::Servable;
        let spec = ApiDoc::openapi();
        let json = axum::Json(spec.clone());
        Router::from(utoipa_scalar::Scalar::with_url("/docs", spec)).route(
            "/openapi.json",
            axum::routing::get(move || {
                let json = json.clone();
                async move { json }
            }),
        )
    }
    #[cfg(not(feature = "apidocs"))]
    {
        Router::new()
    }
}
"##;

/// The contract test, written next to `openapi.rs`. It proves the spec *that
/// is there* is honest (parses, `/docs` answers, the routes present when this
/// ran are listed) — it cannot prove the spec covers *every* route, because
/// axum 0.8 has no way to list a `Router`'s routes back out to compare against
/// `paths`. That half of the promise is proven in `kodegen`'s own test suite
/// (`scanned_openapi_paths`, `add_feature`'s marker insert) and by CI grepping
/// this test's own `println!` output for routes scaffolded after this ran —
/// see `.github/workflows/ci.yml`.
const APIDOCS_TEST_TPL: &str = r##"#![cfg(feature = "apidocs")]
//! The OpenAPI contract — written by `kodegen add scalar`. See the doc
//! comment on `APIDOCS_TEST_TPL` in kodegen's `capability.rs` for what this
//! does and does not prove.
//!
//! Gated on the file itself, not just the routes it hits: without `apidocs`,
//! `/docs` and `/openapi.json` are gone (see `openapi.rs`), so this whole file
//! compiling out is what keeps "drop apidocs from default" a green build
//! instead of a red `cargo test` — the exact case the README tells someone to
//! do before shipping.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use {{crate_name}}::bootstrap;

#[tokio::test]
async fn openapi_json_parses_and_covers_the_shipped_routes() {
    let app = bootstrap::build_router();
    let res = app
        .oneshot(Request::get("/openapi.json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = axum::body::to_bytes(res.into_body(), 1024 * 1024).await.unwrap();
    let spec: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let paths = spec["paths"].as_object().expect("a `paths` object");
    for known in ["/health", "/hello/{name}", "/users", "/users/{id}"] {
        assert!(paths.contains_key(known), "missing {known} in {:?}", paths.keys().collect::<Vec<_>>());
    }

    // printed, not asserted: a fixed list here would go stale the moment a
    // later `kodegen feature` adds a route. CI greps this output instead.
    for path in paths.keys() {
        println!("openapi path: {path}");
    }
}

#[tokio::test]
async fn docs_page_is_reachable() {
    let app = bootstrap::build_router();
    let res = app
        .oneshot(Request::get("/docs").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
"##;

/// The file as `kodegen new` would have written it into a project called `pkg`
/// — the embedded template with the rename applied, in the same order
/// `substitutions()` applies it (snake before kebab).
fn generated(rel: &str, pkg: &str) -> Result<String> {
    let raw = crate::TEMPLATES
        .get_file(format!("hexagonal/{rel}"))
        .and_then(|f| f.contents_utf8())
        .ok_or_else(|| format!("the built-in hexagonal template no longer contains {rel}"))?;
    Ok(raw
        .replace(&TEMPLATE_PKG.replace('-', "_"), &pkg.replace('-', "_"))
        .replace(TEMPLATE_PKG, pkg))
}

fn default_line(manifest: &str) -> Option<&str> {
    default_span(manifest).map(|(_, line)| line)
}

/// Byte offset + text of the real `default = [...]` line. The offset is what
/// makes the rewrite safe: replacing by substring would happily hit a
/// `# default = [...]` sitting in a comment further up the file.
///
/// The closing `]` has to be on that same line. `add` replaces exactly one line,
/// so an array a formatter (taplo, `cargo fmt --all` on some setups) has split
/// across several would have its first line rewritten and the rest left dangling
/// — a silently corrupted manifest. Refusing is the only safe answer.
fn default_span(manifest: &str) -> Option<(usize, &str)> {
    let mut offset = 0;
    for raw in manifest.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        if let Some(rest) = line.trim_start().strip_prefix("default = [")
            && rest.contains(']')
        {
            return Some((offset, line));
        }
        offset += raw.len();
    }
    None
}

fn parse_features(line: &str) -> Vec<String> {
    line.split('[')
        .nth(1)
        .and_then(|s| s.split(']').next())
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn default_anchor_err(manifest: &str) -> String {
    // told apart on purpose: "you deleted it" and "your formatter wrapped it"
    // need completely different fixes, and the second one looks fine to read
    if manifest.lines().any(|l| l.trim_start().starts_with("default = [")) {
        return "`default = [...]` is split across lines — `kodegen add` rewrites that one line, \
                so it would truncate the rest of the array. Put the list back on a single line \
                (`default = [\"sqlite\"]`) and rerun."
            .to_string();
    }
    "anchor 'default = [' not found — put the `[features]` section back \
     (`default = [\"sqlite\"]`) or switch the backend manually"
        .to_string()
}

/// Turns `feature` on in `default = [...]`. A feature that fills a slot
/// *replaces* whichever one is listed there (there is one `UserRepository`
/// adapter, whether it is Diesel or Mongo); everything else is additive and joins
/// the list.
fn enable_feature(manifest: &str, feature: &str, slot: Option<Slot>) -> Result<String> {
    let (offset, line) = default_span(manifest).ok_or_else(|| default_anchor_err(manifest))?;
    let indent = &line[..line.len() - line.trim_start().len()];
    let mut kept: Vec<String> = parse_features(line)
        .into_iter()
        .filter(|f| !slot.is_some_and(|s| s.features().contains(&f.as_str())))
        .collect();
    // `add` refuses a second run long before this, but the rewrite itself has no
    // business producing `["cache", "cache"]` for anyone who calls it otherwise
    if !kept.iter().any(|f| f == feature) {
        kept.push(feature.to_string());
    }
    let replacement = format!(
        "{indent}default = [{}]",
        kept.iter().map(|f| format!("\"{f}\"")).collect::<Vec<_>>().join(", ")
    );
    Ok(format!("{}{replacement}{}", &manifest[..offset], &manifest[offset + line.len()..]))
}

/// `diesel_repo.rs` is patched, not replaced, so queries added below the
/// generated ones survive: `save` becomes a dialect-appropriate upsert, and the
/// test module — which cannot keep using a throwaway SQLite file once a server
/// is involved — is swapped for an env-gated one.
///
/// The test module is matched as an exact *suffix* of what `kodegen new` wrote.
/// Cutting at the first `#[cfg(test)]` instead would silently eat everything
/// below a `#[cfg(test)] use ...;` the user put above their tests.
fn rewrite_repo(content: &str, cap: Relational, pkg: &str) -> Result<String> {
    let template = generated(DIESEL_REPO, pkg)?;
    let start = template
        .find("#[cfg(test)]")
        .ok_or("the built-in hexagonal template's diesel_repo.rs has no #[cfg(test)] block")?;
    let head = content.strip_suffix(&template[start..]).ok_or_else(|| {
        refusal(DIESEL_REPO, "the generated `#[cfg(test)] mod tests` block, unchanged and last in the file", cap)
    })?;
    if !head.contains(SAVE_REPLACE_INTO) {
        return Err(refusal(DIESEL_REPO, "the generated `replace_into` upsert in save()", cap).into());
    }
    Ok(head.replacen(SAVE_REPLACE_INTO, &cap.render(SAVE_UPSERT), 1) + &cap.render(REPO_TESTS_TPL))
}

/// Rewrites every seed migration kodegen owns, and leaves anything else alone.
fn migrations(project: &Path, cap: Relational) -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir(project.join("migrations")) else {
        warn_no_seed(cap);
        return Vec::new();
    };
    let mut found = false;
    let mut seeds = Vec::new();
    // a rewrite to the bytes that are already there is not a change: `add
    // postgres` leaves the SQLite seed's `up.sql` exactly as it found it, and
    // both dialects leave `down.sql` alone. Reporting them would make `--dry-run`
    // name files it is not going to touch.
    let mut rewrite = |rel: String, current: &str, wanted: &str| {
        if current.trim() != wanted.trim() {
            seeds.push((rel, wanted.to_string()));
        }
    };
    for e in entries.flatten().filter(|e| e.path().is_dir()) {
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = format!("migrations/{name}/up.sql");
        // a directory with no up.sql is not a migration we can judge, let alone
        // one of ours — saying so on every run is noise, not a warning
        let Ok(content) = fs::read_to_string(project.join(&rel)) else {
            continue;
        };
        if is_known(KNOWN_SEEDS, &content) {
            found = true;
            rewrite(rel, &content, cap.up_sql());
            // the hypertable variant's `up.sql` creates a second table, so a
            // `down.sql` that still only drops `users` would leave the revert
            // half-done and the next migrate failing on `CREATE TABLE events`.
            // Rewritten only while it is still one kodegen wrote — a revert the
            // user has edited is theirs.
            let down = format!("migrations/{name}/down.sql");
            if let Ok(current) = fs::read_to_string(project.join(&down))
                && is_known(KNOWN_DOWNS, &current)
            {
                rewrite(down, &current, cap.down_sql());
            }
        } else {
            eprintln!(
                "warning: migrations/{name} was not generated by kodegen — check its SQL is valid \
                 {} before running it",
                cap.dialect()
            );
        }
    }
    // "nothing to rewrite" and "nothing of ours to rewrite" are different: the
    // first is what `add postgres` does to a SQLite seed that already matches
    if !found {
        warn_no_seed(cap);
    }
    // read_dir order is not defined; the plan `--dry-run` prints must be stable
    seeds.sort();
    seeds
}

fn warn_no_seed(cap: Relational) {
    eprintln!(
        "warning: no kodegen-generated migration found under migrations/ — \
         create the users table for {} yourself",
        cap.dialect()
    );
}

/// Line-anchored, so a `services:` in a comment or nested under another key
/// cannot be mistaken for the top-level one a compose file hangs off.
fn add_compose_service(compose: &str, name: &str, block: &str) -> Result<String> {
    let mut lines: Vec<&str> = compose.lines().collect();
    let at = lines.iter().position(|l| l.trim_end() == "services:").ok_or_else(|| {
        format!(
            "{COMPOSE} has no top-level `services:` line — put one back (or delete the file and rerun) \
             so `kodegen add` knows where to write the {name} service"
        )
    })?;
    let header = format!("  {name}:");
    if lines.iter().any(|l| l.trim_end() == header) {
        return Err(format!(
            "{COMPOSE} already defines a `{name}` service — remove it first, or keep the one you have"
        )
        .into());
    }
    lines.insert(at + 1, block.trim_end_matches('\n'));
    Ok(lines.join("\n") + "\n")
}

/// Refuse rather than clobber: the file no longer looks like what kodegen
/// wrote, so overwriting it would silently drop the user's own code.
fn refusal(rel: &str, signature: &str, cap: Relational) -> String {
    format!(
        "{rel} no longer matches what kodegen generated (expected {signature}) — \
         refusing to overwrite your changes.\nSwitch to {} by hand instead:\n  \
         1. Cargo.toml: `default = [\"{}\"]`\n  \
         2. src/infrastructure/diesel_db.rs: replace `SqliteConnection` with `{}` \
         (and drop the SQLite-only WAL/pragma setup)\n  \
         3. migrations/*_create_users/up.sql: `{}`\n  \
         4. run with DATABASE_URL={}",
        cap.dialect(),
        // this refusal only ever comes from the Diesel path: it is about
        // `diesel_db.rs`, which the SeaORM path does not touch at all
        cap.feature(Orm::Diesel),
        cap.connection(),
        cap.up_sql().trim(),
        cap.database_url()
    )
}

/// Whether a migration file is still one kodegen wrote. Trimmed on both sides:
/// the bodies below are written as they appear in a file, and an editor that adds
/// or drops a trailing newline has not made the SQL somebody else's.
fn is_known(bodies: &[&str], content: &str) -> bool {
    bodies.iter().any(|body| body.trim() == content.trim())
}

/// Seed `up.sql` bodies kodegen has generated — used to tell "still ours" from
/// "the user wrote their own schema".
const KNOWN_SEEDS: &[&str] = &[
    "CREATE TABLE users (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL);",
    "CREATE TABLE users (id VARCHAR(64) PRIMARY KEY NOT NULL, name TEXT NOT NULL);",
    TIMESCALE_UP_SQL,
];

/// The same, for the revert half. Only TimescaleDB writes a `down.sql` of its
/// own, but both bodies are listed so that undoing it by hand and re-running
/// `add postgres` still recognises the file as kodegen's.
const KNOWN_DOWNS: &[&str] = &[USERS_DOWN_SQL, TIMESCALE_DOWN_SQL];

const USERS_DOWN_SQL: &str = "DROP TABLE users;\n";

/// The `users` table as the SeaORM path creates it — the migration's `CREATE
/// TABLE` with `IF NOT EXISTS` and no trailing semicolon, because these are
/// executed one statement at a time rather than read by a migration harness.
/// Same column types as the Diesel path, for the same reasons: MySQL cannot
/// index a `TEXT` column without a prefix length.
const USERS_DDL_PG: &str =
    "CREATE TABLE IF NOT EXISTS users (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL)";
const USERS_DDL_MYSQL: &str =
    "CREATE TABLE IF NOT EXISTS users (id VARCHAR(64) PRIMARY KEY NOT NULL, name TEXT NOT NULL)";

/// TimescaleDB is PostgreSQL plus an extension, so the users half is the
/// PostgreSQL migration verbatim — same driver, same types, same upsert. What
/// the capability actually adds is below it: the extension, and one example
/// hypertable to show what it is for.
///
/// `CREATE EXTENSION` comes first because Diesel runs each migration inside one
/// transaction and TimescaleDB requires its extension to be created before
/// anything else in that transaction.
const TIMESCALE_UP_SQL: &str = r##"CREATE EXTENSION IF NOT EXISTS timescaledb;

CREATE TABLE users (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL);

-- An example, and the only thing here that is not plain PostgreSQL: a
-- hypertable is a normal table that the extension partitions by time for you.
-- Nothing in the generated Rust reads it — replace it with your own measurement
-- table (`kodegen add clickhouse`/`influxdb`/`greptimedb` write the app-side
-- `EventSink` port instead, which is a different thing and can be added
-- alongside).
CREATE TABLE events (
    recorded_at TIMESTAMPTZ NOT NULL,
    name TEXT NOT NULL,
    value DOUBLE PRECISION NOT NULL
);
SELECT create_hypertable('events', by_range('recorded_at'));
"##;

/// Drops the example table too: a `down.sql` that only dropped `users` would
/// leave `events` behind, and the next `migration run` would fail on
/// `CREATE TABLE events`. The extension is left alone on purpose — dropping it
/// would take every other hypertable in the database with it.
const TIMESCALE_DOWN_SQL: &str = "DROP TABLE events;\nDROP TABLE users;\n";

const SAVE_REPLACE_INTO: &str = r##"                diesel::replace_into(users::table)
                    .values(UserRow {
                        id: user.id,
                        name: user.name,
                    })
                    .execute(conn)"##;

const SAVE_UPSERT: &str = r##"                {{upsert_note}}
                let name = user.name.clone();
                diesel::insert_into(users::table)
                    .values(UserRow {
                        id: user.id,
                        name: user.name,
                    })
                    .on_conflict({{conflict_target}})
                    .do_update()
                    .set(users::name.eq(name))
                    .execute(conn)"##;

const DIESEL_DB_TPL: &str = r##"//! Shared Diesel/{{Dialect}} plumbing for every repository: one pool, one
//! spawn_blocking entrypoint, one error mapping. A new repository only
//! needs a `Db` clone and its own queries.
//!
//! Written by `kodegen add {{capability}}`. The connection type is the only
//! dialect-specific thing in here — `Db::run`, the ports and the repositories
//! above it are unchanged, which is the whole point of keeping the pool in one
//! place.

use crate::domain::{DomainError, Readiness};
use async_trait::async_trait;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

pub type DbPool = Pool<ConnectionManager<{{Conn}}>>;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

/// Maps any infrastructure error (pool, diesel, join) to `DomainError::Other`.
pub fn infra(e: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Other(Box::new(e))
}

/// Builds the shared pool and runs pending migrations.
/// Owned by the composition root (bootstrap), not by repositories.
pub fn build_pool(database_url: &str) -> Result<DbPool, Box<dyn std::error::Error + Send + Sync>> {
    // migrate on one connection before the pool exists: no other connection is
    // open yet, so nothing can race the schema change at startup
    let mut setup = {{Conn}}::establish(database_url)?;
    setup.run_pending_migrations(MIGRATIONS)?;
    drop(setup);

    let manager = ConnectionManager::<{{Conn}}>::new(database_url);
    Ok(Pool::builder().build(manager)?)
}

/// Cheap-to-clone handle every Diesel repository shares.
#[derive(Clone)]
pub struct Db {
    pool: DbPool,
}

impl Db {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }

    /// Runs a blocking Diesel closure on the blocking thread pool with a
    /// connection checked out from the shared pool.
    pub async fn run<T, F>(&self, f: F) -> Result<T, DomainError>
    where
        T: Send + 'static,
        F: FnOnce(&mut {{Conn}}) -> Result<T, DomainError> + Send + 'static,
    {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = pool.get().map_err(infra)?;
            f(&mut conn)
        })
        .await
        .map_err(infra)?
    }
}

/// The `/ready` probe gives each check the same 2s, and past it the answer is
/// already "timed out" — so this is how long a connection is worth waiting for.
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[async_trait]
impl Readiness for Db {
    fn name(&self) -> &'static str {
        "{{capability}}"
    }

    /// A connection from the pool and one statement through it — the whole
    /// path a query takes, minus the query.
    async fn ready(&self) -> Result<(), DomainError> {
        let pool = self.pool.clone();
        tokio::task::spawn_blocking(move || {
            // `get_timeout`, not `Db::run`'s `pool.get()`: the probe's async 2s
            // timeout cannot cancel a closure already on the blocking pool, so a
            // dead database parks that thread for r2d2's 30s default while the
            // gRPC loop fires another probe every 10s — and dropping the runtime
            // waits for all of them, overrunning the 10s drain bound. The pool's
            // own `connection_timeout` is left alone: that one governs real
            // requests.
            let mut conn = pool.get_timeout(READY_TIMEOUT).map_err(infra)?;
            diesel::sql_query("SELECT 1")
                .execute(&mut conn)
                .map(|_| ())
                .map_err(infra)
        })
        .await
        .map_err(infra)?
    }
}
"##;

const REPO_TESTS_TPL: &str = r##"#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::diesel_db::{build_pool, DbPool};
    use std::sync::OnceLock;

    /// {{Dialect}} needs a server, so these are gated on `DATABASE_URL` rather
    /// than `#[ignore]`: `docker compose -f docker-compose.dev.yml up -d` plus
    /// the env var is all it takes to run them, and CI stays green without one
    /// (the in-memory adapter is what runs when `DATABASE_URL` is unset —
    /// exactly how the app itself picks its adapter).
    ///
    /// One pool for the whole test binary: `build_pool` runs the migrations and
    /// tests run concurrently, so a pool per test would have them racing each
    /// other's `CREATE TABLE`.
    fn repo() -> Option<DieselUserRepository> {
        static POOL: OnceLock<Option<DbPool>> = OnceLock::new();
        let pool = POOL.get_or_init(|| {
            let url = std::env::var("DATABASE_URL").ok()?;
            Some(build_pool(&url).expect("pool init"))
        });
        Some(DieselUserRepository::new(Db::new(pool.clone()?)))
    }

    #[tokio::test]
    async fn save_then_get_roundtrips() {
        let Some(repo) = repo() else {
            eprintln!("DATABASE_URL unset — skipping the {{Dialect}} roundtrip test");
            return;
        };

        // save is an upsert, so re-running this against a live database must not
        // fail on the row the previous run left behind
        let user = User::new("u1", "Alice").expect("valid user");
        repo.save(user).await.expect("save");

        let got = repo.get("u1").await.expect("get");
        assert_eq!(got.name, "Alice");
    }

    #[tokio::test]
    async fn get_missing_is_not_found() {
        let Some(repo) = repo() else {
            return;
        };

        let err = repo.get("no-such-user").await.unwrap_err();
        assert!(matches!(err, DomainError::NotFound));
    }
}
"##;

/// The three blocks of the generated `bootstrap.rs` that name the Diesel handle,
/// and what `add mongodb` puts in their place. Matched verbatim — see
/// `rewrite_bootstrap`.
const BOOTSTRAP_USE: &str = r##"        diesel_db::{build_pool, Db},
        diesel_repo::DieselUserRepository,
        grpc_greeter::GrpcGreeter,
        in_memory_repo::InMemoryUserRepository,
"##;

/// `mongo_repo` last: the list is sorted, and `cargo fmt --check` runs in the
/// generated project's CI.
const BOOTSTRAP_USE_MONGO: &str = r##"        grpc_greeter::GrpcGreeter,
        in_memory_repo::InMemoryUserRepository,
        mongo_repo::{Mongo, MongoUserRepository},
"##;

/// What `kodegen feature` writes for a feature that has no adapter of its own
/// yet: the handle is passed in and unused, so only its type has to move.
///
/// No parentheses in the pattern. `kodegen feature` runs `cargo fmt`, and a long
/// enough feature name pushes the signature over the width limit — rustfmt then
/// puts the parameter on its own line, so `(_db:` and `)` are no longer next to
/// it. The parameter itself is what identifies the scaffolded builder, and `_db`
/// rather than `db` is what keeps it from matching a real adapter's.
const FEATURE_BUILDER_DB: &str = "_db: Option<&Db>";
const FEATURE_BUILDER_MONGO: &str = "_db: Option<&Mongo>";

const BOOTSTRAP_DB: &str = r##"    let db = std::env::var("DATABASE_URL")
        .ok()
        .map(|url| Db::new(build_pool(&url).expect("diesel pool init failed")));"##;

/// Still called `db`, and still resolved in the same place: the datastore slot
/// is one slot, so both `api::Services` literals keep working untouched.
const BOOTSTRAP_DB_MONGO: &str = r##"    // no pool to build and no migrations to run: the client is created on first
    // use (see mongo_repo.rs), so a database that is not up yet cannot stop the
    // service booting — the same property the lazy Redis connection has
    let db = std::env::var("MONGODB_URL").ok().map(|uri| {
        Mongo::new(
            uri,
            std::env::var("MONGODB_DB").unwrap_or_else(|_| "app".into()),
        )
    });"##;

const BOOTSTRAP_BUILD_USER: &str = r##"fn build_user_service(db: Option<&Db>) -> Arc<dyn UserService> {
    let repo: Arc<dyn UserRepository> = match db {
        Some(db) => Arc::new(DieselUserRepository::new(db.clone())),
        None => Arc::new(InMemoryUserRepository::new()),
    };
    Arc::new(UserServiceImpl::new(repo))
}"##;

const BOOTSTRAP_BUILD_USER_MONGO: &str = r##"fn build_user_service(db: Option<&Mongo>) -> Arc<dyn UserService> {
    let repo: Arc<dyn UserRepository> = match db {
        Some(db) => Arc::new(MongoUserRepository::new(db.clone())),
        None => Arc::new(InMemoryUserRepository::new()),
    };
    Arc::new(UserServiceImpl::new(repo))
}"##;

const MONGO_REPO_TPL: &str = r##"//! `UserRepository` adapter over MongoDB. Written by `kodegen add mongodb`.
//!
//! No new port: this is the second adapter of the port the template already
//! ships, which is why the application service, the routes and `tests/api.rs`
//! are untouched by `add mongodb` — and why the port's contract is what this
//! file has to honor rather than reinterpret. `save` is an **upsert** (an
//! existing id is overwritten, never `NotFound`), `get` on a missing id is
//! `NotFound`.
//!
//! The handle is owned by the composition root (`bootstrap.rs`) and handed in,
//! exactly like the Diesel `Db`: a repository never reads `MONGODB_URL` itself.

use crate::domain::{DomainError, Readiness, User, UserRepository};
use async_trait::async_trait;
use mongodb::bson::doc;
use mongodb::{Client, Collection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::OnceCell;

/// The stored document. `_id` *is* the user's id rather than a generated
/// ObjectId: it is already unique and is what the port looks users up by, so a
/// separate key would only be a second thing to keep in sync — and it is what
/// makes the upsert below a single matched write.
#[derive(Serialize, Deserialize)]
struct UserDoc {
    #[serde(rename = "_id")]
    id: String,
    name: String,
}

/// Cheap-to-clone handle every Mongo repository shares — this backend's `Db`.
///
/// The client is built on **first use**, not at startup: every constructor the
/// driver offers is async (`mongodb+srv://` URIs are resolved through DNS) and
/// `build_services_from_env()` is a plain `fn`. Deferring it costs one
/// `OnceCell` and avoids blocking a runtime thread to open a connection nobody
/// has asked to use yet.
#[derive(Clone)]
pub struct Mongo(Arc<MongoInner>);

struct MongoInner {
    uri: String,
    database: String,
    client: OnceCell<Client>,
}

impl Mongo {
    pub fn new(uri: impl Into<String>, database: impl Into<String>) -> Self {
        Self(Arc::new(MongoInner {
            uri: uri.into(),
            database: database.into(),
            client: OnceCell::new(),
        }))
    }

    /// The shared client, dialing on the first call. `Client` owns its own
    /// connection pool, so every caller must get the *same* one — that is what
    /// the `OnceCell` behind the `Arc` is for, rather than a client per clone.
    ///
    /// A failure here is not cached: `get_or_try_init` leaves the cell empty, so
    /// a server that was down when the first request arrived is retried on the
    /// next one instead of poisoning the process.
    async fn database(&self) -> Result<mongodb::Database, DomainError> {
        let client = self
            .0
            .client
            .get_or_try_init(|| Client::with_uri_str(&self.0.uri))
            .await
            .map_err(infra)?;
        Ok(client.database(&self.0.database))
    }
}

/// A server that cannot be reached is `Unavailable`, which `api::to_http_err`
/// turns into a 503 — the same treatment `GrpcGreeter` gives a dead upstream.
/// Anything the server itself answered (a command or write error) is a real
/// `Other`: the dependency is up, the request was wrong.
pub fn infra(e: mongodb::error::Error) -> DomainError {
    use mongodb::error::ErrorKind;
    match *e.kind {
        ErrorKind::Io(_)
        | ErrorKind::ServerSelection { .. }
        | ErrorKind::ConnectionPoolCleared { .. }
        | ErrorKind::DnsResolve { .. } => DomainError::Unavailable,
        _ => DomainError::Other(Box::new(e)),
    }
}

#[async_trait]
impl Readiness for Mongo {
    fn name(&self) -> &'static str {
        "mongodb"
    }

    /// The first call is the first connection; afterwards the server's own
    /// `ping` command, which needs no collection to exist.
    async fn ready(&self) -> Result<(), DomainError> {
        self.database()
            .await?
            .run_command(doc! { "ping": 1 })
            .await
            .map(|_| ())
            .map_err(infra)
    }
}

pub struct MongoUserRepository {
    mongo: Mongo,
}

impl MongoUserRepository {
    pub fn new(mongo: Mongo) -> Self {
        Self { mongo }
    }

    async fn users(&self) -> Result<Collection<UserDoc>, DomainError> {
        Ok(self.mongo.database().await?.collection("users"))
    }
}

#[async_trait]
impl UserRepository for MongoUserRepository {
    async fn save(&self, user: User) -> Result<(), DomainError> {
        let doc = UserDoc {
            id: user.id,
            name: user.name,
        };
        // replace_one + upsert, not insert_one: the port documents `save` as an
        // upsert, so an id that is already there is overwritten rather than
        // answered with a duplicate-key error. There is no `migrations/` step to
        // go with it — the collection appears with the first write.
        self.users()
            .await?
            .replace_one(doc! { "_id": &doc.id }, &doc)
            .upsert(true)
            .await
            .map(|_| ())
            .map_err(infra)
    }

    async fn get(&self, id: &str) -> Result<User, DomainError> {
        self.users()
            .await?
            .find_one(doc! { "_id": id })
            .await
            .map_err(infra)?
            .map(|d| User {
                id: d.id,
                name: d.name,
            })
            .ok_or(DomainError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MongoDB needs a server, so these are gated on `MONGODB_URL` rather than
    /// `#[ignore]`: `docker compose -f docker-compose.dev.yml up -d` plus the env
    /// var is all it takes to run them, and CI stays green without one (the
    /// in-memory adapter is what runs when `MONGODB_URL` is unset — exactly how
    /// the app itself picks its adapter).
    fn repo() -> Option<MongoUserRepository> {
        let uri = std::env::var("MONGODB_URL").ok()?;
        let database = std::env::var("MONGODB_DB").unwrap_or_else(|_| "app".into());
        Some(MongoUserRepository::new(Mongo::new(uri, database)))
    }

    #[tokio::test]
    async fn save_then_get_roundtrips() {
        let Some(repo) = repo() else {
            eprintln!("MONGODB_URL unset — skipping the MongoDB roundtrip test");
            return;
        };

        let user = User::new("u1", "Alice").expect("valid user");
        repo.save(user).await.expect("save");
        assert_eq!(repo.get("u1").await.expect("get").name, "Alice");

        // save is an upsert on every adapter, which against a real server is two
        // things at once: the same id twice must update rather than fail, and
        // re-running this test must not trip over the document the last run left
        repo.save(User::new("u1", "Alicia").expect("valid user"))
            .await
            .expect("saving an existing id must update it");
        assert_eq!(repo.get("u1").await.expect("get").name, "Alicia");
    }

    #[tokio::test]
    async fn get_missing_is_not_found() {
        let Some(repo) = repo() else {
            return;
        };

        let err = repo.get("no-such-user").await.unwrap_err();
        assert!(matches!(err, DomainError::NotFound));
    }
}
"##;

/// The SeaORM half of the three swapped blocks. `sea_db`/`sea_repo` come last
/// because the list is sorted and `cargo fmt --check` runs in the generated
/// project's CI.
///
/// Nothing in these three constants may contain the word "diesel" in any case:
/// that is exactly what `SEA_SWAP.leftovers` looks for once the swap is done,
/// so a comment mentioning the other ORM would make the command refuse its own
/// output.
const BOOTSTRAP_USE_SEA: &str = r##"        grpc_greeter::GrpcGreeter,
        in_memory_repo::InMemoryUserRepository,
        sea_db::Db,
        sea_repo::SeaUserRepository,
"##;

/// Still called `db`, still resolved in the same place, still `DATABASE_URL`:
/// the datastore slot is one slot and this is the same server, so both
/// `api::Services` literals keep working untouched.
const BOOTSTRAP_DB_SEA: &str = r##"    // no pool to build and no migration step: the connection is opened on first
    // use and the schema applied with it (see sea_db.rs), so a database that is
    // not up yet cannot stop the service booting — the same property the lazy
    // Redis connection has
    let db = std::env::var("DATABASE_URL").ok().map(Db::new);"##;

const BOOTSTRAP_BUILD_USER_SEA: &str = r##"fn build_user_service(db: Option<&Db>) -> Arc<dyn UserService> {
    let repo: Arc<dyn UserRepository> = match db {
        Some(db) => Arc::new(SeaUserRepository::new(db.clone())),
        None => Arc::new(InMemoryUserRepository::new()),
    };
    Arc::new(UserServiceImpl::new(repo))
}"##;

const SEA_DB_TPL: &str = r##"//! Shared SeaORM/{{Dialect}} plumbing for every repository: one connection
//! pool, one schema, one error mapping. A new repository only needs a `Db`
//! clone and its own queries.
//!
//! Written by `kodegen add {{capability}} --orm seaorm`.
//!
//! **This handle is called `Db` deliberately.** The Diesel one is called `Db`
//! too, and the two can never be compiled together (`src/lib.rs` refuses it), so
//! sharing the name is what makes switching ORMs cost nothing above this layer:
//! the composition root's binding keeps its name and every builder `kodegen
//! feature` scaffolded (`_db: Option<&Db>`) keeps compiling in both directions.
//! Two types, two modules, one at a time — say
//! `use crate::infrastructure::sea_db::Db;` (rather than the Diesel module's)
//! when a file has to name which one it means. Only the name is shared: there is
//! no `Db::run` here, because nothing blocks.
//!
//! **There are no versioned migrations on this path.** `SCHEMA` below is applied
//! on the first connection, every statement idempotent, and `migrations/` is not
//! read at all — the same deal the Mongo, ClickHouse and S3 adapters already
//! make. That is fine while the schema is the seed. The first time you change a
//! table that already holds production data, move to versioned migrations
//! (`sea-orm-migration = "1.1"`, pinned: its 2.x needs rustc 1.94) and delete
//! this constant — `CREATE TABLE IF NOT EXISTS` cannot express an `ALTER`, and a
//! startup that silently does nothing is worse than one that fails.
//!
//! **The role in `DATABASE_URL` must be allowed to create tables**, and that is
//! not softened by the tables already existing: PostgreSQL checks `CREATE` on
//! the schema *before* it checks whether the object is there, so
//! `CREATE TABLE IF NOT EXISTS users` answers `permission denied for schema
//! public` to a role without it even when `users` has existed for a year
//! (measured, not assumed). What that looks like is a failing *request*, never a
//! failing boot — the connection is opened lazily, the schema goes with it, and
//! the error is a 500 (the server answered; only an unreachable server is the
//! 503 case) repeated on every request forever, because a failed init is not
//! cached.
//!
//! So in an environment where the application role does not own the schema,
//! pick one: `GRANT CREATE ON SCHEMA public TO <role>` (it creates nothing after
//! the first boot — the grant is what lets it *ask*), or empty `SCHEMA` here and
//! own the schema out of band, which is the same move as going to versioned
//! migrations and is one edit in this file.
//!
//! **A database that is down costs the request 30 seconds** — sqlx's default
//! acquire timeout — before the 503, and every request until it is back. Add
//! `opts.acquire_timeout(Duration::from_secs(2));` below to find out sooner.
//!
//! **`CREATE TABLE IF NOT EXISTS` is idempotent, not atomic.** Inside one
//! process that does not matter — there is one handle, so the `OnceCell` below
//! runs the schema exactly once. Across processes it does: two instances whose
//! *first* connection lands at the same moment on a database that has neither
//! table yet can collide, and PostgreSQL answers one of them with a duplicate
//! key on `pg_type` rather than "it exists now". That is what the retry in
//! `conn()` is for, and it is the cheapest thing that turns a startup race into
//! a no-op. It retries only what the server answered — a connection error is
//! returned as it is, so a database that is down still costs one acquire
//! timeout and not two. It is not a substitute for migrations once the schema is
//! worth versioning.

use crate::domain::{DomainError, Readiness};
use async_trait::async_trait;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbErr};
use std::sync::Arc;
use tokio::sync::OnceCell;

/// The schema this adapter needs, applied once when the connection is opened.
///
/// One statement per entry rather than one string with semicolons: multiple
/// statements in a single round trip are not enabled by default on every
/// backend, and one at a time behaves the same everywhere. Every statement is
/// idempotent, which is what makes running this on every boot a no-op rather
/// than an error on the second one.
const SCHEMA: &[&str] = &[
{{schema}}];

/// Cheap-to-clone handle every repository here shares.
///
/// The connection is opened on **first use**, not at startup:
/// `Database::connect` is async and `build_services_from_env()` is a plain
/// `fn`. Deferring it costs one `OnceCell` and means a database that is still
/// coming up cannot stop the process from booting — the request that arrives
/// first pays for the connection, and a `DomainError::Unavailable` answers 503
/// until it succeeds.
#[derive(Clone)]
pub struct Db(Arc<Inner>);

struct Inner {
    url: String,
    conn: OnceCell<DatabaseConnection>,
}

impl Db {
    pub fn new(url: impl Into<String>) -> Self {
        Self(Arc::new(Inner {
            url: url.into(),
            conn: OnceCell::new(),
        }))
    }

    /// The shared connection, dialing and applying `SCHEMA` on the first call.
    /// `DatabaseConnection` owns its own pool, so every caller must get the
    /// *same* one — that is what the `OnceCell` behind the `Arc` is for, rather
    /// than a connection per clone.
    ///
    /// A failure here is not cached: `get_or_try_init` leaves the cell empty, so
    /// a server that was down when the first request arrived is retried on the
    /// next one instead of poisoning the process.
    pub async fn conn(&self) -> Result<&DatabaseConnection, DomainError> {
        self.0
            .conn
            .get_or_try_init(|| async {
                let mut opts = ConnectOptions::new(self.0.url.as_str());
                // every statement at INFO otherwise, which is the application's
                // log budget spent on its own SQL
                opts.sqlx_logging(false);
                let conn = Database::connect(opts).await?;
                // `IF NOT EXISTS` is idempotent but **not atomic**: two
                // processes whose first connection lands at the same moment on a
                // database that does not have these objects yet can collide, and
                // PostgreSQL answers one of them with a duplicate key on
                // `pg_type` rather than "it exists now". The loser retries once,
                // by which time the statement is a no-op — and a second failure
                // is a real one, so it is returned rather than swallowed.
                //
                // Only for errors the *server* answered. A dead connection or an
                // exhausted pool will not have healed by the next line: the retry
                // would just pay the acquire timeout a second time (30s by
                // default, so 60 before the caller hears anything) and fail the
                // same way, so those are returned untried. A permission error
                // *is* server-answered, so it does get the second attempt — it
                // costs one round trip and then reports the same denial, which is
                // cheaper than a rule that tries to tell the two apart.
                for stmt in SCHEMA {
                    if let Err(e) = conn.execute_unprepared(stmt).await {
                        if matches!(e, DbErr::Conn(_) | DbErr::ConnectionAcquire(_)) {
                            return Err(e);
                        }
                        conn.execute_unprepared(stmt).await?;
                    }
                }
                Ok(conn)
            })
            .await
            .map_err(infra)
    }
}

/// A server that cannot be reached is `Unavailable`, which `api::to_http_err`
/// turns into a 503 — the same treatment `GrpcGreeter` gives a dead upstream.
/// Anything the server itself answered (a rejected statement, a constraint, a
/// permission) is a real `Other` and a 500: the dependency is up, the request or
/// the schema is wrong, and dressing that up as "try again later" would hide it.
pub fn infra(e: DbErr) -> DomainError {
    match e {
        DbErr::Conn(_) | DbErr::ConnectionAcquire(_) => DomainError::Unavailable,
        _ => DomainError::Other(Box::new(e)),
    }
}

#[async_trait]
impl Readiness for Db {
    fn name(&self) -> &'static str {
        "{{capability}}"
    }

    /// The first call is the first connection (and the schema); afterwards one
    /// round trip on the pool.
    async fn ready(&self) -> Result<(), DomainError> {
        self.conn().await?.ping().await.map_err(infra)
    }
}
"##;

const SEA_REPO_TPL: &str = r##"//! `UserRepository` adapter over SeaORM/{{Dialect}}. Written by
//! `kodegen add {{capability}} --orm seaorm`.
//!
//! No new port: this is another adapter of the port the template already ships,
//! which is why the application service, the routes and `tests/api.rs` are
//! untouched — and why the port's contract (`save` is an upsert, `get` on a
//! missing id is `NotFound`) is what this file has to honor rather than
//! reinterpret.
//!
//! The entity lives here next to the queries, the way `diesel::table!` does in
//! the other adapter. The handle is owned by the composition root
//! (`bootstrap.rs`) and handed in: a repository never reads `DATABASE_URL`.
//!
//! Nothing here blocks and nothing is spawned: sqlx is async to the socket, so
//! there is no `spawn_blocking`, no second pool and no closure to hand work to.

use crate::domain::{DomainError, User, UserRepository};
use crate::infrastructure::sea_db::{infra, Db};
use async_trait::async_trait;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, EntityTrait};

/// The stored row. `id` is the user's own id rather than a generated key: it is
/// already unique and is what the port looks users up by, which is also what
/// makes the upsert below a single write.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    pub name: String,
}

/// No foreign keys in the seed schema, so there is nothing to relate to yet —
/// the enum still has to exist for `EntityTrait`.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

pub struct SeaUserRepository {
    db: Db,
}

impl SeaUserRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl UserRepository for SeaUserRepository {
    async fn save(&self, user: User) -> Result<(), DomainError> {
        let row = ActiveModel {
            id: Set(user.id),
            name: Set(user.name),
        };
        // upsert, because that is what the port documents `save` as: an id that
        // is already there is overwritten, never answered with a duplicate-key
        // error. `update_column`, not `do_nothing` — ignoring the new value
        // would be a different contract (and MySQL has no `DO NOTHING` to begin
        // with).
        //
        // `exec_without_returning` rather than `exec`: an upsert whose new
        // values equal the old ones reports 0 affected rows on some backends,
        // and `exec` turns that into `DbErr::RecordNotInserted` — an error for a
        // `save` the port says must return `Ok(())`. This one returns the count
        // and leaves the judgment here, where the contract is known.
        Entity::insert(row)
            .on_conflict(
                OnConflict::column(Column::Id)
                    .update_column(Column::Name)
                    .to_owned(),
            )
            .exec_without_returning(self.db.conn().await?)
            .await
            .map(|_| ())
            .map_err(infra)
    }

    async fn get(&self, id: &str) -> Result<User, DomainError> {
        Entity::find_by_id(id)
            .one(self.db.conn().await?)
            .await
            .map_err(infra)?
            .map(|row| User {
                id: row.id,
                name: row.name,
            })
            .ok_or(DomainError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// {{Dialect}} needs a server, so these are gated on `DATABASE_URL` rather
    /// than `#[ignore]`: `docker compose -f docker-compose.dev.yml up -d` plus
    /// the env var is all it takes to run them, and CI stays green without one
    /// (the in-memory adapter is what runs when `DATABASE_URL` is unset —
    /// exactly how the app itself picks its adapter).
    ///
    /// A handle **per test**, not one shared by the binary the way the Diesel
    /// adapter shares its pool: `#[tokio::test]` builds a runtime per test and
    /// drops it at the end, and a sqlx pool belongs to the runtime that created
    /// it — shared, the second test to use it waits out the acquire timeout on
    /// connections whose runtime is gone. Which leaves these tests applying the
    /// schema concurrently on an empty database, and that is handled where it
    /// has to be for real deployments too: the retry in `sea_db.rs`.
    fn repo() -> Option<SeaUserRepository> {
        let url = std::env::var("DATABASE_URL").ok()?;
        Some(SeaUserRepository::new(Db::new(url)))
    }

    #[tokio::test]
    async fn save_then_get_roundtrips() {
        let Some(repo) = repo() else {
            eprintln!("DATABASE_URL unset — skipping the {{Dialect}} roundtrip test");
            return;
        };

        // save is an upsert, so re-running this against a live database must not
        // fail on the row the previous run left behind
        let user = User::new("u1", "Alice").expect("valid user");
        repo.save(user).await.expect("save");
        assert_eq!(repo.get("u1").await.expect("get").name, "Alice");

        repo.save(User::new("u1", "Alicia").expect("valid user"))
            .await
            .expect("saving an existing id must update it");
        assert_eq!(repo.get("u1").await.expect("get").name, "Alicia");
    }

    /// The case that is a *runtime* difference between backends rather than a
    /// compile-time one: an upsert whose values are unchanged affects 0 rows on
    /// MySQL, which sea-orm's `exec` reports as `RecordNotInserted`. The port
    /// says `save` returns `Ok(())`, so this test is the contract — and the
    /// reason `save` uses `exec_without_returning`. Both writes are in one test
    /// on purpose: split across two, a stale row from an earlier run would make
    /// the second one pass for the wrong reason.
    #[tokio::test]
    async fn save_twice_identical_values_is_ok() {
        let Some(repo) = repo() else {
            return;
        };

        let user = User::new("u2", "Bob").expect("valid user");
        repo.save(user.clone()).await.expect("save");
        repo.save(user)
            .await
            .expect("saving identical values again must still be Ok");
        assert_eq!(repo.get("u2").await.expect("get").name, "Bob");
    }

    #[tokio::test]
    async fn get_missing_is_not_found() {
        let Some(repo) = repo() else {
            return;
        };

        let err = repo.get("no-such-user").await.unwrap_err();
        assert!(matches!(err, DomainError::NotFound));
    }
}
"##;

/// The SQL Server half of the three swapped blocks, and under the same rule as
/// the SeaORM ones: **nothing in these three constants may contain the word
/// "diesel" in any case**, because that is exactly what `MSSQL_SWAP.leftovers`
/// looks for once the swap is done. A comment mentioning the other ORM would make
/// the command refuse its own output, on every project, forever.
const BOOTSTRAP_USE_MSSQL: &str = r##"        grpc_greeter::GrpcGreeter,
        in_memory_repo::InMemoryUserRepository,
        mssql_db::Db,
        mssql_repo::MssqlUserRepository,
"##;

/// Still called `db`, still resolved in the same place, still `DATABASE_URL` —
/// the datastore slot is one slot, so both `api::Services` literals keep working
/// untouched. What changes is the *shape* of the value: an ADO.NET connection
/// string, not a URL.
const BOOTSTRAP_DB_MSSQL: &str = r##"    // no pool to build and no migration step: the pool is opened on first use
    // and the schema applied with it (see mssql_db.rs), so a database that is
    // not up yet cannot stop the service booting — the same property the lazy
    // Redis connection has. The value is an ADO.NET connection string
    // (`server=tcp:host,1433;user id=...;database=...`), not a URL.
    let db = std::env::var("DATABASE_URL").ok().map(Db::new);"##;

const BOOTSTRAP_BUILD_USER_MSSQL: &str = r##"fn build_user_service(db: Option<&Db>) -> Arc<dyn UserService> {
    let repo: Arc<dyn UserRepository> = match db {
        Some(db) => Arc::new(MssqlUserRepository::new(db.clone())),
        None => Arc::new(InMemoryUserRepository::new()),
    };
    Arc::new(UserServiceImpl::new(repo))
}"##;

const MSSQL_DB_TPL: &str = r##"//! Shared SQL Server plumbing for every repository: one pool, one schema, one
//! error mapping. A new repository only needs a `Db` clone and its own queries.
//!
//! Written by `kodegen add mssql`.
//!
//! **This handle is called `Db` deliberately.** So are the Diesel and SeaORM
//! ones, and no two of them are ever compiled together, so sharing the name is
//! what makes switching datastores cost nothing above this layer: the
//! composition root's binding keeps its name and every builder `kodegen feature`
//! scaffolded (`_db: Option<&Db>`) keeps compiling in both directions. Only the
//! name is shared — say `use crate::infrastructure::mssql_db::Db;` when a file
//! has to name which one it means.
//!
//! **There is no ORM behind this file, on purpose.** Neither Diesel nor SeaORM
//! supports SQL Server in open source, so the queries here are SQL text with
//! bound parameters. Every value goes through `execute`/`query`'s parameter
//! array — `@P1`, `@P2` — which is a real TDS bind, not interpolation. There is
//! no `format!` anywhere in this adapter and there must not be one: that is the
//! only thing standing between this file and SQL injection.
//!
//! **There are no versioned migrations on this path.** `SCHEMA` below is applied
//! when the pool is opened, every statement idempotent, and `migrations/` is not
//! read at all — the same deal the Mongo, SeaORM, ClickHouse and S3 adapters
//! already make. The first time you change a table that already holds production
//! data, move to a migration tool and empty this constant: `IF OBJECT_ID(...) IS
//! NULL CREATE TABLE` cannot express an `ALTER`, and a startup that silently does
//! nothing is worse than one that fails.
//!
//! **The database itself is not created here.** SQL Server's image has no
//! `POSTGRES_DB` equivalent — it boots with master/tempdb/model/msdb — so
//! `docker-compose.dev.yml` runs a one-shot service that issues
//! `CREATE DATABASE app`, and a real deployment does the same thing in its
//! deployment step. That is not a gap: an application role in production is
//! almost never granted `CREATE DATABASE`. If it is missing you will see SQL
//! Server's own words — "Cannot open database ... requested by the login" — in a
//! 500 on every request, because `infra` below passes the server's message
//! through rather than swallowing it.
//!
//! **The role in `DATABASE_URL` must be allowed to create tables**, for the same
//! reason and with the same symptom: a failing *request*, never a failing boot,
//! because the pool is opened lazily and a failed init is not cached.
//!
//! **Collation matters here in a way it does not on PostgreSQL.** The image's
//! default (`SQL_Latin1_General_CP1_CI_AS`) is case-**in**sensitive, so `"u1"`
//! and `"U1"` are the same primary key — MySQL's behaviour, not PostgreSQL's.
//!
//! **`NVARCHAR`, never `VARCHAR`.** tiberius sends every Rust string as
//! `NVARCHAR`; against a `VARCHAR` column that makes each `WHERE id = @P1` an
//! implicit conversion, which turns an index seek into a scan. Nothing warns —
//! it is a pure performance failure, the same class as MySQL's `TEXT PRIMARY KEY`.
//!
//! **On a macOS host, `TrustServerCertificate=true` does not connect.** This is
//! the driver, not this file, and it is worth knowing before you spend an hour on
//! it. `native-tls` is OpenSSL on Linux but Apple's Secure Transport on macOS,
//! and tiberius 0.12.3's trust-everything path also turns SNI off — which Secure
//! Transport will not do, so the handshake is torn down and you get
//! `Tls("connection closed via error")`. Ordinary validation works there (point
//! it at a server with a real certificate and it connects), and the whole
//! `TrustServerCertificate` path works on Linux, which is what the runtime image
//! and CI are. On a Mac, run the service in a container, or talk to the local
//! dev server with `encrypt=DANGER_PLAINTEXT` in place of
//! `encrypt=true;TrustServerCertificate=true` — **only** against a database on
//! your own machine: that setting sends the login packet in the clear.

use crate::domain::{DomainError, Readiness};
use async_trait::async_trait;
use bb8::{Pool, PooledConnection, RunError};
use bb8_tiberius::ConnectionManager;
use std::sync::Arc;
use tiberius::error::Error as TiberiusError;
use tokio::sync::OnceCell;

/// The schema this adapter needs, applied once when the pool is opened.
///
/// **SQL Server has no `CREATE TABLE IF NOT EXISTS`** — not in 2022, not in 2025
/// — so idempotency is spelled `IF OBJECT_ID(N'dbo.users', N'U') IS NULL`.
/// Copying the `CREATE TABLE IF NOT EXISTS` from another adapter here is a
/// *runtime* syntax error, not a compile error.
///
/// `dbo` explicitly, so there is no `CREATE SCHEMA` to run — that statement has
/// to be first in its batch, while a `CREATE TABLE` nests inside an `IF` fine.
///
/// **The widths are the domain's limits, not round numbers.** `User::new`
/// accepts an id of up to 64 bytes and a name of up to 256, so a column narrower
/// than that would reject a value the domain calls valid — and on this backend it
/// would *fail* rather than truncate: SQL Server runs with `ANSI_WARNINGS ON`, so
/// an over-long string is error 2628, which arrives here as a 500 on a request
/// every other datastore accepts. 256 UTF-16 units covers 256 UTF-8 bytes exactly
/// (the widest character costs 4 bytes and 2 units), so the two limits line up
/// with nothing left over.
///
/// One statement per entry, like every other adapter here.
const SCHEMA: &[&str] = &[
    "IF OBJECT_ID(N'dbo.users', N'U') IS NULL
         CREATE TABLE dbo.users (
             id   NVARCHAR(64)  NOT NULL PRIMARY KEY,
             name NVARCHAR(256) NOT NULL
         )",
];

/// Cheap-to-clone handle every repository here shares.
///
/// The pool is built on **first use**, not at startup: opening it dials the
/// server and `build_services_from_env()` is a plain `fn`. Deferring it costs one
/// `OnceCell` and means a database that is still coming up cannot stop the
/// process from booting — the request that arrives first pays for the
/// connection, and a `DomainError::Unavailable` answers 503 until it succeeds.
#[derive(Clone)]
pub struct Db(Arc<Inner>);

struct Inner {
    /// An **ADO.NET** connection string, which is what `tiberius::Config` parses
    /// — `server=tcp:host,1433;user id=sa;password=...;database=app;encrypt=true`.
    /// Not a URL: a `postgres://...` left in `DATABASE_URL` by a previous
    /// backend fails to parse on the first request, loudly.
    ado: String,
    pool: OnceCell<Pool<ConnectionManager>>,
}

impl Db {
    pub fn new(ado: impl Into<String>) -> Self {
        Self(Arc::new(Inner {
            ado: ado.into(),
            pool: OnceCell::new(),
        }))
    }

    /// A connection from the shared pool, opening it and applying `SCHEMA` on
    /// the first call.
    ///
    /// A failure here is not cached: `get_or_try_init` leaves the cell empty, so
    /// a server that was down when the first request arrived is retried on the
    /// next one instead of poisoning the process.
    pub async fn conn(&self) -> Result<PooledConnection<'_, ConnectionManager>, DomainError> {
        let pool = self
            .0
            .pool
            .get_or_try_init(|| async {
                let manager = ConnectionManager::build(self.0.ado.as_str()).map_err(manager_infra)?;
                // `min_idle(1)` is what makes the next line report a real error,
                // and it is not a tuning knob. bb8's `build()` opens
                // `min_idle` connections and returns the first failure verbatim
                // — but `min_idle` defaults to *none*, which means zero, which
                // makes `build()` identical to `build_unchecked()`: it returns
                // `Ok` without ever dialling, and the first thing the caller
                // hears is bb8's 30-second acquire timeout as a bare `TimedOut`
                // with the server's own words thrown away. Those words are the
                // whole diagnosis for the two mistakes this backend invites —
                // "Cannot open database 'app' requested by the login" (nobody
                // ran CREATE DATABASE) and "Login failed for user 'sa'". With
                // one idle connection asked for, they arrive on the first
                // request instead. Measured against a real server, not assumed.
                //
                // `retry_connection(false)` is the other half of it. bb8's
                // default is to keep retrying a failing connect with backoff
                // until `connection_timeout` (30s) expires and *then* report —
                // so a wrong password costs every request 30 seconds before the
                // 500. That retry is already covered one layer up and better:
                // `get_or_try_init` does not cache a failure, so the next
                // request opens the pool again. Once here, once per request,
                // instead of eight times inside one.
                let pool = Pool::builder()
                    .max_size(8)
                    .min_idle(1)
                    .retry_connection(false)
                    .build(manager)
                    .await
                    .map_err(manager_infra)?;
                {
                    let mut conn = pool.get().await.map_err(pool_infra)?;
                    // The DDL is idempotent but **not atomic**: two processes
                    // whose first connection lands at the same moment on a
                    // database that does not have the table yet can collide, and
                    // SQL Server answers one of them with error 2714 ("There is
                    // already an object named 'users'") rather than "it exists
                    // now". The loser retries once, by which time the statement
                    // is a no-op — and a second failure is a real one, so it is
                    // returned rather than swallowed.
                    //
                    // Only for errors the *server* answered. A dead connection
                    // will not have healed by the next line: retrying it would
                    // pay the same failure twice before the caller hears
                    // anything, so it is returned untried. A permission error
                    // *is* server-answered, so it does get the second attempt —
                    // one round trip, then the same denial, which is cheaper than
                    // a rule that tries to tell the two apart.
                    for stmt in SCHEMA {
                        if let Err(e) = conn.execute(*stmt, &[]).await {
                            if !matches!(e, TiberiusError::Server(_)) {
                                return Err(infra(e));
                            }
                            conn.execute(*stmt, &[]).await.map_err(infra)?;
                        }
                    }
                }
                Ok(pool)
            })
            .await?;
        pool.get().await.map_err(pool_infra)
    }
}

/// A server that cannot be reached is `Unavailable`, which `api::to_http_err`
/// turns into a 503 — the same treatment `GrpcGreeter` gives a dead upstream.
/// Anything SQL Server itself answered is a real `Other` and a 500: the
/// dependency is up, the request or the deployment is wrong, and dressing that
/// up as "try again later" would hide it. `Error::Server` carries the server's
/// own message, so "Cannot open database 'app' requested by the login" and
/// "Login failed for user 'sa'" come through intact — which is the one thing
/// that makes a missing `CREATE DATABASE` step diagnosable.
///
/// **`Tls` is on the 500 side**, which is the one judgement here that is worth
/// arguing about. A failed handshake means the server *was* reached and
/// answered; what went wrong is a certificate or a trust setting, and neither
/// gets better by retrying. Calling it 503 would say "try again later" about a
/// deployment mistake, forever — and would swallow the message that names it.
pub fn infra(e: TiberiusError) -> DomainError {
    match e {
        // never reached, or the framing broke mid-stream
        TiberiusError::Io { .. } | TiberiusError::Protocol(_) => DomainError::Unavailable,
        _ => DomainError::Other(Box::new(e)),
    }
}

/// The same judgement for the pool's own errors. `TimedOut` is bb8 giving up
/// waiting for a connection, which is the definition of `Unavailable`.
pub fn pool_infra(e: RunError<bb8_tiberius::Error>) -> DomainError {
    match e {
        RunError::TimedOut => DomainError::Unavailable,
        RunError::User(e) => manager_infra(e),
    }
}

fn manager_infra(e: bb8_tiberius::Error) -> DomainError {
    match e {
        bb8_tiberius::Error::Tiberius(e) => infra(e),
        // the TCP connect itself failed
        bb8_tiberius::Error::Io(_) => DomainError::Unavailable,
    }
}

#[async_trait]
impl Readiness for Db {
    fn name(&self) -> &'static str {
        "mssql"
    }

    /// The first call is the first connection (and the schema); afterwards one
    /// round trip on a pooled connection. The row is read so the stream is
    /// drained before the connection goes back to the pool.
    async fn ready(&self) -> Result<(), DomainError> {
        let mut conn = self.conn().await?;
        // bound rather than returned: the stream borrows `conn`, and a tail
        // expression's temporaries outlive the locals
        let _row = conn
            .query("SELECT 1", &[])
            .await
            .map_err(infra)?
            .into_row()
            .await
            .map_err(infra)?;
        Ok(())
    }
}
"##;

const MSSQL_REPO_TPL: &str = r##"//! `UserRepository` adapter over SQL Server. Written by `kodegen add mssql`.
//!
//! No new port: this is another adapter of the port the template already ships,
//! which is why the application service, the routes and `tests/api.rs` are
//! untouched — and why the port's contract (`save` is an upsert, `get` on a
//! missing id is `NotFound`) is what this file has to honor rather than
//! reinterpret.
//!
//! The handle is owned by the composition root (`bootstrap.rs`) and handed in: a
//! repository never reads `DATABASE_URL`.
//!
//! **Every value is a bound parameter.** `@P1`/`@P2` are filled by TDS from the
//! slice passed to `execute`/`query`; there is no `format!` in this file and
//! there must never be one. A unit test in kodegen itself asserts that.

use crate::domain::{DomainError, User, UserRepository};
use crate::infrastructure::mssql_db::{infra, Db};
use async_trait::async_trait;

/// The upsert, and two things about it that are not style choices:
///
/// - **`WITH (HOLDLOCK)` is required.** Without it two concurrent `MERGE`s on the
///   same key can both take the NOT MATCHED branch and one gets a duplicate-key
///   error. It fails no test and only shows up under a real concurrent writer,
///   which is exactly the kind of bug that ships. Do not remove it.
/// - **The trailing `;` is required.** `MERGE` is the one statement SQL Server
///   insists on terminating; without it this is a syntax error at run time.
///
/// **When to stop using `MERGE` here.** Its known bug list is real, and every
/// item on it needs something this table does not have: a trigger, a foreign key
/// pointing at `users`, a filtered index, an `OUTPUT $action` clause, or a view
/// as the target. Add any of those to `users` and re-evaluate — the replacement
/// is `UPDATE` then `INSERT` when `@@ROWCOUNT = 0`, inside an explicit
/// `BEGIN TRAN`/`COMMIT` with `SET XACT_ABORT ON`. It is only correct *with* that
/// transaction: in autocommit each statement commits on its own, so the range
/// lock the `UPDATE` took is released before the `INSERT` runs and the race is
/// back. That is three things to get right, which is why a single atomic
/// statement is the default here.
const UPSERT: &str = "MERGE dbo.users WITH (HOLDLOCK) AS t
     USING (VALUES (@P1, @P2)) AS s (id, name)
        ON t.id = s.id
      WHEN MATCHED THEN UPDATE SET name = s.name
      WHEN NOT MATCHED THEN INSERT (id, name) VALUES (s.id, s.name);";

const SELECT: &str = "SELECT TOP 1 id, name FROM dbo.users WHERE id = @P1";

pub struct MssqlUserRepository {
    db: Db,
}

impl MssqlUserRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl UserRepository for MssqlUserRepository {
    async fn save(&self, user: User) -> Result<(), DomainError> {
        let mut conn = self.db.conn().await?;
        // the affected-row count is deliberately not inspected: `save` of
        // unchanged values is `WHEN MATCHED` -> an UPDATE that writes the same
        // bytes, and whether the server calls that 1 row or 0 is not the port's
        // business. It says `Ok(())`, so this returns `Ok(())`.
        conn.execute(UPSERT, &[&user.id, &user.name])
            .await
            .map(|_| ())
            .map_err(infra)
    }

    async fn get(&self, id: &str) -> Result<User, DomainError> {
        let mut conn = self.db.conn().await?;
        let row = conn
            .query(SELECT, &[&id])
            .await
            .map_err(infra)?
            .into_row()
            .await
            .map_err(infra)?
            .ok_or(DomainError::NotFound)?;
        // by name rather than index, so adding a column ahead of these two in
        // `SCHEMA` cannot silently swap them
        let (Some(id), Some(name)) = (row.get::<&str, _>("id"), row.get::<&str, _>("name")) else {
            return Err(DomainError::NotFound);
        };
        Ok(User {
            id: id.to_string(),
            name: name.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SQL Server needs a server, so these are gated on `DATABASE_URL` rather
    /// than `#[ignore]`: `docker compose -f docker-compose.dev.yml up -d` plus
    /// the env var is all it takes to run them, and CI stays green without one
    /// (the in-memory adapter is what runs when `DATABASE_URL` is unset —
    /// exactly how the app itself picks its adapter).
    ///
    /// A handle **per test**, not one shared by the binary: `#[tokio::test]`
    /// builds a runtime per test and drops it at the end, and a pool belongs to
    /// the runtime that created it — shared, the second test to use it would wait
    /// out the acquire timeout on connections whose runtime is gone. Which leaves
    /// these tests applying the schema concurrently on an empty database, and
    /// that is handled where it has to be for real deployments too: the retry in
    /// `mssql_db.rs`.
    fn repo() -> Option<MssqlUserRepository> {
        let ado = std::env::var("DATABASE_URL").ok()?;
        Some(MssqlUserRepository::new(Db::new(ado)))
    }

    #[tokio::test]
    async fn save_then_get_roundtrips() {
        let Some(repo) = repo() else {
            eprintln!("DATABASE_URL unset — skipping the SQL Server roundtrip test");
            return;
        };

        // save is an upsert, so re-running this against a live database must not
        // fail on the row the previous run left behind
        let user = User::new("u1", "Alice").expect("valid user");
        repo.save(user).await.expect("save");
        assert_eq!(repo.get("u1").await.expect("get").name, "Alice");

        repo.save(User::new("u1", "Alicia").expect("valid user"))
            .await
            .expect("saving an existing id must update it");
        assert_eq!(repo.get("u1").await.expect("get").name, "Alicia");
    }

    /// The upsert against unchanged values, which is where an adapter that reads
    /// the affected-row count would start reporting an error for a call the port
    /// documents as `Ok(())`. Both writes are in one test on purpose: split
    /// across two, a stale row from an earlier run would make the second one pass
    /// for the wrong reason.
    #[tokio::test]
    async fn save_twice_identical_values_is_ok() {
        let Some(repo) = repo() else {
            return;
        };

        let user = User::new("u2", "Bob").expect("valid user");
        repo.save(user.clone()).await.expect("save");
        repo.save(user)
            .await
            .expect("saving identical values again must still be Ok");
        assert_eq!(repo.get("u2").await.expect("get").name, "Bob");
    }

    #[tokio::test]
    async fn get_missing_is_not_found() {
        let Some(repo) = repo() else {
            return;
        };

        let err = repo.get("no-such-user").await.unwrap_err();
        assert!(matches!(err, DomainError::NotFound));
    }
}
"##;

const COMPOSE_HEADER: &str = r##"# Dev-only services, written by `kodegen add`. Not the production deployment —
# that is the Dockerfile. Boots what the env-gated integration tests need:
#
#   docker compose -f docker-compose.dev.yml up -d
#   DATABASE_URL=... cargo run          # or REDIS_URL=..., per `kodegen add`
#
services:
"##;

const POSTGRES_COMPOSE: &str = r##"  postgres:
    image: postgres:17-alpine
    environment:
      POSTGRES_USER: app
      POSTGRES_PASSWORD: app
      POSTGRES_DB: app
    ports:
      - "5432:5432"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U app -d app"]
      interval: 5s
      timeout: 3s
      retries: 20
"##;

const MYSQL_COMPOSE: &str = r##"  mysql:
    image: mysql:8.4
    environment:
      MYSQL_ROOT_PASSWORD: app
      MYSQL_DATABASE: app
      MYSQL_USER: app
      MYSQL_PASSWORD: app
    ports:
      - "3306:3306"
    healthcheck:
      test: ["CMD", "mysqladmin", "ping", "-h", "127.0.0.1", "-uapp", "-papp"]
      interval: 5s
      timeout: 3s
      retries: 20
"##;

const MARIADB_COMPOSE: &str = r##"  mariadb:
    image: mariadb:11
    environment:
      MARIADB_ROOT_PASSWORD: app
      MARIADB_DATABASE: app
      MARIADB_USER: app
      MARIADB_PASSWORD: app
    ports:
      - "3306:3306"
    healthcheck:
      test: ["CMD", "healthcheck.sh", "--connect", "--innodb_initialized"]
      interval: 5s
      timeout: 3s
      retries: 20
"##;

/// The same PostgreSQL, with the extension preloaded — which is why the service
/// keeps the `postgres` name, the port and the credentials: `DATABASE_URL` does
/// not change when you move a project from one to the other.
const TIMESCALE_COMPOSE: &str = r##"  postgres:
    # TimescaleDB is a PostgreSQL extension, not a different server: same wire
    # protocol, same driver, same DATABASE_URL. The image is what differs, and it
    # ships the extension preloaded (`shared_preload_libraries`), which is what
    # lets the migration's `CREATE EXTENSION` succeed.
    image: timescale/timescaledb:latest-pg17
    environment:
      POSTGRES_USER: app
      POSTGRES_PASSWORD: app
      POSTGRES_DB: app
    ports:
      - "5432:5432"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U app -d app"]
      interval: 5s
      timeout: 3s
      retries: 20
"##;

const CLICKHOUSE_COMPOSE: &str = r##"  clickhouse:
    # The HTTP interface (8123) is what the driver speaks; 9000 is the native
    # protocol this adapter does not use. The credentials are dev-only, like
    # every other service in this file — but unlike them they are not optional:
    # since 25.x the image refuses to leave its `default` user without a
    # password, so a user is created here and CLICKHOUSE_USER/CLICKHOUSE_PASSWORD
    # are what the app connects as.
    image: clickhouse/clickhouse-server:25.3-alpine
    environment:
      CLICKHOUSE_DB: app
      CLICKHOUSE_USER: app
      CLICKHOUSE_PASSWORD: app
    ports:
      - "8123:8123"
    # the server needs these to start without warnings on some kernels
    ulimits:
      nofile:
        soft: 262144
        hard: 262144
    healthcheck:
      test: ["CMD", "wget", "--spider", "-q", "http://127.0.0.1:8123/ping"]
      interval: 5s
      timeout: 3s
      retries: 20
"##;

/// InfluxDB v2 because it is the one that sets itself up from environment
/// variables. The adapter is not v2-specific — see the README for the v3 and
/// GreptimeDB URLs, which are the same POST against a different path.
const INFLUX_COMPOSE: &str = r##"  influxdb:
    # `setup` mode creates the org, the bucket and the token on first boot, so
    # INFLUX_TOKEN below is usable without a UI visit. InfluxDB 3 Core
    # (`image: influxdb:3-core`) and GreptimeDB (`image: greptime/greptimedb`)
    # take the same line protocol — on v3 only INFLUX_URL's path changes, and on
    # GreptimeDB INFLUX_BUCKET must be `public` as well (`kodegen add greptimedb`
    # does both; see README).
    image: influxdb:2
    environment:
      DOCKER_INFLUXDB_INIT_MODE: setup
      DOCKER_INFLUXDB_INIT_USERNAME: app
      DOCKER_INFLUXDB_INIT_PASSWORD: kodegen-dev-password
      DOCKER_INFLUXDB_INIT_ORG: app
      DOCKER_INFLUXDB_INIT_BUCKET: app
      DOCKER_INFLUXDB_INIT_ADMIN_TOKEN: kodegen-dev-token
    ports:
      - "8086:8086"
    healthcheck:
      test: ["CMD", "influx", "ping"]
      interval: 5s
      timeout: 3s
      retries: 20
"##;

/// GreptimeDB standalone: the same line protocol as InfluxDB, one path deeper,
/// which is why `kodegen add greptimedb` writes the same adapter and differs
/// from `add influxdb` in this block and in two environment variables.
const GREPTIME_COMPOSE: &str = r##"  greptimedb:
    # `standalone` is the whole database in one process — the same server the
    # distributed mode runs, without a metasrv to point it at.
    #
    # `--http-addr 0.0.0.0:4000` is not optional: the default binds 127.0.0.1,
    # which inside a container means the published port answers nothing.
    #
    # INFLUX_BUCKET is `public`, not `app`: Greptime creates a table on write but
    # never a database, and `public` is the one that exists on a fresh server.
    image: greptime/greptimedb:v1.1.4
    command: standalone start --http-addr 0.0.0.0:4000
    ports:
      - "4000:4000"
    healthcheck:
      # curl ships in the image (it is Ubuntu-based); there is no wget
      test: ["CMD", "curl", "-f", "http://127.0.0.1:4000/health"]
      interval: 5s
      timeout: 3s
      retries: 20
"##;

/// SQL Server, and the two things that make it unlike every other service in
/// this file: it needs a second service to create the database, and it has no
/// arm64 image.
const MSSQL_COMPOSE: &str = r##"  mssql:
    # 2022 rather than 2025 (`2025-latest` works and is a one-line change): the
    # 2025 image crashed at start between RTM and CU2 when
    # sockets x cores_per_socket did not equal the total processor count, which
    # is a property of whatever machine happens to run it.
    #
    # There is **no arm64 image** — the manifest is amd64 only and Microsoft does
    # not support emulating it — so `platform:` is here to make Apple Silicon
    # emulate rather than fail with a confusing manifest error. Expect it to be
    # slow there.
    #
    # And one more macOS caveat, this one about the client: an app built *on* a
    # Mac cannot reach this server with `TrustServerCertificate=true`, because
    # native-tls is Apple's Secure Transport there and tiberius's trust-everything
    # path turns SNI off, which Secure Transport refuses. Use
    # `encrypt=DANGER_PLAINTEXT` against this container, or run the service in a
    # container too. On Linux — the runtime image, and CI — it works as written.
    #
    # ACCEPT_EULA is the Developer edition's licence, which is free for
    # development, testing and demonstration — including on shared machines like
    # a CI runner — and not for production. MSSQL_PID is not needed: the image is
    # built as `developer` already.
    image: mcr.microsoft.com/mssql/server:2022-latest
    platform: linux/amd64
    environment:
      ACCEPT_EULA: "Y"
      # >= 8 characters and three of upper/lower/digit/symbol, or the server
      # refuses to start. This is why it is not `app:app` like the others — it
      # has to match the password in DATABASE_URL.
      MSSQL_SA_PASSWORD: "kodeGen!2026"
    ports:
      - "1433:1433"
    healthcheck:
      # sqlcmd lives in /opt/mssql-tools18/bin and **/opt/mssql-tools is gone** —
      # most examples on the web still point at the old path and silently never
      # pass. `-C` is not optional either: tools18 encrypts by default and this
      # server has a self-signed certificate. `-b` is what turns a failed query
      # into a non-zero exit.
      test: ["CMD-SHELL", "/opt/mssql-tools18/bin/sqlcmd -S localhost -C -U sa -P \"$$MSSQL_SA_PASSWORD\" -Q 'SELECT 1' -b -o /dev/null"]
      interval: 5s
      timeout: 5s
      retries: 40
      # the server takes appreciably longer to accept connections than postgres
      # or mysql, and a start_period keeps the early failures from counting
      start_period: 30s
"##;

/// The service that has no equivalent anywhere else in this file, because this
/// image has no `POSTGRES_DB`/`MYSQL_DATABASE`: it boots with master, tempdb,
/// model and msdb, and `database=app` in the connection string resolves to
/// nothing until somebody runs `CREATE DATABASE`.
///
/// Deliberately not done by the application. It would need a connection to
/// `master` and then a second one to `app` — two configurations behind one handle
/// — and in production an application role is almost never granted
/// `CREATE DATABASE` anyway. It is a deployment step, and this is the dev
/// deployment.
///
/// **`docker compose up -d --wait` exits 1 when this service finishes**, even on
/// a clean `exit 0`: `--wait` treats a container that is no longer running as a
/// failed dependency. Measured, not assumed. So boot it in two commands —
/// `up -d --wait mssql` then `run --rm mssql-init` — which is what the generated
/// CI does and what the README says.
const MSSQL_INIT_COMPOSE: &str = r##"  mssql-init:
    # the same image, so there is nothing extra to pull
    image: mcr.microsoft.com/mssql/server:2022-latest
    platform: linux/amd64
    depends_on:
      mssql:
        condition: service_healthy
    restart: "no"
    environment:
      MSSQL_SA_PASSWORD: "kodeGen!2026"
    entrypoint: ["/bin/bash", "-c", "/opt/mssql-tools18/bin/sqlcmd -S mssql -C -U sa -P \"$$MSSQL_SA_PASSWORD\" -b -Q \"IF DB_ID('app') IS NULL CREATE DATABASE app\""]
"##;

const MONGO_COMPOSE: &str = r##"  mongo:
    # No MONGO_INITDB_ROOT_USERNAME/PASSWORD: an authenticated server would need
    # credentials in MONGODB_URL, and this file is the dev one. A deployment that
    # needs them changes the URL, not the adapter.
    #
    # If this service is not up, the app still boots (the client is dialed on
    # first use) but the first request spends the driver's server-selection
    # timeout — 30s by default — before answering 503. Append
    # `?serverSelectionTimeoutMS=2000` to MONGODB_URL to find out sooner.
    image: mongo:8
    ports:
      - "27017:27017"
    healthcheck:
      # mongosh ships in the image; `--quiet` keeps a passing check silent
      test: ["CMD", "mongosh", "--quiet", "--eval", "db.adminCommand('ping')"]
      interval: 5s
      timeout: 3s
      retries: 20
"##;

const REDIS_COMPOSE: &str = r##"  redis:
    # Valkey is a drop-in replacement: `image: valkey/valkey:8-alpine` and
    # `valkey-cli` in the healthcheck. Same protocol, same port, same client —
    # nothing in the generated code changes.
    image: redis:7-alpine
    ports:
      - "6379:6379"
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 5s
      timeout: 3s
      retries: 20
"##;

/// Appended to `domain/ports.rs`. `std::time::Duration` is spelled out rather
/// than imported: appending is what makes this edit anchor-free, and adding a
/// `use` at the top of the file would not be.
const CACHE_PORT_TPL: &str = r##"/// Outbound port: a keyed cache with optional expiry. Written by
/// `kodegen add redis`.
///
/// Both adapters must honor this contract — the in-memory one is what runs when
/// `REDIS_URL` is unset, so a behavioral difference between them shows up as a
/// bug that only reproduces in production. `tests/cache.rs` runs one set of
/// assertions against both.
///
/// A cache is a **soft dependency**: nothing here is the system of record, and
/// the default adapter is process-local, so entries do not survive a restart.
/// Callers that would rather recompute than fail can degrade a dead cache to a
/// miss with `.unwrap_or(None)`; ones that cannot should let the error through.
#[async_trait]
pub trait Cache: Send + Sync {
    /// Overwrites any existing value **and its expiry**: `ttl` of `None` clears
    /// an expiry a previous `set` left on the key, it does not preserve it.
    ///
    /// A `ttl` below one millisecond is rounded up to 1ms — Redis has no finer
    /// unit — so it is never silently turned into "no expiry".
    async fn set(
        &self,
        key: &str,
        value: &str,
        ttl: Option<std::time::Duration>,
    ) -> Result<(), DomainError>;
    /// `Ok(None)` when the key is absent or has expired: a miss is not an error,
    /// so unlike `UserRepository::get` this never returns `NotFound`.
    async fn get(&self, key: &str) -> Result<Option<String>, DomainError>;
    /// Idempotent — deleting a key that is not there is `Ok(())`.
    async fn delete(&self, key: &str) -> Result<(), DomainError>;
}
"##;

const IN_MEMORY_CACHE_TPL: &str = r##"//! Default `Cache` adapter: no server required, so `cargo run` and `cargo test`
//! work with `REDIS_URL` unset — the same rule `in_memory_repo` follows for
//! `DATABASE_URL`.
//!
//! Written by `kodegen add redis`.

use crate::domain::{Cache, DomainError};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct InMemoryCache {
    // ponytail: std RwLock — no .await is held across the lock
    // ponytail: expiry is lazy (checked on read), so an expired key nobody reads
    // again keeps its memory until it is overwritten; add a sweeper task if that
    // ever shows up in a heap profile
    inner: RwLock<HashMap<String, (String, Option<Instant>)>>,
}

impl InMemoryCache {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Cache for InMemoryCache {
    async fn set(&self, key: &str, value: &str, ttl: Option<Duration>) -> Result<(), DomainError> {
        // checked_add: `Instant::now() + Duration::MAX` panics, and a caller
        // asking for an absurd ttl means "never expire", not "abort the process"
        let expires_at = ttl.and_then(|ttl| Instant::now().checked_add(ttl));
        self.inner
            .write()
            .expect("lock poisoned")
            .insert(key.to_string(), (value.to_string(), expires_at));
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<String>, DomainError> {
        Ok(self
            .inner
            .read()
            .expect("lock poisoned")
            .get(key)
            .filter(|(_, expires_at)| expires_at.is_none_or(|at| Instant::now() < at))
            .map(|(value, _)| value.clone()))
    }

    async fn delete(&self, key: &str) -> Result<(), DomainError> {
        self.inner.write().expect("lock poisoned").remove(key);
        Ok(())
    }
}
"##;

const REDIS_CACHE_TPL: &str = r##"//! `Cache` adapter over Redis — and over Valkey, which speaks the same RESP
//! protocol and is driven by the same client.
//!
//! Written by `kodegen add redis`. The connection is owned by the composition
//! root (`bootstrap.rs`) and handed in, exactly like the Diesel `Db` handle: a
//! repository or adapter never reads `REDIS_URL` itself.

use crate::domain::{Cache, DomainError, Readiness};
use async_trait::async_trait;
use redis::AsyncTypedCommands;
use std::time::Duration;

pub struct RedisCache {
    conn: redis::aio::ConnectionManager,
}

impl RedisCache {
    pub fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self { conn }
    }

    /// The manager multiplexes over one socket and is cheap to clone; cloning is
    /// how a `&self` method gets the `&mut` the command traits want, and does
    /// not open a second connection.
    fn conn(&self) -> redis::aio::ConnectionManager {
        self.conn.clone()
    }
}

/// A cache that cannot be reached is `Unavailable`, which `api::to_http_err`
/// turns into a 503 — the same treatment `GrpcGreeter` gives a dead upstream.
/// Anything else (a protocol or type error) is a real `Other`, i.e. a bug here
/// rather than a sick dependency, and must not masquerade as one.
fn infra(e: redis::RedisError) -> DomainError {
    if e.is_connection_dropped() || e.is_io_error() {
        return DomainError::Unavailable;
    }
    DomainError::Other(Box::new(e))
}

#[async_trait]
impl Cache for RedisCache {
    async fn set(&self, key: &str, value: &str, ttl: Option<Duration>) -> Result<(), DomainError> {
        let mut conn = self.conn();
        match ttl {
            // PSETEX sets value and expiry in one round trip: SET followed by
            // PEXPIRE leaves the key with no expiry at all if the second command
            // never lands, and a cache entry that never expires is a leak
            //
            // saturate rather than truncate: `as u64` on a 128-bit millisecond
            // count wraps, and a 600-year ttl would come out as a few seconds
            Some(ttl) => {
                let ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1);
                conn.pset_ex(key, value, ms).await.map_err(infra)
            }
            // a plain SET drops any expiry the key had, which is what the port
            // documents `ttl: None` to mean (KEEPTTL is the opt-in, not this)
            None => conn.set(key, value).await.map_err(infra),
        }
    }

    async fn get(&self, key: &str) -> Result<Option<String>, DomainError> {
        let mut conn = self.conn();
        // a nil reply is `None`, which is the miss the port documents
        conn.get(key).await.map_err(infra)
    }

    async fn delete(&self, key: &str) -> Result<(), DomainError> {
        let mut conn = self.conn();
        // DEL answers with how many keys it removed; removing none is not an error
        conn.del(key).await.map(|_| ()).map_err(infra)
    }
}

#[async_trait]
impl Readiness for RedisCache {
    fn name(&self) -> &'static str {
        "redis"
    }

    /// The manager dials lazily, so the first PING is the first connection.
    async fn ready(&self) -> Result<(), DomainError> {
        let mut conn = self.conn();
        conn.ping().await.map(|_| ()).map_err(infra)
    }
}
"##;

/// Inserted above `// kodegen:resources` in `build_services_from_env`.
const REDIS_RESOURCE: &str = r##"    // one lazy, multiplexed Redis connection for the whole process: it dials on
    // first use, so a cache that is not up yet cannot stop the service booting,
    // and it reconnects on its own afterwards
    let redis = std::env::var("REDIS_URL").ok().map(|url| {
        redis::Client::open(url)
            .and_then(|client| {
                client.get_connection_manager_lazy(redis::aio::ConnectionManagerConfig::new())
            })
            .expect("invalid REDIS_URL")
    });
    if let Some(conn) = &redis {
        readiness.push(Arc::new(crate::infrastructure::redis_cache::RedisCache::new(conn.clone())));
    }"##;

/// Appended to `bootstrap.rs`.
const BUILD_CACHE_TPL: &str = r##"/// Redis when `REDIS_URL` is set, in-memory otherwise — the same
/// env-picks-the-adapter rule `build_user_service` follows for `DATABASE_URL`.
fn build_cache(redis: Option<&redis::aio::ConnectionManager>) -> Arc<dyn crate::domain::Cache> {
    match redis {
        Some(conn) => Arc::new(crate::infrastructure::redis_cache::RedisCache::new(conn.clone())),
        None => Arc::new(crate::infrastructure::in_memory_cache::InMemoryCache::new()),
    }
}
"##;

const CACHE_TEST_TPL: &str = r##"//! The `Cache` contract, run against both adapters. Written by
//! `kodegen add redis`.
//!
//! The Redis half is gated on `REDIS_URL` rather than `#[ignore]`: booting
//! `docker compose -f docker-compose.dev.yml up -d` and exporting the variable
//! is all it takes to run it, and CI stays green without a server — the same
//! switch that picks the adapter at runtime.

use std::time::Duration;
use {{crate_name}}::domain::Cache;
use {{crate_name}}::infrastructure::{in_memory_cache::InMemoryCache, redis_cache::RedisCache};

/// One contract, every adapter. Two adapters that pass the same assertions are
/// substitutable; two that are merely similar are a production-only bug.
async fn behaves_like_a_cache(cache: &dyn Cache, key: &str) {
    // the Redis run reuses a real server, so start from a known state
    cache.delete(key).await.expect("delete");
    assert!(
        cache.get(key).await.expect("get").is_none(),
        "a miss must be Ok(None), not an error"
    );

    cache.set(key, "v1", None).await.expect("set");
    assert_eq!(cache.get(key).await.expect("get").as_deref(), Some("v1"));

    cache.set(key, "v2", None).await.expect("overwrite");
    assert_eq!(
        cache.get(key).await.expect("get").as_deref(),
        Some("v2"),
        "set must overwrite"
    );

    cache
        .set(key, "temporary", Some(Duration::from_millis(300)))
        .await
        .expect("set with ttl");
    assert_eq!(
        cache.get(key).await.expect("get").as_deref(),
        Some("temporary"),
        "the value must still be readable before the ttl elapses"
    );

    // a plain set must *clear* the expiry, not inherit it. Redis makes KEEPTTL
    // opt-in for exactly this reason, and an adapter that got it backwards would
    // drop the key 300ms later in production and nowhere else
    cache
        .set(key, "permanent", None)
        .await
        .expect("set without ttl");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        cache.get(key).await.expect("get").as_deref(),
        Some("permanent"),
        "set(.., None) must clear the ttl the previous set left behind"
    );

    cache
        .set(key, "temporary", Some(Duration::from_millis(50)))
        .await
        .expect("set with ttl");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        cache.get(key).await.expect("get").is_none(),
        "the ttl was not honored"
    );

    cache
        .delete(key)
        .await
        .expect("deleting an absent key is Ok(())");
}

#[tokio::test]
async fn in_memory_cache_honors_the_contract() {
    behaves_like_a_cache(&InMemoryCache::new(), "k1").await;
}

#[tokio::test]
async fn redis_cache_honors_the_contract() {
    let Ok(url) = std::env::var("REDIS_URL") else {
        eprintln!("REDIS_URL unset — skipping the Redis contract test");
        return;
    };
    let conn = redis::Client::open(url)
        .expect("invalid REDIS_URL")
        .get_connection_manager_lazy(redis::aio::ConnectionManagerConfig::new())
        .expect("connection manager");
    behaves_like_a_cache(&RedisCache::new(conn), "kodegen:test:k1").await;
}
"##;

const S3_COMPOSE: &str = r##"  s3:
    # RustFS, but any S3-compatible server does: the generated Rust talks to the
    # API, not to a vendor. MinIO is the drop-in swap — `image: minio/minio`,
    # `command: server /data`, `MINIO_ROOT_USER`/`MINIO_ROOT_PASSWORD` instead of
    # the two below, and the same 9000. AWS itself needs no S3_ENDPOINT at all.
    image: rustfs/rustfs:1.0.0-beta.11
    environment:
      RUSTFS_ACCESS_KEY: rustfsadmin
      RUSTFS_SECRET_KEY: rustfsadmin
    ports:
      - "9000:9000"
    healthcheck:
      test: ["CMD", "curl", "-fsS", "http://127.0.0.1:9000/health"]
      interval: 5s
      timeout: 3s
      retries: 20
"##;

/// Appended to `domain/ports.rs`, next to the `Cache` port `add redis` writes.
const BLOB_PORT_TPL: &str = r##"/// Outbound port: keyed binary objects. Written by `kodegen add s3`.
///
/// Both adapters must honor this contract — the filesystem one is what runs when
/// `S3_BUCKET` is unset, so a behavioral difference between them shows up as a
/// bug that only reproduces in production. `tests/blob.rs` runs one set of
/// assertions against both.
///
/// Unlike a cache, a blob store **is** a system of record: a failed `put` must
/// not be swallowed, and nothing here expires on its own.
/// **Keys must not be prefixes of one another.** `a` and `a/b` can both exist on
/// S3, which has no directories; the filesystem adapter has to make `a` a
/// directory to hold `a/b`, so one of the two writes loses. Pick a layout where
/// every key is a leaf (`blobs/<id>`, never `blobs` *and* `blobs/<id>`).
#[async_trait]
pub trait BlobStore: Send + Sync {
    /// Overwrites any object already under `key`. Whole-object writes only —
    /// streaming and multipart uploads are out of scope for the port.
    ///
    /// The one case the adapters cannot agree on: writing `a` when `a/b` exists
    /// fails on the filesystem (`a` is a directory) and succeeds on S3. See the
    /// note on the trait — `get` and `delete` are aligned, `put` cannot be.
    async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), DomainError>;
    /// `Ok(None)` when the key is absent: a miss is not an error, so unlike
    /// `UserRepository::get` this never returns `NotFound`.
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, DomainError>;
    /// Idempotent — deleting a key that is not there is `Ok(())`, which is also
    /// what S3 itself does.
    async fn delete(&self, key: &str) -> Result<(), DomainError>;
}

/// The key rules every `BlobStore` adapter enforces before touching its backend.
///
/// S3 would accept most of what this rejects, but the filesystem adapter maps
/// keys onto paths, where `../` walks out of the blob directory. Checking in one
/// place keeps the two adapters substitutable *and* puts the traversal check on
/// the path everything takes, rather than trusting each adapter to remember it.
pub fn check_blob_key(key: &str) -> Result<(), DomainError> {
    let invalid = |why: &str| Err(DomainError::Validation(format!("blob key {key:?}: {why}")));
    if key.is_empty() {
        return invalid("must not be empty");
    }
    // S3's own limit, in bytes rather than chars
    if key.len() > 1024 {
        return invalid("must be at most 1024 bytes");
    }
    // ':' for Windows, where `C:evil.txt` is a *drive-relative* path and
    // `PathBuf::push` drops everything joined before it. Rejected on every
    // platform so a key that works in dev cannot fail in production.
    if key.contains('\\') || key.contains(':') || key.chars().any(|c| c.is_control()) {
        return invalid("must not contain backslashes, colons or control characters");
    }
    // `..` is the one that matters: the filesystem adapter joins the key onto its
    // root, and a single `../` there reads or overwrites a file outside it
    if key
        .split('/')
        .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return invalid("must not contain empty, '.' or '..' path segments");
    }
    // and then whatever *this* platform reads as more than a plain name: a root,
    // a drive prefix, a UNC share. Asking `Path` is the only check that stays
    // right on a platform this was not written on.
    //
    // ponytail: Windows reserved device names (CON, NUL, COM1) still get
    // through — they are only a problem if you deploy the filesystem adapter on
    // Windows, and rejecting them is a list, not a rule
    if std::path::Path::new(key)
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return invalid("must be a plain relative path: no root, drive or prefix components");
    }
    Ok(())
}
"##;

const FS_BLOBS_TPL: &str = r##"//! Default `BlobStore` adapter: one file per key under a directory, so
//! `cargo run` and `cargo test` work with `S3_BUCKET` unset — the same rule
//! `in_memory_repo` follows for `DATABASE_URL`.
//!
//! Written by `kodegen add s3`.

use crate::domain::{check_blob_key, BlobStore, DomainError};
use async_trait::async_trait;
use std::io::ErrorKind;
use std::path::PathBuf;

pub struct FsBlobStore {
    root: PathBuf,
}

impl FsBlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The only place a key becomes a path. `check_blob_key` has already refused
    /// `..` segments and absolute keys, which is what makes the join safe.
    fn path(&self, key: &str) -> Result<PathBuf, DomainError> {
        check_blob_key(key)?;
        Ok(self.root.join(key))
    }
}

fn infra(e: std::io::Error) -> DomainError {
    DomainError::Other(Box::new(e))
}

#[async_trait]
impl BlobStore for FsBlobStore {
    async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), DomainError> {
        let path = self.path(key)?;
        // a key with slashes in it is a nested path here and a flat name on S3;
        // both end up addressable by the same key, which is all the port promises
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(infra)?;
        }
        // ponytail: writes in place, so a reader can see a half-written object.
        // Write to `<key>.tmp` and rename if concurrent readers ever matter — on
        // S3 the same call is atomic, which is the difference this leaves open
        tokio::fs::write(&path, bytes).await.map_err(infra)
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, DomainError> {
        match tokio::fs::read(self.path(key)?).await {
            Ok(bytes) => Ok(Some(bytes)),
            // all three are "nothing stored under this key", which the port
            // documents as `Ok(None)` — matching what S3 answers for each:
            // NotFound is the plain miss, NotADirectory is `a/b` where `a` is a
            // file, IsADirectory is `a` where only `a/b` was ever written
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::NotFound | ErrorKind::NotADirectory | ErrorKind::IsADirectory
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(infra(e)),
        }
    }

    async fn delete(&self, key: &str) -> Result<(), DomainError> {
        // ponytail: leaves the directories a nested key created behind, and a
        // path segment over 255 bytes fails as ENAMETOOLONG where S3 would take
        // it. Both are the price of one file per key; sweep empty directories or
        // hash long keys here if either ever bites
        match tokio::fs::remove_file(self.path(key)?).await {
            Ok(()) => Ok(()),
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::NotFound | ErrorKind::NotADirectory | ErrorKind::IsADirectory
                ) =>
            {
                Ok(())
            }
            Err(e) => Err(infra(e)),
        }
    }
}
"##;

const S3_BLOBS_TPL: &str = r##"//! `BlobStore` adapter over the S3 API — which is also RustFS, MinIO and Ceph,
//! since they all speak it (SigV4 over the same requests). Only the endpoint
//! differs, and that is configuration, not code.
//!
//! Written by `kodegen add s3`. The client is owned by the composition root
//! (`bootstrap.rs`) and handed in, exactly like the Diesel `Db` handle: an
//! adapter never reads `S3_ENDPOINT` itself.

use crate::domain::{check_blob_key, BlobStore, DomainError, Readiness};
use async_trait::async_trait;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;

pub struct S3BlobStore {
    client: Client,
    bucket: String,
}

impl S3BlobStore {
    pub fn new(client: Client, bucket: impl Into<String>) -> Self {
        Self {
            client,
            bucket: bucket.into(),
        }
    }
}

/// A bucket that cannot be reached is `Unavailable`, which `api::to_http_err`
/// turns into a 503 — the same treatment `GrpcGreeter` gives a dead upstream.
/// Anything the service itself answered (no such bucket, access denied) is a
/// real `Other`: the dependency is up, the request was wrong.
fn infra<E, R>(e: SdkError<E, R>) -> DomainError
where
    SdkError<E, R>: std::error::Error + Send + Sync + 'static,
{
    match &e {
        SdkError::DispatchFailure(_) | SdkError::TimeoutError(_) => DomainError::Unavailable,
        _ => DomainError::Other(Box::new(e)),
    }
}

#[async_trait]
impl BlobStore for S3BlobStore {
    async fn put(&self, key: &str, bytes: Vec<u8>) -> Result<(), DomainError> {
        check_blob_key(key)?;
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(bytes))
            .send()
            .await
            .map(|_| ())
            .map_err(infra)
    }

    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, DomainError> {
        check_blob_key(key)?;
        let out = match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(out) => out,
            // a missing key is the miss the port documents, not an error
            Err(e) if e.as_service_error().is_some_and(|e| e.is_no_such_key()) => return Ok(None),
            Err(e) => return Err(infra(e)),
        };
        // the body arrives as a stream; the port hands back bytes, so it is
        // collected here rather than leaking S3's shape into the domain
        let bytes = out
            .body
            .collect()
            .await
            .map_err(|e| DomainError::Other(Box::new(e)))?;
        Ok(Some(bytes.to_vec()))
    }

    async fn delete(&self, key: &str) -> Result<(), DomainError> {
        check_blob_key(key)?;
        // S3 answers 204 whether or not the key was there, which is the
        // idempotence the port documents — nothing to special-case
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map(|_| ())
            .map_err(infra)
    }
}

#[async_trait]
impl Readiness for S3BlobStore {
    fn name(&self) -> &'static str {
        "s3"
    }

    /// `HEAD` on the bucket: reachable, credentials accepted, bucket exists —
    /// the three things a first `put` would otherwise be the one to find out.
    /// It is authorized by `s3:ListBucket` on the *bucket* ARN, which a policy
    /// that only grants the object ARNs does not cover.
    async fn ready(&self) -> Result<(), DomainError> {
        self.client
            .head_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(infra)
    }
}
"##;

/// Inserted above `// kodegen:resources` in `build_services_from_env`.
///
/// Static credentials out of the environment rather than `aws-config`'s provider
/// chain: that chain is built asynchronously and this composition root is a
/// plain `fn`. Swapping it in is a local change — make `build_services_from_env`
/// `async`, `.await` the loader here, and everything below stays as it is.
const S3_RESOURCE: &str = r##"    // one S3 client for the whole process. `S3_BUCKET` is what turns it on: a
    // bucket is the one thing with no sensible default, so leaving it unset is
    // how a project stays on the filesystem adapter.
    let s3 = std::env::var("S3_BUCKET").ok().map(|bucket| {
        // read once, at startup, and never refreshed: static keys only. Anything
        // that rotates — STS, IRSA, SSO, an instance role — needs `aws-config`'s
        // provider chain, which is async (see the README for that swap).
        let creds = aws_sdk_s3::config::Credentials::new(
            std::env::var("AWS_ACCESS_KEY_ID")
                .expect("S3_BUCKET is set but AWS_ACCESS_KEY_ID is not"),
            std::env::var("AWS_SECRET_ACCESS_KEY")
                .expect("S3_BUCKET is set but AWS_SECRET_ACCESS_KEY is not"),
            std::env::var("AWS_SESSION_TOKEN").ok(),
            None,
            "env",
        );
        let mut conf = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(
                std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".into()),
            ))
            .credentials_provider(creds);
        // RustFS, MinIO and Ceph serve every bucket from one host, so the bucket
        // has to stay in the path: virtual-host style would address
        // `<bucket>.<endpoint>`, a name that does not resolve. AWS needs neither
        // setting, which is why both hang off S3_ENDPOINT.
        if let Ok(endpoint) = std::env::var("S3_ENDPOINT") {
            conf = conf.endpoint_url(endpoint).force_path_style(true);
        }
        (aws_sdk_s3::Client::from_conf(conf.build()), bucket)
    });
    if let Some((client, bucket)) = &s3 {
        readiness.push(Arc::new(crate::infrastructure::s3_blobs::S3BlobStore::new(
            client.clone(),
            bucket,
        )));
    }"##;

/// Appended to `bootstrap.rs`.
const BUILD_BLOB_STORE_TPL: &str = r##"/// S3 when `S3_BUCKET` is set, a directory on disk otherwise — the same
/// env-picks-the-adapter rule `build_user_service` follows for `DATABASE_URL`.
fn build_blob_store(
    s3: Option<&(aws_sdk_s3::Client, String)>,
) -> Arc<dyn crate::domain::BlobStore> {
    match s3 {
        Some((client, bucket)) => Arc::new(crate::infrastructure::s3_blobs::S3BlobStore::new(
            client.clone(),
            bucket,
        )),
        // relative by default, so a `cargo run` in the project directory writes
        // under it and nowhere else
        None => Arc::new(crate::infrastructure::fs_blobs::FsBlobStore::new(
            std::env::var("BLOB_DIR").unwrap_or_else(|_| "./data/blobs".into()),
        )),
    }
}
"##;

const BLOB_TEST_TPL: &str = r##"//! The `BlobStore` contract, run against both adapters. Written by
//! `kodegen add s3`.
//!
//! The S3 half is gated on `S3_BUCKET` rather than `#[ignore]`: booting
//! `docker compose -f docker-compose.dev.yml up -d` and exporting the variables
//! is all it takes to run it, and CI stays green without a server — the same
//! switch that picks the adapter at runtime.

use {{crate_name}}::domain::{BlobStore, DomainError};
use {{crate_name}}::infrastructure::{fs_blobs::FsBlobStore, s3_blobs::S3BlobStore};

/// One contract, every adapter. Two adapters that pass the same assertions are
/// substitutable; two that are merely similar are a production-only bug.
async fn behaves_like_a_blob_store(store: &dyn BlobStore, key: &str) {
    // the S3 run reuses a real bucket, so start from a known state
    store
        .delete(key)
        .await
        .expect("deleting an absent key is Ok(())");
    assert!(
        store.get(key).await.expect("get").is_none(),
        "a miss must be Ok(None), not an error"
    );

    store.put(key, b"v1".to_vec()).await.expect("put");
    assert_eq!(
        store.get(key).await.expect("get").as_deref(),
        Some(&b"v1"[..])
    );

    store.put(key, b"v2".to_vec()).await.expect("overwrite");
    assert_eq!(
        store.get(key).await.expect("get").as_deref(),
        Some(&b"v2"[..]),
        "put must overwrite"
    );

    // bytes, not text: an adapter that round-tripped through a String would pass
    // every assertion above and corrupt the first image anybody stores
    let binary: Vec<u8> = (0u8..=255).collect();
    store.put(key, binary.clone()).await.expect("put binary");
    assert_eq!(store.get(key).await.expect("get"), Some(binary));

    store.delete(key).await.expect("delete");
    assert!(
        store.get(key).await.expect("get").is_none(),
        "the object was still there after delete"
    );

    // every adapter rejects the same keys, and rejects them the same way: the
    // filesystem one would otherwise walk out of its directory, and an S3-only
    // deployment would accept keys that stop working the day it moves to disk
    for bad in [
        "",
        "/absolute",
        "../escape",
        "nested/../escape",
        "a//b",
        "a/./b",
        // drive-relative on Windows: `PathBuf::push` would drop the blob root
        "C:evil.txt",
        "back\\slash",
    ] {
        assert!(
            matches!(
                store.put(bad, b"x".to_vec()).await,
                Err(DomainError::Validation(_))
            ),
            "key {bad:?} must be refused"
        );
        assert!(matches!(
            store.get(bad).await,
            Err(DomainError::Validation(_))
        ));
        assert!(matches!(
            store.delete(bad).await,
            Err(DomainError::Validation(_))
        ));
    }
}

#[tokio::test]
async fn fs_blob_store_honors_the_contract() {
    let dir = std::env::temp_dir().join(format!("{{crate_name}}-blob-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // a nested key exercises the directory creation `put` has to do
    behaves_like_a_blob_store(&FsBlobStore::new(&dir), "objects/k1").await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn s3_blob_store_honors_the_contract() {
    let Ok(bucket) = std::env::var("S3_BUCKET") else {
        eprintln!("S3_BUCKET unset — skipping the S3 contract test");
        return;
    };
    // the same wiring bootstrap.rs does, so this tests the configuration the app
    // actually runs with
    let creds = aws_sdk_s3::config::Credentials::new(
        std::env::var("AWS_ACCESS_KEY_ID").expect("AWS_ACCESS_KEY_ID"),
        std::env::var("AWS_SECRET_ACCESS_KEY").expect("AWS_SECRET_ACCESS_KEY"),
        std::env::var("AWS_SESSION_TOKEN").ok(),
        None,
        "env",
    );
    let mut conf = aws_sdk_s3::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new(
            std::env::var("S3_REGION").unwrap_or_else(|_| "us-east-1".into()),
        ))
        .credentials_provider(creds);
    if let Ok(endpoint) = std::env::var("S3_ENDPOINT") {
        conf = conf.endpoint_url(endpoint).force_path_style(true);
    }
    let client = aws_sdk_s3::Client::from_conf(conf.build());
    // the bucket is the one piece of setup the app does not do for itself
    if let Err(e) = client.create_bucket().bucket(&bucket).send().await {
        let already_there = e
            .as_service_error()
            .is_some_and(|e| e.is_bucket_already_owned_by_you() || e.is_bucket_already_exists());
        assert!(already_there, "cannot create bucket {bucket}: {e}");
    }
    behaves_like_a_blob_store(&S3BlobStore::new(client, bucket), "kodegen/test/k1").await;
}
"##;

/// Appended to `domain/ports.rs`, next to the `Cache` and `BlobStore` ports.
/// `std::time::SystemTime` is spelled out rather than imported for the same
/// reason `Cache` spells out `Duration`: appending is what makes this edit
/// anchor-free, and adding a `use` at the top of the file would not be.
const EVENT_PORT_TPL: &str = r##"/// Outbound port: append-only measurements. Written by `kodegen add clickhouse`
/// (ClickHouse) or `kodegen add influxdb` (InfluxDB v2/v3, GreptimeDB) — one port
/// for both, because the write path of an analytical store and of a time-series
/// database is the same shape.
///
/// **Write only.** There is no read method and no query method: what you ask an
/// analytical store is not what you ask a time-series one, and a port that tried
/// to be both would be an interface nobody could implement twice. Query the
/// store directly from whatever needs to (a report, a dashboard, a job); this
/// port is how the application *records*.
///
/// Both adapters must honor this contract — the log-only one is what runs when
/// the sink's URL is unset, so a behavioral difference between them shows up as
/// a bug that only reproduces in production. `tests/events.rs` runs one set of
/// assertions against both.
#[async_trait]
pub trait EventSink: Send + Sync {
    /// Records every event in `events`.
    ///
    /// **Not fire-and-forget:** this awaits the round trip and returns the
    /// failure rather than swallowing it. There is no queue, no retry and no
    /// backpressure anywhere behind this port — one call is one request — so a
    /// caller that must not wait on the sink should batch and `tokio::spawn`,
    /// and one that can afford to lose events should decide that for itself
    /// (`let _ = sink.write(..).await;`) rather than have the port decide.
    ///
    /// Validation is all-or-nothing: `check_event` runs over the whole batch
    /// before anything is sent, so an invalid event means nothing was written.
    /// Once the batch is on the wire, an error says nothing about how much of it
    /// landed — the sinks behind this are append-only, not transactional.
    ///
    /// An empty batch is `Ok(())` and sends nothing.
    async fn write(&self, events: Vec<Event>) -> Result<(), DomainError>;
}

/// One measurement at one instant: a name, when it happened, and its numbers.
///
/// The shape is the intersection of what both backends take, deliberately: it
/// maps to one line of InfluxDB line protocol and to one row per field in a
/// ClickHouse `MergeTree` table.
///
/// What it does **not** have, so that neither adapter has to fake it:
/// - **no tags/dimensions.** Line protocol has them (indexed); ClickHouse would
///   need a `Map` column and a schema decision. Put the dimension in `name`
///   (`http.requests.checkout`) until a real requirement says otherwise.
/// - **no field types other than `f64`.** Line protocol also has integers,
///   booleans and strings; ClickHouse has all of them and more. One numeric type
///   is what keeps the two adapters honestly interchangeable.
pub struct Event {
    /// The measurement (line protocol) / the `name` column (ClickHouse).
    pub name: String,
    /// Stored at the backend's own resolution: ClickHouse keeps `DateTime64(3)`
    /// (milliseconds) and line protocol is sent in nanoseconds, so the same event
    /// can land up to 1ms apart on the two of them.
    ///
    /// ponytail: a timestamp past ~year 2554 saturates rather than wrapping (the
    /// nanosecond count stops fitting in `u64`) — not guarded, because an event
    /// recorded half a millennium out is a bug in the caller's clock
    pub at: std::time::SystemTime,
    /// At least one — a point with no fields is not a point line protocol will
    /// accept.
    pub fields: Vec<(String, f64)>,
}

impl Event {
    /// The common case: something just happened.
    pub fn now(name: impl Into<String>, fields: Vec<(String, f64)>) -> Self {
        Self {
            name: name.into(),
            at: std::time::SystemTime::now(),
            fields,
        }
    }
}

/// The rules every `EventSink` adapter enforces before touching its backend.
///
/// The character rule is what lets the line-protocol adapter skip escaping
/// entirely: a comma, a space or an `=` in a measurement or field key changes
/// what the line *means*, and an adapter that escaped while another did not
/// would put the same event in two different places. Checking in one place keeps
/// the two adapters substitutable and keeps the escaping question closed.
pub fn check_event(event: &Event) -> Result<(), DomainError> {
    let invalid = |why: String| Err(DomainError::Validation(format!("event {:?}: {why}", event.name)));
    // a name that is legal in one backend and not the other is exactly what this
    // exists to stop, so the rule is the intersection: ASCII, and nothing line
    // protocol or SQL would read as syntax
    //
    // A leading `_` is refused because it is Flux's own namespace: `_time`,
    // `_value`, `_field` and `_measurement` are the columns a query is made of,
    // so a point named into it is written (InfluxDB 2 answers 204 — measured)
    // and then collides with the query engine's own columns on the way back out.
    // Refusing it here is what keeps "it stored" and "it can be read" the same
    // answer on both adapters. `_` *inside* a name is fine, which is why this is
    // not a character-class rule.
    let plain = |s: &str| {
        !s.is_empty()
            && s.len() <= 128
            && !s.starts_with('_')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':'))
    };
    if !plain(&event.name) {
        return invalid(
            "name must be 1-128 characters of ASCII letters, digits or _-./: — no spaces, commas or \
             equals signs (line protocol reads those as syntax) and no leading underscore (InfluxDB \
             reserves it)"
                .into(),
        );
    }
    if event.fields.is_empty() {
        return invalid("must have at least one field — line protocol rejects a point without one".into());
    }
    for (key, value) in &event.fields {
        if !plain(key) {
            return invalid(format!("field key {key:?} must follow the same rule as the name"));
        }
        // `time` is the timestamp's own column in InfluxDB, which answers a
        // point carrying it with `422 invalid field name` (measured) — so this
        // is refused here, where every adapter refuses it the same way, rather
        // than only in production
        if key == "time" {
            return invalid("field key \"time\" is reserved — InfluxDB uses it for the timestamp".into());
        }
        // NaN and the infinities have no line-protocol spelling, and would land
        // in ClickHouse as values no aggregate can use
        if !value.is_finite() {
            return invalid(format!("field {key:?} is {value}, which is not a finite number"));
        }
    }
    if event.at < std::time::UNIX_EPOCH {
        return invalid("timestamp is before the Unix epoch".into());
    }
    Ok(())
}
"##;

const LOG_EVENTS_TPL: &str = r##"//! Default `EventSink` adapter: writes each event to the tracing subscriber the
//! service already installs, so `cargo run` and `cargo test` work with no server
//! — the same rule `in_memory_repo` follows for `DATABASE_URL`.
//!
//! Written by `kodegen add clickhouse` / `kodegen add influxdb`.
//!
//! It is a **log**, not a store: nothing here can be queried, aggregated or read
//! back, and events disappear with the process. That is the honest default for
//! this port — the port has no read path to pretend to implement, and an
//! in-memory buffer would grow without bound while looking like durability.

use crate::domain::{check_event, DomainError, Event, EventSink};
use async_trait::async_trait;

#[derive(Default)]
pub struct LogEventSink;

impl LogEventSink {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl EventSink for LogEventSink {
    async fn write(&self, events: Vec<Event>) -> Result<(), DomainError> {
        // the same all-or-nothing validation the remote adapters do, and for the
        // reason the port documents: a project developed against this adapter
        // must not discover the rules the day it points at a real server
        for event in &events {
            check_event(event)?;
        }
        for event in &events {
            tracing::info!(
                target: "events",
                name = %event.name,
                at = ?event.at,
                fields = ?event.fields,
                "event"
            );
        }
        Ok(())
    }
}
"##;

const CLICKHOUSE_EVENTS_TPL: &str = r##"//! `EventSink` adapter over ClickHouse, using the official client over its HTTP
//! interface.
//!
//! Written by `kodegen add clickhouse`. The client is owned by the composition
//! root (`bootstrap.rs`) and handed in, exactly like the Diesel `Db` handle: an
//! adapter never reads `CLICKHOUSE_URL` itself.

use crate::domain::{check_event, DomainError, Event, EventSink, Readiness};
use async_trait::async_trait;
use clickhouse::{Client, Row};
use serde::Serialize;
use tokio::sync::OnceCell;

/// One row per field ("long" format) rather than one row per event with a
/// `Map(String, Float64)` column: every column here is a scalar, which is what
/// keeps the row type plain `derive(Row)` and the table something you can
/// `GROUP BY name, field` without unpacking anything. A wide table is the right
/// answer once you know your fields — that is a schema decision, and this file
/// is where you would make it.
#[derive(Row, Serialize)]
struct EventRow {
    name: String,
    field: String,
    value: f64,
    /// `DateTime64(3)` is milliseconds since the epoch, which is what the driver
    /// maps `i64` to.
    at: i64,
}

/// One budget for a whole `write`, table creation included. Neither the client
/// nor hyper underneath it has a default timeout, so without this a server that
/// accepts a connection and then says nothing holds the caller's task — and, on
/// a request path, the request — for as long as it likes.
///
/// ponytail: one flat 5s for the batch, not per row; make it a field on the sink
/// if a deployment needs it configurable. The `Cache` port's Redis adapter is
/// deliberately *not* wrapped this way — its `ConnectionManager` is unbounded
/// too, but it owns its own reconnect loop and a cache call is not the thing
/// that must not hang.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Created on first write rather than by a migration: ClickHouse is not the
/// project's relational database, so it has no `migrations/` directory and
/// `diesel_migrations` never sees it. `IF NOT EXISTS` makes every process safe
/// to start in any order.
const CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS events (
        name String,
        field String,
        value Float64,
        at DateTime64(3)
    ) ENGINE = MergeTree
    ORDER BY (name, at)";

pub struct ClickhouseEventSink {
    client: Client,
    /// A failure is not cached: `get_or_try_init` leaves the cell empty, so a
    /// server that was down for the first write is retried on the next one
    /// instead of poisoning the process.
    table: OnceCell<()>,
}

impl ClickhouseEventSink {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            table: OnceCell::new(),
        }
    }

    async fn ensure_table(&self) -> Result<(), DomainError> {
        self.table
            .get_or_try_init(|| self.client.query(CREATE_TABLE).execute())
            .await
            .map(|_| ())
            .map_err(infra)
    }
}

/// A server that cannot be reached is `Unavailable`, which `api::to_http_err`
/// turns into a 503 — the same treatment `GrpcGreeter` gives a dead upstream.
/// Anything the server itself answered (a bad query, a type mismatch) is a real
/// `Other`: the dependency is up, the request was wrong.
fn infra(e: clickhouse::error::Error) -> DomainError {
    match e {
        clickhouse::error::Error::Network(_) | clickhouse::error::Error::TimedOut => {
            DomainError::Unavailable
        }
        other => DomainError::Other(Box::new(other)),
    }
}

/// `check_event` has already refused anything before the epoch; a timestamp far
/// enough in the future to overflow saturates rather than wrapping into the past.
fn millis(at: std::time::SystemTime) -> i64 {
    at.duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[async_trait]
impl EventSink for ClickhouseEventSink {
    async fn write(&self, events: Vec<Event>) -> Result<(), DomainError> {
        for event in &events {
            check_event(event)?;
        }
        if events.is_empty() {
            return Ok(());
        }
        let send = async {
            self.ensure_table().await?;
            // one INSERT for the whole batch: the client streams the rows into a
            // single request and `end()` is what waits for the server to accept it
            let mut insert = self.client.insert("events").map_err(infra)?;
            for event in &events {
                let at = millis(event.at);
                for (field, value) in &event.fields {
                    insert
                        .write(&EventRow {
                            name: event.name.clone(),
                            field: field.clone(),
                            value: *value,
                            at,
                        })
                        .await
                        .map_err(infra)?;
                }
            }
            insert.end().await.map_err(infra)
        };
        // a sink that stops answering mid-insert is a sink that is down, which is
        // what `Unavailable` means everywhere else in this project
        tokio::time::timeout(TIMEOUT, send)
            .await
            .unwrap_or(Err(DomainError::Unavailable))
    }
}

#[async_trait]
impl Readiness for ClickhouseEventSink {
    fn name(&self) -> &'static str {
        "clickhouse"
    }

    /// One row fetched rather than `execute()`, so the answer is read back and
    /// not just the request accepted. Needs no table.
    async fn ready(&self) -> Result<(), DomainError> {
        self.client
            .query("SELECT 1")
            .fetch_one::<u8>()
            .await
            .map(|_| ())
            .map_err(infra)
    }
}
"##;

/// Inserted above `// kodegen:resources` in `build_services_from_env`.
const CLICKHOUSE_RESOURCE: &str = r##"    // one ClickHouse client for the whole process. It is a handle, not a
    // connection: nothing is dialed until the first write, so a sink that is not
    // up cannot stop the service booting.
    let clickhouse = std::env::var("CLICKHOUSE_URL").ok().map(|url| {
        let mut client = clickhouse::Client::default()
            .with_url(url)
            .with_database(std::env::var("CLICKHOUSE_DB").unwrap_or_else(|_| "default".into()));
        // both optional here, but not on the dev server: the images have
        // required a password for `default` since 25.x, so the compose service
        // creates a user and these are how the app connects as it
        if let Ok(user) = std::env::var("CLICKHOUSE_USER") {
            client = client.with_user(user);
        }
        if let Ok(password) = std::env::var("CLICKHOUSE_PASSWORD") {
            client = client.with_password(password);
        }
        client
    });
    if let Some(client) = &clickhouse {
        readiness.push(Arc::new(
            crate::infrastructure::clickhouse_events::ClickhouseEventSink::new(client.clone()),
        ));
    }"##;

/// Appended to `bootstrap.rs`.
const BUILD_EVENT_SINK_CLICKHOUSE: &str = r##"/// ClickHouse when `CLICKHOUSE_URL` is set, the tracing log otherwise — the same
/// env-picks-the-adapter rule `build_user_service` follows for `DATABASE_URL`.
fn build_event_sink(clickhouse: Option<&clickhouse::Client>) -> Arc<dyn crate::domain::EventSink> {
    match clickhouse {
        Some(client) => Arc::new(
            crate::infrastructure::clickhouse_events::ClickhouseEventSink::new(client.clone()),
        ),
        None => Arc::new(crate::infrastructure::log_events::LogEventSink::new()),
    }
}
"##;

const LINE_PROTOCOL_EVENTS_TPL: &str = r##"//! `EventSink` adapter over the InfluxDB line protocol — which is InfluxDB v2,
//! InfluxDB v3 **and** GreptimeDB, because all three accept the same text on the
//! same kind of HTTP endpoint. The URL — and on GreptimeDB the bucket, which
//! has to name a database that already exists — is all that differs between
//! them, and that is configuration, not code (see the README).
//!
//! Written by `kodegen add influxdb` (or `kodegen add greptimedb` — the same
//! adapter, a different image and URL). The client and the fully-resolved write
//! URL are owned by the composition root (`bootstrap.rs`) and handed in, exactly
//! like the Diesel `Db` handle: an adapter never reads `INFLUX_URL` itself.
//!
//! Why hyper rather than a client crate: the write path is one POST of one text
//! body, and `tonic` already puts hyper in this project's dependency graph — so
//! this adapter costs Cargo.lock nothing at all. The trade is that it speaks
//! **plain HTTP only**; for a managed endpoint on https, either put it behind a
//! TLS-terminating proxy or swap `build_http()` below for a `hyper-rustls`
//! connector (one line here, one dependency in Cargo.toml).

use crate::domain::{check_event, DomainError, Event, EventSink, Readiness};
use async_trait::async_trait;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::Request;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;

/// One budget for the whole exchange — connect, send, and read the answer.
/// hyper's client has no default timeout of its own, so this is the only thing
/// bounding a write.
///
/// ponytail: one flat 5s, not configurable; make it a field on the sink if a
/// deployment needs it. The `Cache` port's Redis adapter is deliberately *not*
/// wrapped this way — its `ConnectionManager` is unbounded too, but it owns its
/// own reconnect loop and a cache call is not the thing that must not hang.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub struct LineProtocolEventSink {
    client: Client<HttpConnector, Full<Bytes>>,
    /// The complete write endpoint, query string included — see
    /// `bootstrap.rs`, which is where the vendor differences live.
    url: String,
    /// `<base>/health`: the one path all three servers answer on the same base
    /// the write endpoint hangs off (`/health` on InfluxDB v2 and v3,
    /// `/v1/influxdb/health` on GreptimeDB), with 2xx meaning healthy.
    health: String,
    token: Option<String>,
}

impl LineProtocolEventSink {
    pub fn new(base: &str, url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            // one pooled, keep-alive client for the whole process; it dials on
            // the first write, so a sink that is not up cannot stop the service
            // booting
            client: Client::builder(TokioExecutor::new()).build_http(),
            url: url.into(),
            health: format!("{}/health", base.trim_end_matches('/')),
            token,
        }
    }
}

/// `<name> <key>=<value>[,<key>=<value>] <nanoseconds>`.
///
/// No escaping anywhere, and that is a property rather than an omission:
/// `check_event` has already refused every character line protocol would need
/// escaped (spaces, commas, `=`, quotes, newlines), so what goes on the wire is
/// what the caller wrote.
fn line(event: &Event) -> String {
    let fields = event
        .fields
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",");
    // nanoseconds, matching the `precision=ns` the composition root asks for. A
    // timestamp too far in the future saturates rather than wrapping into the
    // past; `check_event` has already refused anything before the epoch.
    let at = event
        .at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    format!("{} {fields} {at}", event.name)
}

#[async_trait]
impl EventSink for LineProtocolEventSink {
    async fn write(&self, events: Vec<Event>) -> Result<(), DomainError> {
        for event in &events {
            check_event(event)?;
        }
        if events.is_empty() {
            return Ok(());
        }
        let body = events.iter().map(line).collect::<Vec<_>>().join("\n");
        // a sink that accepts the connection and then says nothing would
        // otherwise hold this task — and the request it is on — forever
        tokio::time::timeout(TIMEOUT, self.post(body))
            .await
            .unwrap_or(Err(DomainError::Unavailable))
    }
}

impl LineProtocolEventSink {
    async fn post(&self, body: String) -> Result<(), DomainError> {
        let mut req = Request::post(&self.url).header(CONTENT_TYPE, "text/plain; charset=utf-8");
        if let Some(token) = &self.token {
            // v2's own scheme, which InfluxDB 3 accepts as well. A server that
            // wants `Bearer` or basic auth is a one-line change here.
            req = req.header(AUTHORIZATION, format!("Token {token}"));
        }
        let req = req
            .body(Full::new(Bytes::from(body)))
            .map_err(|e| DomainError::Other(Box::new(e)))?;

        let res = self.client.request(req).await.map_err(|e| {
            // nothing answered: the sink is down, which `api::to_http_err` turns
            // into a 503 — the same treatment `GrpcGreeter` gives a dead upstream
            if e.is_connect() {
                DomainError::Unavailable
            } else {
                DomainError::Other(Box::new(e))
            }
        })?;

        let status = res.status();
        if status.is_success() {
            return Ok(());
        }
        // the server answered, so this is a real `Other`: the dependency is up
        // and the request was wrong (a bad token, an unknown bucket, a
        // malformed point). Its body says which, so it is not thrown away.
        let body = res
            .into_body()
            .collect()
            .await
            .map(|b| String::from_utf8_lossy(&b.to_bytes()).into_owned())
            .unwrap_or_default();
        Err(DomainError::Other(
            format!("event sink answered {status}: {}", body.trim()).into(),
        ))
    }
}

#[async_trait]
impl Readiness for LineProtocolEventSink {
    fn name(&self) -> &'static str {
        "timeseries"
    }

    /// `GET <base>/health`. The token goes along for a server that gates it;
    /// the three known ones do not.
    async fn ready(&self) -> Result<(), DomainError> {
        let mut req = Request::get(&self.health);
        if let Some(token) = &self.token {
            req = req.header(AUTHORIZATION, format!("Token {token}"));
        }
        let req = req
            .body(Full::default())
            .map_err(|e| DomainError::Other(Box::new(e)))?;
        let res = self.client.request(req).await.map_err(|e| {
            if e.is_connect() {
                DomainError::Unavailable
            } else {
                DomainError::Other(Box::new(e))
            }
        })?;
        // the same judgement as `post`: a non-2xx is the server's answer, and
        // it says what is wrong (a 503 from `/health` is the server saying "not
        // healthy"; a 404 is the wrong base URL) — but it is an `Other`, so that
        // status reaches the operator through the warn log and the caller's
        // `/ready` says "internal error"
        if res.status().is_success() {
            Ok(())
        } else {
            Err(DomainError::Other(
                format!("health endpoint answered {}", res.status()).into(),
            ))
        }
    }
}
"##;

/// Inserted above `// kodegen:resources` in `build_services_from_env`.
///
/// The vendor differences live here, in the URL, rather than in the adapter:
/// this is the one place that knows there is such a thing as a bucket.
const INFLUX_RESOURCE: &str = r##"    // the write endpoint, assembled once. `INFLUX_URL` is the server's base
    // address and the path is appended, which is what makes one adapter cover:
    //
    //   InfluxDB v2    INFLUX_URL=http://127.0.0.1:8086          (+ INFLUX_ORG)
    //   InfluxDB v3    INFLUX_URL=http://127.0.0.1:8181          (bucket = database)
    //   GreptimeDB     INFLUX_URL=http://127.0.0.1:4000/v1/influxdb
    //                  (bucket = an existing database; `public` on a fresh server)
    //
    // ponytail: the bucket and org go into the query string verbatim, so a name
    // with a space or an `&` in it would need percent-encoding — reach for a URL
    // crate if you ever have one
    let influx = std::env::var("INFLUX_URL").ok().map(|base| {
        let bucket = std::env::var("INFLUX_BUCKET").expect("INFLUX_URL is set but INFLUX_BUCKET is not");
        let mut url = format!(
            "{}/api/v2/write?bucket={bucket}&precision=ns",
            base.trim_end_matches('/')
        );
        // v2 only: v3 and GreptimeDB ignore it, so it is set when it exists
        if let Ok(org) = std::env::var("INFLUX_ORG") {
            url.push_str(&format!("&org={org}"));
        }
        // the base travels too: the readiness check is `<base>/health`
        (base, url, std::env::var("INFLUX_TOKEN").ok())
    });
    if let Some((base, url, token)) = &influx {
        readiness.push(Arc::new(
            crate::infrastructure::line_protocol_events::LineProtocolEventSink::new(
                base,
                url,
                token.clone(),
            ),
        ));
    }"##;

/// Appended to `bootstrap.rs`.
const BUILD_EVENT_SINK_INFLUX: &str = r##"/// The time-series database when `INFLUX_URL` is set, the tracing log otherwise
/// — the same env-picks-the-adapter rule `build_user_service` follows for
/// `DATABASE_URL`.
fn build_event_sink(
    influx: Option<&(String, String, Option<String>)>,
) -> Arc<dyn crate::domain::EventSink> {
    match influx {
        Some((base, url, token)) => Arc::new(
            crate::infrastructure::line_protocol_events::LineProtocolEventSink::new(
                base,
                url,
                token.clone(),
            ),
        ),
        None => Arc::new(crate::infrastructure::log_events::LogEventSink::new()),
    }
}
"##;

/// One contract test with the server half filled in per backend: the assertions
/// that do not need a server are the same either way, and they are the ones that
/// keep the two adapters interchangeable.
const EVENTS_TEST_TPL: &str = r##"//! The `EventSink` contract, run against both adapters. Written by
//! `kodegen add clickhouse` / `influxdb` / `greptimedb`.
//!
//! The server half is gated on the sink's URL rather than `#[ignore]`: booting
//! `docker compose -f docker-compose.dev.yml up -d` and exporting the variables
//! is all it takes to run it, and CI stays green without a server — the same
//! switch that picks the adapter at runtime.

use std::time::{Duration, SystemTime};
use {{crate_name}}::domain::{DomainError, Event, EventSink};
use {{crate_name}}::infrastructure::{{{adapter_module}}::*, log_events::LogEventSink};

/// A name unique to this run: the server adapters append to a real table that
/// earlier runs have already written to, so the assertions below must not be
/// able to see anything but their own rows.
fn measurement(tag: &str) -> String {
    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("kodegen.test.{tag}.{unique}")
}

/// One contract, every adapter. Two adapters that pass the same assertions are
/// substitutable; two that are merely similar are a production-only bug.
async fn behaves_like_an_event_sink(sink: &dyn EventSink, name: &str) {
    sink.write(vec![]).await.expect("an empty batch writes nothing and is Ok");

    sink.write(vec![Event::now(name, vec![("value".into(), 1.0)])])
        .await
        .expect("write");

    // a batch, several fields, and a timestamp that is not `now` — the three
    // things the two adapters shape differently on the wire
    sink.write(vec![
        Event {
            name: name.to_string(),
            at: SystemTime::now() - Duration::from_secs(60),
            fields: vec![("value".into(), 2.5), ("count".into(), 3.0)],
        },
        Event::now(name, vec![("value".into(), -0.5)]),
    ])
    .await
    .expect("batch write");

    // every adapter refuses the same events, and refuses them the same way: line
    // protocol would silently mean something else for most of these, and a
    // project developed against the default adapter must not discover that on
    // the day it points at a real server
    for bad in [
        Event::now("", vec![("value".into(), 1.0)]),
        Event::now("has space", vec![("value".into(), 1.0)]),
        Event::now("has,comma", vec![("value".into(), 1.0)]),
        Event::now("ok", vec![]),
        Event::now("ok", vec![("has=equals".into(), 1.0)]),
        // reserved names: `time` is the timestamp's own column and InfluxDB
        // rejects a point carrying it outright, while the `_` prefix is Flux's
        // namespace — written happily and then indistinguishable from the query
        // engine's own columns
        Event::now("_reserved", vec![("value".into(), 1.0)]),
        Event::now("ok", vec![("_reserved".into(), 1.0)]),
        Event::now("ok", vec![("time".into(), 1.0)]),
        Event::now("ok", vec![("value".into(), f64::NAN)]),
        Event::now("ok", vec![("value".into(), f64::INFINITY)]),
        Event {
            name: "ok".into(),
            at: SystemTime::UNIX_EPOCH - Duration::from_secs(1),
            fields: vec![("value".into(), 1.0)],
        },
    ] {
        let name = bad.name.clone();
        assert!(
            matches!(sink.write(vec![bad]).await, Err(DomainError::Validation(_))),
            "event {name:?} must be refused"
        );
    }
}

#[tokio::test]
async fn log_event_sink_honors_the_contract() {
    behaves_like_an_event_sink(&LogEventSink::new(), &measurement("log")).await;
}

{{server_test}}"##;

/// The ClickHouse half of `tests/events.rs`. It is the one adapter whose write
/// can be *verified* rather than only acknowledged — the client can query, even
/// though the port cannot.
const CLICKHOUSE_TEST: &str = r##"#[tokio::test]
async fn clickhouse_event_sink_honors_the_contract() {
    let Ok(url) = std::env::var("CLICKHOUSE_URL") else {
        eprintln!("CLICKHOUSE_URL unset — skipping the ClickHouse contract test");
        return;
    };
    // the same wiring bootstrap.rs does, so this tests the configuration the app
    // actually runs with — credentials included, since the dev server needs them
    let mut client = clickhouse::Client::default()
        .with_url(url)
        .with_database(std::env::var("CLICKHOUSE_DB").unwrap_or_else(|_| "default".into()));
    if let Ok(user) = std::env::var("CLICKHOUSE_USER") {
        client = client.with_user(user);
    }
    if let Ok(password) = std::env::var("CLICKHOUSE_PASSWORD") {
        client = client.with_password(password);
    }
    let name = measurement("clickhouse");
    behaves_like_an_event_sink(&ClickhouseEventSink::new(client.clone()), &name).await;

    // and then the half the port cannot express: the rows are actually there.
    // Four of them: the three valid events written above have four fields
    // between them, and the table is one row per field.
    let rows: u64 = client
        .query("SELECT count() FROM events WHERE name = ?")
        .bind(&name)
        .fetch_one()
        .await
        .expect("count");
    assert_eq!(rows, 4, "one row per field of every event written");
}
"##;

/// The line-protocol half. There is no read-back here on purpose: what "query"
/// means differs across the three servers this adapter supports, while a 2xx from
/// the write endpoint means the same thing on all of them (the adapter turns
/// anything else into an error, which is what makes this assertion real).
const INFLUX_TEST: &str = r##"#[tokio::test]
async fn line_protocol_event_sink_honors_the_contract() {
    let Ok(base) = std::env::var("INFLUX_URL") else {
        eprintln!("INFLUX_URL unset — skipping the line-protocol contract test");
        return;
    };
    // the same wiring bootstrap.rs does, so this tests the configuration the app
    // actually runs with
    let bucket = std::env::var("INFLUX_BUCKET").expect("INFLUX_BUCKET");
    let mut url = format!(
        "{}/api/v2/write?bucket={bucket}&precision=ns",
        base.trim_end_matches('/')
    );
    if let Ok(org) = std::env::var("INFLUX_ORG") {
        url.push_str(&format!("&org={org}"));
    }
    let sink = LineProtocolEventSink::new(&base, url, std::env::var("INFLUX_TOKEN").ok());
    // every write above is asserted to succeed, and the adapter only succeeds on
    // a 2xx — a rejected point, a bad token or an unknown bucket all fail here
    behaves_like_an_event_sink(&sink, &measurement("influx")).await;
}
"##;

#[cfg(test)]
mod tests {
    use super::*;

    /// A real generated project, straight from the embedded template — anything
    /// less would not exercise the whole-file and suffix comparisons `add` does.
    fn fixture(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("kodegen-capability-{tag}"));
        let _ = fs::remove_dir_all(&root);
        let (tpl, staging) = crate::resolve_template("hexagonal").unwrap();
        let subs = crate::substitutions(&tpl, "demo-svc");
        crate::render_tree(&tpl, &root, &subs).unwrap();
        // this is the one call site that runs once per test rather than once
        // per CLI invocation — leaving the staged copy behind here is exactly
        // what turned into thousands of them (see resolve_template's doc)
        if let Some(dir) = staging {
            let _ = fs::remove_dir_all(&dir);
        }
        root
    }

    /// A generated file with its comments stripped. The SQL adapters name the
    /// traps they avoid *in their comments* — "SQL Server has no `CREATE TABLE IF
    /// NOT EXISTS`", "there must never be a `format!` here" — so an assertion
    /// that a spelling is absent has to look at what the file does, not at what
    /// it says about itself. Line comments only, which is all these templates
    /// have.
    fn code_only(src: &str) -> String {
        src.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(dir).unwrap().flatten() {
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                } else {
                    let rel = e.path().strip_prefix(root).unwrap().display().to_string();
                    out.push((rel, fs::read(e.path()).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn flips_default_without_touching_dependencies() {
        let root = fixture("flip");
        let before = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        add(&root, Capability::Postgres, None, false).unwrap();
        let after = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(after.contains("default = [\"postgres\"]"), "{after}");
        // the lock stays valid only as long as `add` never adds a dependency:
        // every line but `default` must come through byte for byte
        let strip = |s: &str| {
            s.lines().filter(|l| !l.starts_with("default = [")).collect::<Vec<_>>().join("\n")
        };
        assert_eq!(strip(&before), strip(&after));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mariadb_uses_the_mysql_backend() {
        let root = fixture("mariadb");
        add(&root, Capability::Mariadb, None, false).unwrap();
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"mysql\"]"), "{toml}");
        let db = fs::read_to_string(root.join(DIESEL_DB)).unwrap();
        assert!(db.contains("MysqlConnection"), "{db}");
        // `/ready` names what the operator runs, which is the alias, not the driver
        assert!(
            db.contains("impl Readiness for Db") && db.contains("\"mariadb\""),
            "{db}"
        );
        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("mariadb:11"), "{compose}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mysql_migration_avoids_a_text_primary_key() {
        let root = fixture("mysql-migration");
        add(&root, Capability::Mysql, None, false).unwrap();
        let up = fs::read_to_string(root.join("migrations/2026-07-26-000000_create_users/up.sql"))
            .unwrap();
        // MySQL cannot index TEXT without a prefix length — this one fails at
        // migration time, not compile time
        assert!(up.contains("VARCHAR(64) PRIMARY KEY"), "{up}");
        assert!(!up.contains("id TEXT"), "{up}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// REPLACE INTO is a DELETE + INSERT on MySQL, so it would cascade to every
    /// row referencing a user on what `save` documents as an update.
    #[test]
    fn both_dialects_get_a_real_upsert() {
        for (tag, cap, target) in [
            ("pg-upsert", Capability::Postgres, "users::id"),
            ("mysql-upsert", Capability::Mysql, "diesel::dsl::DuplicatedKeys"),
        ] {
            let root = fixture(tag);
            add(&root, cap, None, false).unwrap();
            let repo = fs::read_to_string(root.join(DIESEL_REPO)).unwrap();
            assert!(!repo.contains("replace_into"), "{tag}: {repo}");
            assert!(repo.contains(&format!(".on_conflict({target})")), "{tag}: {repo}");
            assert!(repo.contains("DATABASE_URL"), "{tag}: tests must be env-gated");
            fs::remove_dir_all(&root).unwrap();
        }
    }

    /// The Phase 2 path this whole capability exists to prove: a new port, a new
    /// `Services` field, and a builder wired into both composition roots.
    #[test]
    fn redis_writes_the_cache_port_and_wires_both_composition_roots() {
        let root = fixture("redis");
        add(&root, Capability::Redis, None, false).unwrap();

        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"sqlite\", \"cache\"]"), "{toml}");

        let ports = fs::read_to_string(root.join(PORTS)).unwrap();
        assert!(ports.contains("pub trait Cache: Send + Sync"), "{ports}");
        assert!(ports.contains("pub trait UserRepository"), "the existing ports were lost: {ports}");
        assert!(
            fs::read_to_string(root.join("src/domain/mod.rs")).unwrap().contains("pub use ports::Cache;")
        );

        let api = fs::read_to_string(root.join("src/api/mod.rs")).unwrap();
        assert!(api.contains("pub cache: Arc<dyn crate::domain::Cache>"), "{api}");

        let bootstrap = fs::read_to_string(root.join("src/bootstrap.rs")).unwrap();
        assert!(bootstrap.contains("REDIS_URL"), "{bootstrap}");
        assert!(bootstrap.contains("cache: build_cache(redis.as_ref()),"), "{bootstrap}");
        assert!(bootstrap.contains("cache: build_cache(None),"), "{bootstrap}");
        assert!(bootstrap.contains("fn build_cache("), "{bootstrap}");
        // the markers have to survive so the next `add`/`feature` still lands
        for marker in [RESOURCES_MARKER, "// kodegen:services:env", "// kodegen:services:default"] {
            assert!(bootstrap.contains(marker), "marker '{marker}' was consumed: {bootstrap}");
        }

        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("image: redis:7-alpine"), "{compose}");
        assert!(compose.contains("valkey"), "the Valkey swap must be documented where it is made");
        assert!(
            fs::read_to_string(root.join(CACHE_TEST)).unwrap().contains("demo_svc::domain::Cache"),
            "the contract test must import the project's own crate"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// Redis is not a relational backend, so the "only one backend" rule must
    /// not catch it — and neither `add` may undo the other's feature.
    #[test]
    fn redis_coexists_with_a_relational_backend() {
        let root = fixture("redis-and-pg");
        add(&root, Capability::Postgres, None, false).unwrap();
        add(&root, Capability::Redis, None, false).unwrap();
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"postgres\", \"cache\"]"), "{toml}");
        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("  postgres:") && compose.contains("  redis:"), "{compose}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn adding_redis_twice_fails() {
        let root = fixture("redis-twice");
        add(&root, Capability::Redis, None, false).unwrap();
        let err = add(&root, Capability::Redis, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("capability 'redis' already added"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn redis_dry_run_writes_nothing_but_plans_the_same_change() {
        let root = fixture("redis-dry");
        let before = snapshot(&root);
        let planned = add(&root, Capability::Redis, None, true).unwrap();
        assert_eq!(before, snapshot(&root), "dry run touched the tree");
        assert_eq!(planned, add(&root, Capability::Redis, None, false).unwrap());
        fs::remove_dir_all(&root).unwrap();
    }

    /// The marker is the only anchor the redis path has no legacy fallback for,
    /// so its error has to be enough to repair the file by hand.
    #[test]
    fn redis_names_the_missing_resources_marker() {
        let root = fixture("redis-no-marker");
        let bootstrap = fs::read_to_string(root.join("src/bootstrap.rs")).unwrap();
        fs::write(
            root.join("src/bootstrap.rs"),
            bootstrap.replace(RESOURCES_MARKER, ""),
        )
        .unwrap();
        let err = add(&root, Capability::Redis, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains(RESOURCES_MARKER), "{err}");
        assert!(err.contains("build_services_from_env"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// A `default` array a formatter has wrapped is the one shape where the
    /// line-at-a-time rewrite would corrupt the manifest instead of failing, so
    /// it has to be refused — and told apart from a `default` that is simply gone.
    #[test]
    fn a_multiline_default_array_is_refused_not_truncated() {
        let wrapped = "[features]\ndefault = [\n    \"sqlite\",\n    \"cache\",\n]\n";
        assert!(default_span(wrapped).is_none(), "a wrapped array must not look like an anchor");
        let err = enable_feature(wrapped, "postgres", Some(Slot::Datastore)).unwrap_err().to_string();
        assert!(err.contains("split across lines"), "{err}");
        assert!(err.contains("single line"), "must say how to fix it: {err}");
        // and the other shape still gets the other message
        assert!(default_anchor_err("[features]\n").contains("not found"));

        // through `add`, on a real project, with --dry-run: same code path
        let root = fixture("wrapped-default");
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            toml.replace("default = [\"sqlite\"]", "default = [\n    \"sqlite\",\n]"),
        )
        .unwrap();
        for cap in [Capability::Postgres, Capability::Redis] {
            let err = add(&root, cap, None, true).unwrap_err().to_string();
            assert!(err.contains("split across lines"), "{}: {err}", cap.name());
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// Appending to `ports.rs` is anchor-free, but it can still produce a file
    /// that will not compile — so it checks the two ways that happens.
    #[test]
    fn redis_refuses_a_ports_file_it_cannot_append_to() {
        let root = fixture("redis-ports-guard");
        let ports = fs::read_to_string(root.join(PORTS)).unwrap();

        fs::write(root.join(PORTS), format!("{ports}\npub trait Cache {{}}\n")).unwrap();
        let err = add(&root, Capability::Redis, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already declares a `Cache` trait"), "{err}");

        fs::write(root.join(PORTS), ports.replace(ASYNC_TRAIT_USE, "")).unwrap();
        let err = add(&root, Capability::Redis, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains(ASYNC_TRAIT_USE), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The same shape as the redis test, for the same reason: a new port, a new
    /// `Services` field, and a builder wired into both composition roots.
    #[test]
    fn s3_writes_the_blob_port_and_wires_both_composition_roots() {
        let root = fixture("s3");
        add(&root, Capability::S3, None, false).unwrap();

        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"sqlite\", \"blob\"]"), "{toml}");

        let ports = fs::read_to_string(root.join(PORTS)).unwrap();
        assert!(ports.contains("pub trait BlobStore: Send + Sync"), "{ports}");
        assert!(ports.contains("pub fn check_blob_key"), "{ports}");
        assert!(ports.contains("pub trait UserRepository"), "the existing ports were lost: {ports}");
        assert!(
            fs::read_to_string(root.join("src/domain/mod.rs"))
                .unwrap()
                .contains("pub use ports::{check_blob_key, BlobStore};")
        );

        let api = fs::read_to_string(root.join("src/api/mod.rs")).unwrap();
        assert!(api.contains("pub blobs: Arc<dyn crate::domain::BlobStore>"), "{api}");

        let bootstrap = fs::read_to_string(root.join("src/bootstrap.rs")).unwrap();
        assert!(bootstrap.contains("S3_BUCKET"), "{bootstrap}");
        // path-style is what makes RustFS/MinIO work at all: without it the
        // request goes to <bucket>.<endpoint>, which resolves nowhere
        assert!(bootstrap.contains("force_path_style(true)"), "{bootstrap}");
        assert!(bootstrap.contains("blobs: build_blob_store(s3.as_ref()),"), "{bootstrap}");
        assert!(bootstrap.contains("blobs: build_blob_store(None),"), "{bootstrap}");
        assert!(bootstrap.contains("fn build_blob_store("), "{bootstrap}");
        for marker in [RESOURCES_MARKER, "// kodegen:services:env", "// kodegen:services:default"] {
            assert!(bootstrap.contains(marker), "marker '{marker}' was consumed: {bootstrap}");
        }

        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("image: rustfs/rustfs:"), "{compose}");
        assert!(compose.contains("minio"), "the MinIO swap must be documented where it is made");
        let test = fs::read_to_string(root.join(BLOB_TEST)).unwrap();
        assert!(test.contains("demo_svc::domain::{BlobStore, DomainError}"), "{test}");
        // the default adapter has to be exercised with no server, or the
        // "generated projects are green out of the box" promise is untested
        assert!(test.contains("fn fs_blob_store_honors_the_contract"), "{test}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Object storage is neither a backend nor a cache, so all three coexist —
    /// and none of the three `add`s may undo another's feature or compose block.
    #[test]
    fn s3_coexists_with_a_backend_and_a_cache() {
        let root = fixture("s3-and-friends");
        add(&root, Capability::Postgres, None, false).unwrap();
        add(&root, Capability::Redis, None, false).unwrap();
        add(&root, Capability::S3, None, false).unwrap();
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"postgres\", \"cache\", \"blob\"]"), "{toml}");
        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        for service in ["  postgres:", "  redis:", "  s3:"] {
            assert!(compose.contains(service), "{service} missing: {compose}");
        }
        let api = fs::read_to_string(root.join("src/api/mod.rs")).unwrap();
        assert!(api.contains("pub cache:") && api.contains("pub blobs:"), "{api}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn adding_s3_twice_fails() {
        let root = fixture("s3-twice");
        add(&root, Capability::S3, None, false).unwrap();
        let err = add(&root, Capability::S3, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("capability 's3' already added"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn s3_dry_run_writes_nothing_but_plans_the_same_change() {
        let root = fixture("s3-dry");
        let before = snapshot(&root);
        let planned = add(&root, Capability::S3, None, true).unwrap();
        assert_eq!(before, snapshot(&root), "dry run touched the tree");
        assert_eq!(planned, add(&root, Capability::S3, None, false).unwrap());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn s3_names_the_missing_resources_marker() {
        let root = fixture("s3-no-marker");
        let bootstrap = fs::read_to_string(root.join("src/bootstrap.rs")).unwrap();
        fs::write(
            root.join("src/bootstrap.rs"),
            bootstrap.replace(RESOURCES_MARKER, ""),
        )
        .unwrap();
        let err = add(&root, Capability::S3, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains(RESOURCES_MARKER), "{err}");
        assert!(err.contains("build_services_from_env"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Appending to `ports.rs` is anchor-free, but it can still produce a file
    /// that will not compile — and `add s3` appends a free function as well as a
    /// trait, so it has one more collision to refuse than `add redis` does.
    #[test]
    fn s3_refuses_a_ports_file_it_cannot_append_to() {
        let root = fixture("s3-ports-guard");
        let ports = fs::read_to_string(root.join(PORTS)).unwrap();

        fs::write(
            root.join(PORTS),
            format!("{ports}\npub trait BlobStore {{}}\n"),
        )
        .unwrap();
        let err = add(&root, Capability::S3, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("already declares a `BlobStore` trait"),
            "{err}"
        );

        fs::write(
            root.join(PORTS),
            format!("{ports}\npub fn check_blob_key() {{}}\n"),
        )
        .unwrap();
        let err = add(&root, Capability::S3, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already declares `check_blob_key`"), "{err}");

        fs::write(root.join(PORTS), ports.replace(ASYNC_TRAIT_USE, "")).unwrap();
        let err = add(&root, Capability::S3, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains(ASYNC_TRAIT_USE), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// An adapter file the user already has is theirs — `add` creates files, it
    /// does not overwrite them.
    #[test]
    fn s3_refuses_to_overwrite_an_existing_adapter() {
        let root = fixture("s3-existing-adapter");
        fs::write(root.join(S3_BLOBS), "// mine now\n").unwrap();
        let err = add(&root, Capability::S3, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(S3_BLOBS) && err.contains("already exists"),
            "{err}"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// The Phase 2 path this capability exists to prove, `scalar`'s version: no
    /// port and no adapter, so what `add` writes is one new file and five
    /// `cfg_attr` insertions into files the template already ships.
    #[test]
    fn scalar_writes_openapi_rs_and_annotates_the_shipped_handlers() {
        let root = fixture("scalar");
        add(&root, Capability::Scalar, None, false).unwrap();

        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"sqlite\", \"apidocs\"]"), "{toml}");

        let openapi = fs::read_to_string(root.join(OPENAPI_RS)).unwrap();
        for path in [
            "crate::api::health",
            "crate::api::ready",
            "crate::api::user::create_user",
            "crate::api::user::get_user",
            "crate::api::hello::say_hello",
        ] {
            assert!(openapi.contains(path), "{openapi}");
        }
        assert!(openapi.contains("pub struct ApiDoc"), "{openapi}");
        assert!(openapi.contains("Scalar::with_url"), "{openapi}");
        assert!(openapi.contains("/openapi.json"), "{openapi}");

        let api_mod = fs::read_to_string(root.join(API_MOD)).unwrap();
        assert!(api_mod.contains("pub mod openapi;"), "{api_mod}");
        assert!(api_mod.contains(".merge(openapi::routes())"), "{api_mod}");
        assert!(
            api_mod.matches("utoipa::path").count() == 2
                && api_mod.matches("utoipa::ToSchema").count() == 2,
            "health/ready/ErrorRes/ReadyRes were not annotated: {api_mod}"
        );

        let user = fs::read_to_string(root.join(USER_API)).unwrap();
        assert!(user.matches("utoipa::path").count() == 2, "{user}");
        assert!(user.contains("utoipa::ToSchema"), "{user}");

        let hello = fs::read_to_string(root.join(HELLO_API)).unwrap();
        assert!(hello.contains("utoipa::path"), "{hello}");

        let model = fs::read_to_string(root.join(DOMAIN_MODEL)).unwrap();
        assert!(model.contains("utoipa::ToSchema"), "{model}");

        let test = fs::read_to_string(root.join(APIDOCS_TEST)).unwrap();
        assert!(test.contains("demo_svc::bootstrap"), "the contract test must import the project's own crate: {test}");
        assert!(test.contains("/openapi.json") && test.contains("/docs"), "{test}");

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn adding_scalar_twice_fails() {
        let root = fixture("scalar-twice");
        add(&root, Capability::Scalar, None, false).unwrap();
        let err = add(&root, Capability::Scalar, None, true).unwrap_err().to_string();
        assert!(err.contains("capability 'scalar' already added"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scalar_dry_run_writes_nothing_but_plans_the_same_change() {
        let root = fixture("scalar-dry");
        let before = snapshot(&root);
        let planned = add(&root, Capability::Scalar, None, true).unwrap();
        assert_eq!(before, snapshot(&root), "dry run touched the tree");
        assert_eq!(planned, add(&root, Capability::Scalar, None, false).unwrap());
        fs::remove_dir_all(&root).unwrap();
    }

    /// No port, no slot: `scalar` refuses no one and no one refuses it.
    #[test]
    fn scalar_coexists_with_a_backend_a_cache_and_an_event_sink() {
        let root = fixture("scalar-coexist");
        add(&root, Capability::Postgres, None, false).unwrap();
        add(&root, Capability::Redis, None, false).unwrap();
        add(&root, Capability::Clickhouse, None, false).unwrap();
        add(&root, Capability::Scalar, None, false).unwrap();
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            toml.contains("default = [\"postgres\", \"cache\", \"analytics\", \"apidocs\"]"),
            "{toml}"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// `--orm` means nothing to a capability with no ORM to choose, and `scalar`
    /// is one of them — same rule `redis`/`s3`/the event sinks already prove,
    /// pinned here so the arm that added `Kind::ApiDocs` to `check_orm` cannot
    /// quietly become a no-op.
    #[test]
    fn scalar_refuses_an_orm_flag() {
        let root = fixture("scalar-orm");
        let err = add(&root, Capability::Scalar, Some(Orm::Diesel), true).unwrap_err().to_string();
        assert!(err.contains("no ORM"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Direction 1 of the module doc: `kodegen feature order` first, so its
    /// handlers are already annotated by the time `add scalar` scans for them.
    #[test]
    fn feature_before_scalar_is_found_by_the_scanner() {
        let root = fixture("feature-then-scalar");
        crate::scaffold::add_feature(&root, "order", crate::scaffold::Transport::Rest, false).unwrap();
        add(&root, Capability::Scalar, None, false).unwrap();
        let openapi = fs::read_to_string(root.join(OPENAPI_RS)).unwrap();
        assert!(openapi.contains("crate::api::order::create_order"), "{openapi}");
        assert!(openapi.contains("crate::api::order::get_order"), "{openapi}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Direction 2: `add scalar` first, so `openapi.rs` does not exist yet when
    /// the scanner would have looked — `kodegen feature` has to notice the file
    /// afterward instead, which is `add_feature`'s `is_file()` check.
    #[test]
    fn scalar_before_feature_gets_its_paths_inserted_afterward() {
        let root = fixture("scalar-then-feature");
        add(&root, Capability::Scalar, None, false).unwrap();
        crate::scaffold::add_feature(&root, "order", crate::scaffold::Transport::Rest, false).unwrap();
        let openapi = fs::read_to_string(root.join(OPENAPI_RS)).unwrap();
        assert!(openapi.contains("crate::api::order::create_order"), "{openapi}");
        assert!(openapi.contains("crate::api::order::get_order"), "{openapi}");
        // the marker must survive so a *third* feature still has somewhere to land
        assert!(openapi.contains("// kodegen:openapi:paths"), "{openapi}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// `kodegen feature <name> --transport grpc` still annotates nothing OpenAPI
    /// can use (no HTTP route), so `add_feature` must not touch `openapi.rs` for
    /// it and the scanner must not walk into `*_grpc.rs` looking for a route
    /// that is not there.
    #[test]
    fn grpc_features_are_invisible_to_the_spec() {
        let root = fixture("scalar-grpc");
        crate::scaffold::add_feature(&root, "billing", crate::scaffold::Transport::Grpc, false).unwrap();
        add(&root, Capability::Scalar, None, false).unwrap();
        let openapi = fs::read_to_string(root.join(OPENAPI_RS)).unwrap();
        assert!(!openapi.contains("billing"), "{openapi}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// A project with no `kodegen add scalar` on it still scaffolds features
    /// that carry the `cfg_attr` annotations — they simply have no `apidocs`
    /// feature to turn on, which `cfg_attr` on an undeclared feature treats as
    /// off rather than an error (proven for real, not just asserted here, by
    /// `cargo build` in the verification pass over a generated project).
    #[test]
    fn feature_annotates_even_without_scalar_on_the_project() {
        let root = fixture("feature-no-scalar");
        crate::scaffold::add_feature(&root, "order", crate::scaffold::Transport::Rest, false).unwrap();
        let api = fs::read_to_string(root.join("src/api/order.rs")).unwrap();
        assert!(api.contains("cfg_attr(feature = \"apidocs\""), "{api}");
        assert!(!root.join(OPENAPI_RS).exists(), "no openapi.rs without add scalar");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The four S3 variables are printed as one line because three of them are
    /// useless on their own — and the first has to be the one that selects the
    /// adapter, since that is the one `add` tells you to leave unset.
    #[test]
    fn every_capability_leads_with_the_variable_that_selects_it() {
        for (cap, first) in [
            (Capability::Postgres, "DATABASE_URL"),
            (Capability::Timescaledb, "DATABASE_URL"),
            (Capability::Mongodb, "MONGODB_URL"),
            (Capability::Mssql, "DATABASE_URL"),
            (Capability::Redis, "REDIS_URL"),
            (Capability::S3, "S3_BUCKET"),
            (Capability::Clickhouse, "CLICKHOUSE_URL"),
            (Capability::Influxdb, "INFLUX_URL"),
            (Capability::Greptimedb, "INFLUX_URL"),
        ] {
            assert_eq!(cap.env()[0].0, first, "{}", cap.name());
        }
        let s3: Vec<&str> = Capability::S3.env().iter().map(|(k, _)| *k).collect();
        assert_eq!(s3, ["S3_BUCKET", "S3_ENDPOINT", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"]);
    }

    #[test]
    fn adding_the_same_capability_twice_fails() {
        let root = fixture("twice");
        add(&root, Capability::Postgres, None, false).unwrap();
        let err = add(&root, Capability::Postgres, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already added"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_second_relational_backend_fails_with_instructions() {
        let root = fixture("second");
        add(&root, Capability::Postgres, None, false).unwrap();
        let err = add(&root, Capability::Mysql, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("already this project's datastore"), "{err}");
        assert!(err.contains("default = [\"sqlite\"]"), "must say how to undo it: {err}");
        assert!(err.contains("diesel_db.rs"), "must name the files to restore: {err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn with_more_than_one_relational_backend_is_refused() {
        let err = check_combination(&[Capability::Postgres, Capability::Mysql], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("at most one datastore"), "{err}");
        // deduped by cargo feature, not by variant: these are one capability
        let err = check_combination(&[Capability::Mysql, Capability::Mariadb], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("the same capability"), "{err}");
        assert!(err.contains("`mysql` cargo feature"), "{err}");
        let err = check_combination(&[Capability::Redis, Capability::Redis], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("the same capability"), "{err}");
        // mongodb is not relational, but it is the same slot: `Db` or `Mongo`,
        // one adapter behind one `UserRepository`
        let err = check_combination(&[Capability::Postgres, Capability::Mongodb], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("at most one datastore"), "{err}");
        assert!(err.contains("postgres, mongodb"), "must name both: {err}");
        assert!(check_combination(&[Capability::Postgres], None).is_ok());
        // redis is not a backend, so it rides along with one
        assert!(check_combination(&[Capability::Postgres, Capability::Redis], None).is_ok());
        assert!(
            check_combination(
                &[Capability::Mongodb, Capability::Redis, Capability::S3],
                None
            )
            .is_ok(),
            "the additive capabilities must ride along with mongodb too"
        );
        assert!(check_combination(&[], None).is_ok());
    }

    /// The Phase 4 path: a second adapter of a port the template already has, so
    /// the whole capability is one new file plus three swapped blocks — no new
    /// port, no new `Services` field, and `tests/api.rs` still covers the routes.
    #[test]
    fn mongodb_takes_the_datastore_slot_and_leaves_diesel_on_disk() {
        let root = fixture("mongodb");
        add(&root, Capability::Mongodb, None, false).unwrap();

        // the slot, not a second entry beside it: sqlite is gone
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"document\"]"), "{toml}");

        let repo = fs::read_to_string(root.join(MONGO_REPO)).unwrap();
        assert!(repo.contains("impl UserRepository for MongoUserRepository"), "{repo}");
        // the port documents save as an upsert, and this is the line that keeps
        // that true on a server that would otherwise answer with a duplicate key
        assert!(repo.contains(".upsert(true)"), "{repo}");
        assert!(repo.contains("MONGODB_URL"), "the adapter test must be env-gated: {repo}");

        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("MONGODB_URL"), "{bootstrap}");
        assert!(bootstrap.contains("fn build_user_service(db: Option<&Mongo>)"), "{bootstrap}");
        assert!(!bootstrap.contains("DieselUserRepository"), "the Diesel wiring survived: {bootstrap}");
        assert!(!bootstrap.contains("DATABASE_URL"), "the Diesel wiring survived: {bootstrap}");
        // the `Services` literals are untouched — the slot is the same slot, so
        // the binding keeps its name and both call sites keep working
        assert!(bootstrap.contains("user: build_user_service(db.as_ref()),"), "{bootstrap}");
        assert!(bootstrap.contains("user: build_user_service(None),"), "{bootstrap}");
        for marker in [RESOURCES_MARKER, "// kodegen:services:env", "// kodegen:services:default"] {
            assert!(bootstrap.contains(marker), "marker '{marker}' was consumed: {bootstrap}");
        }

        // undo has to be possible: the Diesel files are gated, never deleted
        assert!(root.join(DIESEL_DB).is_file() && root.join(DIESEL_REPO).is_file());
        let infra = fs::read_to_string(root.join(INFRA_MOD)).unwrap();
        // gated both ways, which is what makes undo one line in Cargo.toml: with
        // `document` off, mongo_repo.rs stops compiling instead of failing to
        assert!(infra.contains(MONGO_MOD), "mongo_repo must be cfg-gated: {infra}");
        assert!(
            infra.matches("#[cfg(feature = \"relational\")]").count() == 2,
            "both diesel modules must stay behind the relational gate: {infra}"
        );

        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("image: mongo:8"), "{compose}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Both directions of the slot rule, because a user can arrive at it from
    /// either side and the second one is the path `add_relational` no longer
    /// checks for itself.
    #[test]
    fn a_relational_backend_on_top_of_mongodb_fails_with_instructions() {
        let root = fixture("mongo-then-pg");
        add(&root, Capability::Mongodb, None, false).unwrap();
        let err = add(&root, Capability::Postgres, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'document' is already this project's datastore"),
            "{err}"
        );
        assert!(
            err.contains("mongo_repo.rs"),
            "must name the file to remove: {err}"
        );
        // and the other direction
        let root2 = fixture("pg-then-mongo");
        add(&root2, Capability::Postgres, None, false).unwrap();
        let err = add(&root2, Capability::Mongodb, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'postgres (diesel)' is already this project's datastore"),
            "{err}"
        );
        assert!(err.contains("rerun `kodegen add mongodb`"), "{err}");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&root2).unwrap();
    }

    #[test]
    fn adding_mongodb_twice_fails() {
        let root = fixture("mongo-twice");
        add(&root, Capability::Mongodb, None, false).unwrap();
        let err = add(&root, Capability::Mongodb, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("capability 'mongodb' already added"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// A document store is a datastore, a cache and a blob store are not — so
    /// all three coexist, and neither of the additive `add`s may undo the
    /// `document` feature the way a relational one would.
    #[test]
    fn mongodb_coexists_with_a_cache_and_a_blob_store() {
        let root = fixture("mongo-and-friends");
        add(&root, Capability::Mongodb, None, false).unwrap();
        add(&root, Capability::Redis, None, false).unwrap();
        add(&root, Capability::S3, None, false).unwrap();
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"document\", \"cache\", \"blob\"]"), "{toml}");
        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        for service in ["  mongo:", "  redis:", "  s3:"] {
            assert!(compose.contains(service), "{service} missing: {compose}");
        }
        // the additive capabilities wire into the same file `add mongodb`
        // rewrote, and must not have lost the rewrite
        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("MONGODB_URL") && bootstrap.contains("REDIS_URL"), "{bootstrap}");
        assert!(bootstrap.contains("cache: build_cache(redis.as_ref()),"), "{bootstrap}");
        assert!(bootstrap.contains("blobs: build_blob_store(s3.as_ref()),"), "{bootstrap}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The other order, because `add redis` writes into `bootstrap.rs` first and
    /// `add mongodb` then has to find its three generated blocks in a file that
    /// already grew a cache builder.
    #[test]
    fn a_cache_added_before_mongodb_survives_the_rewrite() {
        let root = fixture("redis-then-mongo");
        add(&root, Capability::Redis, None, false).unwrap();
        add(&root, Capability::Mongodb, None, false).unwrap();
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"cache\", \"document\"]"), "{toml}");
        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("MONGODB_URL") && bootstrap.contains("REDIS_URL"), "{bootstrap}");
        assert!(bootstrap.contains("cache: build_cache(redis.as_ref()),"), "{bootstrap}");
        assert!(bootstrap.contains("fn build_user_service(db: Option<&Mongo>)"), "{bootstrap}");
        assert!(!bootstrap.contains("Db"), "the Diesel handle survived: {bootstrap}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// `add mongodb` deletes the `Db` type, so a feature scaffolded *after* it
    /// must ask for the handle the project actually has. The name is long on
    /// purpose: it pushes the generated signature past rustfmt's width, which is
    /// the shape the reverse order has to cope with too.
    #[test]
    fn a_feature_scaffolded_after_mongodb_takes_the_mongo_handle() {
        let root = fixture("mongo-then-feature");
        add(&root, Capability::Mongodb, None, false).unwrap();
        crate::scaffold::add_feature(
            &root,
            "notification",
            crate::scaffold::Transport::Rest,
            false,
        )
        .unwrap();
        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("_db: Option<&Mongo>"), "{bootstrap}");
        assert!(!bootstrap.contains("Db"), "a builder asked for a type that is gone: {bootstrap}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// And the reverse order, where the sweep is what has to work: `cargo fmt`
    /// has already broken the generated signature across lines by the time
    /// `add mongodb` goes looking for it.
    #[test]
    fn the_sweep_finds_a_builder_rustfmt_wrapped() {
        let root = fixture("feature-then-mongo");
        crate::scaffold::add_feature(&root, "notification", crate::scaffold::Transport::Rest, false)
            .unwrap();
        let before = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        // the precondition this test exists for — if rustfmt stops wrapping here,
        // the case is no longer covered and this says so rather than passing
        assert!(
            !before.contains("(_db: Option<&Db>)"),
            "expected rustfmt to wrap the long signature: {before}"
        );
        add(&root, Capability::Mongodb, None, false).unwrap();
        let after = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(after.contains("_db: Option<&Mongo>"), "the wrapped builder was missed: {after}");
        assert!(!after.contains("Db"), "{after}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// `Slot::features()` is a list of feature *names* that has to stay in step
    /// with `Kind::slot()`: the exhaustive matches are what make a new kind or a
    /// new slot a compile error, but nothing makes a new feature name show up in
    /// the right list — and a capability missing from its slot's list would be
    /// added on top of the one already there instead of refused.
    #[test]
    fn every_slot_lists_the_features_of_the_capabilities_that_fill_it() {
        use clap::ValueEnum;
        // both ORMs, because both put a name in the datastore slot's list: a
        // `seaorm-*` feature missing from it would be added on top of whatever
        // is already there rather than refused
        for cap in Capability::value_variants() {
            for orm in [Orm::Diesel, Orm::Seaorm] {
                for slot in Slot::ALL {
                    assert_eq!(
                        cap.kind().slot() == Some(slot),
                        slot.features().contains(&cap.feature(orm)),
                        "'{}' (feature `{}`, {orm:?}) disagrees with the {} slot",
                        cap.name(),
                        cap.feature(orm),
                        slot.what()
                    );
                }
            }
        }
        // not reachable through any capability — it is what the template ships
        // with, and the one entry `add` skips when it looks for a rival
        assert_eq!(Slot::Datastore.shipped(), Some("sqlite"));
        assert!(
            Slot::Datastore.features().contains(&"sqlite"),
            "the default backend must be in the list: {:?}",
            Slot::Datastore.features()
        );
        // the event sink has no shipped occupant: a fresh project has no
        // `EventSink` port at all, so the first `add` is always the only one
        assert_eq!(Slot::Events.shipped(), None);
        // `ALL` is the one list a new variant does not break by itself, so it is
        // pinned against a match that a new variant *does* break
        for slot in Slot::ALL {
            match slot {
                Slot::Datastore | Slot::Events => {}
            }
        }
        assert_eq!(Slot::ALL.len(), 2, "a new Slot variant has to join Slot::ALL");
    }

    #[test]
    fn mongodb_dry_run_writes_nothing_but_plans_the_same_change() {
        let root = fixture("mongo-dry");
        let before = snapshot(&root);
        let planned = add(&root, Capability::Mongodb, None, true).unwrap();
        assert_eq!(before, snapshot(&root), "dry run touched the tree");
        assert_eq!(
            planned,
            add(&root, Capability::Mongodb, None, false).unwrap()
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// `bootstrap.rs` is patched in place, so each of the three blocks is a
    /// signature: a project that no longer has one is refused rather than
    /// half-rewritten into something that will not compile.
    #[test]
    fn refuses_a_hand_edited_bootstrap() {
        for (tag, spike) in [
            ("mongo-edited-use", "diesel_repo::DieselUserRepository,"),
            ("mongo-edited-db", "expect(\"diesel pool init failed\")"),
            ("mongo-edited-builder", "fn build_user_service(db: Option<&Db>)"),
        ] {
            let root = fixture(tag);
            let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
            let spiked = bootstrap.replace(spike, "// mine now");
            fs::write(root.join(BOOTSTRAP), &spiked).unwrap();

            let err = add(&root, Capability::Mongodb, None, false)
                .unwrap_err()
                .to_string();
            assert!(err.contains("refusing to overwrite"), "{tag}: {err}");
            assert!(err.contains("MONGODB_URL"), "{tag}: must list the manual steps: {err}");
            assert_eq!(
                fs::read_to_string(root.join(BOOTSTRAP)).unwrap(),
                spiked,
                "{tag}: a refusal must not write"
            );
            assert!(!root.join(MONGO_REPO).exists(), "{tag}: a refusal must not write");
            fs::remove_dir_all(&root).unwrap();
        }
    }

    /// The interaction only a real `kodegen feature` run shows: every scaffolded
    /// builder takes the datastore handle, unused, and would name a type that no
    /// longer exists.
    #[test]
    fn scaffolded_feature_builders_move_to_the_mongo_handle() {
        let root = fixture("mongo-after-feature");
        crate::scaffold::add_feature(&root, "order", crate::scaffold::Transport::Rest, false)
            .unwrap();
        add(&root, Capability::Mongodb, None, false).unwrap();

        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("fn build_order_service(_db: Option<&Mongo>)"), "{bootstrap}");
        assert!(!bootstrap.contains("Db"), "the Diesel handle is gone, so nothing may name it: {bootstrap}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// A `Db` reference kodegen did not write is a hand-written adapter: the type
    /// disappears with the relational feature, so the project is refused rather
    /// than rewritten into something that will not compile.
    #[test]
    fn refuses_a_bootstrap_that_uses_the_db_handle_elsewhere() {
        let root = fixture("mongo-custom-db");
        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        fs::write(
            root.join(BOOTSTRAP),
            format!("{bootstrap}\nfn mine(db: &Db) -> &Db {{ db }}\n"),
        )
        .unwrap();
        let err = add(&root, Capability::Mongodb, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Diesel `Db` handle"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mongodb_refuses_to_overwrite_an_existing_adapter() {
        let root = fixture("mongo-existing-adapter");
        fs::write(root.join(MONGO_REPO), "// mine now\n").unwrap();
        let err = add(&root, Capability::Mongodb, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(MONGO_REPO) && err.contains("already exists"),
            "{err}"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// The Phase 6a path: the same backend as `add postgres`, behind the other
    /// ORM. Everything asserted here is either "the slot behaved like the slot"
    /// or "the Diesel-only work did not happen" — the second half is the payoff
    /// the capability exists for.
    #[test]
    fn seaorm_takes_the_datastore_slot_and_leaves_the_diesel_path_alone() {
        let root = fixture("seaorm-pg");
        let manifest_before = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let dockerfile_before = fs::read_to_string(root.join("Dockerfile")).unwrap();
        let seed = "migrations/2026-07-26-000000_create_users/up.sql";
        let seed_before = fs::read_to_string(root.join(seed)).unwrap();

        add(&root, Capability::Postgres, Some(Orm::Seaorm), false).unwrap();

        // the slot, not a second entry beside it: sqlite is gone, and the lock
        // stays valid because nothing but that one line moved
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"seaorm-postgres\"]"), "{toml}");
        let strip = |s: &str| {
            s.lines()
                .filter(|l| !l.starts_with("default = ["))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip(&manifest_before), strip(&toml));

        let db = fs::read_to_string(root.join(SEA_DB)).unwrap();
        assert!(
            db.contains("pub struct Db("),
            "the handle must be `Db`: {db}"
        );
        assert!(
            db.contains("CREATE TABLE IF NOT EXISTS users (id TEXT PRIMARY KEY NOT NULL"),
            "the schema must be the startup DDL, idempotent: {db}"
        );
        assert!(!db.contains("build_pool"), "{db}");
        // the startup-DDL race fix, and its limit: one retry, and not for an
        // error that says the server was never reached — retrying that pays the
        // acquire timeout twice for the same failure
        assert!(
            db.contains("if matches!(e, DbErr::Conn(_) | DbErr::ConnectionAcquire(_))"),
            "the schema loop must retry once, and never on a connection error: {db}"
        );

        let repo = fs::read_to_string(root.join(SEA_REPO)).unwrap();
        assert!(
            repo.contains("impl UserRepository for SeaUserRepository"),
            "{repo}"
        );
        // the port documents save as an upsert, and these two lines are what
        // keep that true against a server that answers 0 affected rows
        assert!(repo.contains(".update_column(Column::Name)"), "{repo}");
        assert!(repo.contains("exec_without_returning"), "{repo}");
        assert!(
            repo.contains("fn save_twice_identical_values_is_ok"),
            "{repo}"
        );
        assert!(
            repo.contains("DATABASE_URL"),
            "the adapter test must be env-gated: {repo}"
        );

        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(
            bootstrap.contains("sea_repo::SeaUserRepository"),
            "{bootstrap}"
        );
        assert!(
            bootstrap.contains("DATABASE_URL"),
            "the env var does not change: {bootstrap}"
        );
        assert!(
            bootstrap.contains("fn build_user_service(db: Option<&Db>)"),
            "{bootstrap}"
        );
        for gone in ["diesel", "Diesel", "build_pool"] {
            assert!(
                !bootstrap.contains(gone),
                "'{gone}' survived the swap: {bootstrap}"
            );
        }
        // the `Services` literals are untouched — same slot, same binding name
        assert!(
            bootstrap.contains("user: build_user_service(db.as_ref()),"),
            "{bootstrap}"
        );
        assert!(
            bootstrap.contains("user: build_user_service(None),"),
            "{bootstrap}"
        );
        for marker in [
            RESOURCES_MARKER,
            "// kodegen:services:env",
            "// kodegen:services:default",
        ] {
            assert!(
                bootstrap.contains(marker),
                "marker '{marker}' was consumed: {bootstrap}"
            );
        }

        // undo has to be possible: nothing is deleted, both sides are gated
        assert!(root.join(DIESEL_DB).is_file() && root.join(DIESEL_REPO).is_file());
        let infra = fs::read_to_string(root.join(INFRA_MOD)).unwrap();
        assert!(
            infra.contains(SEA_MOD),
            "the SeaORM modules must be cfg-gated: {infra}"
        );
        assert_eq!(
            infra.matches("#[cfg(feature = \"relational\")]").count(),
            2,
            "both Diesel modules must stay behind the relational gate: {infra}"
        );

        // and the two things this path exists to *not* do
        assert_eq!(
            dockerfile_before,
            fs::read_to_string(root.join("Dockerfile")).unwrap(),
            "the SeaORM path is pure Rust — the Dockerfile must not be touched"
        );
        assert_eq!(
            seed_before,
            fs::read_to_string(root.join(seed)).unwrap(),
            "migrations/ is not used on this path, and not ours to rewrite either"
        );

        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("image: postgres:17-alpine"), "{compose}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// TimescaleDB's startup DDL is the one place an idempotency bug hides
    /// until the *second* boot: `create_hypertable` without `if_not_exists`
    /// works once and then fails forever.
    #[test]
    fn timescaledb_on_seaorm_ships_an_idempotent_hypertable() {
        let root = fixture("seaorm-timescale");
        add(&root, Capability::Timescaledb, Some(Orm::Seaorm), false).unwrap();
        let db = fs::read_to_string(root.join(SEA_DB)).unwrap();
        for stmt in [
            "CREATE EXTENSION IF NOT EXISTS timescaledb",
            "CREATE TABLE IF NOT EXISTS users",
            "CREATE TABLE IF NOT EXISTS events",
            "if_not_exists => TRUE",
        ] {
            assert!(db.contains(stmt), "startup DDL is missing `{stmt}`: {db}");
        }
        // same feature as postgres on this ORM too, the way it is on the other
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"seaorm-postgres\"]"), "{toml}");
        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("timescale/timescaledb"), "{compose}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The Phase 6b path. MySQL and MariaDB are one cargo feature and one
    /// adapter on this ORM exactly as they are on the other — what differs is
    /// the column type (MySQL cannot index a bare `TEXT`) and the compose image.
    #[test]
    fn mysql_and_mariadb_on_seaorm_are_one_feature_and_differ_only_in_the_image() {
        let mut generated = Vec::new();
        for (tag, cap, image) in [
            ("seaorm-mysql", Capability::Mysql, "image: mysql:8.4"),
            ("seaorm-mariadb", Capability::Mariadb, "image: mariadb:11"),
        ] {
            let root = fixture(tag);
            let dockerfile_before = fs::read_to_string(root.join("Dockerfile")).unwrap();
            add(&root, cap, Some(Orm::Seaorm), false).unwrap();

            let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
            assert!(toml.contains("default = [\"seaorm-mysql\"]"), "{tag}: {toml}");

            let db = fs::read_to_string(root.join(SEA_DB)).unwrap();
            // the dialect lesson from the Diesel path, repeated here because the
            // startup DDL is a separate constant: `TEXT PRIMARY KEY` is rejected
            // by MySQL at run time, and this path has no migration step to fail
            // in first
            assert!(
                db.contains("CREATE TABLE IF NOT EXISTS users (id VARCHAR(64) PRIMARY KEY NOT NULL"),
                "{tag}: the MySQL startup DDL must not index a bare TEXT: {db}"
            );

            // the upsert is the reason this phase exists: `ON DUPLICATE KEY
            // UPDATE` reports 0 affected rows when the new values equal the old,
            // and `exec` would call that `RecordNotInserted`
            let repo = fs::read_to_string(root.join(SEA_REPO)).unwrap();
            assert!(repo.contains("exec_without_returning"), "{tag}: {repo}");
            assert!(
                repo.contains("fn save_twice_identical_values_is_ok"),
                "{tag}: {repo}"
            );

            // the payoff over `--orm diesel` on this backend is bigger than it is
            // on postgres: no mysqlclient-sys, so no cmake either
            assert_eq!(
                dockerfile_before,
                fs::read_to_string(root.join("Dockerfile")).unwrap(),
                "{tag}: the SeaORM path adds no C driver, so the build stage is untouched"
            );

            let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
            assert!(compose.contains(image), "{tag}: {compose}");
            generated.push((root, compose));
        }

        // one feature, one adapter, one set of generated Rust: apart from the
        // name in the doc comments, only the compose service tells the two apart
        // — which is what makes a CI row for mariadb a row that proves nothing
        let [(mysql_root, mysql_compose), (maria_root, maria_compose)]: [_; 2] =
            generated.try_into().unwrap();
        for file in [SEA_DB, SEA_REPO, BOOTSTRAP, "Cargo.toml"] {
            let maria = fs::read_to_string(maria_root.join(file))
                .unwrap()
                .replace("MariaDB", "MySQL")
                .replace("mariadb", "mysql");
            assert_eq!(
                fs::read_to_string(mysql_root.join(file)).unwrap(),
                maria,
                "{file} differs between mysql and mariadb on SeaORM by more than the name"
            );
        }
        assert_ne!(mysql_compose, maria_compose);

        for dir in [mysql_root, maria_root] {
            fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// Every direction into the datastore slot, because a user can arrive from
    /// any of them and each one has to name what to put back.
    #[test]
    fn the_two_orms_refuse_each_other_from_every_direction() {
        // Diesel first, SeaORM second
        let root = fixture("pg-then-seaorm");
        add(&root, Capability::Postgres, None, false).unwrap();
        let err = add(&root, Capability::Postgres, Some(Orm::Seaorm), true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'postgres (diesel)' is already this project's datastore"),
            "{err}"
        );
        assert!(
            err.contains("diesel_db.rs"),
            "must name the files to restore: {err}"
        );
        assert!(
            err.contains("rerun `kodegen add postgres --orm seaorm`"),
            "the last step must be the command that was typed: {err}"
        );

        // SeaORM first, Diesel second
        let root2 = fixture("seaorm-then-pg");
        add(&root2, Capability::Postgres, Some(Orm::Seaorm), false).unwrap();
        let err = add(&root2, Capability::Mysql, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'postgres (seaorm)' is already this project's datastore"),
            "{err}"
        );
        assert!(
            err.contains("sea_db.rs + sea_repo.rs"),
            "must name what can stay: {err}"
        );
        assert!(err.contains("rerun `kodegen add mysql`"), "{err}");

        // and the third occupant, both ways
        let err = add(&root2, Capability::Mongodb, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'postgres (seaorm)' is already this project's datastore"),
            "{err}"
        );
        let root3 = fixture("mongo-then-seaorm");
        add(&root3, Capability::Mongodb, None, false).unwrap();
        let err = add(&root3, Capability::Postgres, Some(Orm::Seaorm), true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("'document' is already this project's datastore"),
            "{err}"
        );

        // the same capability on the same ORM twice is the other error
        let err = add(&root2, Capability::Timescaledb, Some(Orm::Seaorm), true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("capability 'timescaledb' already added"),
            "{err}"
        );

        for dir in [root, root2, root3] {
            fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// `--orm` is a flag about *this* command, so it is checked before anything
    /// is read — an ignored flag is how a project ends up on an ORM nobody
    /// asked for.
    #[test]
    fn the_orm_flag_is_refused_where_it_means_nothing() {
        let root = fixture("orm-flag");
        for cap in [
            Capability::Redis,
            Capability::S3,
            Capability::Mongodb,
            Capability::Clickhouse,
            Capability::Influxdb,
        ] {
            for orm in [Orm::Diesel, Orm::Seaorm] {
                let err = add(&root, cap, Some(orm), true).unwrap_err().to_string();
                assert!(
                    err.contains("--orm applies only to relational capabilities")
                        && err.contains(cap.name()),
                    "{}: {err}",
                    cap.name()
                );
            }
        }
        // and it is refused before the project is even looked at
        let err = add(
            Path::new("/nonexistent-project"),
            Capability::Redis,
            Some(Orm::Seaorm),
            true,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("--orm applies only"), "{err}");

        // ...and every relational backend takes both ORMs, which is the other
        // half of the same rule: the flag is refused where it means nothing, and
        // accepted everywhere it means something
        for cap in [
            Capability::Postgres,
            Capability::Timescaledb,
            Capability::Mysql,
            Capability::Mariadb,
        ] {
            for orm in [Orm::Diesel, Orm::Seaorm] {
                assert!(
                    check_orm(cap, Some(orm)).is_ok(),
                    "{} must accept both ORMs",
                    cap.name()
                );
            }
        }

        // `new --with` takes the same flag, and it has to land on something
        let err = check_combination(&[Capability::Redis, Capability::S3], Some(Orm::Seaorm))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--orm needs a relational capability"), "{err}");
        assert!(
            check_combination(
                &[Capability::Postgres, Capability::Redis],
                Some(Orm::Seaorm)
            )
            .is_ok(),
            "the ORM belongs to the relational entry; the others must not be refused"
        );
        assert!(check_combination(&[Capability::Mysql], Some(Orm::Seaorm)).is_ok());
        fs::remove_dir_all(&root).unwrap();
    }

    /// The guard has to *pass* on the file kodegen itself writes, not only
    /// refuse a hand-edited one: the three replacement blocks are the output it
    /// then inspects, so one stray "Diesel" in a comment inside them would make
    /// the command refuse its own work — on every project, forever.
    #[test]
    fn the_seaorm_guard_passes_on_a_fresh_bootstrap_and_refuses_a_spiked_one() {
        let fresh = crate::TEMPLATES
            .get_file("hexagonal/src/bootstrap.rs")
            .unwrap()
            .contents_utf8()
            .unwrap();
        let out =
            rewrite_bootstrap(fresh, &SEA_SWAP).expect("the swap must succeed on the template");
        for word in SEA_SWAP.leftovers {
            assert!(
                !out.contains(word),
                "the replacement blocks mention '{word}': {out}"
            );
        }
        assert!(out.contains("sea_db::Db,"), "{out}");

        // ...and a hand-written call to the old handle is the refusal case: the
        // type keeps its name across the swap, so only this catches it
        let spiked = format!(
            "{fresh}\nfn mine(db: &Db) {{ let _ = db.run(|_| diesel::result::QueryResult::Ok(())); }}\n"
        );
        let err = rewrite_bootstrap(&spiked, &SEA_SWAP)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no mention of Diesel"), "{err}");
        assert!(
            err.contains("DATABASE_URL="),
            "must list the manual steps: {err}"
        );
    }

    /// The three blocks are signatures on this path too, and a refusal must not
    /// leave half a swap or a stray adapter behind.
    #[test]
    fn seaorm_refuses_a_hand_edited_bootstrap() {
        for (tag, spike) in [
            ("sea-edited-use", "diesel_repo::DieselUserRepository,"),
            ("sea-edited-db", "expect(\"diesel pool init failed\")"),
            (
                "sea-edited-builder",
                "fn build_user_service(db: Option<&Db>)",
            ),
        ] {
            let root = fixture(tag);
            let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
            let spiked = bootstrap.replace(spike, "// mine now");
            fs::write(root.join(BOOTSTRAP), &spiked).unwrap();

            let err = add(&root, Capability::Postgres, Some(Orm::Seaorm), false)
                .unwrap_err()
                .to_string();
            assert!(err.contains("refusing to overwrite"), "{tag}: {err}");
            assert!(
                err.contains("sea_db.rs"),
                "{tag}: must list the manual steps: {err}"
            );
            assert_eq!(
                fs::read_to_string(root.join(BOOTSTRAP)).unwrap(),
                spiked,
                "{tag}: a refusal must not write"
            );
            assert!(
                !root.join(SEA_DB).exists(),
                "{tag}: a refusal must not write"
            );
            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn seaorm_refuses_to_overwrite_an_existing_adapter() {
        let root = fixture("seaorm-existing-adapter");
        fs::write(root.join(SEA_REPO), "// mine now\n").unwrap();
        let err = add(&root, Capability::Postgres, Some(Orm::Seaorm), true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(SEA_REPO) && err.contains("already exists"),
            "{err}"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn seaorm_dry_run_writes_nothing_but_plans_the_same_change() {
        // both backends: they write the same four paths, and a plan that says so
        // for one and not the other would be the first sign they had drifted
        for (tag, cap) in [
            ("seaorm-dry", Capability::Postgres),
            ("seaorm-dry-mysql", Capability::Mysql),
        ] {
            let root = fixture(tag);
            let before = snapshot(&root);
            let planned = add(&root, cap, Some(Orm::Seaorm), true).unwrap();
            assert_eq!(before, snapshot(&root), "{tag}: dry run touched the tree");
            assert_eq!(planned, add(&root, cap, Some(Orm::Seaorm), false).unwrap());
            fs::remove_dir_all(&root).unwrap();
        }
    }

    /// The whole reason the SeaORM handle is called `Db`: a builder scaffolded
    /// on either side of the switch asks for a type that exists on both, so
    /// neither order needs a rewrite — and `scaffold.rs` needs no third state.
    #[test]
    fn scaffolded_builders_survive_the_switch_in_both_orders() {
        let root = fixture("seaorm-then-feature");
        add(&root, Capability::Postgres, Some(Orm::Seaorm), false).unwrap();
        crate::scaffold::add_feature(
            &root,
            "notification",
            crate::scaffold::Transport::Rest,
            false,
        )
        .unwrap();
        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("_db: Option<&Db>"), "{bootstrap}");
        assert!(!bootstrap.contains("Diesel"), "{bootstrap}");

        // and the other order, where `cargo fmt` has already wrapped the
        // generated signature by the time `add` runs
        let root2 = fixture("feature-then-seaorm");
        crate::scaffold::add_feature(
            &root2,
            "notification",
            crate::scaffold::Transport::Rest,
            false,
        )
        .unwrap();
        add(&root2, Capability::Postgres, Some(Orm::Seaorm), false).unwrap();
        let after = fs::read_to_string(root2.join(BOOTSTRAP)).unwrap();
        assert!(after.contains("_db: Option<&Db>"), "{after}");
        assert!(!after.contains("diesel"), "{after}");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&root2).unwrap();
    }

    /// The additive capabilities wire into the same `bootstrap.rs` this path
    /// rewrites, from both sides.
    #[test]
    fn seaorm_coexists_with_a_cache_a_blob_store_and_an_event_sink() {
        let root = fixture("seaorm-and-friends");
        add(&root, Capability::Redis, None, false).unwrap();
        add(&root, Capability::Postgres, Some(Orm::Seaorm), false).unwrap();
        add(&root, Capability::S3, None, false).unwrap();
        add(&root, Capability::Clickhouse, None, false).unwrap();

        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            toml.contains("default = [\"cache\", \"seaorm-postgres\", \"blob\", \"analytics\"]"),
            "{toml}"
        );
        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("SeaUserRepository"), "{bootstrap}");
        assert!(
            bootstrap.contains("cache: build_cache(redis.as_ref()),"),
            "{bootstrap}"
        );
        assert!(
            bootstrap.contains("blobs: build_blob_store(s3.as_ref()),"),
            "{bootstrap}"
        );
        assert!(
            bootstrap.contains("events: build_event_sink(clickhouse.as_ref()),"),
            "{bootstrap}"
        );
        assert!(!bootstrap.contains("diesel"), "{bootstrap}");
        assert_eq!(
            bootstrap.matches("readiness.push(").count(),
            4,
            "{bootstrap}"
        );
        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        for service in ["  postgres:", "  redis:", "  s3:", "  clickhouse:"] {
            assert!(compose.contains(service), "{service} missing: {compose}");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    /// The Phase 7 path: the datastore slot with no ORM behind it at all.
    ///
    /// Structurally `add_seaorm`, so most of what is asserted here is the same
    /// list. What is new is the SQL — every statement of it is a *runtime*
    /// failure if it is wrong, because nothing on this path compiles a query.
    #[test]
    fn mssql_takes_the_datastore_slot_with_no_orm_behind_it() {
        let root = fixture("mssql");
        let manifest_before = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        let seed = "migrations/2026-07-26-000000_create_users/up.sql";
        let seed_before = fs::read_to_string(root.join(seed)).unwrap();

        add(&root, Capability::Mssql, None, false).unwrap();

        // the slot, not a second entry beside it — and the lock stays valid
        // because nothing but that one line moved
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"mssql\"]"), "{toml}");
        let strip = |s: &str| {
            s.lines()
                .filter(|l| !l.starts_with("default = ["))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(strip(&manifest_before), strip(&toml));

        let db = code_only(&fs::read_to_string(root.join(MSSQL_DB)).unwrap());
        assert!(
            db.contains("pub struct Db("),
            "the handle must be `Db`, or every scaffolded builder breaks: {db}"
        );
        // SQL Server has no `CREATE TABLE IF NOT EXISTS`. Copying it in from
        // another adapter is a syntax error nothing here would catch until a
        // server saw it.
        assert!(
            db.contains("IF OBJECT_ID(N'dbo.users', N'U') IS NULL"),
            "the DDL must be idempotent the way SQL Server spells it: {db}"
        );
        assert!(
            !db.contains("CREATE TABLE IF NOT EXISTS"),
            "that spelling does not exist on this backend: {db}"
        );
        // NVARCHAR, not VARCHAR: tiberius sends every Rust string as NVARCHAR,
        // and the mismatch is a silent index-seek-to-scan, never an error
        assert!(db.contains("id   NVARCHAR(64)"), "{db}");
        // ...and the widths are the domain's own limits. A name of exactly 256
        // bytes is a valid `User`, and `ANSI_WARNINGS ON` makes an over-long
        // string error 2628 rather than a truncation — so a narrower column is a
        // 500 on a request every other datastore in this repo accepts.
        assert!(db.contains("name NVARCHAR(256)"), "{db}");
        assert!(
            !db.contains(" VARCHAR("),
            "a VARCHAR column would make every lookup an implicit conversion: {db}"
        );
        // the startup-DDL race fix and its limit, exactly as on the SeaORM path:
        // one retry, and only for something the server answered
        assert!(
            db.contains("if !matches!(e, TiberiusError::Server(_))"),
            "the schema loop must retry once, and never on a transport error: {db}"
        );

        let repo = code_only(&fs::read_to_string(root.join(MSSQL_REPO)).unwrap());
        assert!(
            repo.contains("impl UserRepository for MssqlUserRepository"),
            "{repo}"
        );
        // the two halves of the upsert that fail silently when they are missing:
        // without HOLDLOCK the MERGE is racy under a concurrent writer and every
        // test still passes; without the trailing `;` it is a syntax error
        assert!(repo.contains("MERGE dbo.users WITH (HOLDLOCK)"), "{repo}");
        assert!(
            repo.contains("VALUES (s.id, s.name);"),
            "MERGE must be terminated with a semicolon: {repo}"
        );
        assert!(
            repo.contains("fn save_twice_identical_values_is_ok"),
            "{repo}"
        );
        assert!(
            repo.contains("DATABASE_URL"),
            "the adapter test must be env-gated: {repo}"
        );

        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("mssql_repo::MssqlUserRepository"), "{bootstrap}");
        assert!(
            bootstrap.contains("DATABASE_URL"),
            "the env var does not change: {bootstrap}"
        );
        assert!(
            bootstrap.contains("fn build_user_service(db: Option<&Db>)"),
            "{bootstrap}"
        );
        for gone in ["diesel", "Diesel", "build_pool"] {
            assert!(!bootstrap.contains(gone), "'{gone}' survived the swap: {bootstrap}");
        }
        // the `Services` literals are untouched — same slot, same binding name
        assert!(bootstrap.contains("user: build_user_service(db.as_ref()),"), "{bootstrap}");
        assert!(bootstrap.contains("user: build_user_service(None),"), "{bootstrap}");
        for marker in [
            RESOURCES_MARKER,
            "// kodegen:services:env",
            "// kodegen:services:default",
        ] {
            assert!(bootstrap.contains(marker), "marker '{marker}' was consumed: {bootstrap}");
        }

        // undo has to be possible: nothing is deleted, both sides are gated
        assert!(root.join(DIESEL_DB).is_file() && root.join(DIESEL_REPO).is_file());
        let infra = fs::read_to_string(root.join(INFRA_MOD)).unwrap();
        assert!(infra.contains(MSSQL_MOD), "the modules must be cfg-gated: {infra}");
        assert_eq!(
            infra.matches("#[cfg(feature = \"relational\")]").count(),
            2,
            "both Diesel modules must stay behind the relational gate: {infra}"
        );

        // migrations/ is not used on this path, and not ours to rewrite either
        assert_eq!(seed_before, fs::read_to_string(root.join(seed)).unwrap());

        // the Dockerfile *is* touched here, unlike the SeaORM path — vendored
        // OpenSSL needs a build toolchain, and a statically linked one needs to
        // be told where the CA bundle is
        let dockerfile = fs::read_to_string(root.join("Dockerfile")).unwrap();
        assert!(
            dockerfile.contains("protobuf-compiler perl make"),
            "the build stage needs perl/make for vendored OpenSSL: {dockerfile}"
        );
        assert!(
            dockerfile.contains("ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt"),
            "without this, TLS fails against every server with a real certificate while dev and \
             CI stay green: {dockerfile}"
        );
        // ...in the runtime stage, not the build one, or it does nothing
        let (_, runtime) = dockerfile.split_once(RUNTIME_ANCHOR).expect("runtime stage");
        assert!(runtime.contains("ENV SSL_CERT_FILE="), "{dockerfile}");

        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("image: mcr.microsoft.com/mssql/server:2022-latest"), "{compose}");
        // the second service, which no other capability needs: this image
        // creates no database of its own
        assert!(compose.contains("  mssql-init:"), "{compose}");
        assert!(compose.contains("CREATE DATABASE app"), "{compose}");
        // and the two things that are wrong in most examples on the web
        assert!(compose.contains("/opt/mssql-tools18/bin/sqlcmd"), "{compose}");
        assert!(compose.contains("platform: linux/amd64"), "no arm64 image exists: {compose}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Cheap test, expensive bug. This adapter is the only one in the tree whose
    /// queries are strings rather than a builder, so "no interpolation anywhere"
    /// is the whole of its injection story — and a `format!` is how that story
    /// ends. The parameters go through `execute`/`query`'s slice, which is a real
    /// TDS bind.
    #[test]
    fn the_mssql_adapter_never_builds_sql_by_formatting() {
        for (what, tpl) in [("repo", MSSQL_REPO_TPL), ("db", MSSQL_DB_TPL)] {
            assert!(
                !code_only(tpl).contains("format!"),
                "{what}: SQL in this adapter must never be built by formatting"
            );
        }
        assert!(MSSQL_REPO_TPL.contains("&[&user.id, &user.name]"), "the upsert must bind");
        assert!(MSSQL_REPO_TPL.contains("&[&id]"), "the lookup must bind");
    }

    /// One password, three places — the connection string kodegen prints, the
    /// server that has to accept it and the init service that has to log in with
    /// it. Change it in one and the compose file boots a server nothing can
    /// reach, which reads like a network problem.
    ///
    /// Spelled out here rather than shared as a constant: this is the definition,
    /// and the three below are what it pins.
    #[test]
    fn the_dev_password_is_the_same_everywhere_it_appears() {
        const SA: &str = "kodeGen!2026";
        for (what, s) in [
            ("DATABASE_URL", MSSQL_URL),
            ("the mssql service", MSSQL_COMPOSE),
            ("the init service", MSSQL_INIT_COMPOSE),
        ] {
            assert!(
                s.contains(SA),
                "{what} no longer carries the SA password the others use"
            );
        }
        // and it still passes the image's own policy — at least 8 characters and
        // three of upper/lower/digit/symbol — which is why this is not `app:app`
        // like every other dev service in that file
        assert!(SA.len() >= 8);
        assert!(SA.chars().any(|c| c.is_ascii_uppercase()));
        assert!(SA.chars().any(|c| c.is_ascii_lowercase()));
        assert!(SA.chars().any(|c| c.is_ascii_digit()));
        assert!(SA.chars().any(|c| c.is_ascii_punctuation()));
    }

    /// `--orm` on `mssql` gets its own sentence, because the general one is
    /// wrong here: SQL Server *is* relational, so "--orm applies only to
    /// relational capabilities" would read as a contradiction to whoever just
    /// typed it. What is missing is an ORM, and the message has to say so.
    #[test]
    fn the_orm_flag_on_mssql_says_why_rather_than_that_it_is_not_relational() {
        for orm in [Orm::Diesel, Orm::Seaorm] {
            // before the project is even looked at
            let err = add(Path::new("/nonexistent-project"), Capability::Mssql, Some(orm), true)
                .unwrap_err()
                .to_string();
            assert!(err.contains("--orm does not apply to 'mssql'"), "{err}");
            assert!(err.contains("SeaORM X"), "must say where the support is: {err}");
            assert!(
                !err.contains("applies only to relational capabilities"),
                "the general message is the wrong one here: {err}"
            );
        }
        // ...and `new --with mssql --orm seaorm` gets the *same* sentence, not
        // the generic "nothing in this list has an ORM": the user named a SQL
        // database, so the answer has to be about that database.
        let err = check_combination(&[Capability::Mssql, Capability::Redis], Some(Orm::Seaorm))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--orm does not apply to 'mssql'"), "{err}");
        // the generic one is still what a list with no SQL database at all gets
        let err = check_combination(&[Capability::Redis, Capability::S3], Some(Orm::Seaorm))
            .unwrap_err()
            .to_string();
        assert!(err.contains("--orm needs a relational capability"), "{err}");
    }

    /// The datastore slot has four occupants now, and a user can arrive from any
    /// of them.
    #[test]
    fn mssql_refuses_and_is_refused_by_every_other_datastore() {
        // into mssql, from each of the other three
        for (tag, first, orm, occupant) in [
            ("pg-then-mssql", Capability::Postgres, None, "'postgres (diesel)'"),
            (
                "seaorm-then-mssql",
                Capability::Postgres,
                Some(Orm::Seaorm),
                "'postgres (seaorm)'",
            ),
            ("mongo-then-mssql", Capability::Mongodb, None, "'document'"),
        ] {
            let root = fixture(tag);
            add(&root, first, orm, false).unwrap();
            let err = add(&root, Capability::Mssql, None, true).unwrap_err().to_string();
            assert!(
                err.contains(&format!("capability {occupant} is already this project's datastore")),
                "{tag}: {err}"
            );
            assert!(err.contains("rerun `kodegen add mssql`"), "{tag}: {err}");
            fs::remove_dir_all(&root).unwrap();
        }

        // ...and out of mssql, into each of them
        let root = fixture("mssql-then-others");
        add(&root, Capability::Mssql, None, false).unwrap();
        for (cap, orm) in [
            (Capability::Postgres, None),
            (Capability::Mysql, Some(Orm::Seaorm)),
            (Capability::Mongodb, None),
        ] {
            let err = add(&root, cap, orm, true).unwrap_err().to_string();
            assert!(
                err.contains("capability 'mssql' is already this project's datastore"),
                "{}: {err}",
                cap.name()
            );
            // no "(diesel)" suffix on an occupant that has no ORM
            assert!(!err.contains("'mssql (diesel)'"), "{err}");
            assert!(
                err.contains("mssql_db.rs + mssql_repo.rs"),
                "must name what can stay: {err}"
            );
        }
        // and twice is the other error
        let err = add(&root, Capability::Mssql, None, true).unwrap_err().to_string();
        assert!(err.contains("capability 'mssql' already added"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The guard has to *pass* on the file kodegen itself writes, not only refuse
    /// a hand-edited one — the three replacement blocks are the output it then
    /// inspects, so one stray "Diesel" in a comment inside them would make the
    /// command refuse its own work, on every project, forever.
    #[test]
    fn the_mssql_guard_passes_on_a_fresh_bootstrap_and_refuses_a_spiked_one() {
        let fresh = crate::TEMPLATES
            .get_file("hexagonal/src/bootstrap.rs")
            .unwrap()
            .contents_utf8()
            .unwrap();
        let out = rewrite_bootstrap(fresh, &MSSQL_SWAP).expect("the swap must succeed on the template");
        for word in MSSQL_SWAP.leftovers {
            assert!(!out.contains(word), "the replacement blocks mention '{word}': {out}");
        }
        assert!(out.contains("mssql_db::Db,"), "{out}");

        // the handle keeps its name across the swap, so a hand-written call to
        // the old one is the only thing this can catch
        let spiked = format!(
            "{fresh}\nfn mine(db: &Db) {{ let _ = db.run(|_| diesel::result::QueryResult::Ok(())); }}\n"
        );
        let err = rewrite_bootstrap(&spiked, &MSSQL_SWAP).unwrap_err().to_string();
        assert!(err.contains("no mention of Diesel"), "{err}");
        assert!(err.contains("DATABASE_URL="), "must list the manual steps: {err}");
        assert!(err.contains("SSL_CERT_FILE"), "the manual steps must include the CA path: {err}");
    }

    #[test]
    fn mssql_refuses_a_hand_edited_bootstrap() {
        for (tag, spike) in [
            ("mssql-edited-use", "diesel_repo::DieselUserRepository,"),
            ("mssql-edited-db", "expect(\"diesel pool init failed\")"),
            ("mssql-edited-builder", "fn build_user_service(db: Option<&Db>)"),
        ] {
            let root = fixture(tag);
            let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
            let spiked = bootstrap.replace(spike, "// mine now");
            fs::write(root.join(BOOTSTRAP), &spiked).unwrap();

            let err = add(&root, Capability::Mssql, None, false).unwrap_err().to_string();
            assert!(err.contains("refusing to overwrite"), "{tag}: {err}");
            assert!(err.contains("mssql_db.rs"), "{tag}: must list the manual steps: {err}");
            assert_eq!(
                fs::read_to_string(root.join(BOOTSTRAP)).unwrap(),
                spiked,
                "{tag}: a refusal must not write"
            );
            assert!(!root.join(MSSQL_DB).exists(), "{tag}: a refusal must not write");
            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn mssql_refuses_to_overwrite_an_existing_adapter() {
        let root = fixture("mssql-existing-adapter");
        fs::write(root.join(MSSQL_REPO), "// mine now\n").unwrap();
        let err = add(&root, Capability::Mssql, None, true).unwrap_err().to_string();
        assert!(err.contains(MSSQL_REPO) && err.contains("already exists"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Two anchors, two different repairs — and the second one is the whole
    /// reason this capability touches the Dockerfile at all, so its message has
    /// to say what is lost by skipping it.
    #[test]
    fn mssql_refuses_a_dockerfile_it_cannot_complete() {
        let root = fixture("mssql-no-apt");
        fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
        let err = add(&root, Capability::Mssql, None, true).unwrap_err().to_string();
        assert!(err.contains(APT_ANCHOR) && err.contains("perl make"), "{err}");

        let root2 = fixture("mssql-no-runtime");
        let dockerfile = fs::read_to_string(root2.join("Dockerfile")).unwrap();
        fs::write(
            root2.join("Dockerfile"),
            dockerfile.replace(RUNTIME_ANCHOR, "FROM debian:bookworm-slim"),
        )
        .unwrap();
        let err = add(&root2, Capability::Mssql, None, true).unwrap_err().to_string();
        assert!(err.contains("SSL_CERT_FILE"), "{err}");
        assert!(
            err.contains("TrustServerCertificate"),
            "must say what stays green while this is broken: {err}"
        );
        for dir in [root, root2] {
            fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn mssql_dry_run_writes_nothing_but_plans_the_same_change() {
        let root = fixture("mssql-dry");
        let before = snapshot(&root);
        let planned = add(&root, Capability::Mssql, None, true).unwrap();
        assert_eq!(before, snapshot(&root), "dry run touched the tree");
        assert_eq!(planned, add(&root, Capability::Mssql, None, false).unwrap());
        fs::remove_dir_all(&root).unwrap();
    }

    /// The reason this handle is called `Db` as well: a builder scaffolded on
    /// either side of the switch asks for a type that exists on both.
    #[test]
    fn scaffolded_builders_survive_the_mssql_switch_in_both_orders() {
        let root = fixture("mssql-then-feature");
        add(&root, Capability::Mssql, None, false).unwrap();
        crate::scaffold::add_feature(&root, "notification", crate::scaffold::Transport::Rest, false)
            .unwrap();
        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("_db: Option<&Db>"), "{bootstrap}");
        assert!(!bootstrap.contains("Diesel"), "{bootstrap}");

        // and the other order, where `cargo fmt` has already wrapped the
        // generated signature by the time `add` runs
        let root2 = fixture("feature-then-mssql");
        crate::scaffold::add_feature(&root2, "notification", crate::scaffold::Transport::Rest, false)
            .unwrap();
        add(&root2, Capability::Mssql, None, false).unwrap();
        let after = fs::read_to_string(root2.join(BOOTSTRAP)).unwrap();
        assert!(after.contains("_db: Option<&Db>"), "{after}");
        assert!(!after.contains("diesel"), "{after}");
        for dir in [root, root2] {
            fs::remove_dir_all(&dir).unwrap();
        }
    }

    /// The additive capabilities wire into the same `bootstrap.rs` and the same
    /// compose file this path rewrites, from both sides.
    #[test]
    fn mssql_coexists_with_a_cache_a_blob_store_and_an_event_sink() {
        let root = fixture("mssql-and-friends");
        add(&root, Capability::Redis, None, false).unwrap();
        add(&root, Capability::Mssql, None, false).unwrap();
        add(&root, Capability::S3, None, false).unwrap();
        add(&root, Capability::Clickhouse, None, false).unwrap();

        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(
            toml.contains("default = [\"cache\", \"mssql\", \"blob\", \"analytics\"]"),
            "{toml}"
        );
        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("MssqlUserRepository"), "{bootstrap}");
        assert!(bootstrap.contains("cache: build_cache(redis.as_ref()),"), "{bootstrap}");
        assert!(bootstrap.contains("blobs: build_blob_store(s3.as_ref()),"), "{bootstrap}");
        assert!(bootstrap.contains("events: build_event_sink(clickhouse.as_ref()),"), "{bootstrap}");
        assert!(!bootstrap.contains("diesel"), "{bootstrap}");
        assert_eq!(
            bootstrap.matches("readiness.push(").count(),
            4,
            "{bootstrap}"
        );
        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        for service in ["  mssql:", "  mssql-init:", "  redis:", "  s3:", "  clickhouse:"] {
            assert!(compose.contains(service), "{service} missing: {compose}");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refuses_to_clobber_a_hand_edited_diesel_db() {
        let root = fixture("handedited");
        fs::write(root.join(DIESEL_DB), "// mine now\n").unwrap();
        let err = add(&root, Capability::Postgres, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing to overwrite"), "{err}");
        assert!(err.contains("PgConnection"), "must list the manual steps: {err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Why the test block is matched as an exact suffix rather than cut at the
    /// first `#[cfg(test)]`: cutting would drop this import *and* the entire
    /// `impl UserRepository` between it and the tests. Matching the tail keeps
    /// both and still swaps the module that has to change.
    #[test]
    fn keeps_a_cfg_test_item_that_sits_above_the_test_module() {
        let root = fixture("cfg-test-import");
        let repo = fs::read_to_string(root.join(DIESEL_REPO)).unwrap();
        let at = repo.find("#[cfg(test)]").unwrap();
        fs::write(
            root.join(DIESEL_REPO),
            format!("{}#[cfg(test)]\nuse std::sync::Arc;\n\n{}", &repo[..at], &repo[at..]),
        )
        .unwrap();

        add(&root, Capability::Postgres, None, false).unwrap();
        let out = fs::read_to_string(root.join(DIESEL_REPO)).unwrap();
        assert!(out.contains("use std::sync::Arc;"), "the stray cfg(test) item was eaten: {out}");
        assert!(out.contains("impl UserRepository for DieselUserRepository"), "the impl was eaten: {out}");
        assert!(out.contains("DATABASE_URL"), "the test module was not swapped: {out}");
        assert!(!out.contains("fn temp_repo"), "the SQLite test helper survived: {out}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// A test module that is no longer the one kodegen wrote must not be
    /// replaced — that is a user's tests, and we would be deleting them.
    #[test]
    fn refuses_a_hand_edited_test_module() {
        let root = fixture("edited-tests");
        let repo = fs::read_to_string(root.join(DIESEL_REPO)).unwrap();
        let spiked = repo.replace("fn temp_repo(tag: &str)", "fn temp_repo(tag: &'static str)");
        fs::write(root.join(DIESEL_REPO), &spiked).unwrap();

        let err = add(&root, Capability::Postgres, None, false)
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing to overwrite"), "{err}");
        assert_eq!(fs::read_to_string(root.join(DIESEL_REPO)).unwrap(), spiked, "a refusal must not write");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Queries added below the generated ones are exactly what the suffix match
    /// exists to preserve.
    #[test]
    fn keeps_hand_written_code_above_the_test_module() {
        let root = fixture("extra-query");
        let repo = fs::read_to_string(root.join(DIESEL_REPO)).unwrap();
        let at = repo.find("#[cfg(test)]").unwrap();
        fs::write(
            root.join(DIESEL_REPO),
            format!("{}pub fn count_users() -> usize {{\n    0\n}}\n\n{}", &repo[..at], &repo[at..]),
        )
        .unwrap();

        add(&root, Capability::Postgres, None, false).unwrap();
        let out = fs::read_to_string(root.join(DIESEL_REPO)).unwrap();
        assert!(out.contains("pub fn count_users()"), "{out}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refuses_a_dockerfile_without_the_apt_anchor() {
        let root = fixture("no-apt");
        fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
        let err = add(&root, Capability::Postgres, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains(APT_ANCHOR), "{err}");
        assert!(err.contains("perl make"), "must name the packages to add: {err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn compose_anchor_is_line_matched_and_service_names_are_unique() {
        // a commented-out `services:` is not somewhere to write a service
        let pg = |c: &str| add_compose_service(c, "postgres", POSTGRES_COMPOSE);
        let err = pg("# services:\n").unwrap_err().to_string();
        assert!(err.contains("no top-level `services:` line"), "{err}");

        let one = pg("services:\n").unwrap();
        assert!(one.contains("  postgres:\n    image: postgres:17-alpine"), "{one}");

        let err = pg(&one).unwrap_err().to_string();
        assert!(err.contains("already defines a `postgres` service"), "{err}");

        // a second, different capability appends rather than collides
        let two = add_compose_service(&one, "redis", REDIS_COMPOSE).unwrap();
        assert!(two.contains("  redis:") && two.contains("  postgres:"), "{two}");
    }

    #[test]
    fn dry_run_writes_nothing_but_plans_the_same_change() {
        let root = fixture("dry");
        let before = snapshot(&root);
        let planned = add(&root, Capability::Postgres, None, true).unwrap();
        assert_eq!(before, snapshot(&root), "dry run touched the tree");
        assert_eq!(
            planned,
            add(&root, Capability::Postgres, None, false).unwrap()
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dry_run_still_validates_the_manifest() {
        let root = fixture("no-anchor");
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo-svc\"\n").unwrap();
        let err = add(&root, Capability::Postgres, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("default = ["), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The contract between `add` and the template. Only this test notices if
    /// the template drifts, which would make `add` refuse on every freshly
    /// generated project instead of on a hand-edited one.
    #[test]
    fn embedded_template_still_matches_every_signature_add_checks() {
        let file = |p: &str| crate::TEMPLATES.get_file(p).unwrap().contents_utf8().unwrap();
        assert!(
            file("hexagonal/Cargo.toml").contains(&format!("name = \"{TEMPLATE_PKG}\"")),
            "TEMPLATE_PKG no longer matches the template's package name"
        );
        let repo = file("hexagonal/src/infrastructure/diesel_repo.rs");
        assert!(repo.contains(SAVE_REPLACE_INTO), "template lost the replace_into save() block");
        assert!(repo.contains("#[cfg(test)]"), "template lost the diesel_repo.rs test module");
        assert!(file("hexagonal/Dockerfile").contains(APT_ANCHOR), "Dockerfile lost the apt anchor");
        // the three blocks `add mongodb` swaps, and the gate that lets it turn
        // the relational feature off without deleting the Diesel files
        let bootstrap = file("hexagonal/src/bootstrap.rs");
        for (what, block) in [
            ("the infrastructure import list", BOOTSTRAP_USE),
            ("the DATABASE_URL block", BOOTSTRAP_DB),
            ("fn build_user_service", BOOTSTRAP_BUILD_USER),
        ] {
            assert!(bootstrap.contains(block), "template's bootstrap.rs no longer has {what}");
        }
        let infra = file("hexagonal/src/infrastructure/mod.rs");
        assert_eq!(
            infra.matches("#[cfg(feature = \"relational\")]").count(),
            2,
            "the Diesel modules are no longer gated — `add mongodb` would leave a project that \
             cannot compile"
        );
        let toml = file("hexagonal/Cargo.toml");
        // `relational` carries the Diesel deps themselves, so turning it off is
        // what keeps them out of a Mongo project's build, not just out of its
        // module tree
        for feature in [
            "relational = [\"dep:diesel\", \"dep:diesel_migrations\"]",
            "document = [\"dep:mongodb\"",
            // the SeaORM half of the same contract: `add_seaorm` only rewrites
            // `default = [...]`, so both of these have to already be declared
            "seaorm = [\"dep:sea-orm\"",
            "seaorm-postgres = [\"seaorm\"",
            "seaorm-mysql = [\"seaorm\"",
            // and the SQL Server one. `dep:openssl-sys` is the half that is easy
            // to drop and impossible to notice: without it `native-tls` links the
            // *system* libssl, which distroless/cc does not have — the build
            // succeeds on a developer's machine and the image will not start.
            "mssql = [\"dep:tiberius\", \"dep:bb8\", \"dep:bb8-tiberius\", \"dep:openssl-sys\", \
             \"tokio/sync\"]",
        ] {
            assert!(toml.contains(feature), "template's [features] lost `{feature}`");
        }
        // `seaorm-mysql` is the one feature that costs an advisory to declare:
        // `sea-orm/sqlx-mysql` pulls `rsa` (RUSTSEC-2023-0071, no fixed version)
        // into Cargo.lock, and cargo-audit reads the lock without caring which
        // features are on. Without this entry the audit job of *every* generated
        // project fails, MySQL or not — so the two lines travel together.
        // by line, not by substring: a commented-out entry still contains the id,
        // and it is the live entry that keeps cargo-audit quiet
        assert!(
            file("hexagonal/.cargo/audit.toml")
                .lines()
                .any(|l| l.trim() == "\"RUSTSEC-2023-0071\","),
            "the template declares `seaorm-mysql` but does not ignore the `rsa` advisory it puts \
             in Cargo.lock — that fails the audit job of every generated project"
        );
        // and the line that makes "one ORM per project" a compile error rather
        // than a convention — a project with both on links two ORMs
        assert!(
            file("hexagonal/src/lib.rs")
                .contains("#[cfg(all(feature = \"relational\", feature = \"seaorm\"))]"),
            "template's lib.rs lost the one-ORM-per-project compile_error!"
        );
        assert!(
            crate::TEMPLATES
                .get_dir("hexagonal/migrations")
                .unwrap()
                .dirs()
                .flat_map(|d| d.files())
                .filter(|f| f.path().ends_with("up.sql"))
                .any(|f| is_known(KNOWN_SEEDS, f.contents_utf8().unwrap())),
            "the template's seed migration is no longer one `add` recognises as its own"
        );
    }

    /// Both compared files must be free of project-specific text. If one ever
    /// embeds the crate name again, rustfmt wraps that line differently
    /// depending on how long the name is — and then the comparison holds for
    /// some project names and not others, so `kodegen feature` (which runs
    /// `cargo fmt`) would leave `add` refusing on a project nobody touched.
    #[test]
    fn the_replaced_files_carry_no_project_name() {
        for rel in [DIESEL_DB, DIESEL_REPO] {
            let raw = crate::TEMPLATES
                .get_file(format!("hexagonal/{rel}"))
                .unwrap()
                .contents_utf8()
                .unwrap();
            assert_eq!(generated(rel, "any-name-at-all").unwrap(), raw, "{rel} still mentions the crate name");
        }
    }

    /// The Phase 5 path, and the same shape as the redis one: a new port, a new
    /// `Services` field and a builder in both composition roots — plus the thing
    /// that is new here, a *slot* around a port that is otherwise additive.
    #[test]
    fn clickhouse_writes_the_event_port_and_wires_both_composition_roots() {
        let root = fixture("clickhouse");
        add(&root, Capability::Clickhouse, None, false).unwrap();

        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        // additive: the datastore it arrived with is untouched
        assert!(toml.contains("default = [\"sqlite\", \"analytics\"]"), "{toml}");

        let ports = fs::read_to_string(root.join(PORTS)).unwrap();
        assert!(ports.contains("pub trait EventSink: Send + Sync"), "{ports}");
        assert!(ports.contains("pub fn check_event"), "{ports}");
        assert!(ports.contains("pub struct Event {"), "{ports}");
        assert!(ports.contains("pub trait UserRepository"), "the existing ports were lost: {ports}");
        // write path only — an analytical query port is a different shape per
        // backend and is deliberately out of scope
        assert!(!ports.contains("EventSink") || !ports.contains("async fn query"), "{ports}");
        assert!(
            fs::read_to_string(root.join("src/domain/mod.rs"))
                .unwrap()
                .contains("pub use ports::{check_event, Event, EventSink};")
        );

        let api = fs::read_to_string(root.join("src/api/mod.rs")).unwrap();
        assert!(api.contains("pub events: Arc<dyn crate::domain::EventSink>"), "{api}");

        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("CLICKHOUSE_URL"), "{bootstrap}");
        assert!(bootstrap.contains("events: build_event_sink(clickhouse.as_ref()),"), "{bootstrap}");
        assert!(bootstrap.contains("events: build_event_sink(None),"), "{bootstrap}");
        assert!(bootstrap.contains("fn build_event_sink("), "{bootstrap}");
        for marker in [RESOURCES_MARKER, "// kodegen:services:env", "// kodegen:services:default"] {
            assert!(bootstrap.contains(marker), "marker '{marker}' was consumed: {bootstrap}");
        }

        // the default adapter is what makes a generated project green with no
        // server, so it is written whichever backend was asked for
        let log = fs::read_to_string(root.join(LOG_EVENTS)).unwrap();
        assert!(log.contains("impl EventSink for LogEventSink"), "{log}");
        assert!(log.contains("check_event(event)?"), "both adapters validate: {log}");
        let adapter = fs::read_to_string(root.join(CLICKHOUSE_EVENTS)).unwrap();
        assert!(adapter.contains("impl EventSink for ClickhouseEventSink"), "{adapter}");
        assert!(adapter.contains("CREATE TABLE IF NOT EXISTS events"), "{adapter}");
        assert!(!root.join(LINE_PROTOCOL_EVENTS).exists(), "the other backend was written too");

        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("image: clickhouse/clickhouse-server:"), "{compose}");
        let test = fs::read_to_string(root.join(EVENTS_TEST)).unwrap();
        assert!(test.contains("demo_svc::domain::{DomainError, Event, EventSink}"), "{test}");
        assert!(test.contains("clickhouse_events::*"), "{test}");
        // the default adapter has to be exercised with no server, or the
        // "generated projects are green out of the box" promise is untested
        assert!(test.contains("fn log_event_sink_honors_the_contract"), "{test}");
        assert!(test.contains("CLICKHOUSE_URL"), "the server half must be env-gated: {test}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The other backend of the same slot: same port, same default adapter, same
    /// wiring — a different adapter file, resource and compose service. If these
    /// two ever stop sharing everything else, the slot is the wrong abstraction.
    #[test]
    fn influxdb_writes_the_same_port_with_the_line_protocol_adapter() {
        let root = fixture("influxdb");
        add(&root, Capability::Influxdb, None, false).unwrap();

        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"sqlite\", \"timeseries\"]"), "{toml}");
        let ports = fs::read_to_string(root.join(PORTS)).unwrap();
        assert!(ports.contains("pub trait EventSink: Send + Sync"), "{ports}");

        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        assert!(bootstrap.contains("INFLUX_URL") && bootstrap.contains("INFLUX_BUCKET"), "{bootstrap}");
        // the one place that knows a bucket exists is the composition root
        assert!(bootstrap.contains("/api/v2/write?bucket="), "{bootstrap}");
        assert!(bootstrap.contains("events: build_event_sink(influx.as_ref()),"), "{bootstrap}");
        assert!(bootstrap.contains("events: build_event_sink(None),"), "{bootstrap}");
        assert!(bootstrap.contains("readiness.push("), "the sink's readiness check is not registered: {bootstrap}");

        let adapter = fs::read_to_string(root.join(LINE_PROTOCOL_EVENTS)).unwrap();
        assert!(adapter.contains("impl EventSink for LineProtocolEventSink"), "{adapter}");
        assert!(adapter.contains("impl Readiness for LineProtocolEventSink"), "{adapter}");
        // the reason this covers three products with one adapter, documented
        // where the decision is made
        assert!(adapter.contains("GreptimeDB"), "{adapter}");
        assert!(root.join(LOG_EVENTS).is_file(), "the default adapter is written either way");
        assert!(!root.join(CLICKHOUSE_EVENTS).exists());

        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("image: influxdb:2"), "{compose}");
        assert!(compose.contains("greptime"), "the Greptime swap must be documented where it is made");
        fs::remove_dir_all(&root).unwrap();
    }

    /// GreptimeDB is `influxdb` with a different image and two different
    /// variables — the relationship mariadb has with mysql. Everything the two
    /// commands write must therefore be identical apart from the compose service:
    /// the moment one of them grows a file of its own, this is not an alias.
    #[test]
    fn greptimedb_is_the_influxdb_capability_with_its_own_service() {
        let influx = fixture("greptime-parity-influx");
        let greptime = fixture("greptime-parity");
        let planned_influx = add(&influx, Capability::Influxdb, None, false).unwrap();
        assert_eq!(
            planned_influx,
            add(&greptime, Capability::Greptimedb, None, false).unwrap()
        );

        // one cargo feature, one adapter, one composition root: the only file
        // that may differ is the compose one
        for (rel, bytes) in snapshot(&influx) {
            if rel == COMPOSE {
                continue;
            }
            let mine = fs::read(greptime.join(&rel)).unwrap();
            assert_eq!(bytes, mine, "{rel} differs between influxdb and greptimedb");
        }

        let compose = fs::read_to_string(greptime.join(COMPOSE)).unwrap();
        assert!(compose.contains("image: greptime/greptimedb:"), "{compose}");
        // the two facts a real server made us learn: it binds loopback unless
        // told otherwise, and `public` is the database that exists
        assert!(compose.contains("--http-addr 0.0.0.0:4000"), "{compose}");
        assert_eq!(
            Capability::Greptimedb.env(),
            [("INFLUX_URL", "http://127.0.0.1:4000/v1/influxdb"), ("INFLUX_BUCKET", "public")]
        );
        fs::remove_dir_all(&influx).unwrap();
        fs::remove_dir_all(&greptime).unwrap();
    }

    /// Two names for the `timeseries` feature, so they refuse each other from
    /// both sides — the same answer `mysql` and `mariadb` give.
    #[test]
    fn greptimedb_and_influxdb_are_the_same_capability() {
        let err = check_combination(&[Capability::Influxdb, Capability::Greptimedb], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("the same capability"), "{err}");
        assert!(err.contains("`timeseries` cargo feature"), "{err}");

        for (tag, first, second) in [
            ("greptime-on-influx", Capability::Influxdb, Capability::Greptimedb),
            ("influx-on-greptime", Capability::Greptimedb, Capability::Influxdb),
        ] {
            let root = fixture(tag);
            add(&root, first, None, false).unwrap();
            let err = add(&root, second, None, true).unwrap_err().to_string();
            assert!(
                err.contains(&format!("capability '{}' already added", second.name())),
                "{tag}: {err}"
            );
            fs::remove_dir_all(&root).unwrap();
        }

        // and the other occupant of the slot still refuses it as a *different*
        // capability, with the undo steps
        let root = fixture("greptime-on-clickhouse");
        add(&root, Capability::Clickhouse, None, false).unwrap();
        let err = add(&root, Capability::Greptimedb, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("the `analytics` event sink (kodegen add clickhouse)"),
            "{err}"
        );
        assert!(err.contains("rerun `kodegen add greptimedb`"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Both directions of the event-sink slot, because a user can arrive at it
    /// from either side — the same rule the datastore slot has, which is the
    /// whole point of there being a `Slot` at all.
    #[test]
    fn a_second_event_sink_fails_with_instructions() {
        let root = fixture("clickhouse-then-influx");
        add(&root, Capability::Clickhouse, None, false).unwrap();
        let err = add(&root, Capability::Influxdb, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("the `analytics` event sink (kodegen add clickhouse)"),
            "{err}"
        );
        assert!(
            err.contains("clickhouse_events.rs"),
            "must name the file to remove: {err}"
        );
        assert!(err.contains("rerun `kodegen add influxdb`"), "{err}");
        // nothing was written by the refusal
        assert!(!root.join(LINE_PROTOCOL_EVENTS).exists());

        let root2 = fixture("influx-then-clickhouse");
        add(&root2, Capability::Influxdb, None, false).unwrap();
        let err = add(&root2, Capability::Clickhouse, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("the `timeseries` event sink (kodegen add influxdb / greptimedb)"),
            "{err}"
        );
        assert!(err.contains("line_protocol_events.rs"), "{err}");
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&root2).unwrap();
    }

    #[test]
    fn adding_an_event_sink_twice_fails() {
        for (tag, cap) in [
            ("ch-twice", Capability::Clickhouse),
            ("influx-twice", Capability::Influxdb),
            ("greptime-twice", Capability::Greptimedb),
        ] {
            let root = fixture(tag);
            add(&root, cap, None, false).unwrap();
            let err = add(&root, cap, None, true).unwrap_err().to_string();
            assert!(
                err.contains(&format!("capability '{}' already added", cap.name())),
                "{err}"
            );
            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn event_sink_dry_run_writes_nothing_but_plans_the_same_change() {
        for (tag, cap) in [
            ("ch-dry", Capability::Clickhouse),
            ("influx-dry", Capability::Influxdb),
            ("greptime-dry", Capability::Greptimedb),
        ] {
            let root = fixture(tag);
            let before = snapshot(&root);
            let planned = add(&root, cap, None, true).unwrap();
            assert_eq!(before, snapshot(&root), "dry run touched the tree");
            assert_eq!(planned, add(&root, cap, None, false).unwrap());
            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn an_event_sink_names_the_missing_resources_marker() {
        let root = fixture("ch-no-marker");
        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        fs::write(
            root.join(BOOTSTRAP),
            bootstrap.replace(RESOURCES_MARKER, ""),
        )
        .unwrap();
        let err = add(&root, Capability::Clickhouse, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains(RESOURCES_MARKER), "{err}");
        assert!(err.contains("build_services_from_env"), "{err}");
        // the variable to resolve by hand is the one that selects the adapter
        assert!(err.contains("CLICKHOUSE_URL"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// Appending to `ports.rs` is anchor-free, but it can still produce a file
    /// that will not compile — and the event port appends a trait, a struct *and*
    /// a free function, so it has three collisions to refuse.
    #[test]
    fn an_event_sink_refuses_a_ports_file_it_cannot_append_to() {
        let root = fixture("events-ports-guard");
        let ports = fs::read_to_string(root.join(PORTS)).unwrap();
        for (spike, expected) in [
            ("pub trait EventSink {}", "an `EventSink` trait"),
            ("pub struct Event {}", "an `Event` type"),
            ("pub fn check_event() {}", "`check_event`"),
        ] {
            fs::write(root.join(PORTS), format!("{ports}\n{spike}\n")).unwrap();
            let err = add(&root, Capability::Clickhouse, None, true)
                .unwrap_err()
                .to_string();
            assert!(err.contains(expected), "{spike}: {err}");
            // the second half names a command to skip, so it has to be one that
            // exists — `line_protocol_events` is a file, not a capability
            assert!(
                err.contains("skip `kodegen add clickhouse`"),
                "{spike}: {err}"
            );
            let err = add(&root, Capability::Greptimedb, None, true)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("skip `kodegen add greptimedb`"),
                "{spike}: {err}"
            );
        }
        fs::write(root.join(PORTS), ports.replace(ASYNC_TRAIT_USE, "")).unwrap();
        let err = add(&root, Capability::Influxdb, None, true)
            .unwrap_err()
            .to_string();
        assert!(err.contains(ASYNC_TRAIT_USE), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_event_sink_refuses_to_overwrite_an_existing_adapter() {
        let root = fixture("events-existing-adapter");
        fs::write(root.join(LOG_EVENTS), "// mine now\n").unwrap();
        let err = add(&root, Capability::Clickhouse, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(LOG_EVENTS) && err.contains("already exists"),
            "{err}"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// Four capabilities on four different axes, which is the arrangement the
    /// slot model has to allow while refusing a second occupant of either slot.
    #[test]
    fn an_event_sink_coexists_with_a_datastore_a_cache_and_a_blob_store() {
        let root = fixture("all-four-slots");
        add(&root, Capability::Mongodb, None, false).unwrap();
        add(&root, Capability::Redis, None, false).unwrap();
        add(&root, Capability::S3, None, false).unwrap();
        add(&root, Capability::Clickhouse, None, false).unwrap();
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"document\", \"cache\", \"blob\", \"analytics\"]"), "{toml}");
        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        for service in ["  mongo:", "  redis:", "  s3:", "  clickhouse:"] {
            assert!(compose.contains(service), "{service} missing: {compose}");
        }
        let api = fs::read_to_string(root.join("src/api/mod.rs")).unwrap();
        assert!(api.contains("pub cache:") && api.contains("pub blobs:") && api.contains("pub events:"), "{api}");
        let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
        for line in [
            "user: build_user_service(db.as_ref()),",
            "cache: build_cache(redis.as_ref()),",
            "blobs: build_blob_store(s3.as_ref()),",
            "events: build_event_sink(clickhouse.as_ref()),",
        ] {
            assert!(bootstrap.contains(line), "{line} missing: {bootstrap}");
        }
        // one readiness check per real backend — the datastore's is the block
        // the swap leaves in place
        assert_eq!(
            bootstrap.matches("readiness.push(").count(),
            4,
            "{bootstrap}"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// The other order, because `add clickhouse` writes into `bootstrap.rs`
    /// before `add mongodb` goes looking for the three blocks it has to swap.
    #[test]
    fn an_event_sink_added_before_mongodb_survives_the_rewrite() {
        // both backends, because `add mongodb` refuses any `Db` it did not write
        // and each of these leaves a different resource block behind for it to
        // walk past
        for (tag, cap, feature, url, resource) in [
            ("influx-then-mongo", Capability::Influxdb, "timeseries", "INFLUX_URL", "influx"),
            ("ch-then-mongo", Capability::Clickhouse, "analytics", "CLICKHOUSE_URL", "clickhouse"),
        ] {
            let root = fixture(tag);
            add(&root, cap, None, false).unwrap();
            add(&root, Capability::Mongodb, None, false).unwrap();
            let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
            assert!(toml.contains(&format!("default = [\"{feature}\", \"document\"]")), "{toml}");
            let bootstrap = fs::read_to_string(root.join(BOOTSTRAP)).unwrap();
            assert!(bootstrap.contains(url) && bootstrap.contains("MONGODB_URL"), "{bootstrap}");
            assert!(
                bootstrap.contains(&format!("events: build_event_sink({resource}.as_ref()),")),
                "{bootstrap}"
            );
            assert!(bootstrap.contains("fn build_user_service(db: Option<&Mongo>)"), "{bootstrap}");
            // the sweep that makes `add mongodb` safe: nothing may still name the
            // Diesel handle, and an event sink's own code must not look like one
            assert!(!bootstrap.contains("Db"), "the Diesel handle survived: {bootstrap}");
            fs::remove_dir_all(&root).unwrap();
        }
    }

    /// TimescaleDB is the PostgreSQL capability with one migration on top: same
    /// feature, same connection type, same adapter files. Only the migration and
    /// the image differ, and this is what says so.
    #[test]
    fn timescaledb_is_postgres_plus_a_hypertable_migration() {
        let root = fixture("timescale");
        add(&root, Capability::Timescaledb, None, false).unwrap();

        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"postgres\"]"), "the ORM feature is postgres's: {toml}");
        let db = fs::read_to_string(root.join(DIESEL_DB)).unwrap();
        assert!(db.contains("PgConnection"), "{db}");
        assert!(db.contains("TimescaleDB"), "the docs must say which server this is for: {db}");
        let repo = fs::read_to_string(root.join(DIESEL_REPO)).unwrap();
        assert!(repo.contains(".on_conflict(users::id)"), "the PostgreSQL upsert: {repo}");

        let dir = "migrations/2026-07-26-000000_create_users";
        let up = fs::read_to_string(root.join(format!("{dir}/up.sql"))).unwrap();
        assert!(up.contains("CREATE EXTENSION IF NOT EXISTS timescaledb;"), "{up}");
        assert!(up.contains("create_hypertable('events', by_range('recorded_at'))"), "{up}");
        // the users table is still the PostgreSQL one, byte for byte
        assert!(up.contains("CREATE TABLE users (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL);"), "{up}");
        // and the revert drops what the migration created, or the next `migration
        // run` fails on a table that is already there
        let down = fs::read_to_string(root.join(format!("{dir}/down.sql"))).unwrap();
        assert!(down.contains("DROP TABLE events;") && down.contains("DROP TABLE users;"), "{down}");

        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("image: timescale/timescaledb:"), "{compose}");
        // the service keeps postgres's name, port and credentials: DATABASE_URL
        // does not change when a project moves between the two
        assert!(compose.contains("  postgres:") && compose.contains("\"5432:5432\""), "{compose}");
        assert_eq!(Capability::Timescaledb.env()[0].1, Capability::Postgres.env()[0].1);
        fs::remove_dir_all(&root).unwrap();
    }

    /// A rewrite to the bytes already on disk is not a change, and `--dry-run`
    /// must not name a file it will leave alone: PostgreSQL's `CREATE TABLE` is
    /// what the SQLite seed already says, and only TimescaleDB has a `down.sql`
    /// of its own.
    #[test]
    fn only_the_migrations_that_actually_change_are_in_the_plan() {
        for (tag, cap, up, down) in [
            ("plan-pg", Capability::Postgres, false, false),
            ("plan-mysql", Capability::Mysql, true, false),
            ("plan-timescale", Capability::Timescaledb, true, true),
        ] {
            let root = fixture(tag);
            let (_, edited) = add(&root, cap, None, true).unwrap();
            let has = |f: &str| edited.iter().any(|e| e.ends_with(f));
            assert_eq!(has("up.sql"), up, "{tag}: up.sql in {edited:?}");
            assert_eq!(has("down.sql"), down, "{tag}: down.sql in {edited:?}");
            fs::remove_dir_all(&root).unwrap();
        }
    }

    /// TimescaleDB and PostgreSQL are one cargo feature, so they are one
    /// capability as far as `add` is concerned — the same relationship mariadb
    /// has with mysql, and it has to be refused the same way.
    #[test]
    fn timescaledb_and_postgres_are_the_same_capability() {
        let err = check_combination(&[Capability::Postgres, Capability::Timescaledb], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("the same capability"), "{err}");
        assert!(err.contains("`postgres` cargo feature"), "{err}");

        let root = fixture("timescale-on-pg");
        add(&root, Capability::Postgres, None, false).unwrap();
        let err = add(&root, Capability::Timescaledb, None, true)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("capability 'timescaledb' already added"),
            "{err}"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    /// The slot rule through `--with`, where it has to fire *before* `new`
    /// renders anything.
    #[test]
    fn with_more_than_one_event_sink_is_refused() {
        let err = check_combination(&[Capability::Clickhouse, Capability::Influxdb], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("at most one event sink"), "{err}");
        assert!(
            err.contains("clickhouse, influxdb"),
            "must name both: {err}"
        );
        let err = check_combination(&[Capability::Clickhouse, Capability::Greptimedb], None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("at most one event sink"), "{err}");
        // one of each slot, plus the two additive ones, is the arrangement that
        // has to keep working
        assert!(
            check_combination(
                &[
                    Capability::Mongodb,
                    Capability::Redis,
                    Capability::S3,
                    Capability::Clickhouse
                ],
                None
            )
            .is_ok()
        );
        assert!(check_combination(&[Capability::Timescaledb, Capability::Influxdb], None).is_ok());
    }

    #[test]
    fn default_is_matched_by_line_not_by_substring() {
        // the commented line comes first, and is the state of nothing
        let manifest = "[features]\n# default = [\"mysql\"]\ndefault = [\"sqlite\", \"cache\"]\n";
        assert_eq!(parse_features(default_line(manifest).unwrap()), ["sqlite", "cache"]);
        let out = enable_feature(manifest, "postgres", Some(Slot::Datastore)).unwrap();
        assert_eq!(out, "[features]\n# default = [\"mysql\"]\ndefault = [\"cache\", \"postgres\"]\n");
        // a cache is additive: the backend that is on stays on, and enabling a
        // feature that is already listed is a no-op rather than a duplicate
        let out = enable_feature(manifest, "cache", None).unwrap();
        assert_eq!(out, manifest, "already-enabled must be idempotent");
        let out = enable_feature("[features]\ndefault = [\"sqlite\"]\n", "cache", None).unwrap();
        assert_eq!(out, "[features]\ndefault = [\"sqlite\", \"cache\"]\n");
    }
}
