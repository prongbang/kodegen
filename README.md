# kodegen

CLI for generating Rust starter projects and scaffolding features into them.

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
   `~/.kodegen/templates/` and reused from there (delete the cached copy to
   refresh)

No placeholders required: kodegen reads the template's own package name from
its `Cargo.toml` and rewrites it (kebab **and** snake case) to the new project
name across file contents, file names, and `Cargo.lock`. `{{project_name}}` /
`{{crate_name}}` placeholders are also supported for dedicated template repos.
`.git`, and `target` are never copied.

### Scaffold a feature

Inside a project generated from the `hexagonal` template:

```bash
kodegen feature order                    # REST (default): POST /order, GET /order/{id}
kodegen feature payment --transport grpc # tonic service: payment.PaymentApi Create/Get
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
