mod capability;
mod scaffold;

use clap::{Parser, Subcommand};
use include_dir::{Dir, include_dir};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Templates maintained in this repo: every directory under templates/ is one,
/// embedded into the binary at build time. No placeholders needed — the
/// template's own package name is detected and replaced at generation time.
static TEMPLATES: Dir = include_dir!("$CARGO_MANIFEST_DIR/templates");

#[derive(Parser)]
#[command(name = "kodegen", version, about = "Generate Rust starter projects")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a new project from a template
    New {
        /// Project name (used as crate name)
        name: String,
        /// Template: registry name, owner/repo, git URL, or local path
        #[arg(short, long, default_value = "hexagonal")]
        template: String,
        /// Directory to create the project in
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        /// Capability to add right after generating (e.g. --with postgres)
        #[arg(long, value_delimiter = ',')]
        with: Vec<capability::Capability>,
        /// ORM for the relational capability in --with (default: diesel)
        #[arg(long, value_enum)]
        orm: Option<capability::Orm>,
    },
    /// Add an infrastructure capability (relational backend, cache, object storage) to a generated project
    Add {
        /// Capability to add
        capability: Option<capability::Capability>,
        /// Project root
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// ORM for a relational capability: diesel (default) or seaorm
        ///
        /// Deliberately not `default_value = "diesel"`: `--orm diesel` next to
        /// `redis` is a mistake worth an error, and "not given" is the only way
        /// to tell it from a capability that simply has no ORM.
        #[arg(long, value_enum)]
        orm: Option<capability::Orm>,
        /// Print what would change, without writing anything
        #[arg(long)]
        dry_run: bool,
        /// List supported capabilities and the ones this project has on
        #[arg(long, conflicts_with_all = ["capability", "dry_run", "orm"])]
        list: bool,
    },
    /// Scaffold a feature (domain/application/infrastructure/api + wiring) into a generated project
    Feature {
        /// Feature name in snake_case (e.g. order)
        name: String,
        /// Delivery layer for the feature
        #[arg(short, long, value_enum, default_value = "rest")]
        transport: scaffold::Transport,
        /// Project root
        #[arg(short, long, default_value = ".")]
        path: PathBuf,
        /// Print what would be created and wired, without writing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-clone a cached template (registry name, owner/repo, or git URL)
    Update {
        /// Template to refresh in ~/.kodegen/templates
        template: String,
    },
    /// List registry and cached templates
    List,
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// Errors are printed with Display, not the Debug that `fn main() -> Result`
/// would use: the marker/refusal messages are multi-line manual instructions,
/// and escaped `\n`s make them unreadable exactly when they matter.
fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    match Cli::parse().cmd {
        Cmd::List => {
            for dir in TEMPLATES.dirs() {
                println!("{}  (built-in)", dir.path().display());
            }
            if let Ok(entries) = fs::read_dir(cache_root()?) {
                for e in entries.flatten().filter(|e| e.path().is_dir()) {
                    println!("{}  (cached)", e.file_name().to_string_lossy());
                }
            }
        }
        Cmd::New { name, template, output, with, orm } => {
            validate_name(&name)?;
            // checked before rendering: failing halfway through `--with` would
            // leave a project on disk that the user did not ask for — including
            // an `--orm` that no entry in the list can take
            capability::check_combination(&with, orm)?;
            let dest = output.join(&name);
            if dest.exists() {
                return Err(format!("{} already exists", dest.display()).into());
            }
            let tpl_dir = resolve_template(&template)?;
            let subs = substitutions(&tpl_dir, &name);
            render_tree(&tpl_dir, &dest, &subs)?;
            println!("Created {} from template '{template}'", dest.display());
            // --with is a loop over the same `add`, not a second code path, so
            // there is nothing here for `kodegen add` to drift away from
            let mut env: Vec<(&str, &str)> = Vec::new();
            for cap in with {
                // the ORM belongs to the one relational entry in the list; the
                // others would be refused for taking a flag they have no use for
                let (created, edited) =
                    capability::add(&dest, cap, orm.filter(|_| cap.takes_orm()), false)?;
                println!("Added capability '{}':", cap.name());
                print_plan(&created, &edited);
                env.extend_from_slice(cap.env());
            }
            // one runnable line for every capability at once — printing each
            // capability's variables on their own leaves the reader to assemble
            // a command, and half of them are useless on their own anyway
            let run = if env.is_empty() {
                "cargo run".to_string()
            } else {
                format!("{} cargo run", env_line(&env))
            };
            println!("  cd {name} && {run}");
        }
        Cmd::Add { capability: cap, path, orm, dry_run, list } => {
            if list {
                return capability::list(&path);
            }
            let cap = cap.ok_or("missing <CAPABILITY> — try `kodegen add --list`")?;
            let (created, edited) = capability::add(&path, cap, orm, dry_run)?;
            if dry_run {
                println!("Dry run — capability '{}' would be added, nothing written:", cap.name());
            } else {
                println!("Added capability '{}':", cap.name());
            }
            print_plan(&created, &edited);
            if dry_run {
                return Ok(());
            }
            let env = cap.env();
            println!("Start it and point the app at it:");
            println!("  docker compose -f docker-compose.dev.yml up -d");
            println!("  {} cargo run", env_line(env));
            // the first one is the switch: s3 needs four variables, but only the
            // bucket decides which adapter is built
            println!("Leave {} unset for the default adapter, as before.", env[0].0);
        }
        Cmd::Update { template } => {
            let (_, cached) = remote_target(&template)?;
            // a failed removal would leave resolve_template reusing the stale clone
            // and us reporting a refresh that never happened
            if let Err(e) = fs::remove_dir_all(&cached) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    return Err(format!("cannot remove cached {}: {e}", cached.display()).into());
                }
            }
            resolve_template(&template)?;
            println!("Updated {}", cached.display());
        }
        Cmd::Feature { name, transport, path, dry_run } => {
            // the plan is reported from what add_feature actually computed, so
            // --dry-run and a real run can never drift apart
            let (created, edited) = scaffold::add_feature(&path, &name, transport, dry_run)?;
            if dry_run {
                println!("Dry run — feature '{name}' would be scaffolded, nothing written:");
            } else {
                println!("Added feature '{name}':");
            }
            print_plan(&created, &edited);
            if dry_run {
                return Ok(());
            }
            match transport {
                scaffold::Transport::Rest => {
                    println!("Routes: POST /{name}, GET /{name}/{{id}} — run `cargo test` to verify")
                }
                scaffold::Transport::Grpc => println!(
                    "gRPC: {name}.{}Api Create/Get — serve with GRPC_PORT=50052, run `cargo test` to verify",
                    scaffold::pascal_case(&name)
                ),
            }
        }
    }
    Ok(())
}

/// `+` created, `~` edited — the plan exactly as the scaffolder computed it.
fn print_plan(created: &[String], edited: &[String]) {
    for f in created {
        println!("  + {f}");
    }
    for f in edited {
        println!("  ~ {f}");
    }
}

/// The capability's env vars as one shell prefix — an S3 client with three of
/// its four variables is not a client, so they are printed together.
fn env_line(env: &[(&str, &str)]) -> String {
    env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

fn cache_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".kodegen/templates"))
}

/// Built-in name -> extract the embedded copy to a temp dir. Local path -> use
/// as-is. Otherwise clone into ~/.kodegen/templates/<name> on first use
/// (owner/repo shorthand or full git URL).
fn resolve_template(template: &str) -> Result<PathBuf> {
    if let Some(embedded) = TEMPLATES.get_dir(template) {
        // A fresh directory per call. The staging tree is written and then read,
        // so a fixed path means one caller deleting or re-extracting what another
        // is still reading — two `kodegen new`s in a script, or this binary's own
        // parallel tests. pid + counter also makes the path unguessable, so
        // nothing in a world-writable /tmp can pre-create it as a symlink.
        // ponytail: one directory per call is left in /tmp for the OS to reap;
        // reach for a tempdir crate with a Drop guard only if that ever matters.
        static NTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let staged = std::env::temp_dir()
            .join(format!(
                "kodegen-{}-{}",
                std::process::id(),
                NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ))
            .join(template);
        fs::create_dir_all(&staged)?;
        // include_dir extracts relative to its root, so extract from the parent
        embedded.extract(staged.parent().unwrap())?;
        return Ok(staged);
    }

    let as_path = Path::new(template);
    if as_path.is_dir() {
        return Ok(as_path.to_path_buf());
    }

    let (url, cached) = remote_target(template)?;
    if cached.is_dir() {
        return Ok(cached);
    }

    fs::create_dir_all(cached.parent().unwrap())?;
    println!("Downloading {url} ...");
    let status = Command::new("git")
        .args(["clone", "--depth", "1"])
        .arg(&url)
        .arg(&cached)
        .status()?;
    if !status.success() {
        let _ = fs::remove_dir_all(&cached);
        return Err(format!("git clone failed for {url}").into());
    }
    let _ = fs::remove_dir_all(cached.join(".git"));
    Ok(cached)
}

/// Clone URL + cache directory for a remote template. Built-in names and local
/// paths have no cache, so they are an error here (`resolve_template` handles
/// them before it gets this far).
fn remote_target(template: &str) -> Result<(String, PathBuf)> {
    if TEMPLATES.get_dir(template).is_some() {
        return Err(format!("'{template}' is a built-in template — it ships with the binary, nothing to update").into());
    }
    if Path::new(template).is_dir() {
        return Err(format!("'{template}' is a local path — it is used in place, nothing to update").into());
    }
    let url = if template.contains("://") || template.starts_with("git@") {
        template.to_string()
    } else if template.contains('/') {
        format!("https://github.com/{template}")
    } else {
        return Err(format!("unknown template '{template}' (try `kodegen list`, owner/repo, or a git URL)").into());
    };

    // "." / ".." would escape the cache root — `update owner/..` must not be able
    // to point remove_dir_all at ~/.kodegen itself
    let cache_name = url
        .strip_suffix(".git")
        .unwrap_or(&url)
        .rsplit(['/', ':'])
        .next()
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .ok_or_else(|| format!("cannot derive a template name from '{url}'"))?;
    let cached = cache_root()?.join(cache_name);
    Ok((url, cached))
}

/// Replacements applied to file names and contents:
/// {{project_name}}/{{crate_name}} placeholders, plus the template's own
/// package name (kebab + snake), so any plain Rust repo works as a template.
fn substitutions(tpl_dir: &Path, name: &str) -> Vec<(String, String)> {
    let snake = name.replace('-', "_");
    let mut subs = vec![
        ("{{project_name}}".into(), name.to_string()),
        ("{{crate_name}}".into(), snake.clone()),
    ];
    if let Some(src) = package_name(tpl_dir).filter(|s| !s.starts_with("{{")) {
        subs.push((src.replace('-', "_"), snake));
        subs.push((src, name.to_string()));
    }
    subs
}

fn package_name(dir: &Path) -> Option<String> {
    let toml = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    toml.lines().find_map(|line| {
        line.trim()
            .strip_prefix("name")?
            .trim_start()
            .strip_prefix('=')?
            .trim()
            .strip_prefix('"')?
            .split('"')
            .next()
            .map(str::to_string)
    })
}

fn apply(s: &str, subs: &[(String, String)]) -> String {
    subs.iter().fold(s.to_string(), |acc, (from, to)| acc.replace(from, to))
}

fn render_tree(src: &Path, dst: &Path, subs: &[(String, String)]) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_name = entry.file_name().to_string_lossy().into_owned();
        if matches!(file_name.as_str(), ".git" | "target") {
            continue;
        }
        let out = dst.join(apply(&file_name, subs));
        if entry.file_type()?.is_dir() {
            render_tree(&entry.path(), &out, subs)?;
        } else {
            let bytes = fs::read(entry.path())?;
            match String::from_utf8(bytes) {
                Ok(text) => fs::write(&out, apply(&text, subs))?,
                Err(e) => fs::write(&out, e.into_bytes())?, // binary: copy as-is
            }
        }
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(format!("invalid project name '{name}': use lowercase letters, digits, '-', '_', starting with a letter").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_placeholder_and_source_name() {
        let subs = vec![
            ("{{project_name}}".into(), "my-app".into()),
            ("hexagonal_rs".into(), "my_app".into()),
            ("hexagonal-rs".into(), "my-app".into()),
        ];
        assert_eq!(apply("name = \"{{project_name}}\"", &subs), "name = \"my-app\"");
        assert_eq!(apply("use hexagonal_rs::bootstrap;", &subs), "use my_app::bootstrap;");
        assert_eq!(apply("docker build -t hexagonal-rs .", &subs), "docker build -t my-app .");
    }

    #[test]
    fn parses_package_name() {
        let dir = std::env::temp_dir().join("kodegen-pkg-name-test");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"demo-svc\"\nversion = \"0.1.0\"\n").unwrap();
        assert_eq!(package_name(&dir).as_deref(), Some("demo-svc"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn embedded_hexagonal_template_exists() {
        let toml = TEMPLATES.get_file("hexagonal/Cargo.toml").unwrap().contents_utf8().unwrap();
        assert!(toml.contains("name = \"hexagonal-rs\""));
    }

    /// The scaffolder's test fixtures carry their own copy of these markers, so
    /// only this test notices if the template loses one — which would silently
    /// downgrade every generated project to the legacy anchor path.
    #[test]
    fn embedded_template_carries_the_wiring_markers() {
        let bootstrap = TEMPLATES
            .get_file("hexagonal/src/bootstrap.rs")
            .unwrap()
            .contents_utf8()
            .unwrap();
        for marker in [
            "// kodegen:resources",
            "// kodegen:services:env",
            "// kodegen:services:default",
        ] {
            assert!(bootstrap.contains(marker), "template lost the '{marker}' marker");
        }
        let toml = TEMPLATES.get_file("hexagonal/Cargo.toml").unwrap().contents_utf8().unwrap();
        assert!(toml.contains("default = ["), "template lost the [features] default anchor");
    }

    #[test]
    fn name_validation() {
        assert!(validate_name("my-app").is_ok());
        assert!(validate_name("My App").is_err());
        assert!(validate_name("1app").is_err());
    }
}
