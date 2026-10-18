//! Delivery: turn a verified SDK directory into something shippable.
//!
//! Adds a README, package metadata and CI config where the generated code has none, runs the
//! language's formatter in the sandbox, then packages the result as a zip or a fresh git repo.
//! Publishing to a registry is deliberately not done here.

use std::io::Write;
use std::path::{Path, PathBuf};

use minijinja::{context, Environment};
use sdkforge_ir::Api;
use sdkforge_verify::{format_recipe, run_script, SANDBOX_ARTIFACTS};

use crate::{Ctx, EngineError, Host, Log, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Package {
    /// Leave the files where they are.
    None,
    /// `<dir>.zip` next to the directory.
    Zip,
    /// `git init` and a first commit in the directory.
    Repo,
}

pub struct DeliverRequest {
    pub dir: PathBuf,
    pub lang: String,
    pub api: Api,
    /// Run the language formatter in the sandbox (needs Docker and network).
    pub format: bool,
    pub package: Package,
}

const README: &str = "\
# {{ name }} ({{ lang }})

{{ name }} API client, version {{ version }}.
{% if base_url %}
Default base URL: `{{ base_url }}`
{% endif %}
## Install

```
{{ install }}
```

## Operations
{% for r in resources %}
### {{ r.name }}
{% for o in r.operations %}
- `{{ o.method }} {{ o.path }}` ({{ o.id }})
{%- endfor %}
{% endfor %}
## Tests

```
{{ test }}
```

Generated with SDK Forge and verified in a network-isolated sandbox.
";

const GITIGNORE: &str = "__pycache__/\n.pytest_cache/\n";

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

fn install_and_test(lang: &str, pkg: &str) -> (String, &'static str) {
    match lang {
        "python" => (format!("pip install ./{pkg}"), "python -m pytest tests"),
        "node" => ("npm install".into(), "node --test"),
        "php" => ("composer install".into(), "vendor/bin/phpunit"),
        "rust" => ("cargo add --path .".into(), "cargo test"),
        _ => ("go get .".into(), "go test ./..."),
    }
}

fn ci(lang: &str) -> Option<&'static str> {
    let steps = match lang {
        "python" => {
            "      - uses: actions/setup-python@v5\n        with:\n          python-version: '3.12'\n      - run: pip install requests responses pytest\n      - run: python -m pytest -q tests\n"
        }
        "node" => {
            "      - uses: actions/setup-node@v4\n        with:\n          node-version: 22\n      - run: npm install --ignore-scripts\n      - run: node --test\n"
        }
        "go" => {
            "      - uses: actions/setup-go@v5\n        with:\n          go-version: stable\n      - run: go test ./...\n"
        }
        "php" => {
            "      - uses: shivammathur/setup-php@v2\n        with:\n          php-version: '8.3'\n      - run: composer install --no-interaction\n      - run: vendor/bin/phpunit\n"
        }
        "rust" => {
            "      - uses: dtolnay/rust-toolchain@stable\n      - run: cargo test\n"
        }
        _ => return None,
    };
    Some(steps)
}

/// Package metadata for languages whose generated code came without a manifest.
fn manifest(lang: &str, name: &str, version: &str) -> Option<(&'static str, String)> {
    let pkg = slug(name);
    let version = if version.is_empty() || version == "0.0.0" {
        "0.1.0"
    } else {
        version
    };
    match lang {
        "python" => Some((
            "pyproject.toml",
            format!(
                "[project]\nname = \"{pkg}\"\nversion = \"{version}\"\ndescription = \"{name} API client\"\nrequires-python = \">=3.9\"\ndependencies = [\"requests\"]\n\n[build-system]\nrequires = [\"setuptools\"]\nbuild-backend = \"setuptools.build_meta\"\n"
            ),
        )),
        "node" => Some((
            "package.json",
            format!(
                "{{\n  \"name\": \"{pkg}\",\n  \"version\": \"{version}\",\n  \"description\": \"{name} API client\",\n  \"scripts\": {{ \"test\": \"node --test\" }}\n}}\n"
            ),
        )),
        "php" => Some((
            "composer.json",
            format!(
                "{{\n  \"name\": \"sdkforge/{pkg}\",\n  \"description\": \"{name} API client\",\n  \"version\": \"{version}\",\n  \"require\": {{ \"php\": \">=8.1\" }}\n}}\n"
            ),
        )),
        "rust" => Some((
            "Cargo.toml",
            format!(
                "[package]\nname = \"{pkg}\"\nversion = \"{version}\"\nedition = \"2021\"\n"
            ),
        )),
        _ => None,
    }
}

fn write_if_missing(dir: &Path, rel: &str, content: &str, log: &Log) -> Result<()> {
    let p = dir.join(rel);
    if p.exists() {
        return Ok(());
    }
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ctx()?;
    }
    std::fs::write(&p, content).ctx()?;
    log.info(format!("added {rel}"));
    Ok(())
}

pub(crate) fn render_readme(api: &Api, lang: &str) -> Result<String> {
    let (install, test) = install_and_test(lang, &slug(&api.name));
    let mut env = Environment::new();
    env.add_template("readme", README).ctx()?;
    let resources: Vec<_> = api
        .resources
        .iter()
        .map(|r| {
            let ops: Vec<_> = r
                .operations
                .iter()
                .map(|o| context! { id => o.id, method => o.method, path => o.path })
                .collect();
            context! { name => r.name, operations => ops }
        })
        .collect();
    env.get_template("readme")
        .ctx()?
        .render(context! {
            name => api.name,
            version => api.version,
            lang => lang,
            base_url => api.base_url,
            install => install,
            test => test,
            resources => resources,
        })
        .ctx()
}

/// Returns a summary of what was delivered.
pub fn deliver(host: &dyn Host, req: &DeliverRequest) -> Result<String> {
    let log = Log::new(host, "deliver");
    let dir = &req.dir;
    if !dir.is_dir() {
        return Err(EngineError(format!("{} is not a directory", dir.display())));
    }
    let lang = req.lang.as_str();

    write_if_missing(dir, "README.md", &render_readme(&req.api, lang)?, &log)?;
    if let Some((file, body)) = manifest(lang, &req.api.name, &req.api.version) {
        write_if_missing(dir, file, &body, &log)?;
    }
    if let Some(steps) = ci(lang) {
        let wf = format!(
            "name: ci\non:\n  push:\n    branches: [main]\n  pull_request:\njobs:\n  test:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n{steps}"
        );
        write_if_missing(dir, ".github/workflows/ci.yml", &wf, &log)?;
    }
    let mut ignore = GITIGNORE.to_string();
    for a in SANDBOX_ARTIFACTS {
        ignore.push_str(&format!("{a}/\n"));
    }
    write_if_missing(dir, ".gitignore", &ignore, &log)?;

    if req.format {
        match format_recipe(lang) {
            Some((image, script)) => {
                log.info("formatting in the sandbox...");
                match run_script(dir, image, true, script) {
                    Ok(o) if o.success => log.info("formatted"),
                    Ok(o) => log.warn(format!("formatter reported problems:\n{}", o.output)),
                    Err(e) => log.warn(format!("formatter did not run: {e}")),
                }
            }
            None => log.info(format!("no formatter for {lang}; skipped")),
        }
    }

    match req.package {
        Package::None => Ok(format!("delivered in {}", dir.display())),
        Package::Repo => {
            sdkforge_repo::init_with_first_commit(dir, "feat: initial SDK generated by SDK Forge")
                .ctx()?;
            Ok(format!(
                "new git repository with a first commit: {}",
                dir.display()
            ))
        }
        Package::Zip => {
            let zip_path = zip_dir(dir)?;
            Ok(format!("zipped: {}", zip_path.display()))
        }
    }
}

/// `<dir>.zip`, skipping sandbox artifacts and git metadata.
fn zip_dir(dir: &Path) -> Result<PathBuf> {
    let dir = dir.canonicalize().ctx()?;
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("sdk")
        .to_string();
    let out = dir.with_file_name(format!("{name}.zip"));
    let file = std::fs::File::create(&out).ctx()?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();
    let mut stack = vec![dir.clone()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).ctx()? {
            let entry = entry.ctx()?;
            let path = entry.path();
            let fname = entry.file_name().to_string_lossy().into_owned();
            if fname == ".git" || SANDBOX_ARTIFACTS.contains(&fname.as_str()) {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path
                    .strip_prefix(&dir)
                    .ctx()?
                    .to_string_lossy()
                    .replace('\\', "/");
                zip.start_file(format!("{name}/{rel}"), opts).ctx()?;
                zip.write_all(&std::fs::read(&path).ctx()?).ctx()?;
            }
        }
    }
    zip.finish().ctx()?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdkforge_ir::{Operation, Resource};

    fn api() -> Api {
        Api {
            name: "Mini Pets".into(),
            version: "1.2.0".into(),
            base_url: Some("https://api.example.com".into()),
            resources: vec![Resource {
                name: "pets".into(),
                operations: vec![Operation {
                    id: "getPet".into(),
                    method: "GET".into(),
                    path: "/pets/{id}".into(),
                    params: vec![],
                    request: None,
                    responses: vec![],
                }],
            }],
            types: vec![],
        }
    }

    #[test]
    fn readme_lists_operations() {
        let r = render_readme(&api(), "python").unwrap();
        assert!(r.contains("# Mini Pets (python)"));
        assert!(r.contains("`GET /pets/{id}` (getPet)"));
        assert!(r.contains("pip install ./mini-pets"));
        assert!(r.contains("https://api.example.com"));
    }

    struct Quiet;
    impl Host for Quiet {
        fn event(&self, _: crate::Event) {}
        fn approve(&self, _: &str) -> crate::Verdict {
            crate::Verdict::Reject
        }
    }

    #[test]
    fn deliver_fills_gaps_and_zips_without_overwriting() {
        let dir = std::env::temp_dir().join(format!("sdkforge-deliver-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("mini_pets")).unwrap();
        std::fs::write(dir.join("mini_pets/__init__.py"), "x = 1\n").unwrap();
        std::fs::write(dir.join("README.md"), "mine\n").unwrap();
        let msg = deliver(
            &Quiet,
            &DeliverRequest {
                dir: dir.clone(),
                lang: "python".into(),
                api: api(),
                format: false,
                package: Package::Zip,
            },
        )
        .unwrap();
        assert!(msg.starts_with("zipped"));
        assert_eq!(
            std::fs::read_to_string(dir.join("README.md")).unwrap(),
            "mine\n"
        );
        assert!(dir.join("pyproject.toml").exists());
        assert!(dir.join(".github/workflows/ci.yml").exists());
        let zip = dir.with_extension("zip");
        assert!(zip.metadata().unwrap().len() > 0);
        std::fs::remove_dir_all(&dir).ok();
        std::fs::remove_file(zip).ok();
    }
}
