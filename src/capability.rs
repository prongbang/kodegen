//! `kodegen add <capability>`: switch a generated project's relational backend.
//!
//! Same shape as `scaffold::add_feature` — the whole change is computed in
//! memory (which is what validates every anchor and signature) and written in
//! one phase at the end, so `--dry-run` is the same code path minus the writes.
//!
//! Unlike `feature`, this is not purely additive: `infrastructure/diesel_db.rs`
//! is written against one connection type from top to bottom, so it is
//! replaced, not edited. Anything replaced is first compared against what
//! `kodegen new` actually wrote (the embedded template, with the project's name
//! substituted in) — a file the user has touched is refused with manual
//! instructions rather than clobbered.

use crate::scaffold::{edit, require_files, write_plan};
use std::{error::Error, fs, path::Path};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Capability {
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

impl Capability {
    pub fn name(self) -> &'static str {
        match self {
            Capability::Postgres => "postgres",
            Capability::Mysql => "mysql",
            Capability::Mariadb => "mariadb",
        }
    }

    /// MariaDB speaks the MySQL wire protocol, so it is the same driver, the
    /// same adapter and the same cargo feature — only the image and the docs
    /// differ.
    fn feature(self) -> &'static str {
        match self {
            Capability::Postgres => "postgres",
            Capability::Mysql | Capability::Mariadb => "mysql",
        }
    }

    fn dialect(self) -> &'static str {
        match self {
            Capability::Postgres => "PostgreSQL",
            Capability::Mysql => "MySQL",
            Capability::Mariadb => "MariaDB",
        }
    }

    fn connection(self) -> &'static str {
        match self {
            Capability::Postgres => "PgConnection",
            Capability::Mysql | Capability::Mariadb => "MysqlConnection",
        }
    }

    /// 127.0.0.1, never `localhost`: libmysqlclient reads `localhost` as "use
    /// the unix socket" and ignores the port entirely, so a compose service
    /// published on 3306 fails with a socket error naming a path nobody
    /// configured. The literal address forces TCP on every backend.
    pub fn database_url(self) -> &'static str {
        match self {
            Capability::Postgres => "postgres://app:app@127.0.0.1:5432/app",
            Capability::Mysql | Capability::Mariadb => "mysql://app:app@127.0.0.1:3306/app",
        }
    }

    /// MySQL cannot index a `TEXT` column without a prefix length, so
    /// `TEXT PRIMARY KEY` is rejected at migration time — hence `VARCHAR(64)`,
    /// which matches the 64-char id limit the domain already enforces.
    fn up_sql(self) -> &'static str {
        match self {
            Capability::Postgres => "CREATE TABLE users (id TEXT PRIMARY KEY NOT NULL, name TEXT NOT NULL);\n",
            Capability::Mysql | Capability::Mariadb => {
                "CREATE TABLE users (id VARCHAR(64) PRIMARY KEY NOT NULL, name TEXT NOT NULL);\n"
            }
        }
    }

    /// What `.on_conflict(...)` takes. Both dialects get a real upsert, so
    /// `save`'s documented "saving an existing id overwrites it" means the same
    /// thing whichever backend is on.
    fn conflict_target(self) -> &'static str {
        match self {
            Capability::Postgres => "users::id",
            Capability::Mysql | Capability::Mariadb => "diesel::dsl::DuplicatedKeys",
        }
    }

    fn upsert_note(self) -> &'static str {
        match self {
            Capability::Postgres => "// upsert: `save` is documented as one, so an existing id is updated",
            // the trapdoor this avoids stays invisible until something
            // references users(id) — exactly when it is expensive to discover
            Capability::Mysql | Capability::Mariadb => {
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
            Capability::Postgres => "perl make",
            // mysqlclient-src drives its build with cmake
            Capability::Mysql | Capability::Mariadb => "perl make cmake",
        }
    }

    fn compose_service(self) -> &'static str {
        match self {
            Capability::Postgres => POSTGRES_COMPOSE,
            Capability::Mysql => MYSQL_COMPOSE,
            Capability::Mariadb => MARIADB_COMPOSE,
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
    println!();
    println!("Only one relational backend can be on at a time.");
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
    if with.len() > 1 {
        return Err(format!(
            "--with takes one capability: {} are all relational backends, and a project can only have one \
             (the `Db` handle is a single connection type)",
            with.iter().map(|c| c.name()).collect::<Vec<_>>().join(", ")
        )
        .into());
    }
    Ok(())
}

/// Returns the plan as (files created, files edited) for the caller to report.
pub fn add(project: &Path, cap: Capability, dry_run: bool) -> Result<(Vec<String>, Vec<String>)> {
    require_files(project, &["Cargo.toml", "Dockerfile", DIESEL_DB, DIESEL_REPO])?;
    let pkg = crate::package_name(project)
        .ok_or("Cargo.toml with a package name not found — run inside a generated project")?;

    let manifest = fs::read_to_string(project.join("Cargo.toml"))?;
    let enabled = parse_features(default_line(&manifest).ok_or_else(default_anchor_err)?);
    if enabled.iter().any(|f| f == cap.feature()) {
        return Err(format!("capability '{}' already added", cap.name()).into());
    }
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
        edit(project, "Cargo.toml", |c| flip_default(&c, cap))?,
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
        edits.push(edit(project, COMPOSE, |c| add_compose_service(&c, cap))?);
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
fn default_span(manifest: &str) -> Option<(usize, &str)> {
    let mut offset = 0;
    for raw in manifest.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        if line.trim_start().starts_with("default = [") {
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

fn default_anchor_err() -> String {
    "anchor 'default = [' not found — put the `[features]` section back \
     (`default = [\"sqlite\"]`) or switch the backend manually"
        .to_string()
}

fn flip_default(manifest: &str, cap: Capability) -> Result<String> {
    let (offset, line) = default_span(manifest).ok_or_else(default_anchor_err)?;
    let indent = &line[..line.len() - line.trim_start().len()];
    let mut kept: Vec<String> = parse_features(line)
        .into_iter()
        .filter(|f| !RELATIONAL.contains(&f.as_str()))
        .collect();
    kept.push(cap.feature().to_string());
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
fn rewrite_repo(content: &str, cap: Capability, pkg: &str) -> Result<String> {
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
fn migrations(project: &Path, cap: Capability) -> Vec<(String, String)> {
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
fn add_compose_service(compose: &str, cap: Capability) -> Result<String> {
    let mut lines: Vec<&str> = compose.lines().collect();
    let at = lines.iter().position(|l| l.trim_end() == "services:").ok_or_else(|| {
        format!(
            "{COMPOSE} has no top-level `services:` line — put one back (or delete the file and rerun) \
             so `kodegen add` knows where to write the {} service",
            cap.name()
        )
    })?;
    let header = format!("  {}:", cap.name());
    if lines.iter().any(|l| l.trim_end() == header) {
        return Err(format!(
            "{COMPOSE} already defines a `{}` service — remove it first, or keep the one you have",
            cap.name()
        )
        .into());
    }
    lines.insert(at + 1, cap.compose_service().trim_end_matches('\n'));
    Ok(lines.join("\n") + "\n")
}

/// Refuse rather than clobber: the file no longer looks like what kodegen
/// wrote, so overwriting it would silently drop the user's own code.
fn refusal(rel: &str, signature: &str, cap: Capability) -> String {
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
#   DATABASE_URL=... cargo run
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
        assert!(err.contains("--with takes one capability"), "{err}");
        assert!(check_combination(&[Capability::Postgres]).is_ok());
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
        let err = add_compose_service("# services:\n", Capability::Postgres).unwrap_err().to_string();
        assert!(err.contains("no top-level `services:` line"), "{err}");

        let one = add_compose_service("services:\n", Capability::Postgres).unwrap();
        assert!(one.contains("  postgres:\n    image: postgres:17-alpine"), "{one}");

        let err = add_compose_service(&one, Capability::Postgres).unwrap_err().to_string();
        assert!(err.contains("already defines a `postgres` service"), "{err}");
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
        let out = flip_default(manifest, Capability::Postgres).unwrap();
        assert_eq!(out, "[features]\n# default = [\"mysql\"]\ndefault = [\"cache\", \"postgres\"]\n");
    }
}
