//! Docker sandboxes and the build/test fix loop.
//!
//! Uses the `docker` CLI. Install runs with network; tests run with `--network none`.
//! No host environment variables (and so no API keys or tokens) are passed in.
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub token_budget: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            token_budget: 500_000,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("sandbox error: {0}")]
    Sandbox(String),
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub success: bool,
    pub output: String,
}

/// Per-language build/test recipe, run inside `image`.
#[derive(Debug, Clone, Deserialize)]
pub struct Toolchain {
    pub image: String,
    /// Run with network access.
    pub install: String,
    /// Run with the network disabled.
    pub test: String,
}

fn tc(image: &str, install: &str, test: &str) -> Toolchain {
    Toolchain {
        image: image.into(),
        install: install.into(),
        test: test.into(),
    }
}

pub fn python() -> Toolchain {
    tc(
        "python:3.12-slim",
        "pip install --quiet --target .deps requests responses pytest",
        "PYTHONPATH=/work:/work/.deps python -m pytest -q -p no:cacheprovider tests",
    )
}

/// Sandbox recipe for a Port target language. Offline-friendly: the generated projects use no third-party deps.
pub fn for_target(lang: &str) -> Option<Toolchain> {
    match lang {
        "python" => Some(python()),
        "node" => Some(tc(
            "node:22-slim",
            "[ -f package.json ] && npm install --ignore-scripts --no-audit --no-fund; true",
            "node --test",
        )),
        "go" => Some(tc(
            "golang:1.23",
            "export GOMODCACHE=/work/.gocache GOFLAGS=-buildvcs=false; [ -f go.mod ] && go mod download; true",
            "export GOMODCACHE=/work/.gocache GOFLAGS=-buildvcs=false GOCACHE=/work/.gobuild; go test ./...",
        )),
        "php" => Some(php_toolchain()),
        "rust" => Some(rust_toolchain()),
        _ => None,
    }
}

const COMPOSER_ENV: &str = "export COMPOSER_ALLOW_SUPERUSER=1 COMPOSER_HOME=/tmp/composer";
const CARGO_ENV: &str = "export CARGO_HOME=/work/.cargo CARGO_TARGET_DIR=/work/target";

fn php_toolchain() -> Toolchain {
    Toolchain {
        image: "composer:2".into(),
        install: format!("{COMPOSER_ENV}; [ -f composer.json ] && composer install --no-interaction --no-progress; true"),
        test: "vendor/bin/phpunit --colors=never --do-not-cache-result".into(),
    }
}

fn rust_toolchain() -> Toolchain {
    Toolchain {
        image: "rust:1".into(),
        install: format!("{CARGO_ENV}; cargo fetch"),
        test: format!("{CARGO_ENV}; cargo test --offline"),
    }
}

/// Detect the toolchain of an existing repo. A `.sdkforge.toml` with
/// `image`, `install` and `test` keys overrides detection.
pub fn detect(dir: &Path) -> Option<Toolchain> {
    if let Ok(text) = std::fs::read_to_string(dir.join(".sdkforge.toml")) {
        if let Ok(tc) = toml::from_str::<Toolchain>(&text) {
            return Some(tc);
        }
    }
    let has = |f: &str| dir.join(f).exists();
    let lang = if has("composer.json") {
        "php"
    } else if has("go.mod") {
        "go"
    } else if has("Cargo.toml") {
        "rust"
    } else if has("package.json") {
        "node"
    } else if has("pyproject.toml") || has("setup.py") || has("requirements.txt") {
        "python"
    } else {
        return None;
    };
    detect_as(dir, lang)
}

/// The recipe for a repo known to be written in `lang` (uses the repo's own manifests where it helps).
pub fn detect_as(dir: &Path, lang: &str) -> Option<Toolchain> {
    if let Ok(text) = std::fs::read_to_string(dir.join(".sdkforge.toml")) {
        if let Ok(tc) = toml::from_str::<Toolchain>(&text) {
            return Some(tc);
        }
    }
    match lang {
        "python" => Some(tc(
            "python:3.12-slim",
            "pip install -q --target .deps pytest; \
             [ -f requirements.txt ] && pip install -q --target .deps -r requirements.txt; \
             [ -f pyproject.toml -o -f setup.py ] && pip install -q --target .deps . ; true",
            // Local source goes first so it shadows the copy installed into .deps.
            "PYTHONPATH=/work:/work/src:/work/.deps python -m pytest -q -p no:cacheprovider",
        )),
        "node" => Some(tc("node:22-slim", "npm ci || npm install", "npm test --silent")),
        "go" => Some(tc(
            "golang:1.23",
            "export GOMODCACHE=/work/.gocache GOFLAGS=-buildvcs=false; go mod download",
            "export GOMODCACHE=/work/.gocache GOFLAGS=-buildvcs=false GOCACHE=/work/.gobuild; go test ./...",
        )),
        "rust" => Some(rust_toolchain()),
        "php" => Some(php_toolchain()),
        _ => None,
    }
}

/// Paths the sandbox writes into the repo; keep them out of git.
pub const SANDBOX_ARTIFACTS: [&str; 8] = [
    ".deps",
    ".gocache",
    ".gobuild",
    ".cargo",
    "__pycache__",
    "target",
    "vendor",
    "node_modules",
];

fn docker(dir: &Path, network: &str, image: &str, script: &str) -> Result<Outcome, VerifyError> {
    let dir = dir
        .canonicalize()
        .map_err(|e| VerifyError::Sandbox(e.to_string()))?;
    let out = Command::new("docker")
        // Rust builds need far more memory than the interpreted toolchains.
        .args([
            "run",
            "--rm",
            "--network",
            network,
            "--memory",
            if image.starts_with("rust") {
                "4g"
            } else {
                "1g"
            },
            "--cpus",
            "2",
            "--pids-limit",
            "512",
        ])
        .arg("-v")
        .arg(format!("{}:/work", dir.display()))
        .args(["-w", "/work", image, "sh", "-c", script])
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .output()
        .map_err(|e| VerifyError::Sandbox(format!("cannot run docker: {e}")))?;
    let mut output = String::from_utf8_lossy(&out.stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok(Outcome {
        success: out.status.success(),
        output,
    })
}

/// Run a one-off script in `image` with the directory mounted at /work.
/// `network` lets it fetch tools; leave it off for anything that does not need to.
pub fn run_script(
    dir: &Path,
    image: &str,
    network: bool,
    script: &str,
) -> Result<Outcome, VerifyError> {
    docker(dir, if network { "bridge" } else { "none" }, image, script)
}

/// Image and script that format (and lightly lint) generated code in place, per language.
/// PHP has no ubiquitous formatter, so it has none.
pub fn format_recipe(lang: &str) -> Option<(&'static str, &'static str)> {
    match lang {
        "python" => Some((
            "python:3.12-slim",
            "pip install -q --target /tmp/t ruff && export PYTHONPATH=/tmp/t && python -m ruff format . && python -m ruff check --fix --exit-zero .",
        )),
        "node" => Some((
            "node:22-slim",
            "npx --yes prettier@3 --write '**/*.{js,mjs,cjs,ts,json,md}' '!node_modules/**'",
        )),
        "go" => Some(("golang:1.23", "gofmt -w . && GOFLAGS=-buildvcs=false go vet ./...")),
        "rust" => Some((
            "rust:1",
            "export CARGO_HOME=/work/.cargo CARGO_TARGET_DIR=/work/target; rustup component add rustfmt >/dev/null 2>&1; cargo fmt",
        )),
        _ => None,
    }
}

/// Install dependencies with network access.
pub fn install(dir: &Path, tc: &Toolchain) -> Result<Outcome, VerifyError> {
    docker(dir, "bridge", &tc.image, &tc.install)
}

/// Where the parity driver lives and how it is run, per language.
pub fn driver(lang: &str) -> (&'static str, &'static str) {
    match lang {
        "node" => ("parity/driver.js", "node parity/driver.js"),
        "go" => (
            "parity/driver/main.go",
            "export GOMODCACHE=/work/.gocache GOFLAGS=-buildvcs=false GOCACHE=/work/.gobuild; go run ./parity/driver",
        ),
        "php" => ("parity/driver.php", "php parity/driver.php"),
        "rust" => (
            "examples/parity_driver.rs",
            "export CARGO_HOME=/work/.cargo CARGO_TARGET_DIR=/work/target; cargo run --offline --quiet --example parity_driver",
        ),
        _ => ("parity/driver.py", "PYTHONPATH=/work:/work/src:/work/.deps python parity/driver.py"),
    }
}

fn docker_cli(args: &[&str]) -> Result<String, VerifyError> {
    let out = Command::new("docker")
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .output()
        .map_err(|e| VerifyError::Sandbox(format!("cannot run docker: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(VerifyError::Sandbox(format!(
            "docker {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Run an SDK driver program against the recording mock server and return what it sent.
///
/// The recorder and the driver share an `--internal` Docker network: they can reach each other
/// and nothing else, so no real network is involved. No host environment or secrets are passed in.
pub fn record_traffic(
    dir: &Path,
    tc: &Toolchain,
    driver_cmd: &str,
) -> Result<(Outcome, sdkforge_mock::Recording), VerifyError> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let id = format!(
        "{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    );
    let rec = std::env::temp_dir().join(format!("sdkforge-rec-{id}"));
    let (net, name) = (format!("sdkforge-net-{id}"), format!("sdkforge-rec-{id}"));
    let sandbox = |e: std::io::Error| VerifyError::Sandbox(e.to_string());
    std::fs::create_dir_all(&rec).map_err(sandbox)?;
    std::fs::write(rec.join("recorder.py"), sdkforge_mock::RECORDER_PY).map_err(sandbox)?;

    let result = (|| {
        let ids = |flag: &str| docker_cli_id(flag);
        docker_cli(&["network", "create", "--internal", &net])?;
        docker_cli(&[
            "run",
            "-d",
            "--name",
            &name,
            "--network",
            &net,
            "--network-alias",
            "recorder",
            "--user",
            &format!("{}:{}", ids("-u"), ids("-g")),
            "--memory",
            "256m",
            "-v",
            &format!("{}:/rec", rec.display()),
            "python:3.12-slim",
            "python",
            "/rec/recorder.py",
        ])?;
        let mut ready = false;
        for _ in 0..75 {
            if rec.join("ready").exists() {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if !ready {
            return Err(VerifyError::Sandbox("recorder did not start".into()));
        }
        let outcome = docker(
            dir,
            &net,
            &tc.image,
            &format!("export BASE_URL=http://recorder:8080; {driver_cmd}"),
        )?;
        let log = std::fs::read_to_string(rec.join("log.json")).unwrap_or_default();
        Ok((outcome, serde_json::from_str(&log).unwrap_or_default()))
    })();

    docker_cli(&["rm", "-f", &name]).ok();
    docker_cli(&["network", "rm", &net]).ok();
    std::fs::remove_dir_all(&rec).ok();
    result
}

fn docker_cli_id(flag: &str) -> String {
    Command::new("id")
        .arg(flag)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "1000".into())
}

/// Install dependencies (network on), then run tests (network off).
pub fn run_in_sandbox(dir: &Path, tc: &Toolchain) -> Result<Outcome, VerifyError> {
    let install = docker(dir, "bridge", &tc.image, &tc.install)?;
    if !install.success {
        return Ok(Outcome {
            success: false,
            output: format!("dependency install failed:\n{}", install.output),
        });
    }
    docker(dir, "none", &tc.image, &tc.test)
}

/// Run `verify`; on failure hand the output to `fix`, up to the policy's attempt cap.
/// `tokens_used` is polled so a stuck run stops at the token budget.
/// Returns the final outcome and the number of attempts made.
pub fn fix_loop(
    policy: &RetryPolicy,
    mut verify: impl FnMut() -> Result<Outcome, VerifyError>,
    mut fix: impl FnMut(&str) -> Result<(), VerifyError>,
    tokens_used: impl Fn() -> u64,
) -> Result<(Outcome, u32), VerifyError> {
    let mut attempt = 1;
    loop {
        let outcome = verify()?;
        if outcome.success || attempt >= policy.max_attempts || tokens_used() >= policy.token_budget
        {
            return Ok((outcome, attempt));
        }
        fix(&outcome.output)?;
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(ok: bool) -> Result<Outcome, VerifyError> {
        Ok(Outcome {
            success: ok,
            output: "boom".into(),
        })
    }

    #[test]
    fn stops_when_passing() {
        let mut n = 0;
        let (o, attempts) = fix_loop(
            &RetryPolicy::default(),
            || {
                n += 1;
                out(n == 3)
            },
            |_| Ok(()),
            || 0,
        )
        .unwrap();
        assert!(o.success);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn respects_attempt_cap_and_budget() {
        let p = RetryPolicy {
            max_attempts: 2,
            token_budget: 10,
        };
        let (o, attempts) = fix_loop(&p, || out(false), |_| Ok(()), || 0).unwrap();
        assert!(!o.success);
        assert_eq!(attempts, 2);
        let (_, attempts) = fix_loop(&p, || out(false), |_| Ok(()), || 99).unwrap();
        assert_eq!(attempts, 1);
    }
}
