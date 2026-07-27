//! `kodegen feature <name>`: scaffold one feature across all layers of a
//! project generated from the hexagonal template, and wire it up.
//!
//! Registration is anchor-based against the template's known structure; if an
//! anchor is missing (the project diverged), we fail with a pointer to the
//! README "Adding a Feature" checklist instead of guessing.

use std::{error::Error, fs, path::Path, process::Command};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum Transport {
    Rest,
    Grpc,
}

/// Computes the whole change in memory first (rendering every new file and
/// applying every edit, which is what validates the anchors), then writes it in
/// one phase at the end — so `dry_run` is the same code path minus the writes.
/// Returns the plan as (files created, files edited) for the caller to report.
pub fn add_feature(
    project: &Path,
    name: &str,
    transport: Transport,
    dry_run: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    validate_feature_name(name)?;
    let crate_name = crate::package_name(project)
        .ok_or("Cargo.toml with a package name not found — run inside a generated project")?
        .replace('-', "_");
    for required in [
        "src/api/mod.rs",
        "src/application/mod.rs",
        "src/domain/mod.rs",
        "src/infrastructure/mod.rs",
        "src/bootstrap.rs",
    ] {
        if !project.join(required).is_file() {
            return Err(format!(
                "{required} not found — this doesn't look like a project generated from the hexagonal template"
            )
            .into());
        }
    }
    let pascal = pascal_case(name);
    let render = |tpl: &str| {
        tpl.replace("{{feature}}", name)
            .replace("{{Feature}}", &pascal)
            .replace("{{crate_name}}", &crate_name)
    };

    // new files, one per layer (+ an integration test); domain, application and
    // infrastructure are transport-agnostic — only the delivery adapter differs
    let mut creates = vec![
        (format!("src/domain/{name}.rs"), render(DOMAIN_TPL)),
        (format!("src/application/{name}.rs"), render(APPLICATION_TPL)),
        (format!("src/infrastructure/{name}_repo.rs"), render(REPO_TPL)),
    ];
    match transport {
        Transport::Rest => {
            creates.push((format!("src/api/{name}.rs"), render(API_TPL)));
            creates.push((format!("tests/{name}.rs"), render(TEST_TPL)));
        }
        Transport::Grpc => {
            creates.push((format!("proto/{name}.proto"), render(PROTO_TPL)));
            creates.push((format!("src/api/{name}_grpc.rs"), render(API_GRPC_TPL)));
            creates.push((format!("tests/{name}.rs"), render(TEST_GRPC_TPL)));
        }
    }

    // the write phase overwrites blindly, so nothing in `creates` may exist yet —
    // this is what stops `kodegen feature api` from clobbering tests/api.rs
    if let Some((rel, _)) = creates.iter().find(|(rel, _)| project.join(rel).exists()) {
        return Err(format!("feature '{name}' already exists ({rel})").into());
    }

    // registrations
    let edits = vec![
        edit(project, "src/domain/mod.rs", |c| {
            Ok(format!("{c}\npub mod {name};\npub use {name}::{{{pascal}, {pascal}Repository}};\n"))
        })?,
        edit(project, "src/application/mod.rs", |c| {
            Ok(format!("{c}\npub mod {name};\npub use {name}::{{{pascal}Service, {pascal}ServiceImpl}};\n"))
        })?,
        edit(project, "src/infrastructure/mod.rs", |c| {
            Ok(format!("{c}\npub mod {name}_repo;\n"))
        })?,
        edit(project, "src/api/mod.rs", |c| {
            let c = match transport {
                Transport::Rest => {
                    let c = insert_pub_mod(&c, name)?;
                    insert_before_line(
                        &c,
                        ".with_state(services)",
                        &format!("        .merge({name}::routes())"),
                    )?
                }
                Transport::Grpc => insert_pub_mod(&c, &format!("{name}_grpc"))?,
            };
            insert_after_line(
                &c,
                "pub struct Services {",
                &format!("    pub {name}: Arc<dyn crate::application::{pascal}Service>,"),
            )
        })?,
        edit(project, "src/bootstrap.rs", |c| {
            let c = insert_at_marker(
                &c,
                "// kodegen:services:env",
                "build_services_from_env",
                "user: build_user_service(db.as_ref()),",
                &format!("        {name}: build_{name}_service(db.as_ref()),"),
            )?;
            let mut c = insert_at_marker(
                &c,
                "// kodegen:services:default",
                "build_services",
                "user: build_user_service(None),",
                &format!("        {name}: build_{name}_service(None),"),
            )?;
            if transport == Transport::Grpc {
                c = insert_before_line(
                    &c,
                    "routes.routes()",
                    &format!(
                        "    routes.add_service(crate::api::{name}_grpc::server(services.{name}.clone()));"
                    ),
                )?;
            }
            Ok(format!("{c}\n{}", render(BOOTSTRAP_TPL)))
        })?,
    ];

    let paths = |v: &[(String, String)]| v.iter().map(|(rel, _)| rel.clone()).collect();

    // everything above is pure, so this is the only side effect — dry-run skips it
    // wholesale and reports the very same plan
    if !dry_run {
        for (rel, content) in creates.iter().chain(&edits) {
            let path = project.join(rel);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(path, content)?;
        }

        // scaffolded line widths vary with the feature name; let rustfmt settle them
        let fmt_ok = Command::new("cargo")
            .arg("fmt")
            .current_dir(project)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !fmt_ok {
            eprintln!("warning: `cargo fmt` failed — run it manually before committing");
        }
    }
    Ok((paths(&creates), paths(&edits)))
}

/// Applies `f` to `rel`'s current content in memory (this is where anchors are
/// checked) and returns the pending write.
fn edit(project: &Path, rel: &str, f: impl FnOnce(String) -> Result<String>) -> Result<(String, String)> {
    let content = fs::read_to_string(project.join(rel)).map_err(|e| format!("{rel}: {e}"))?;
    Ok((rel.to_string(), f(content).map_err(|e| format!("{rel}: {e}"))?))
}

fn anchor_err(anchor: &str) -> String {
    format!(
        "anchor '{}' not found — the project diverged from the template; \
         wire the feature manually (see README \"Adding a Feature\")",
        anchor.trim()
    )
}

/// Insert above a `// kodegen:*` marker, keeping the marker in place for the
/// next feature. Projects generated before the markers existed fall back to the
/// line the wiring used to hang off, so `kodegen feature` keeps working on them.
fn insert_at_marker(
    content: &str,
    marker: &str,
    func: &str,
    legacy: &str,
    new_line: &str,
) -> Result<String> {
    if content.contains(marker) {
        insert_before_line(content, marker, new_line)
    } else {
        insert_after_line(content, legacy, new_line)
            .map_err(|_| marker_err(marker, func, legacy).into())
    }
}

fn marker_err(marker: &str, func: &str, legacy: &str) -> String {
    format!(
        "marker '{marker}' is missing and so is the legacy anchor '{legacy}' — \
         put the line `        {marker}` back as the last entry of the \
         `api::Services {{ ... }}` literal in `{func}()`, then rerun \
         (or wire the feature manually, see README \"Adding a Feature\")"
    )
}

fn insert_after_line(content: &str, anchor: &str, new_line: &str) -> Result<String> {
    let pos = content.find(anchor).ok_or_else(|| anchor_err(anchor))?;
    let end = content[pos..].find('\n').map(|i| pos + i + 1).unwrap_or(content.len());
    Ok(format!("{}{new_line}\n{}", &content[..end], &content[end..]))
}

fn insert_before_line(content: &str, anchor: &str, new_line: &str) -> Result<String> {
    let pos = content.find(anchor).ok_or_else(|| anchor_err(anchor))?;
    let start = content[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    Ok(format!("{}{new_line}\n{}", &content[..start], &content[start..]))
}

/// Insert `pub mod <name>;` into the leading `pub mod` block, keeping it
/// sorted so rustfmt's module reordering stays a no-op.
fn insert_pub_mod(content: &str, name: &str) -> Result<String> {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let start = lines
        .iter()
        .position(|l| l.starts_with("pub mod "))
        .ok_or_else(|| anchor_err("pub mod"))?;
    let mut end = start;
    while end < lines.len() && lines[end].starts_with("pub mod ") {
        end += 1;
    }
    lines.insert(end, format!("pub mod {name};"));
    lines[start..=end].sort();
    Ok(lines.join("\n") + "\n")
}

pub fn pascal_case(s: &str) -> String {
    s.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut chars = p.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn validate_feature_name(name: &str) -> Result<()> {
    // Rust keywords would not compile as a module name — and "mod" is doing extra
    // work here: `kodegen feature mod` would generate src/domain/mod.rs and friends
    // right on top of the layer mod files this scaffolder edits.
    const KEYWORDS: &[&str] = &[
        "as", "async", "await", "box", "break", "const", "continue", "crate", "dyn", "else",
        "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
        "move", "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while",
    ];
    let mut chars = name.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !KEYWORDS.contains(&name);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid feature name '{name}': snake_case starting with a letter, not a Rust keyword"
        )
        .into())
    }
}

const DOMAIN_TPL: &str = r##"use crate::domain::DomainError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct {{Feature}} {
    pub id: String,
    pub name: String,
}

impl {{Feature}} {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Result<Self, DomainError> {
        let id = id.into();
        let name = name.into();
        if id.trim().is_empty() || id.len() > 64 {
            return Err(DomainError::Validation("invalid id".into()));
        }
        if name.trim().is_empty() || name.len() > 256 {
            return Err(DomainError::Validation("invalid name".into()));
        }
        Ok(Self { id, name })
    }
}

#[async_trait]
pub trait {{Feature}}Repository: Send + Sync {
    /// Upsert: saving an existing `id` overwrites it. Never returns `NotFound`.
    async fn save(&self, item: {{Feature}}) -> Result<(), DomainError>;
    /// Returns `NotFound` when `id` is absent — not `Ok` with a default.
    async fn get(&self, id: &str) -> Result<{{Feature}}, DomainError>;
}
"##;

const APPLICATION_TPL: &str = r##"use crate::domain::{DomainError, {{Feature}}, {{Feature}}Repository};
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait {{Feature}}Service: Send + Sync {
    async fn create_{{feature}}(&self, id: String, name: String) -> Result<(), DomainError>;
    async fn get_{{feature}}(&self, id: String) -> Result<{{Feature}}, DomainError>;
}

pub struct {{Feature}}ServiceImpl {
    repo: Arc<dyn {{Feature}}Repository>,
}

impl {{Feature}}ServiceImpl {
    pub fn new(repo: Arc<dyn {{Feature}}Repository>) -> Self {
        Self { repo }
    }
}

#[async_trait]
impl {{Feature}}Service for {{Feature}}ServiceImpl {
    async fn create_{{feature}}(&self, id: String, name: String) -> Result<(), DomainError> {
        let item = {{Feature}}::new(id, name)?;
        self.repo.save(item).await
    }

    async fn get_{{feature}}(&self, id: String) -> Result<{{Feature}}, DomainError> {
        self.repo.get(&id).await
    }
}
"##;

const REPO_TPL: &str = r##"use crate::domain::{DomainError, {{Feature}}, {{Feature}}Repository};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Default)]
pub struct InMemory{{Feature}}Repository {
    // ponytail: std RwLock — no .await is held across the lock; swap for a DB pool in real use
    inner: RwLock<HashMap<String, {{Feature}}>>,
}

impl InMemory{{Feature}}Repository {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl {{Feature}}Repository for InMemory{{Feature}}Repository {
    async fn save(&self, item: {{Feature}}) -> Result<(), DomainError> {
        self.inner
            .write()
            .expect("lock poisoned")
            .insert(item.id.clone(), item);
        Ok(())
    }

    async fn get(&self, id: &str) -> Result<{{Feature}}, DomainError> {
        self.inner
            .read()
            .expect("lock poisoned")
            .get(id)
            .cloned()
            .ok_or(DomainError::NotFound)
    }
}
"##;

const API_TPL: &str = r##"use super::{to_http_err, ErrorRes, Services};
use crate::application::{{Feature}}Service;
use crate::domain::{{Feature}};
use axum::{
    extract::{FromRef, Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

pub fn routes() -> Router<Services> {
    Router::new()
        .route("/{{feature}}", post(create_{{feature}}))
        .route("/{{feature}}/{id}", get(get_{{feature}}))
}

#[derive(Clone)]
pub struct {{Feature}}Svc(pub Arc<dyn {{Feature}}Service>);

impl FromRef<Services> for {{Feature}}Svc {
    fn from_ref(s: &Services) -> Self {
        {{Feature}}Svc(s.{{feature}}.clone())
    }
}

#[derive(Deserialize)]
pub struct Create{{Feature}}Req {
    pub id: String,
    pub name: String,
}

pub async fn create_{{feature}}(
    State(svc): State<{{Feature}}Svc>,
    Json(req): Json<Create{{Feature}}Req>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<ErrorRes>)> {
    svc.0
        .create_{{feature}}(req.id, req.name)
        .await
        .map_err(to_http_err)?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "ok": true }))))
}

pub async fn get_{{feature}}(
    State(svc): State<{{Feature}}Svc>,
    Path(id): Path<String>,
) -> Result<Json<{{Feature}}>, (StatusCode, Json<ErrorRes>)> {
    let item = svc.0.get_{{feature}}(id).await.map_err(to_http_err)?;
    Ok(Json(item))
}
"##;

const TEST_TPL: &str = r##"use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use {{crate_name}}::bootstrap;

#[tokio::test]
async fn create_then_get_{{feature}}() {
    let app = bootstrap::build_router();

    let res = app
        .clone()
        .oneshot(
            Request::post("/{{feature}}")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"id":"x1","name":"First"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);

    let res = app
        .oneshot(
            Request::get("/{{feature}}/x1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}
"##;

const PROTO_TPL: &str = r##"syntax = "proto3";

package {{feature}};

service {{Feature}}Api {
  rpc Create (Create{{Feature}}Request) returns ({{Feature}}Reply) {}
  rpc Get (Get{{Feature}}Request) returns ({{Feature}}Reply) {}
}

message Create{{Feature}}Request {
  string id = 1;
  string name = 2;
}

message Get{{Feature}}Request {
  string id = 1;
}

message {{Feature}}Reply {
  string id = 1;
  string name = 2;
}
"##;

const API_GRPC_TPL: &str = r##"use crate::application::{{Feature}}Service;
use crate::domain::DomainError;
use std::sync::Arc;
use tonic::{Request, Response, Status};

pub mod proto {
    tonic::include_proto!("{{feature}}");
}

use proto::{{feature}}_api_server::{{Feature}}ApiServer;
use proto::{Create{{Feature}}Request, Get{{Feature}}Request, {{Feature}}Reply};

pub struct {{Feature}}Grpc(pub Arc<dyn {{Feature}}Service>);

pub fn server(svc: Arc<dyn {{Feature}}Service>) -> {{Feature}}ApiServer<{{Feature}}Grpc> {
    {{Feature}}ApiServer::new({{Feature}}Grpc(svc))
}

#[tonic::async_trait]
impl proto::{{feature}}_api_server::{{Feature}}Api for {{Feature}}Grpc {
    async fn create(
        &self,
        req: Request<Create{{Feature}}Request>,
    ) -> Result<Response<{{Feature}}Reply>, Status> {
        let r = req.into_inner();
        self.0
            .create_{{feature}}(r.id.clone(), r.name.clone())
            .await
            .map_err(to_status)?;
        Ok(Response::new({{Feature}}Reply {
            id: r.id,
            name: r.name,
        }))
    }

    async fn get(
        &self,
        req: Request<Get{{Feature}}Request>,
    ) -> Result<Response<{{Feature}}Reply>, Status> {
        let item = self
            .0
            .get_{{feature}}(req.into_inner().id)
            .await
            .map_err(to_status)?;
        Ok(Response::new({{Feature}}Reply {
            id: item.id,
            name: item.name,
        }))
    }
}

fn to_status(e: DomainError) -> Status {
    match e {
        DomainError::NotFound => Status::not_found("not found"),
        DomainError::Validation(m) => Status::invalid_argument(m),
        DomainError::Unavailable => Status::unavailable("service unavailable"),
        DomainError::Other(_) => Status::internal("internal error"),
    }
}
"##;

const TEST_GRPC_TPL: &str = r##"use tokio_stream::wrappers::TcpListenerStream;
use {{crate_name}}::api::{{feature}}_grpc::proto::{{feature}}_api_client::{{Feature}}ApiClient;
use {{crate_name}}::api::{{feature}}_grpc::proto::{Create{{Feature}}Request, Get{{Feature}}Request};
use {{crate_name}}::bootstrap;

#[tokio::test]
async fn create_then_get_{{feature}}_over_grpc() {
    let services = bootstrap::build_services();
    let routes = bootstrap::grpc_routes(&services);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_routes(routes)
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    let mut client = {{Feature}}ApiClient::connect(format!("http://{addr}"))
        .await
        .expect("connect");

    client
        .create(Create{{Feature}}Request {
            id: "x1".into(),
            name: "First".into(),
        })
        .await
        .expect("create");

    let reply = client
        .get(Get{{Feature}}Request { id: "x1".into() })
        .await
        .expect("get")
        .into_inner();
    assert_eq!(reply.name, "First");
}
"##;

const BOOTSTRAP_TPL: &str = r##"fn build_{{feature}}_service(_db: Option<&Db>) -> Arc<dyn crate::application::{{Feature}}Service> {
    // in-memory for now — mirror build_user_service to add a Diesel adapter
    Arc::new(crate::application::{{Feature}}ServiceImpl::new(Arc::new(
        crate::infrastructure::{{feature}}_repo::InMemory{{Feature}}Repository::new(),
    )))
}
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pascal_case_works() {
        assert_eq!(pascal_case("order"), "Order");
        assert_eq!(pascal_case("order_item"), "OrderItem");
    }

    #[test]
    fn feature_name_validation() {
        assert!(validate_feature_name("order").is_ok());
        assert!(validate_feature_name("order_item2").is_ok());
        assert!(validate_feature_name("Order").is_err());
        assert!(validate_feature_name("order-item").is_err());
        assert!(validate_feature_name("type").is_err());
    }

    /// Smallest tree that carries every anchor `add_feature` looks for.
    fn fixture(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("kodegen-scaffold-{tag}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        // edition included so the `cargo fmt` a real run shells out to stays quiet
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo-svc\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        for layer in ["domain", "application", "infrastructure"] {
            fs::create_dir_all(root.join("src").join(layer)).unwrap();
            fs::write(root.join(format!("src/{layer}/mod.rs")), "pub mod user;\n").unwrap();
        }
        fs::create_dir_all(root.join("src/api")).unwrap();
        fs::write(
            root.join("src/api/mod.rs"),
            "pub mod user;\n\npub struct Services {\n    pub user: Arc<dyn crate::application::UserService>,\n}\n\nfn router(services: Services) -> Router {\n    Router::new()\n        .with_state(services)\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/bootstrap.rs"),
            "fn a() -> Services {\n    Services {\n        user: build_user_service(db.as_ref()),\n    }\n}\nfn b() -> Services {\n    Services {\n        user: build_user_service(None),\n    }\n}\nfn c() {\n    routes.routes()\n}\n",
        )
        .unwrap();
        root
    }

    /// Same tree, but carrying the `// kodegen:*` markers the current template
    /// ships — `fixture` stays marker-less on purpose so every other test here
    /// exercises the legacy fallback.
    fn fixture_with_markers(tag: &str) -> std::path::PathBuf {
        let root = fixture(tag);
        fs::write(
            root.join("src/bootstrap.rs"),
            "fn a() -> Services {\n    Services {\n        user: build_user_service(db.as_ref()),\n        // kodegen:services:env\n    }\n}\nfn b() -> Services {\n    Services {\n        user: build_user_service(None),\n        // kodegen:services:default\n    }\n}\nfn c() {\n    routes.routes()\n}\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn markers_take_precedence_and_survive_repeat_runs() {
        let root = fixture_with_markers("markers");
        add_feature(&root, "order", Transport::Rest, false).unwrap();
        add_feature(&root, "cart", Transport::Rest, false).unwrap();
        let out = fs::read_to_string(root.join("src/bootstrap.rs")).unwrap();
        assert!(
            out.contains(
                "        order: build_order_service(db.as_ref()),\n        cart: build_cart_service(db.as_ref()),\n        // kodegen:services:env"
            ),
            "fields did not stack above the marker: {out}"
        );
        assert!(
            out.contains(
                "        order: build_order_service(None),\n        cart: build_cart_service(None),\n        // kodegen:services:default"
            ),
            "{out}"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn falls_back_to_the_legacy_anchor_when_the_marker_is_absent() {
        let out = insert_at_marker(
            "    Services {\n        user: build_user_service(None),\n    }\n",
            "// kodegen:services:default",
            "build_services",
            "user: build_user_service(None),",
            "        order: build_order_service(None),",
        )
        .unwrap();
        assert!(
            out.contains("user: build_user_service(None),\n        order: build_order_service(None),"),
            "{out}"
        );
    }

    #[test]
    fn missing_marker_error_names_the_marker_and_the_line_to_restore() {
        let err = insert_at_marker(
            "fn nothing() {}\n",
            "// kodegen:services:env",
            "build_services_from_env",
            "user: build_user_service(db.as_ref()),",
            "        order: build_order_service(db.as_ref()),",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("// kodegen:services:env"), "{err}");
        assert!(err.contains("put the line"), "{err}");
        assert!(err.contains("build_services_from_env()"), "{err}");
        assert!(err.contains("user: build_user_service(db.as_ref()),"), "{err}");
    }

    fn snapshot(root: &Path) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(dir).unwrap().flatten() {
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                } else {
                    let rel = e.path().strip_prefix(root).unwrap().display().to_string();
                    out.push((rel, fs::read_to_string(e.path()).unwrap()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn dry_run_writes_nothing() {
        for (tag, transport) in [("rest", Transport::Rest), ("grpc", Transport::Grpc)] {
            let root = fixture(tag);
            let before = snapshot(&root);
            add_feature(&root, "order", transport, true).unwrap();
            assert_eq!(before, snapshot(&root), "{tag}: dry run touched the tree");
            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn dry_run_plan_matches_what_a_real_run_creates() {
        let root = fixture("plan");
        let before: Vec<String> = snapshot(&root).into_iter().map(|(rel, _)| rel).collect();
        let planned = add_feature(&root, "order", Transport::Rest, true).unwrap();
        assert_eq!(planned, add_feature(&root, "order", Transport::Rest, false).unwrap());

        let mut appeared: Vec<String> = snapshot(&root)
            .into_iter()
            .map(|(rel, _)| rel)
            .filter(|rel| !before.contains(rel))
            .collect();
        appeared.sort();
        let mut promised = planned.0;
        promised.sort();
        assert_eq!(appeared, promised);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn refuses_to_overwrite_an_existing_file() {
        let root = fixture("collide");
        fs::create_dir_all(root.join("tests")).unwrap();
        fs::write(root.join("tests/api.rs"), "// hand-written\n").unwrap();
        let err = add_feature(&root, "api", Transport::Rest, true).unwrap_err().to_string();
        assert!(err.contains("already exists (tests/api.rs)"), "unexpected error: {err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn dry_run_still_validates_anchors() {
        let root = fixture("no-anchor");
        fs::write(root.join("src/bootstrap.rs"), "fn nothing() {}\n").unwrap();
        let err = add_feature(&root, "order", Transport::Rest, true).unwrap_err().to_string();
        assert!(err.contains("anchor"), "unexpected error: {err}");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn pub_mod_block_stays_sorted() {
        let out = insert_pub_mod("pub mod hello;\npub mod user;\n\nfn x() {}\n", "order").unwrap();
        assert!(out.starts_with("pub mod hello;\npub mod order;\npub mod user;\n"));
    }
}
