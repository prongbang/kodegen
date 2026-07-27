# kodegen

CLI for generating Rust starter projects, scaffolding features into them, and
switching their infrastructure capabilities.

| Command | What it does |
|---|---|
| `kodegen new <name>` | Generate a project from a template |
| `kodegen feature <name>` | Scaffold one feature across every layer and wire it up |
| `kodegen add <capability>` | Add infrastructure: a relational backend (postgres/mysql/mariadb), a cache (redis) or object storage (s3) |
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
kodegen add redis                        # a Cache port + a Redis adapter (Valkey too)
kodegen add s3                           # a BlobStore port + an S3 adapter (RustFS/MinIO/AWS)
kodegen add postgres --path ~/svc        # run from outside the project
kodegen add postgres --dry-run           # show the plan, write nothing
kodegen add --list                       # what's supported + what this project has on
kodegen new svc --with postgres,redis,s3 # generate, then add, in one command
```

| Capability | Kind | Port | Default adapter (no server) | Env |
|---|---|---|---|---|
| `postgres` | relational | the existing `UserRepository` | in-memory repo | `DATABASE_URL` |
| `mysql` | relational | the existing `UserRepository` | in-memory repo | `DATABASE_URL` |
| `mariadb` | relational | the existing `UserRepository` | in-memory repo | `DATABASE_URL` |
| `redis` | cache | **`Cache`** (new) | in-memory cache | `REDIS_URL` |
| `s3` | object storage | **`BlobStore`** (new) | files under `./data/blobs` | `S3_BUCKET` (+ endpoint/credentials) |

`--with` takes at most one **relational** backend — a project can only have one,
so a second is rejected before `new` writes anything — but capabilities on
different axes combine freely (`--with postgres,redis,s3`).

`add postgres` rewrites the six things that actually differ between dialects:

```
Cargo.toml                              default = ["sqlite"] -> ["postgres"]
src/infrastructure/diesel_db.rs         SqliteConnection -> PgConnection (whole file replaced)
src/infrastructure/diesel_repo.rs       save() -> the dialect's upsert; test module replaced
migrations/*_create_users/up.sql        the dialect's CREATE TABLE
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

- **One relational backend per project.** The `Db` handle wraps a single
  connection type, so `add mysql` on top of `postgres` is refused with the
  steps to undo the first one, not applied halfway. `add redis` and `add s3`
  are not backends and are never caught by this rule.
- **Adding the same capability twice** is an error, not a no-op rewrite.

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
compose image and the wording differ. Valkey stands in the same relation to
`redis`: same RESP protocol, same client, same generated Rust — swap the image
in `docker-compose.dev.yml` and nothing else changes. So do MinIO and Ceph to
`s3`: one API, one adapter, one endpoint variable.

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
