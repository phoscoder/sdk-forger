//! Builds the SDK plan (file layout, class names, error types) once per run.
use sdkforge_ir::Api;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SdkPlan {
    pub language: String,
    pub package: String,
    pub client_name: String,
    pub files: Vec<String>,
}

fn snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

fn pascal(s: &str) -> String {
    snake(s)
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

/// The file layout for `language` ("python", "node", "go", "php" or "rust").
pub fn plan(api: &Api, language: &str) -> SdkPlan {
    let package = snake(&api.name);
    let client_name = format!("{}Client", api.name.split_whitespace().collect::<String>());
    let mut files: Vec<String> = Vec::new();
    let mut add = |f: String| files.push(f);
    match language {
        "node" => {
            for f in [
                "package.json",
                "src/index.js",
                "src/client.js",
                "src/errors.js",
                "src/models.js",
            ] {
                add(f.into());
            }
            for r in &api.resources {
                add(format!("src/resources/{}.js", snake(&r.name)));
                add(format!("test/{}.test.js", snake(&r.name)));
            }
        }
        "go" => {
            for f in ["go.mod", "client.go", "errors.go", "models.go"] {
                add(f.into());
            }
            for r in &api.resources {
                add(format!("{}.go", snake(&r.name)));
                add(format!("{}_test.go", snake(&r.name)));
            }
        }
        "php" => {
            for f in [
                "composer.json",
                "phpunit.xml",
                "tests/router.php",
                "src/Exception/ApiException.php",
                "src/Exception/ValidationException.php",
            ] {
                add(f.into());
            }
            add(format!("src/{client_name}.php"));
            for r in &api.resources {
                add(format!("src/Resources/{}Resource.php", pascal(&r.name)));
                add(format!("tests/{}ResourceTest.php", pascal(&r.name)));
            }
        }
        "rust" => {
            for f in [
                "Cargo.toml",
                "src/lib.rs",
                "src/client.rs",
                "src/error.rs",
                "src/models.rs",
                "src/resources/mod.rs",
            ] {
                add(f.into());
            }
            for r in &api.resources {
                add(format!("src/resources/{}.rs", snake(&r.name)));
                add(format!("tests/{}.rs", snake(&r.name)));
            }
        }
        _ => {
            for f in [
                "__init__.py",
                "client.py",
                "errors.py",
                "models.py",
                "resources/__init__.py",
            ] {
                add(format!("{package}/{f}"));
            }
            for r in &api.resources {
                add(format!("{package}/resources/{}.py", snake(&r.name)));
                add(format!("tests/test_{}.py", snake(&r.name)));
            }
        }
    }
    SdkPlan {
        language: language.into(),
        package,
        client_name,
        files,
    }
}
