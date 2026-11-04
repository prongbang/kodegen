# kodegen

CLI for generating Rust starter projects, scaffolding features into them, and
switching their infrastructure capabilities.

## Command reference

| Command | Flags | What it does |
|---|---|---|
| `kodegen new <name>` | `-t, --template <TEMPLATE>` (default `hexagonal`) · `-o, --output <PATH>` (default `.`) · `--with <cap,cap,...>` — capabilities to add right after generating · `--orm <diesel\|seaorm>` (optional; applies to the one relational entry in `--with`, error if none) | Generate a project from a template |
| `kodegen feature <name>` | `-t, --transport <rest\|grpc>` (default `rest`) · `-p, --path <PATH>` (default `.`) · `--dry-run` — print the plan, write nothing | Scaffold a feature (domain/application/infrastructure/api + wiring) into a generated project |
| `kodegen add [<capability>]` | `-p, --path <PATH>` (default `.`) · `--orm <diesel\|seaorm>` (optional, no default; error on a capability with no ORM) · `--dry-run` — print the plan, write nothing · `--list` — list supported capabilities and what the project has on (conflicts with `<capability>`, `--orm`, `--dry-run`) | Add an infrastructure capability to a generated project |
| `kodegen update <template>` | — | Re-clone a cached template (registry name, owner/repo, or git URL) in `~/.kodegen/templates` |
| `kodegen list` | — | List built-in and cached templates |

`kodegen new`'s `--with` takes a comma-separated list of capability names
(e.g. `--with postgres,redis,s3`) and is a loop over `kodegen add`, so the two
never drift apart. Built-in templates ship inside the binary and local paths
are used in place, so `kodegen update` refuses on either — see
[Update a cached template](#update-a-cached-template).

### Capabilities

All 15 supported by `kodegen add <capability>` / `kodegen new --with`:

| Capability | Slot | `--orm` | Default adapter (capability off / env unset) | Main env |
|---|---|---|---|---|
| `postgres` | datastore | `diesel` (default) or `seaorm` | in-memory repo | `DATABASE_URL` |
| `mysql` | datastore | `diesel` (default) or `seaorm` | in-memory repo | `DATABASE_URL` |
| `mariadb` | datastore — alias of `mysql` (same driver, same cargo feature) | `diesel` (default) or `seaorm` | in-memory repo | `DATABASE_URL` |
| `timescaledb` | datastore — alias of `postgres` (same driver, + a hypertable migration) | `diesel` (default) or `seaorm` | in-memory repo | `DATABASE_URL` |
| `mongodb` | datastore | not supported (no ORM to choose — the driver isn't Diesel) | in-memory repo | `MONGODB_URL` (+ `MONGODB_DB`) |
| `mssql` | datastore | **not accepted** — `--orm` is an error (no open-source ORM for SQL Server) | in-memory repo | `DATABASE_URL` (an ADO.NET connection string, not a URL) |
| `redis` | — (additive) | not applicable | in-memory cache | `REDIS_URL` |
| `s3` | — (additive) | not applicable | filesystem (`./data/blobs`, `BLOB_DIR` overrides) | `S3_BUCKET` (+ endpoint/credentials) |
| `clickhouse` | event sink | not applicable | tracing log | `CLICKHOUSE_URL` (+ `_DB`/`_USER`/`_PASSWORD`) |
| `influxdb` | event sink | not applicable | tracing log | `INFLUX_URL` (+ `INFLUX_BUCKET`/`_ORG`/`_TOKEN`) |
| `greptimedb` | event sink — alias of `influxdb` (same adapter, GreptimeDB image + `INFLUX_URL` path) | not applicable | tracing log | `INFLUX_URL` (+ `INFLUX_BUCKET`) |
| `scalar` | — (additive, no port at all) | not applicable | routes absent (`GET /docs`/`GET /openapi.json` compile out with the feature) | none — compile-time switch only |
| `e2ee` | — (additive) | not applicable | middleware + handshake endpoint | `E2EE_SHARED_KEY` (optional) |
| `oauth2` | — (additive) | not applicable | routes absent (`POST /oauth/token`, `GET /.well-known/jwks.json`, `POST /oauth/revoke`) | `JWT_SECRET`, `OAUTH2_CLIENT_ID`, `OAUTH2_CLIENT_SECRET` |
| `mcp` | — (additive) | not applicable | routes absent (`GET /mcp/sse`, `POST /mcp/message`, `POST /mcp`) | none — compile-time switch only |

A **slot** holds exactly one adapter: `postgres`/`mysql`/`mariadb`/`timescaledb`/`mongodb`/`mssql`
are all adapters of the datastore slot (`UserRepository`), and `clickhouse`/`influxdb`/`greptimedb`
are all adapters of the event-sink slot (`EventSink`) — one occupant per slot; `redis`, `s3`,
`scalar`, `e2ee`, `oauth2` and `mcp` fill no slot and sit alongside anything. Usage and per-capability details are in
[Add a capability](#add-a-capability) below.

Every adapter above also implements the template's `Readiness` port with its
cheapest real round-trip (`SELECT 1`, `PING`, `HEAD bucket`, `GET /health`), and
`add` registers it in `bootstrap.rs` — so `GET /ready` (and, with `GRPC_PORT`,
`grpc.health.v1`) answers `503` naming the backend that is down, while
`GET /health` stays a plain liveness `ok`. A project with no capability on
registers nothing and is ready as soon as it listens. Every HTTP response also
carries an `x-request-id` (the caller's own, or a fresh uuid) that appears in
that request's one-line access log alongside its method, path, status and
latency, and every request is bounded at 30s once the handler is invoked →
`408`. gRPC gets the same budget, but tonic applies it outside the layer stack,
so the caller sees `CANCELLED` ("Timeout expired"), not `DEADLINE_EXCEEDED`, and
nothing is logged. `GET /metrics` renders the Prometheus text format — request
count, duration histogram and in-flight gauge, labelled by the *matched route*
(`/users/{id}`), never the raw path (see the template README's `Metrics`).

## Install

Prebuilt binaries for Linux (x86_64/aarch64, static musl), macOS (Intel and
Apple silicon) and Windows are attached to every tagged release, with a sha256
next to each: <https://github.com/prongbang/kodegen/releases>. Download, unpack,
put `kodegen` on your `PATH` — no toolchain needed to *run* it (generating a
project still needs cargo, and `protoc` to build the result).

From GitHub:

```bash
cargo install --git https://github.com/prongbang/kodegen kodegen
```

The trailing `kodegen` names the package to install — the repo also ships
`templates/hexagonal`, a full Rust package with its own binary, so cargo needs
to be told which one you want.

From source:

```bash
cargo install --path .
```

## Usage

### Create a project

```bash
kodegen new my-service                    # built-in 'hexagonal' template
kodegen new my-service -t hexagonal -o ~/Projects
kodegen new my-service -t owner/repo      # any GitHub repo as a template
kodegen new my-service -t https://gitlab.com/x/y.git
kodegen new my-service -t /path/to/local/template
```

Building a project generated from the `hexagonal` template needs `protoc`
(`brew install protobuf` / `apt-get install protobuf-compiler`) — its
`build.rs` compiles the `.proto` files.

Template resolution order for `-t`:

1. A directory name under `templates/` (built into the binary — works offline)
2. An existing local path
3. `owner/repo` shorthand or a full git URL — cloned once into
   `~/.kodegen/templates/` and reused from there (`kodegen update` re-clones it)

No placeholders required: kodegen reads the template's own package name from
its `Cargo.toml` and rewrites it (kebab **and** snake case) to the new project
name across file contents, file names, and `Cargo.lock`. `{{project_name}}` /
`{{crate_name}}` placeholders are also supported for dedicated template repos.
`.git` and `target` are never copied.

### Scaffold a feature

Inside a project generated from the `hexagonal` template:

```bash
kodegen feature order                    # REST (default): POST /order, GET /order/{id}
kodegen feature payment --transport grpc # tonic service: payment.PaymentApi Create/Get
kodegen feature order --dry-run          # show the plan, write nothing
cargo test                               # both come with a passing integration test
```

Creates one file per layer plus an integration test, then wires everything up
(mod declarations, `Services` field, route `.merge()` or gRPC `.add_service()`,
bootstrap builder) and runs `cargo fmt`:

```
src/domain/order.rs               entity + validation + repository port
src/application/order.rs          OrderService + OrderServiceImpl
src/infrastructure/order_repo.rs  in-memory adapter (Diesel: mirror build_user_service)
src/api/order.rs                  routes() + handlers + FromRef wrapper
tests/order.rs                    create → get through the real router
```

With `--transport grpc` the domain/application/infrastructure layers are the
same; the delivery becomes `proto/payment.proto` + `src/api/payment_grpc.rs`
(a tonic adapter registered in `bootstrap::grpc_routes`), and the test talks
to a real tonic server through the generated client. Serve gRPC features with
`GRPC_PORT=50052 cargo run` — HTTP and gRPC run side by side.

Feature names are snake_case (`order`, `order_item`). Wiring is anchor-based
against the template's structure; if the project has diverged the command
fails with a pointer to the template README's "Adding a Feature" checklist
instead of guessing.

`--dry-run` prints the files that would be created (`+`) and the wiring files
that would be edited (`~`), then stops before writing anything (`cargo fmt`
included). It is the same code path as a real run — the whole change is
computed in memory first — so every anchor is still checked and a diverged
project fails exactly as it would without the flag.

### Add a capability

A generated project starts on SQLite. `kodegen add` moves it to a real server —
later, not only on day one:

```bash
kodegen add postgres                     # Diesel + PostgreSQL
kodegen add mysql                        # Diesel + MySQL
kodegen add mariadb                      # same driver as mysql, MariaDB image + docs
kodegen add timescaledb                  # same driver as postgres, + a hypertable migration
kodegen add mongodb                      # the same UserRepository, on the Mongo driver
kodegen add mssql                        # SQL Server / Azure SQL, raw SQL over tiberius (no ORM)
kodegen add redis                        # a Cache port + a Redis adapter (Valkey too)
kodegen add s3                           # a BlobStore port + an S3 adapter (RustFS/MinIO/AWS)
kodegen add clickhouse                   # an EventSink port + a ClickHouse adapter
kodegen add influxdb                     # the same port, over line protocol (InfluxDB v2/v3)
kodegen add greptimedb                   # same adapter as influxdb, GreptimeDB image + URL
kodegen add scalar                       # GET /docs + GET /openapi.json, derived from the code
kodegen add e2ee                         # end-to-end encryption — XChaCha20-Poly1305 middleware
kodegen add oauth2                       # OAuth2 server & token issuing + AuthUser extractor
kodegen add mcp                          # Model Context Protocol (MCP) SSE & JSON-RPC 2.0 server
kodegen add postgres --orm seaorm        # the same PostgreSQL, on SeaORM instead of Diesel
kodegen add mysql --orm seaorm           # same again on MySQL/MariaDB — no C driver to build
kodegen add postgres --path ~/svc        # run from outside the project
kodegen add postgres --dry-run           # show the plan, write nothing
kodegen add --list                       # what's supported + what this project has on
kodegen new svc --with postgres,redis,s3 # generate, then add, in one command
kodegen new svc --with postgres,redis --orm seaorm
```

See the [capability table](#capabilities) above for each one's slot, `--orm`
support and default (server-less) adapter. The four relational capabilities
take `--orm diesel` (default) or `--orm seaorm`; the ORM changes the adapter
and the cargo feature, never `DATABASE_URL` (see
[`--orm seaorm`](#--orm-seaorm)). `mssql` is relational too and takes
neither — there is no open-source ORM for SQL Server, so `--orm` on it is an
error that says why (see [`add mssql`](#add-mssql)).

A **slot** holds exactly one adapter, so the capabilities in it are alternatives:
the four relational backends, `mongodb` and `mssql` are all adapters of the same
`UserRepository`, and `clickhouse`/`influxdb`/`greptimedb` are all adapters of
the same `EventSink`. `--with` rejects a second occupant of either slot before
`new` writes anything. Everything else combines freely — one datastore, one event sink,
a cache and a blob store is a legal set (`--with mongodb,redis,s3,clickhouse`).

`add postgres` rewrites the six things that actually differ between dialects:

```
Cargo.toml                              default = ["sqlite"] -> ["postgres"]
src/infrastructure/diesel_db.rs         SqliteConnection -> PgConnection (whole file replaced)
src/infrastructure/diesel_repo.rs       save() -> the dialect's upsert; test module replaced
migrations/*_create_users/up.sql        the dialect's CREATE TABLE (skipped when it is already what the file says — PostgreSQL's is)
docker-compose.dev.yml                  a dev service with a healthcheck (created if absent)
Dockerfile                              perl/make/cmake for the bundled C driver
```

Then:

```bash
docker compose -f docker-compose.dev.yml up -d
DATABASE_URL=postgres://app:app@127.0.0.1:5432/app cargo run
```

Leaving `DATABASE_URL` unset still selects the in-memory adapter, so `cargo
test` passes with no server running — the adapter tests skip themselves rather
than fail.

Two rules the command enforces:

- **One occupant per slot.** The composition root builds a single handle behind
  each port — `Db`/`Mongo` behind `UserRepository`, one client behind
  `EventSink` — so `add mysql` on top of `postgres`, either on top of `mongodb`,
  or `add influxdb` on top of `clickhouse`, is refused with the steps to undo the
  first one, not applied halfway. `add redis`, `add s3` and `add scalar` fill no
  slot and are never caught by this rule.
- **Adding the same capability twice** is an error, not a no-op rewrite. So is
  adding two names for one cargo feature: `mariadb` after `mysql`,
  `timescaledb` after `postgres`, or `greptimedb` after `influxdb`.

Two of those are replacements, not edits, so both are guarded: `diesel_db.rs`
must still be byte-for-byte what `kodegen new` wrote, and `diesel_repo.rs` must
still end with the generated `#[cfg(test)] mod tests` block. Queries you added
above that block are kept; if either check fails the command refuses and prints
the manual steps instead of clobbering your work. Same for the seed migration —
a `up.sql` you wrote yourself is left alone with a warning, never rewritten.

Both dialects get a real upsert. On MySQL that means `ON DUPLICATE KEY UPDATE`
rather than `REPLACE INTO`: `REPLACE` is a delete followed by an insert, so it
would fire `ON DELETE CASCADE` on everything referencing the row on what
`UserRepository::save` documents as an update.

`--dry-run` is the same code path minus the writes: the whole change is computed
in memory, so every signature is still checked and a diverged project fails
exactly as it would without the flag.

MariaDB is not a separate adapter. It speaks the MySQL wire protocol, so it is
the same driver, the same cargo feature and the same generated code — only the
compose image and the wording differ. TimescaleDB stands in the same relation to
`postgres` (an extension, not a different server — see below), Valkey to `redis`
(same RESP protocol, same client, same generated Rust — swap the image in
`docker-compose.dev.yml` and nothing else changes), MinIO and Ceph to `s3` (one
API, one adapter, one endpoint variable), and `greptimedb` to `influxdb` (the
same line protocol on a different path, so the same `timeseries` feature and the
same adapter file). Ten capabilities, six adapters.

#### `add timescaledb`

Not a fifth dialect either: TimescaleDB *is* PostgreSQL with an extension
loaded, so `add timescaledb` writes the PostgreSQL capability — same
`postgres` cargo feature, same `PgConnection`, same upsert, same
`DATABASE_URL` — and adds the two things that are actually different:

```
migrations/*_create_users/up.sql        CREATE EXTENSION timescaledb + an example hypertable
migrations/*_create_users/down.sql      drops the example table as well as users
docker-compose.dev.yml                  timescale/timescaledb instead of postgres (same name, port, credentials)
```

The example `events` table is a hypertable — a normal table the extension
partitions by time — and nothing in the generated Rust reads it: it is there to
show what the extension is for, and to be replaced by your own measurement
table. `CREATE EXTENSION` is the first statement in the migration because Diesel
runs each migration inside one transaction and TimescaleDB requires it there.

Moving an existing PostgreSQL project onto TimescaleDB is the image plus that
migration; nothing in `src/` changes. That is also why there is no separate
`timescaledb` cargo feature — and why `--with postgres,timescaledb` is refused as
one capability named twice.

An app-side `EventSink` (`add clickhouse` / `influxdb` / `greptimedb`) is a
different thing and can be added alongside: the hypertable is schema in *this*
project's database, the event sink is a port to somebody else's.

#### `--orm seaorm`

The relational capabilities take an ORM: `--orm diesel` (the default, and what
every example above uses) or `--orm seaorm`. It is a flag rather than four more
capability names because the ORM is orthogonal to the backend — the same axis
`--transport rest|grpc` sits on. One ORM per project: they are two adapters of
one `UserRepository`, so they share the datastore slot with each other and with
`mongodb`, and `src/lib.rs` carries a `compile_error!` in case a stray
`--features` ever turns both on anyway.

```bash
kodegen add postgres --orm seaorm
kodegen add timescaledb --orm seaorm     # same feature, plus an idempotent hypertable
kodegen add mysql --orm seaorm           # no libmysqlclient, so no cmake in the build image
kodegen add mariadb --orm seaorm         # = mysql, different compose image
```

All four relational capabilities, on either ORM. MySQL is the backend that had
to prove itself against a real server before it shipped, because its upsert
differs at *runtime* rather than at compile time (below), and CI now runs a row
that does exactly that — twice over the same database.

One thing to know about the MySQL side: `seaorm-mysql` pulls `sqlx-mysql`, which
pulls `rsa`, which carries **RUSTSEC-2023-0071 with no fixed version**. It is in
`Cargo.lock` of every generated project whether or not the feature is on, so
`.cargo/audit.toml` ignores it, with the reasoning in the file. The short
version: the advisory is a timing sidechannel in RSA *decryption*, and
`sqlx-mysql` only ever encrypts — with the public key the server hands over
during `caching_sha2_password` auth, and not at all over TLS. There is no
private key on the client side to attack. Delete the ignore the day `rsa` ships
a fix. The Diesel `mysql` backend never had this: libmysqlclient does its auth
in C.

```
Cargo.toml                              default = ["sqlite"] -> ["seaorm-postgres"]
src/infrastructure/sea_db.rs            the Db handle + the startup schema
src/infrastructure/sea_repo.rs          the entity + the UserRepository adapter
src/infrastructure/mod.rs               two cfg-gated `pub mod` lines appended
src/bootstrap.rs                        the same three blocks `add mongodb` swaps
docker-compose.dev.yml                  the same postgres service as the Diesel path
```

`DATABASE_URL` does not change, the compose service does not change, and
**`Dockerfile` is not touched at all** — that is the payoff: sqlx and rustls are
pure Rust, so the build stage needs no perl/make/cmake and the distroless/cc
runtime image needs nothing new. Nothing is overwritten either: `diesel_db.rs`
and `diesel_repo.rs` stay on disk, gated off, so undo is
`default = ["sqlite"]` plus the three `bootstrap.rs` blocks.

What you give up is versioned migrations:

> **The schema is applied at startup, not by a migration.** `sea_db.rs` holds a
> `SCHEMA` constant of idempotent statements (`CREATE TABLE IF NOT EXISTS`, and
> for TimescaleDB `create_hypertable(..., if_not_exists => TRUE)`), run once when
> the first connection is opened. `migrations/` is not read on this path, and
> `add` says so rather than deleting it — it is exactly what a later
> `--orm diesel` needs back.
>
> **Where that stops being enough:** the first schema change on a database that
> already holds production data. `CREATE TABLE IF NOT EXISTS` cannot express an
> `ALTER`, and a startup step that silently does nothing is worse than one that
> fails. At that point add `sea-orm-migration = "1.1"` (pinned — its 2.x needs
> sea-orm 2.0 and rustc 1.94, and MSRV-aware resolution will otherwise hand you a
> lock that does not build), move the statements into the first migration, and
> empty `SCHEMA`. Before that point — a seed schema, a fresh database — the
> startup DDL is the same deal `add mongodb`, `add clickhouse` and `add s3`
> already make, and it costs one constant.
>
> **The app role must be allowed to create tables**, and existing tables do not
> excuse it: PostgreSQL checks `CREATE` on the schema *before* it checks whether
> the object exists, so a role without it gets `permission denied for schema
> public` even when `users` has been there for a year. That surfaces as a failing
> **request**, never a failing boot (the connection is lazy) — a 500 on every
> request, forever, since a failed init is not cached. In an environment where
> the app role does not own the schema, either
> `GRANT CREATE ON SCHEMA public TO <role>` or empty `SCHEMA` and manage the
> schema out of band.

Two `Db` types, one at a time. The SeaORM handle is *also* called `Db` — that is
deliberate, and it is why `kodegen feature` needs no third state and why builders
scaffolded before or after the switch keep compiling in both directions. They
live in different modules and can never be compiled together, so a file that has
to say which one it means writes
`use crate::infrastructure::sea_db::Db;` (or `diesel_db::Db`). Only the name is
shared: there is no `Db::run` on the SeaORM side, because nothing blocks — no
`spawn_blocking`, no r2d2, no second pool.

`save` is an upsert on both ORMs, and on SeaORM it is
`.on_conflict(...).update_column(...).exec_without_returning(...)`. The last part
is not a style choice: an upsert whose new values equal the old ones affects 0
rows, and `exec` reports that as `DbErr::RecordNotInserted` — an error for a call
the port documents as `Ok(())`. The generated `save_twice_identical_values_is_ok`
test is that contract, and CI runs it twice against the same database.

The two adapter test modules also reach for their handle differently, and copying
one pattern into the other file is a 30-second hang, not a compile error: the
Diesel tests share one handle from a `OnceLock`, while the SeaORM tests build a
`Db` **per test**, because its pool belongs to the `#[tokio::test]` runtime that
opened it and does not outlive it — the next test would sit on a dead pool until
sqlx's acquire timeout gives up.

#### `add mongodb`

Not a fourth dialect: a second driver behind the same port. `UserRepository` is
unchanged, so nothing above `infrastructure/` moves — which is why it takes the
datastore slot instead of sitting alongside a relational backend.

```
Cargo.toml                              default = ["sqlite"] -> ["document"]
src/infrastructure/mongo_repo.rs        the Mongo handle + the UserRepository adapter
src/infrastructure/mod.rs               one cfg-gated `pub mod mongo_repo;` appended
src/bootstrap.rs                        MONGODB_URL/MONGODB_DB instead of DATABASE_URL
docker-compose.dev.yml                  a mongo:8 service with a healthcheck (created if absent)
```

```bash
docker compose -f docker-compose.dev.yml up -d
MONGODB_URL=mongodb://127.0.0.1:27017 MONGODB_DB=app cargo run
```

Four things follow from the driver, not from taste:

- **Every module in the datastore slot is cfg-gated, both ways.** The template
  already ships `diesel_db.rs` and `diesel_repo.rs` behind `relational` (which
  every SQL backend turns on and `document` does not), and `add mongodb` appends
  `mongo_repo.rs` behind `document` the same way. `relational` carries the Diesel
  dependencies themselves, so a Mongo project does not build the ORM at all —
  `cargo tree -e normal | grep diesel` comes back empty. Undo is
  `default = ["sqlite"]` plus the three Diesel blocks in `bootstrap.rs`: no file
  has to be deleted, in either direction.
- **The client is dialed lazily**, through a `tokio::sync::OnceCell` on first
  query: every `mongodb` constructor is `async` and the composition root is a
  plain `fn`. A dead database cannot stop the process from booting, and a failed
  dial is not cached — the next request retries. The cost is that the *first*
  request against an unreachable server takes the driver's server-selection
  timeout to fail (30s by default) before it answers `503`; shorten it in the
  URL if that matters — `?serverSelectionTimeoutMS=2000`.
- **No migration.** `save` upserts on `_id` (the domain id), so it is still an
  insert-or-update and never `NotFound`, with no `CREATE TABLE` step to run
  first.
- **Leaving `MONGODB_URL` unset** selects the in-memory adapter, exactly as an
  unset `DATABASE_URL` does. `cargo test` stays green with no server.

#### `add mssql`

The datastore slot a third time, and the only capability here with no ORM behind
it at all. Structurally it is `--orm seaorm`: two new files, three swapped
`bootstrap.rs` blocks, a handle called `Db`, a schema applied at startup, nothing
overwritten. What is different is that the SQL is text you can read, and that the
`Dockerfile` is touched.

```
Cargo.toml                              default = ["sqlite"] -> ["mssql"]
src/infrastructure/mssql_db.rs          the Db handle, the pool, the startup schema
src/infrastructure/mssql_repo.rs        the UserRepository adapter (MERGE + SELECT)
src/infrastructure/mod.rs               two cfg-gated `pub mod` lines appended
src/bootstrap.rs                        the same three blocks `add mongodb` swaps
Dockerfile                              perl/make for vendored OpenSSL, + ENV SSL_CERT_FILE
docker-compose.dev.yml                  a SQL Server 2022 service **and** an init service
```

```bash
docker compose -f docker-compose.dev.yml up -d --wait mssql
docker compose -f docker-compose.dev.yml run --rm mssql-init
DATABASE_URL='server=tcp:127.0.0.1,1433;user id=sa;password=kodeGen!2026;database=app;encrypt=true;TrustServerCertificate=true' cargo run
```

**Read this before you choose it.** It is the thinnest capability in the list,
and the three facts below are the whole of why:

- tiberius 0.12.3 was released in **July 2024** and has had no release since. It
  is also the only usable TDS implementation in Rust, and there is no ORM on
  either side of it — diesel#2201 has been open since 2019, and SeaORM's SQL
  Server backend lives in the commercial SeaORM X. So this capability writes raw
  SQL, and it is the one capability with no second driver to fall back to.
- It is watched rather than assumed: `cargo audit` runs over the template's lock
  in this repo's CI on every change, and every generated project ships the same
  job.
- **The withdrawal trigger is stated in advance.** A RUSTSEC advisory on tiberius
  itself, or on a dependency it pins that a lock bump cannot move off, deprecates
  this capability in the next release. It does not get an `ignore` entry in
  `.cargo/audit.toml` — that file is for advisories whose unreachability has been
  demonstrated in source, which is not the same thing as an advisory nobody can
  fix.

`DATABASE_URL` keeps its name, because it is the same slot's switch. Its *value*
is an **ADO.NET connection string**, not a URL — that is the shape SSMS, the
Azure portal and Microsoft's own documentation print, and the only shape
`tiberius::Config::from_ado_string` parses. A `postgres://…` left over from
another backend fails to parse on the first request rather than quietly doing
something else.

Six things follow from the backend rather than from taste:

- **No `CREATE TABLE IF NOT EXISTS`.** SQL Server does not have it, in 2022 or in
  2025, so the startup DDL is
  `IF OBJECT_ID(N'dbo.users', N'U') IS NULL CREATE TABLE …`. Copying the spelling
  from any other adapter in this repo is a *runtime* syntax error. The columns are
  `NVARCHAR`, never `VARCHAR`: tiberius sends every Rust string as `NVARCHAR`, and
  against a `VARCHAR` key each lookup becomes an implicit conversion that turns an
  index seek into a scan — with nothing to warn you.
- **`save` is `MERGE … WITH (HOLDLOCK)`.** One statement, so it is atomic in
  autocommit with no transaction to manage. `HOLDLOCK` is not optional: without
  it two concurrent merges on one key can both take the NOT MATCHED branch, which
  fails no test and only appears under a real concurrent writer. Neither is the
  trailing `;`, which `MERGE` requires.
  **When to stop using it:** the known bugs in `MERGE` all need something this
  two-column table does not have — a trigger, a foreign key pointing at `users`,
  a filtered index, an `OUTPUT $action`, or a view as the target. Add any of those
  and re-evaluate; the replacement is `UPDATE` then `INSERT` on `@@ROWCOUNT = 0`
  inside an explicit `BEGIN TRAN`/`COMMIT` with `SET XACT_ABORT ON` — and it is
  only correct *with* that transaction, because in autocommit the `UPDATE`'s range
  lock is released before the `INSERT` runs. That comment lives next to the
  constant too.
- **The database is created by a compose service, not by the app.** This image has
  no `POSTGRES_DB` equivalent — it boots with master/tempdb/model/msdb — so
  `mssql-init` runs `IF DB_ID('app') IS NULL CREATE DATABASE app` once. A real
  deployment does the same in its deployment step: an application role is almost
  never granted `CREATE DATABASE`. Skip it and the first request answers 500 with
  SQL Server's own words in the log ("Cannot open database 'app' requested by the
  login"), which is deliberate — the pool is built with one idle connection and
  connection retries off, so that message arrives immediately instead of behind a
  30-second pool timeout.
- **`docker compose up -d --wait` over the whole file exits 1**, even when
  everything worked: `mssql-init` exits 0 and `--wait` reads a container that is
  no longer running as a failed dependency. Hence the two commands above, which is
  also what CI runs.
- **`ENV SSL_CERT_FILE` in the Dockerfile is load-bearing.** `native-tls` means
  `openssl-sys`, and the distroless/cc runtime image has no libssl, so it is
  linked statically (`vendored`) — which compiles in an `OPENSSLDIR` pointing at a
  build directory that does not exist at runtime. Without that variable the binary
  cannot verify any certificate, and the first thing to notice is a deployment
  failing to reach Azure SQL, because dev and CI connect with
  `TrustServerCertificate=true` and never validate anything. CI asserts the CA
  bundle is in the built image on every run. `TrustServerCertificate=true` is for
  the dev container only — drop it in production and use a real CA.
- **Two host caveats.** There is no arm64 image (the manifest is amd64 only and
  Microsoft does not support emulating it), so the compose service pins
  `platform: linux/amd64` and is slow on Apple Silicon. And on a **macOS host** the
  app cannot reach that container at all with `TrustServerCertificate=true`:
  `native-tls` is Apple's Secure Transport there, tiberius's trust-everything path
  also turns SNI off, and Secure Transport refuses — you get
  `Tls("connection closed via error")`. Ordinary certificate validation works on
  macOS, and the whole path works on Linux, which is what the runtime image and CI
  are. On a Mac, run the service in a container, or use `encrypt=DANGER_PLAINTEXT`
  against your own local container only.

Why not `tiberius/rustls`, given that everything else here is rustls? Because it
pins `tokio-rustls 0.24` → **rustls 0.21**, a major version behind the rest of the
graph and from before rustls had a provider system, so `ring` comes back into a
build that got rid of it and the binary carries two TLS stacks. And `cargo audit`
on that lock is not clean: `rustls-webpki 0.101.7` carries three advisories that
sit squarely in **certificate validation** — the code path every TLS connection
runs — with the fix in a major version tiberius cannot reach. That is not an
advisory anyone can honestly ignore, so the answer was to not take the dependency.

#### `add redis`

Purely additive, and the reason the wiring markers exist: it introduces a port
the template does not ship, rather than swapping an adapter behind one it does.

```
Cargo.toml                              default = [...] gains "cache"
src/domain/ports.rs                     the Cache port, appended (get/set/delete + TTL)
src/domain/mod.rs                       pub use ports::Cache;
src/infrastructure/in_memory_cache.rs   default adapter — no server needed
src/infrastructure/redis_cache.rs       Redis/Valkey adapter
src/api/mod.rs                          one `cache` field on Services
src/bootstrap.rs                        REDIS_URL resolved once + a build_cache builder
tests/cache.rs                          the port's contract, run against both adapters
docker-compose.dev.yml                  a redis service with a healthcheck
```

Both adapters are held to one set of assertions in `tests/cache.rs` — the Redis
half gated on `REDIS_URL`, the in-memory half always. A cache miss is `Ok(None)`
on both, expiry is honored on both; that is what makes them substitutable rather
than merely similar.

#### `add s3`

Additive in exactly the same way, and one adapter for every S3-compatible
server: RustFS, MinIO, Ceph and AWS S3 itself differ by an endpoint, not by code.

```
Cargo.toml                              default = [...] gains "blob"
src/domain/ports.rs                     the BlobStore port + check_blob_key, appended
src/domain/mod.rs                       pub use ports::{check_blob_key, BlobStore};
src/infrastructure/fs_blobs.rs          default adapter — one file per key, no server
src/infrastructure/s3_blobs.rs          S3 adapter (RustFS/MinIO/Ceph/AWS)
src/api/mod.rs                          one `blobs` field on Services
src/bootstrap.rs                        the S3 client built once + a build_blob_store builder
tests/blob.rs                           the port's contract, run against both adapters
docker-compose.dev.yml                  a RustFS service with a healthcheck
```

```bash
docker compose -f docker-compose.dev.yml up -d
S3_BUCKET=app S3_ENDPOINT=http://127.0.0.1:9000 \
  AWS_ACCESS_KEY_ID=rustfsadmin AWS_SECRET_ACCESS_KEY=rustfsadmin cargo run
```

`S3_BUCKET` is the switch: unset, the app writes blobs under `./data/blobs`
(`BLOB_DIR` overrides) and `cargo test` still passes with nothing running.
`S3_ENDPOINT` is what makes RustFS and MinIO work — setting it also turns on
path-style addressing, because a self-hosted server serves every bucket from one
host and `<bucket>.<endpoint>` resolves nowhere. On AWS, leave it unset.

Three things worth knowing before you build on it:

- **Credentials come from `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`**, not from
  `aws-config`'s provider chain: that chain loads asynchronously and
  `build_services_from_env()` is a plain `fn`. They are read **once at startup
  and never refreshed**, so `AWS_SESSION_TOKEN` works only for a token that
  outlives the process — anything rotating (STS, IRSA, SSO, an instance role)
  needs the chain. Adding it means `aws-config` as a dependency and `async`
  spreading up the call chain: `build_services_from_env` → `build_router` →
  `main.rs`, plus every caller of `build_router` (`tests/api.rs`,
  `examples/alloc_bench.rs`). The adapter itself does not change.
- **Keys are validated before they reach either adapter** (`check_blob_key`):
  non-empty, at most 1024 bytes, no backslashes, colons or control characters,
  no empty/`.`/`..` segments, and nothing the platform reads as a root, drive or
  prefix. The filesystem adapter maps keys onto paths, so `../` would walk out of
  the blob directory and `C:evil.txt` (drive-relative on Windows) would discard
  it entirely — checking in one place is what keeps the two adapters
  substitutable. Keys must also not be prefixes of one another: S3 has no
  directories, the filesystem does, and `a` cannot be both a file and the
  directory holding `a/b`.
- **The bucket is not created for you.** `tests/blob.rs` creates it because a
  test may; the app assumes it exists, the way it assumes the database does.

#### `add clickhouse` / `add influxdb` / `add greptimedb`

One port, two adapters, three commands, and the only additive capability with a
*slot*: the `EventSink` port has one adapter, so a project has ClickHouse or a
time-series database, never both (`influxdb` and `greptimedb` are two names for
the second one). Everything else about it is additive — any of them sits next to
any datastore, the cache and the blob store.

```
Cargo.toml                                  default = [...] gains "analytics" or "timeseries"
src/domain/ports.rs                         the EventSink port + Event + check_event, appended
src/domain/mod.rs                           pub use ports::{check_event, Event, EventSink};
src/infrastructure/log_events.rs            default adapter — writes to the tracing log, no server
src/infrastructure/clickhouse_events.rs     ClickHouse adapter        (add clickhouse)
src/infrastructure/line_protocol_events.rs  line-protocol adapter     (add influxdb, add greptimedb)
src/api/mod.rs                              one `events` field on Services
src/bootstrap.rs                            the client built once + a build_event_sink builder
tests/events.rs                             the port's contract, run against both adapters
docker-compose.dev.yml                      a clickhouse, influxdb or greptimedb service with a healthcheck
```

```bash
docker compose -f docker-compose.dev.yml up -d
CLICKHOUSE_URL=http://127.0.0.1:8123 CLICKHOUSE_DB=app \
  CLICKHOUSE_USER=app CLICKHOUSE_PASSWORD=app cargo run
# or
INFLUX_URL=http://127.0.0.1:8086 INFLUX_BUCKET=app INFLUX_ORG=app \
  INFLUX_TOKEN=kodegen-dev-token cargo run
# or (add greptimedb)
INFLUX_URL=http://127.0.0.1:4000/v1/influxdb INFLUX_BUCKET=public cargo run
```

**One adapter, three products.** InfluxDB v2, InfluxDB v3 and GreptimeDB all
accept the same line protocol over the same kind of HTTP endpoint, so `add
influxdb` and `add greptimedb` write the same `timeseries` feature and the same
adapter file — what differs is the compose service and `INFLUX_URL`:

| Server | `INFLUX_URL` | Notes |
|---|---|---|
| InfluxDB v2 | `http://127.0.0.1:8086` | set `INFLUX_ORG`; `INFLUX_BUCKET` is the bucket |
| InfluxDB v3 | `http://127.0.0.1:8181` | `INFLUX_BUCKET` is the database; org is ignored |
| GreptimeDB | `http://127.0.0.1:4000/v1/influxdb` | `INFLUX_BUCKET` is the database and it must already exist (the server creates *tables* on write, not databases) — `public` is the one a fresh server has. `INFLUX_ORG` is ignored; `INFLUX_TOKEN` is unused unless authentication is on, and then it is `<user>:<password>`, not a bare token |

The composition root appends `/api/v2/write?bucket=…&precision=ns` to whatever
you set, which is the whole reason the path is yours to choose — GreptimeDB's
compat endpoint lives under `/v1/influxdb`, so that prefix is part of the base
address. Swap the image in `docker-compose.dev.yml` and nothing in the generated
Rust changes; `add greptimedb` is that swap, done for you.

What the port is, and is not:

- **Write only.** There is no query method, deliberately: what you ask an
  analytical store is not what you ask a time-series one, and a port that tried
  to be both would be an interface nobody could implement twice. Query the store
  directly from whatever needs to; this port is how the application *records*.
- **Not fire-and-forget.** `write` awaits the round trip and returns the failure.
  There is no queue, no retry and no backpressure behind it — one call is one
  request. Batch and `tokio::spawn` if the caller must not wait; `let _ =
  sink.write(..).await;` if it can afford to lose events. That is the caller's
  decision to make, not the port's.
- **One event = a name, a timestamp and `f64` fields.** No tags and no other
  field types, because that is the shape both backends take honestly: line
  protocol has tags and integers/booleans/strings, ClickHouse would need a `Map`
  column and a schema decision. Put the dimension in the name
  (`http.requests.checkout`) until a real requirement says otherwise.
- **`check_event` runs in every adapter**, including the default one, and its
  character rule (ASCII letters, digits and `_-./:`) is what lets the
  line-protocol adapter skip escaping entirely — a comma or a space in a
  measurement changes what the line *means*. A batch is validated as a whole
  before anything is sent; once it is on the wire, an error says nothing about
  how much of it landed.
- **The default adapter is a log, not a store.** With no URL set, events go to
  the tracing subscriber the service already installs and nothing is queryable.
  That is the honest default for a port with no read path — an in-memory buffer
  would grow without bound while looking like durability.
- **Every write has a 5s budget.** Neither hyper nor the ClickHouse client has a
  default timeout, so without one a sink that accepts the connection and then
  says nothing holds the caller — and the request it is on — indefinitely. A
  timeout is `Unavailable`, the same as a refused connection. (The `Cache` port's
  Redis adapter is deliberately not wrapped this way: its connection manager is
  unbounded too, but it owns its own reconnect loop and a cache call is not the
  thing that must not hang.)
- **Both adapters speak plain HTTP.** For a managed endpoint on https, put it
  behind a TLS-terminating proxy, or turn on `clickhouse`'s `rustls-tls` feature
  / swap the line-protocol adapter's `build_http()` for a `hyper-rustls`
  connector. The line-protocol adapter is written on hyper rather than a client
  crate precisely because `tonic` already puts hyper in the graph: it costs
  `Cargo.lock` **nothing**, where `reqwest` would have cost 12 packages including
  a QUIC stack.

ClickHouse creates its `events` table on the first write (`CREATE TABLE IF NOT
EXISTS … ENGINE = MergeTree`) — it is not the project's relational database, so
there is no `migrations/` step for it. The table is one row per field, which is
what keeps the row type a plain `derive(Row)`; a wide table is the right answer
once you know your fields, and `clickhouse_events.rs` is where you would make
that change.

#### Why the dependency tree looks bigger than the build

Every backend is declared as an **optional** dependency, and `add` only flips
`default = [...]`. Cargo pins optional dependencies in `Cargo.lock` whether or
not their feature is on, so:

- `Cargo.lock` lists `pq-sys`, `mysqlclient-sys` and friends even in a
  SQLite-only project — **they are not compiled** until a feature turns them on.
- `cargo build --locked` keeps working through every `kodegen add`, offline
  included, because no dependency is ever *added* — only selected.
- `cargo audit` reads the lock, not the binary, so an advisory can fire for a
  crate the project never builds. Check what actually pulls it in before
  treating it as exploitable:

  ```bash
  cargo tree -e normal -i <crate>                     # in the current feature set
  cargo tree -e normal --features postgres -i <crate> # ...and with one turned on
  ```

  Generated projects ship `.cargo/audit.toml` for the cases where the answer is
  "nothing here compiles it" — add the advisory id to `ignore` with a comment
  saying why, and a date to revisit.

The template also sets `resolver = "3"`, which is not cosmetic once the AWS SDK
is in the graph: it requires rustc 1.94 today, well past the template's
`rust-version = "1.88"`. The MSRV-aware resolver picks the newest release that
still builds on the declared MSRV, so a `cargo update` cannot quietly hand
someone a lock their toolchain refuses. Raising `rust-version` is what moves the
SDK forward — and the lock costs 73 more entries with `blob` declared, none of
them compiled until the feature is on.

It is also what holds `clickhouse` at 0.13: 0.14 and 0.15 declare
`rust-version = 1.89`, so the range in `Cargo.toml` is what keeps the declared
MSRV honest rather than letting the resolver fall back to a version nobody on
1.88 can build. The whole `analytics` capability costs 11 lock entries;
`timeseries` costs **zero**, because it is `hyper`/`hyper-util`/`http-body-util`,
which `tonic` already pulls in.

#### `add scalar`

No port, no adapter, no slot — the only capability that is a route and a
derived document rather than a piece of infrastructure, which is why it takes
no `--orm`, needs no env, and sits next to anything (`--with
mongodb,redis,s3,clickhouse,scalar` is a legal set).

```
Cargo.toml               default = [...] gains "apidocs" (utoipa/utoipa-scalar were already optional deps — no lock churn from this alone)
src/api/openapi.rs        created — #[derive(utoipa::OpenApi)] + GET /docs (Scalar) + GET /openapi.json
src/api/mod.rs             .merge(openapi::routes()); cfg_attr(utoipa::path) on health and ready, cfg_attr(ToSchema) on ErrorRes and ReadyRes
src/api/user.rs           same cfg_attr treatment on create_user/get_user/CreateUserReq
src/api/hello.rs          same cfg_attr treatment on say_hello
src/domain/model.rs       cfg_attr(ToSchema) on User
tests/apidocs.rs          created — GET /openapi.json parses + covers the shipped routes, GET /docs is reachable
```

**The spec is derived from the code, not a static file kept in sync by hand.**
`kodegen feature <name>` annotates every handler and DTO it scaffolds with
`#[cfg_attr(feature = "apidocs", utoipa::path(...))]` /
`#[cfg_attr(feature = "apidocs", derive(utoipa::ToSchema))]`
**unconditionally** — whether or not a project has `scalar` on — which is what
makes both orders work:

- `kodegen feature order` then `kodegen add scalar`: the annotations are
  already there, so `add scalar` scans `src/api/*.rs` for
  `utoipa::path(` immediately followed by an `async fn`, and lists what it
  finds in `paths(...)` from the start.
- `kodegen add scalar` then `kodegen feature order`: `kodegen feature` sees
  `src/api/openapi.rs` on disk and inserts the new feature's two paths above
  the `// kodegen:openapi:paths` marker `add scalar` leaves in place for
  exactly that.

A route added by hand, without the attribute, is invisible to the spec —
`kodegen` cannot notice a route that skipped it, so the generated project's own
README carries the same checklist as "Adding a Feature" for this case.

`GET /openapi.json` is fully offline (parse it, contract-test it, pipe it to a
client generator); `GET /docs` renders the Scalar UI from a page whose
`<script>` loads `@scalar/api-reference` off `cdn.jsdelivr.net`, so that one
page needs a network. Building, testing and every other route stay exactly as
offline as before — see the generated project's own README for the vendoring
trade-off if that page needs to work air-gapped.

**Both routes ship in a production build unless you turn them off** — `apidocs`
is a `default = [...]` feature like every other capability here: drop it and
both routes, plus every `cfg_attr`-gated line, compile out completely. Do that
before shipping anything you would not want the API surface publicly readable
for, or put `/docs`/`/openapi.json` behind the same auth/ingress rule as the
rest of the API. There is deliberately no env-based kill switch — see the
generated project's README for why.

#### `add e2ee`

Adds end-to-end request/response payload encryption using **XChaCha20-Poly1305** and **X25519** ECDH key exchange (via `lazynton`). Sits alongside any datastore, cache, or event sink:

```
Cargo.toml               default = [...] gains "e2ee"
src/api/e2ee.rs          created — handshake endpoint (POST /e2ee/handshake) + encryption middleware
src/api/mod.rs           .merge(e2ee::routes()) + wraps the router with e2ee middleware
tests/e2ee.rs            created — handshake verification + encrypted request/response roundtrip
```

- **Per-client ephemeral keys**: Clients perform an X25519 ECDH handshake (`POST /e2ee/handshake`) to negotiate a symmetric session key.
- **Optional pre-shared key**: Setting `E2EE_SHARED_KEY` (64-character hex string) enables static encryption fallback for clients without handshake.
- **Zero overhead when disabled**: Remove `"e2ee"` from `default = [...]` in `Cargo.toml` to compile out the middleware and handshake route entirely.

#### `add oauth2`

Adds an **OAuth2 Authorization Server & Resource Server** (RFC 6749, RFC 7517, RFC 7009) with JWT token issuing, JWKS key discovery, and the `AuthUser` extractor:

```
Cargo.toml               default = [...] gains "oauth2"
src/api/oauth2.rs        created — POST /oauth/token, GET /.well-known/jwks.json, POST /oauth/revoke, AuthUser
src/api/mod.rs           .merge(oauth2::routes())
tests/oauth2.rs          created — client credentials token flow, JWKS verification, and revocation
```

- **Endpoints**:
  - `POST /oauth/token`: Issue access tokens via `grant_type=client_credentials` (accepts both `application/x-www-form-urlencoded` and `application/json`).
  - `GET /.well-known/jwks.json`: Public JSON Web Key Set for upstream token verification.
  - `POST /oauth/revoke`: Token revocation endpoint.
- **Protecting routes**: Inject `auth: AuthUser` as a parameter into any handler; unauthenticated requests receive `401 Unauthorized`.
- **Environment variables**: `JWT_SECRET` (token signing key), `OAUTH2_CLIENT_ID`, and `OAUTH2_CLIENT_SECRET`.

#### `add mcp`

Adds a **Model Context Protocol (MCP 2024-11-05 spec)** server to expose application domain tools to AI agents (such as Antigravity, Claude Desktop, Cursor, and custom agentic frameworks):

```
Cargo.toml               default = [...] gains "mcp"
src/api/mcp.rs           created — GET /mcp/sse, POST /mcp/message, POST /mcp, JSON-RPC 2.0 tool dispatch
src/api/mod.rs           .merge(mcp::routes())
tests/mcp.rs             created — initialize, ping, tools/list, tools/call, and SSE stream tests
```

- **Dual Transport**:
  - **SSE Transport**: `GET /mcp/sse` initiates a streaming connection and emits the session endpoint; requests are sent to `POST /mcp/message?sessionId=<id>`.
  - **Direct JSON-RPC**: `POST /mcp` for direct synchronous request-response execution.
- **Lifecycle & Tools**:
  - Implements standard MCP methods: `initialize` (protocol version `2024-11-05`), `ping`, `tools/list`, `tools/call`, and `resources/list`.
  - Shipped tools: `create_user` and `get_user`.
- **Automatic Feature Integration**:
  - Running `kodegen feature <name>` automatically appends `create_<name>` and `get_<name>` tools into `src/api/mcp.rs`.
  - Running `kodegen add mcp` on an existing project automatically scans and exposes tools for all previously scaffolded features.
- **Zero external heavyweight dependencies**: Built directly on `axum`, `tokio`, `tokio-stream`, and `serde_json`.

### Update a cached template

```bash
kodegen update prongbang/hexagonal-rs    # drop the cached clone and re-clone it
kodegen update https://gitlab.com/x/y.git
```

Built-in templates ship inside the binary and local paths are used in place —
neither has a cache, so `update` reports that instead of doing nothing.

### List templates

```bash
kodegen list
```

## Adding a built-in template

A template is just a folder — copy any Rust project in and rebuild:

```bash
rsync -a --exclude target --exclude .git  ~/path/to/starter/  templates/my-starter/
cargo build
kodegen new demo -t my-starter
```

Keep the template's `Cargo.lock` in: generated projects inherit it (name is
rewritten automatically), so `cargo build --locked` and Docker builds work
straight away.

## Cutting a release

Bump `version` in the root `Cargo.toml`, commit, then push a `vX.Y.Z` tag that
matches it — `.github/workflows/release.yml` builds every target and attaches
the binaries to a GitHub Release. A tag that disagrees with `Cargo.toml` fails
the first step, before anything is published. There is no crates.io publish:
the name `kodegen` is already taken there by an unrelated crate.

## License

MIT — see [LICENSE](LICENSE).
