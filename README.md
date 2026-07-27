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
kodegen feature order          # or: kodegen feature order --path my-service
cargo test                     # POST /order, GET /order/{id} work immediately
```

Creates one file per layer plus an integration test, then wires everything up
(mod declarations, `Services` field, route `.merge()`, bootstrap builder) and
runs `cargo fmt`:

```
src/domain/order.rs               entity + validation + repository port
src/application/order.rs          OrderService + OrderServiceImpl
src/infrastructure/order_repo.rs  in-memory adapter (Diesel: mirror build_user_service)
src/api/order.rs                  routes() + handlers + FromRef wrapper
tests/order.rs                    create → get through the real router
```

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
