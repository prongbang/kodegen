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
    },
    /// List registry and cached templates
    List,
}

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() -> Result<()> {
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
        Cmd::New { name, template, output } => {
            validate_name(&name)?;
            let dest = output.join(&name);
            if dest.exists() {
                return Err(format!("{} already exists", dest.display()).into());
            }
            let tpl_dir = resolve_template(&template)?;
            let subs = substitutions(&tpl_dir, &name);
            render_tree(&tpl_dir, &dest, &subs)?;
            println!("Created {} from template '{template}'", dest.display());
            println!("  cd {name} && cargo run");
        }
        Cmd::Feature { name, transport, path } => {
            scaffold::add_feature(&path, &name, transport)?;
            let delivery = match transport {
                scaffold::Transport::Rest => format!("src/api/{name}.rs"),
                scaffold::Transport::Grpc => format!("proto/{name}.proto + src/api/{name}_grpc.rs"),
            };
            println!("Added feature '{name}':");
            for f in [
                format!("src/domain/{name}.rs"),
                format!("src/application/{name}.rs"),
                format!("src/infrastructure/{name}_repo.rs"),
                delivery,
                format!("tests/{name}.rs"),
            ] {
                println!("  + {f}");
            }
            println!("  ~ wired into api/mod.rs, bootstrap.rs and layer mod files");
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

fn cache_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join(".kodegen/templates"))
}

/// Built-in name -> extract the embedded copy to a temp dir. Local path -> use
/// as-is. Otherwise clone into ~/.kodegen/templates/<name> on first use
/// (owner/repo shorthand or full git URL).
fn resolve_template(template: &str) -> Result<PathBuf> {
    if let Some(embedded) = TEMPLATES.get_dir(template) {
        let staged = std::env::temp_dir().join("kodegen").join(template);
        let _ = fs::remove_dir_all(&staged);
        fs::create_dir_all(&staged)?;
        // include_dir extracts relative to its root, so extract from the parent
        embedded.extract(staged.parent().unwrap())?;
        return Ok(staged);
    }

    let as_path = Path::new(template);
    if as_path.is_dir() {
        return Ok(as_path.to_path_buf());
    }

    let url = if template.contains("://") || template.starts_with("git@") {
        template.to_string()
    } else if template.contains('/') {
        format!("https://github.com/{template}")
    } else {
        return Err(format!("unknown template '{template}' (try `kodegen list`, owner/repo, or a git URL)").into());
    };

    let cache_name = url
        .trim_end_matches(".git")
        .rsplit(['/', ':'])
        .next()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("cannot derive a template name from '{url}'"))?
        .to_string();
    let cached = cache_root()?.join(&cache_name);
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

    #[test]
    fn name_validation() {
        assert!(validate_name("my-app").is_ok());
        assert!(validate_name("My App").is_err());
        assert!(validate_name("1app").is_err());
    }
}
