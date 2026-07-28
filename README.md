# kodegen

CLI for generating Rust starter projects, scaffolding features into them, and
switching their infrastructure capabilities.

| Command | What it does |
|---|---|
| `kodegen new <name>` | Generate a project from a template |
| `kodegen feature <name>` | Scaffold one feature across every layer and wire it up |
| `kodegen add <capability>` | Add infrastructure: a datastore (postgres/mysql/mariadb/timescaledb/mongodb), a cache (redis), object storage (s3) or an event sink (clickhouse/influxdb/greptimedb) |
| `kodegen list` | List built-in and cached templates |
| `kodegen update <template>` | Re-clone a cached remote template |

## Install

From GitHub:

```bash
cargo install --git https://github.com/prongbang/kodegen
```

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
kodegen add redis                        # a Cache port + a Redis adapter (Valkey too)
kodegen add s3                           # a BlobStore port + an S3 adapter (RustFS/MinIO/AWS)
kodegen add clickhouse                   # an EventSink port + a ClickHouse adapter
kodegen add influxdb                     # the same port, over line protocol (InfluxDB v2/v3)
kodegen add greptimedb                   # same adapter as influxdb, GreptimeDB image + URL
kodegen add postgres --orm seaorm        # the same PostgreSQL, on SeaORM instead of Diesel
kodegen add mysql --orm seaorm           # same again on MySQL/MariaDB — no C driver to build
kodegen add postgres --path ~/svc        # run from outside the project
kodegen add postgres --dry-run           # show the plan, write nothing
kodegen add --list                       # what's supported + what this project has on
kodegen new svc --with postgres,redis,s3 # generate, then add, in one command
kodegen new svc --with postgres,redis --orm seaorm
```

| Capability | Slot | Port | Default adapter (no server) | Env |
|---|---|---|---|---|
| `postgres` | datastore | the existing `UserRepository` | in-memory repo | `DATABASE_URL` |
| `mysql` | datastore | the existing `UserRepository` | in-memory repo | `DATABASE_URL` |
| `mariadb` | datastore | the existing `UserRepository` | in-memory repo | `DATABASE_URL` |
| `timescaledb` | datastore | the existing `UserRepository` | in-memory repo | `DATABASE_URL` |
| `mongodb` | datastore | the existing `UserRepository` | in-memory repo | `MONGODB_URL` (+ `MONGODB_DB`) |
| `redis` | — (additive) | **`Cache`** (new) | in-memory cache | `REDIS_URL` |
| `s3` | — (additive) | **`BlobStore`** (new) | files under `./data/blobs` | `S3_BUCKET` (+ endpoint/credentials) |
| `clickhouse` | event sink | **`EventSink`** (new) | the tracing log | `CLICKHOUSE_URL` (+ `_DB`/`_USER`/`_PASSWORD`) |
| `influxdb` | event sink | **`EventSink`** (same) | the tracing log | `INFLUX_URL` (+ `INFLUX_BUCKET`/`_ORG`/`_TOKEN`) |
| `greptimedb` | event sink | **`EventSink`** (same) | the tracing log | `INFLUX_URL` (+ `INFLUX_BUCKET`) |

The four relational rows take `--orm diesel` (default) or `--orm seaorm`; the ORM
changes the adapter and the cargo feature, never the port or `DATABASE_URL` (see
[`--orm seaorm`](#--orm-seaorm)).

A **slot** holds exactly one adapter, so the capabilities in it are alternatives:
the four relational backends and `mongodb` are all adapters of the same
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
  first one, not applied halfway. `add redis` and `add s3` fill no slot and are
  never caught by this rule.
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
