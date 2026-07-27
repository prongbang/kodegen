# kodegen

CLI for generating Rust starter projects, scaffolding features into them, and
switching their infrastructure capabilities.

| Command | What it does |
|---|---|
| `kodegen new <name>` | Generate a project from a template |
| `kodegen feature <name>` | Scaffold one feature across every layer and wire it up |
| `kodegen add <capability>` | Switch the project's relational backend (postgres/mysql/mariadb) |
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
kodegen add postgres --path ~/svc        # run from outside the project
kodegen add postgres --dry-run           # show the plan, write nothing
kodegen add --list                       # what's supported + what this project has on
kodegen new svc --with postgres          # generate, then add, in one command
```

`--with` takes a single capability today: every capability is a relational
backend and a project can only have one, so a longer list is rejected before
`new` writes anything.

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
  steps to undo the first one, not applied halfway.
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
compose image and the wording differ.

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
