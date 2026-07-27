//! Shared Diesel/SQLite plumbing for every repository: one pool, one
//! spawn_blocking entrypoint, one error mapping. A new repository only
//! needs a `Db` clone and its own queries.

use crate::domain::DomainError;
use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};

pub type SqlitePool = Pool<ConnectionManager<SqliteConnection>>;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

/// Maps any infrastructure error (pool, diesel, join) to `DomainError::Other`.
pub fn infra(e: impl std::error::Error + Send + Sync + 'static) -> DomainError {
    DomainError::Other(Box::new(e))
}

/// SQLite allows one writer at a time: without WAL + busy_timeout, concurrent
/// writes fail immediately with SQLITE_BUSY instead of waiting their turn.
#[derive(Debug)]
struct SqlitePragmas;

impl diesel::r2d2::CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for SqlitePragmas {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), diesel::r2d2::Error> {
        // batch_execute, not sql_query: prepared statements only run the first
        // statement of a multi-statement string. WAL is set once in build_pool
        // (it persists in the database file); these two are per-connection.
        conn.batch_execute("PRAGMA busy_timeout = 5000; PRAGMA synchronous = NORMAL;")
            .map_err(diesel::r2d2::Error::QueryError)
    }
}

/// Builds the shared pool and runs pending migrations.
/// Owned by the composition root (bootstrap), not by repositories.
pub fn build_pool(
    database_url: &str,
) -> Result<SqlitePool, Box<dyn std::error::Error + Send + Sync>> {
    // single setup connection BEFORE the pool exists: switch the database file
    // to WAL (persistent, one writer needs it done exactly once) and migrate —
    // no concurrent connections yet, so no SQLITE_BUSY races at startup
    let mut setup = SqliteConnection::establish(database_url)?;
    setup.batch_execute("PRAGMA busy_timeout = 5000; PRAGMA journal_mode = WAL;")?;
    setup.run_pending_migrations(MIGRATIONS)?;
    drop(setup);

    let manager = ConnectionManager::<SqliteConnection>::new(database_url);
    Ok(Pool::builder()
        .connection_customizer(Box::new(SqlitePragmas))
        .build(manager)?)
}

/// Cheap-to-clone handle every Diesel repository shares.
#[derive(Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Runs a blocking Diesel closure on the blocking thread pool with a
    /// connection checked out from the shared pool.
    pub async fn run<T, F>(&self, f: F) -> Result<T, DomainError>
    where
        T: Send + 'static,
        F: FnOnce(&mut SqliteConnection) -> Result<T, DomainError> + Send + 'static,
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
