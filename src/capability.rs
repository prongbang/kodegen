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
//! - **redis** (`add_redis`) and **s3** (`add_s3`) *add a port* the template does
//!   not ship, together with both of its adapters and the wiring. Purely
//!   additive, so they compose with any backend and with each other, and neither
//!   has an existing file to refuse.

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
    Redis,
    S3,
}

/// The axis a capability sits on: relational backends replace each other, a
/// cache and a blob store are additive. Every kind needs its own `add_*`
/// function, its own env vars and its own feature, which is exactly what the
/// matches on this enforce.
enum Kind {
    Relational(Relational),
    Cache,
    Blob,
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
}

/// Cargo features that select a relational backend: at most one may be on,
/// because the `Db` handle in `diesel_db.rs` is a single connection type.
const RELATIONAL: &[&str] = &["sqlite", "postgres", "mysql"];

/// Package name of the built-in template, needed to undo the rename
/// `kodegen new` applies before comparing a project's file to its origin.
const TEMPLATE_PKG: &str = "hexagonal-rs";

const DIESEL_DB: &str = "src/infrastructure/diesel_db.rs";
const DIESEL_REPO: &str = "src/infrastructure/diesel_repo.rs";
const COMPOSE: &str = "docker-compose.dev.yml";
const APT_ANCHOR: &str = "protobuf-compiler";

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
            Capability::Redis => "redis",
            Capability::S3 => "s3",
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
            Capability::Redis => Kind::Cache,
            Capability::S3 => Kind::Blob,
        }
    }

    /// The cargo feature this capability turns on — also how "is it already
    /// added?" is answered, since `default = [...]` is the only state there is.
    fn feature(self) -> &'static str {
        match self.kind() {
            Kind::Relational(rel) => rel.feature(),
            // Redis and Valkey speak the same protocol, so one feature covers
            // both: the capability is the cache, not the vendor
            Kind::Cache => "cache",
            // RustFS, MinIO, Ceph and AWS are one adapter over one API: the
            // capability is object storage, not the vendor
            Kind::Blob => "blob",
        }
    }

    /// The env vars that select this capability's adapter, and values matching
    /// what the generated `docker-compose.dev.yml` boots. The **first** one is
    /// the switch: leave it unset and the default, server-less adapter is what
    /// runs.
    pub fn env(self) -> &'static [(&'static str, &'static str)] {
        match self.kind() {
            Kind::Relational(rel) => rel.database_env(),
            Kind::Cache => &[("REDIS_URL", REDIS_URL)],
            Kind::Blob => S3_ENV,
        }
    }
}

impl Relational {
    fn name(self) -> &'static str {
        match self {
            Relational::Postgres => "postgres",
            Relational::Mysql => "mysql",
            Relational::Mariadb => "mariadb",
        }
    }

    /// MariaDB speaks the MySQL wire protocol, so it is the same driver, the
    /// same adapter and the same cargo feature — only the image and the docs
    /// differ.
    fn feature(self) -> &'static str {
        match self {
            Relational::Postgres => "postgres",
            Relational::Mysql | Relational::Mariadb => "mysql",
        }
    }

    fn dialect(self) -> &'static str {
        match self {
            Relational::Postgres => "PostgreSQL",
            Relational::Mysql => "MySQL",
            Relational::Mariadb => "MariaDB",
        }
    }

    fn connection(self) -> &'static str {
        match self {
            Relational::Postgres => "PgConnection",
            Relational::Mysql | Relational::Mariadb => "MysqlConnection",
        }
    }

    /// 127.0.0.1, never `localhost`: libmysqlclient reads `localhost` as "use
    /// the unix socket" and ignores the port entirely, so a compose service
    /// published on 3306 fails with a socket error naming a path nobody
    /// configured. The literal address forces TCP on every backend.
    fn database_url(self) -> &'static str {
        match self {
            Relational::Postgres => PG_URL,
            Relational::Mysql | Relational::Mariadb => MYSQL_URL,
        }
    }

    fn database_env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Relational::Postgres => &[("DATABASE_URL", PG_URL)],
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
        }
    }

    /// What `.on_conflict(...)` takes. Both dialects get a real upsert, so
    /// `save`'s documented "saving an existing id overwrites it" means the same
    /// thing whichever backend is on.
    fn conflict_target(self) -> &'static str {
        match self {
            Relational::Postgres => "users::id",
            Relational::Mysql | Relational::Mariadb => "diesel::dsl::DuplicatedKeys",
        }
    }

    fn upsert_note(self) -> &'static str {
        match self {
            Relational::Postgres => "// upsert: `save` is documented as one, so an existing id is updated",
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
            Relational::Postgres => "perl make",
            // mysqlclient-src drives its build with cmake
            Relational::Mysql | Relational::Mariadb => "perl make cmake",
        }
    }

    fn compose_service(self) -> &'static str {
        match self {
            Relational::Postgres => POSTGRES_COMPOSE,
            Relational::Mysql => MYSQL_COMPOSE,
            Relational::Mariadb => MARIADB_COMPOSE,
        }
    }

    fn render(self, tpl: &str) -> String {
        tpl.replace("{{Conn}}", self.connection())
            .replace("{{Dialect}}", self.dialect())
            .replace("{{capability}}", self.name())
            .replace("{{conflict_target}}", self.conflict_target())
            .replace("{{upsert_note}}", self.upsert_note())
    }
}

/// `kodegen add --list`: what is supported, and what this project already has on.
pub fn list(project: &Path) -> Result<()> {
    println!("Supported capabilities:");
    println!("  postgres   relational — Diesel + PostgreSQL");
    println!("  mysql      relational — Diesel + MySQL");
    println!("  mariadb    relational — Diesel + MariaDB (MySQL protocol: same driver, same adapter)");
    println!("  redis      cache — the `Cache` port + a Redis adapter (Valkey speaks the same protocol)");
    println!("  s3         object storage — the `BlobStore` port + an S3 adapter (RustFS, MinIO, AWS)");
    println!();
    println!("Only one relational backend can be on at a time; redis and s3 sit alongside it.");
    match fs::read_to_string(project.join("Cargo.toml")).ok().as_deref().and_then(default_line) {
        // `default = [...]` is the only state there is — the features it lists
        // are exactly the dependencies that get compiled
        Some(line) => println!("Enabled in {}: {}", project.display(), parse_features(line).join(", ")),
        None => println!("No [features] default found in {} — not a generated project?", project.display()),
    }
    Ok(())
}

/// Rejects a `--with` list that cannot be applied, *before* `new` renders
/// anything — half a project left on disk is worse than no project.
pub fn check_combination(with: &[Capability]) -> Result<()> {
    // by feature, not by variant: `--with mysql,mariadb` is two names for one
    // cargo feature, and the second `add` would die with "already added" halfway
    // through a `new` that has already written the project
    for (i, cap) in with.iter().enumerate() {
        if let Some(first) = with[..i].iter().find(|earlier| earlier.feature() == cap.feature()) {
            return Err(format!(
                "--with lists '{}' and '{}', which are the same capability (both enable the `{}` \
                 cargo feature)",
                first.name(),
                cap.name(),
                cap.feature()
            )
            .into());
        }
    }
    let relational: Vec<&str> = with
        .iter()
        .filter(|c| matches!(c.kind(), Kind::Relational(_)))
        .map(|c| c.name())
        .collect();
    if relational.len() > 1 {
        return Err(format!(
            "--with takes at most one relational backend: {} are all relational, and a project can \
             only have one (the `Db` handle is a single connection type)",
            relational.join(", ")
        )
        .into());
    }
    Ok(())
}

/// Returns the plan as (files created, files edited) for the caller to report.
pub fn add(project: &Path, cap: Capability, dry_run: bool) -> Result<(Vec<String>, Vec<String>)> {
    require_files(project, &["Cargo.toml"])?;
    let manifest = fs::read_to_string(project.join("Cargo.toml"))?;
    let enabled = parse_features(default_line(&manifest).ok_or_else(|| default_anchor_err(&manifest))?);
    if enabled.iter().any(|f| f == cap.feature()) {
        return Err(format!("capability '{}' already added", cap.name()).into());
    }
    match cap.kind() {
        Kind::Relational(rel) => add_relational(project, rel, &enabled, dry_run),
        Kind::Cache => add_redis(project, dry_run),
        Kind::Blob => add_s3(project, dry_run),
    }
}

/// Switches the project onto `rel`. Not additive: `diesel_db.rs` is written
/// against one connection type from top to bottom, so it is replaced.
fn add_relational(
    project: &Path,
    cap: Relational,
    enabled: &[String],
    dry_run: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    require_files(project, &["Dockerfile", DIESEL_DB, DIESEL_REPO])?;
    let pkg = crate::package_name(project)
        .ok_or("Cargo.toml with a package name not found — run inside a generated project")?;

    // one relational backend per project: `Db` wraps a single connection type,
    // so a second one would need a second pool, a second handle and a second
    // set of repositories — out of scope, and better refused than half-applied
    if let Some(other) = enabled.iter().find(|f| RELATIONAL.contains(&f.as_str()) && *f != "sqlite") {
        return Err(format!(
            "capability '{other}' is already this project's relational backend, and there can only be one \
             (the `Db` handle in diesel_db.rs is a single connection type).\n\
             To switch to '{}', undo '{other}' first:\n  \
             1. Cargo.toml: put `default = [\"sqlite\"]` back\n  \
             2. src/infrastructure/diesel_db.rs + diesel_repo.rs: restore the versions from a fresh \
             `kodegen new`\n  \
             3. rerun `kodegen add {}`",
            cap.name(),
            cap.name()
        )
        .into());
    }

    let mut creates: Vec<(String, String)> = Vec::new();
    let mut edits = vec![
        // the whole point of the optional-dependency layout: `add` flips one
        // list of feature names and never touches [dependencies], so Cargo.lock
        // stays valid and `--locked` keeps working
        edit(project, "Cargo.toml", |c| enable_feature(&c, cap.feature(), true))?,
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

    let seeds = migrations(project, cap);
    if seeds.is_empty() {
        eprintln!(
            "warning: no kodegen-generated migration found under migrations/ — \
             create the users table for {} yourself",
            cap.dialect()
        );
    }
    edits.extend(seeds);

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
        edit(project, "Cargo.toml", |c| enable_feature(&c, "cache", false))?,
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
        edit(project, "Cargo.toml", |c| enable_feature(&c, "blob", false))?,
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

/// Turns `feature` on in `default = [...]`. A relational backend *replaces*
/// whichever one is listed (`Db` is a single connection type); everything else
/// is additive and joins the list.
fn enable_feature(manifest: &str, feature: &str, replaces_relational: bool) -> Result<String> {
    let (offset, line) = default_span(manifest).ok_or_else(|| default_anchor_err(manifest))?;
    let indent = &line[..line.len() - line.trim_start().len()];
    let mut kept: Vec<String> = parse_features(line)
        .into_iter()
        .filter(|f| !(replaces_relational && RELATIONAL.contains(&f.as_str())))
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
        return Vec::new();
    };
    let mut seeds = Vec::new();
    for e in entries.flatten().filter(|e| e.path().is_dir()) {
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = format!("migrations/{name}/up.sql");
        // a directory with no up.sql is not a migration we can judge, let alone
        // one of ours — saying so on every run is noise, not a warning
        let Ok(content) = fs::read_to_string(project.join(&rel)) else {
            continue;
        };
        if KNOWN_SEEDS.contains(&content.trim()) {
            seeds.push((rel, cap.up_sql().to_string()));
        } else {
            eprintln!(
                "warning: migrations/{name} was not generated by kodegen — check its SQL is valid \
                 {} before running it",
                cap.dialect()
            );
        }
    }
    // read_dir order is not defined; the plan `--dry-run` prints must be stable
    seeds.sort();
    seeds
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
        cap.feature(),
        cap.connection(),
        cap.up_sql().trim(),
        cap.database_url()
    )
}

/// Seed `up.sql` bodies kodegen has generated — used to tell "still ours" from
/// "the user wrote their own schema".
const KNOWN_SEEDS: &[&str] = &[
    "CREATE TABLE users (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL);",
    "CREATE TABLE users (id VARCHAR(64) PRIMARY KEY NOT NULL, name TEXT NOT NULL);",
];

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

use crate::domain::DomainError;
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

use crate::domain::{Cache, DomainError};
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
    });"##;

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

use crate::domain::{check_blob_key, BlobStore, DomainError};
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
    });"##;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A real generated project, straight from the embedded template — anything
    /// less would not exercise the whole-file and suffix comparisons `add` does.
    fn fixture(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("kodegen-capability-{tag}"));
        let _ = fs::remove_dir_all(&root);
        let tpl = crate::resolve_template("hexagonal").unwrap();
        let subs = crate::substitutions(&tpl, "demo-svc");
        crate::render_tree(&tpl, &root, &subs).unwrap();
        root
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
        add(&root, Capability::Postgres, false).unwrap();
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
        add(&root, Capability::Mariadb, false).unwrap();
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"mysql\"]"), "{toml}");
        let db = fs::read_to_string(root.join(DIESEL_DB)).unwrap();
        assert!(db.contains("MysqlConnection"), "{db}");
        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("mariadb:11"), "{compose}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn mysql_migration_avoids_a_text_primary_key() {
        let root = fixture("mysql-migration");
        add(&root, Capability::Mysql, false).unwrap();
        let up = fs::read_to_string(root.join("migrations/2026-07-26-000000_create_users/up.sql")).unwrap();
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
            add(&root, cap, false).unwrap();
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
        add(&root, Capability::Redis, false).unwrap();

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
        add(&root, Capability::Postgres, false).unwrap();
        add(&root, Capability::Redis, false).unwrap();
        let toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
        assert!(toml.contains("default = [\"postgres\", \"cache\"]"), "{toml}");
        let compose = fs::read_to_string(root.join(COMPOSE)).unwrap();
        assert!(compose.contains("  postgres:") && compose.contains("  redis:"), "{compose}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn adding_redis_twice_fails() {
        let root = fixture("redis-twice");
        add(&root, Capability::Redis, false).unwrap();
        let err = add(&root, Capability::Redis, true).unwrap_err().to_string();
        assert!(err.contains("capability 'redis' already added"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn redis_dry_run_writes_nothing_but_plans_the_same_change() {
        let root = fixture("redis-dry");
        let before = snapshot(&root);
        let planned = add(&root, Capability::Redis, true).unwrap();
        assert_eq!(before, snapshot(&root), "dry run touched the tree");
        assert_eq!(planned, add(&root, Capability::Redis, false).unwrap());
        fs::remove_dir_all(&root).unwrap();
    }

    /// The marker is the only anchor the redis path has no legacy fallback for,
    /// so its error has to be enough to repair the file by hand.
    #[test]
    fn redis_names_the_missing_resources_marker() {
        let root = fixture("redis-no-marker");
        let bootstrap = fs::read_to_string(root.join("src/bootstrap.rs")).unwrap();
        fs::write(root.join("src/bootstrap.rs"), bootstrap.replace(RESOURCES_MARKER, "")).unwrap();
        let err = add(&root, Capability::Redis, true).unwrap_err().to_string();
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
        let err = enable_feature(wrapped, "postgres", true).unwrap_err().to_string();
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
            let err = add(&root, cap, true).unwrap_err().to_string();
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
        let err = add(&root, Capability::Redis, true).unwrap_err().to_string();
        assert!(err.contains("already declares a `Cache` trait"), "{err}");

        fs::write(root.join(PORTS), ports.replace(ASYNC_TRAIT_USE, "")).unwrap();
        let err = add(&root, Capability::Redis, true).unwrap_err().to_string();
        assert!(err.contains(ASYNC_TRAIT_USE), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The same shape as the redis test, for the same reason: a new port, a new
    /// `Services` field, and a builder wired into both composition roots.
    #[test]
    fn s3_writes_the_blob_port_and_wires_both_composition_roots() {
        let root = fixture("s3");
        add(&root, Capability::S3, false).unwrap();

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
        add(&root, Capability::Postgres, false).unwrap();
        add(&root, Capability::Redis, false).unwrap();
        add(&root, Capability::S3, false).unwrap();
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
        add(&root, Capability::S3, false).unwrap();
        let err = add(&root, Capability::S3, true).unwrap_err().to_string();
        assert!(err.contains("capability 's3' already added"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn s3_dry_run_writes_nothing_but_plans_the_same_change() {
        let root = fixture("s3-dry");
        let before = snapshot(&root);
        let planned = add(&root, Capability::S3, true).unwrap();
        assert_eq!(before, snapshot(&root), "dry run touched the tree");
        assert_eq!(planned, add(&root, Capability::S3, false).unwrap());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn s3_names_the_missing_resources_marker() {
        let root = fixture("s3-no-marker");
        let bootstrap = fs::read_to_string(root.join("src/bootstrap.rs")).unwrap();
        fs::write(root.join("src/bootstrap.rs"), bootstrap.replace(RESOURCES_MARKER, "")).unwrap();
        let err = add(&root, Capability::S3, true).unwrap_err().to_string();
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

        fs::write(root.join(PORTS), format!("{ports}\npub trait BlobStore {{}}\n")).unwrap();
        let err = add(&root, Capability::S3, true).unwrap_err().to_string();
        assert!(err.contains("already declares a `BlobStore` trait"), "{err}");

        fs::write(root.join(PORTS), format!("{ports}\npub fn check_blob_key() {{}}\n")).unwrap();
        let err = add(&root, Capability::S3, true).unwrap_err().to_string();
        assert!(err.contains("already declares `check_blob_key`"), "{err}");

        fs::write(root.join(PORTS), ports.replace(ASYNC_TRAIT_USE, "")).unwrap();
        let err = add(&root, Capability::S3, true).unwrap_err().to_string();
        assert!(err.contains(ASYNC_TRAIT_USE), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// An adapter file the user already has is theirs — `add` creates files, it
    /// does not overwrite them.
    #[test]
    fn s3_refuses_to_overwrite_an_existing_adapter() {
        let root = fixture("s3-existing-adapter");
        fs::write(root.join(S3_BLOBS), "// mine now\n").unwrap();
        let err = add(&root, Capability::S3, true).unwrap_err().to_string();
        assert!(err.contains(S3_BLOBS) && err.contains("already exists"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    /// The four S3 variables are printed as one line because three of them are
    /// useless on their own — and the first has to be the one that selects the
    /// adapter, since that is the one `add` tells you to leave unset.
    #[test]
    fn every_capability_leads_with_the_variable_that_selects_it() {
        for (cap, first) in [
            (Capability::Postgres, "DATABASE_URL"),
            (Capability::Redis, "REDIS_URL"),
            (Capability::S3, "S3_BUCKET"),
        ] {
            assert_eq!(cap.env()[0].0, first, "{}", cap.name());
        }
        let s3: Vec<&str> = Capability::S3.env().iter().map(|(k, _)| *k).collect();
        assert_eq!(s3, ["S3_BUCKET", "S3_ENDPOINT", "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY"]);
    }

    #[test]
    fn adding_the_same_capability_twice_fails() {
        let root = fixture("twice");
        add(&root, Capability::Postgres, false).unwrap();
        let err = add(&root, Capability::Postgres, true).unwrap_err().to_string();
        assert!(err.contains("already added"), "{err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_second_relational_backend_fails_with_instructions() {
        let root = fixture("second");
        add(&root, Capability::Postgres, false).unwrap();
        let err = add(&root, Capability::Mysql, true).unwrap_err().to_string();
        assert!(err.contains("already this project's relational backend"), "{err}");
        assert!(err.contains("default = [\"sqlite\"]"), "must say how to undo it: {err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn with_more_than_one_relational_backend_is_refused() {
        let err = check_combination(&[Capability::Postgres, Capability::Mysql]).unwrap_err().to_string();
        assert!(err.contains("at most one relational backend"), "{err}");
        // deduped by cargo feature, not by variant: these are one capability
        let err = check_combination(&[Capability::Mysql, Capability::Mariadb]).unwrap_err().to_string();
        assert!(err.contains("the same capability"), "{err}");
        assert!(err.contains("`mysql` cargo feature"), "{err}");
        let err = check_combination(&[Capability::Redis, Capability::Redis]).unwrap_err().to_string();
        assert!(err.contains("the same capability"), "{err}");
        assert!(check_combination(&[Capability::Postgres]).is_ok());
        // redis is not a backend, so it rides along with one
        assert!(check_combination(&[Capability::Postgres, Capability::Redis]).is_ok());
        assert!(check_combination(&[]).is_ok());
    }

    #[test]
    fn refuses_to_clobber_a_hand_edited_diesel_db() {
        let root = fixture("handedited");
        fs::write(root.join(DIESEL_DB), "// mine now\n").unwrap();
        let err = add(&root, Capability::Postgres, true).unwrap_err().to_string();
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

        add(&root, Capability::Postgres, false).unwrap();
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

        let err = add(&root, Capability::Postgres, false).unwrap_err().to_string();
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

        add(&root, Capability::Postgres, false).unwrap();
        let out = fs::read_to_string(root.join(DIESEL_REPO)).unwrap();
        assert!(out.contains("pub fn count_users()"), "{out}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refuses_a_dockerfile_without_the_apt_anchor() {
        let root = fixture("no-apt");
        fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
        let err = add(&root, Capability::Postgres, true).unwrap_err().to_string();
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
        let planned = add(&root, Capability::Postgres, true).unwrap();
        assert_eq!(before, snapshot(&root), "dry run touched the tree");
        assert_eq!(planned, add(&root, Capability::Postgres, false).unwrap());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dry_run_still_validates_the_manifest() {
        let root = fixture("no-anchor");
        fs::write(root.join("Cargo.toml"), "[package]\nname = \"demo-svc\"\n").unwrap();
        let err = add(&root, Capability::Postgres, true).unwrap_err().to_string();
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
        assert!(
            crate::TEMPLATES
                .get_dir("hexagonal/migrations")
                .unwrap()
                .dirs()
                .flat_map(|d| d.files())
                .filter(|f| f.path().ends_with("up.sql"))
                .any(|f| KNOWN_SEEDS.contains(&f.contents_utf8().unwrap().trim())),
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

    #[test]
    fn default_is_matched_by_line_not_by_substring() {
        // the commented line comes first, and is the state of nothing
        let manifest = "[features]\n# default = [\"mysql\"]\ndefault = [\"sqlite\", \"cache\"]\n";
        assert_eq!(parse_features(default_line(manifest).unwrap()), ["sqlite", "cache"]);
        let out = enable_feature(manifest, "postgres", true).unwrap();
        assert_eq!(out, "[features]\n# default = [\"mysql\"]\ndefault = [\"cache\", \"postgres\"]\n");
        // a cache is additive: the backend that is on stays on, and enabling a
        // feature that is already listed is a no-op rather than a duplicate
        let out = enable_feature(manifest, "cache", false).unwrap();
        assert_eq!(out, manifest, "already-enabled must be idempotent");
        let out = enable_feature("[features]\ndefault = [\"sqlite\"]\n", "cache", false).unwrap();
        assert_eq!(out, "[features]\ndefault = [\"sqlite\", \"cache\"]\n");
    }
}
