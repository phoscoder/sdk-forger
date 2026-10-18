//! Port mode prompts: rebuild an existing SDK in another language, with the original's tests as the contract.
use crate::{redact, GeneratedFile};

pub const PLAN_SYSTEM: &str = "You are a senior SDK engineer porting an SDK to another language. \
Reply with a concise markdown plan only (no code files): target file layout, public type/function names in the target language's idioms, \
how errors, configuration, retries and pagination are expressed, and a mapping table from each public source symbol to its target name.";

/// Constraints that keep the generated project buildable and testable offline in the sandbox.
pub fn target_rules(lang: &str) -> &'static str {
    match lang {
        "python" => "Python 3.12. Use `requests` for HTTP. Package in its own directory with __init__.py; tests in `tests/` using pytest. \
Tests must start a local `http.server` on 127.0.0.1 in a thread (no other mocking library, no external network).",
        "node" => "Node 22, plain JavaScript (ESM or CommonJS, be consistent). No third-party dependencies: use built-in `fetch`, `node:test` and `node:assert`. \
Include package.json with `\"scripts\": {\"test\": \"node --test\"}`. Tests in `test/*.test.js`; start a local `node:http` server on 127.0.0.1 inside the tests (no external network).",
        "go" => "Go 1.23, standard library only. Include go.mod with module name `sdk`. Tests are `*_test.go` using `net/http/httptest` (no external network). \
Return errors instead of raising; use context.Context for requests.",
        "php" => "PHP 8.3 run with the `composer:2` image. Include composer.json (PSR-4 autoload `Acme\\Sdk\\` => src/, NO runtime dependencies, only `phpunit/phpunit` ^11 in require-dev) \
and phpunit.xml with a testsuite on tests/. Do HTTP with PHP streams (`file_get_contents` + `stream_context_create` with ignore_errors), not ext-curl. \
Tests are PHPUnit classes in tests/; start a local server in the tests with `proc_open` running `php -S 127.0.0.1:<free port> tests/router.php` (write that router script) and stop it in tearDown. No external network.",
        "rust" => "Rust (edition 2021, rust:1 image). Cargo.toml with these dependencies only: `ureq = \"2\"` (blocking HTTP), `serde = { version = \"1\", features = [\"derive\"] }`, `serde_json = \"1\"`. \
Library crate (src/lib.rs). Unit tests in `#[cfg(test)]` modules or integration tests in tests/; each test starts a local server with `std::net::TcpListener` on 127.0.0.1:0 in a thread (no external network). \
Use `Result<T, Error>` with a typed `Error` enum implementing `std::error::Error`; no panics on API errors.",
        _ => "",
    }
}

fn render(files: &[(String, String)]) -> String {
    files
        .iter()
        .map(|(p, c)| format!("=== SOURCE FILE: {p} ===\n{c}\n"))
        .collect()
}

fn render_generated(files: &[GeneratedFile]) -> String {
    files
        .iter()
        .map(|f| format!("=== FILE: {} ===\n{}\n", f.path, f.content))
        .collect()
}

pub struct Source<'a> {
    /// Display name of the source language, e.g. "Python".
    pub from: &'a str,
    /// Contents of the project manifest (go.mod, Cargo.toml, package.json ...), if any.
    pub manifest: &'a str,
    pub outline: &'a str,
    pub readme: &'a str,
    pub instructions: &'a str,
    pub target: &'a str,
}

fn header(s: &Source) -> String {
    format!(
        "Port an existing {} SDK to {}.\nTarget constraints: {}\nUser instructions: {}\n\nProject manifest of the source SDK:\n{}\n\nPublic API of the source SDK:\n{}\nREADME (excerpt):\n{}\n",
        s.from,
        s.target,
        target_rules(s.target),
        redact(s.instructions),
        s.manifest,
        s.outline,
        s.readme
    )
}

pub fn plan_prompt(s: &Source) -> String {
    format!("{}\nWrite the porting plan. Do not translate literally: use idiomatic names and error handling for {}.", header(s), s.target)
}

/// Tests are ported before any implementation exists; they define correct behavior.
pub fn tests_prompt(s: &Source, plan: &str, tests: &[(String, String)]) -> String {
    format!(
        "{}\nPlan:\n{plan}\n\nTranslate the original test suite below into {} tests that exercise the PLANNED API \
(names from the plan). Keep every scenario and assertion; adapt mocking to the target rules. Output only test files.\n\n{}",
        header(s),
        s.target,
        render(tests)
    )
}

pub fn module_prompt(
    s: &Source,
    plan: &str,
    tests: &[GeneratedFile],
    module: &(String, String),
) -> String {
    format!(
        "{}\nPlan:\n{plan}\n\nPorted tests (already written, must pass):\n{}\nImplement the target code for this source module, following the plan's names. \
Reuse files other modules will produce; do not duplicate shared code. Output only implementation files (no tests).\n\n{}",
        header(s),
        render_generated(tests),
        render(std::slice::from_ref(module))
    )
}

const DRIVER_CONTRACT: &str = "Contract for the driver program (it records HTTP traffic so two SDKs can be compared):\n\
- The environment variable BASE_URL is the API base URL. Construct the SDK client with that base URL, api key `test-key`, a timeout of at most 5s, and the smallest retry backoff the SDK allows (<= 10ms if configurable).\n\
- Read `parity/scenarios.json` (path relative to the working directory /work).\n\
- For each scenario, in order: POST {BASE_URL}/__scenario with JSON {\"id\": id, \"responses\": scenario.responses}; call the SDK function named in `call.function` with `call.args`; catch EVERY error; \
then POST {BASE_URL}/__outcome with {\"id\": id, \"outcome\": \"ok\" or \"error\"}. Use only the language's standard library for these two control calls (for Rust, the crates already in Cargo.toml such as serde_json and ureq are fine), never the SDK.\n\
- One failing scenario must not stop the others. Exit with status 0 at the end. Print nothing secret.";

/// A shared list of traffic scenarios both SDKs are driven through.
pub fn scenarios_prompt(s: &Source, tests: &[(String, String)]) -> String {
    format!(
        "{}\nDesign 6 to 12 HTTP traffic scenarios for comparing two implementations of this SDK. Output ONE file `parity/scenarios.json`: a JSON array of \
{{\"id\": snake_case, \"description\": str, \"call\": {{\"function\": \"<public function or Class.method of the SOURCE sdk>\", \"args\": {{name: value}}}}, \
\"responses\": [{{\"status\": int, \"headers\": {{}}, \"body\": json}}]}} where `responses` are the scripted server replies in order. \
Cover each public operation's happy path, one 4xx or 5xx error, a missing or invalid argument where the SDK validates, a 429 followed by success (retry), and pagination if present. \
Use fixed values only (no timestamps or random ids). Do NOT script anything the harness cannot compute: no hashes, signatures, HMACs, nonces or tokens in the replies. \
If the SDK verifies the integrity of responses (a signed hash for example), script only replies that do not need verifying (error replies, for instance) and cover what the SDK SENDS as thoroughly as you can. The original tests show typical usage:\n\n{}",
        header(s),
        render(tests)
    )
}

/// A driver for the original (`plan` None) or the port (`plan` Some).
pub fn driver_prompt(
    s: &Source,
    lang: &str,
    path: &str,
    scenarios: &str,
    plan: Option<&str>,
    code: &[GeneratedFile],
    tests: &[(String, String)],
) -> String {
    let who = match plan {
        Some(p) => format!(
            "the PORTED {lang} SDK, using the names in this plan:\n{p}\n\nPorted code:\n{}",
            render_generated(code)
        ),
        None => format!(
            "the ORIGINAL {lang} SDK (call it exactly as the original tests do):\n{}",
            render(tests)
        ),
    };
    format!(
        "{}{}\nWrite ONE file `{path}` in {lang} that drives {who}\n\nScenarios:\n{scenarios}\n\n{DRIVER_CONTRACT}\nOutput only that file.",
        header(s),
        if plan.is_some() { format!("Target constraints: {}\n", target_rules(lang)) } else { String::new() }
    )
}

pub fn driver_fix_prompt(path: &str, current: &str, output: &str) -> String {
    format!("This traffic driver failed to run or recorded nothing. Fix it and return the full file `{path}`.\n\n=== FILE: {path} ===\n{current}\n=== END ===\n\nOutput:\n{}", redact(output))
}

/// Every public symbol of the source should still exist in the port, under any casing.
pub fn surface_missing(names: &[String], files: &[GeneratedFile]) -> Vec<String> {
    fn norm(s: &str) -> String {
        s.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect()
    }
    let haystack = norm(
        &files
            .iter()
            .filter(|f| !is_test_path(&f.path))
            .map(|f| f.content.as_str())
            .collect::<String>(),
    );
    names
        .iter()
        // Short names ("get", "list") match almost anything, so they prove nothing.
        .filter(|n| norm(n).len() >= 5)
        .filter(|n| !haystack.contains(&norm(n)))
        .cloned()
        .collect()
}

pub(crate) fn is_test_path(p: &str) -> bool {
    p.starts_with("test")
        || p.contains("/test")
        || p.ends_with("_test.go")
        || p.contains(".test.")
        || p.ends_with("Test.php")
        || p.starts_with("parity/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, content: &str) -> GeneratedFile {
        GeneratedFile {
            path: path.into(),
            content: content.into(),
        }
    }

    #[test]
    fn surface_check_ignores_casing_and_tests() {
        let names = vec![
            "get_user".to_string(),
            "ListAllPets".to_string(),
            "retry_after".to_string(),
            "get".to_string(),
        ];
        let files = vec![
            f(
                "client.go",
                "func (c *Client) GetUser() {}\nfunc ListAllPets() {}",
            ),
            f("client_test.go", "retryAfter"),
        ];
        // retry_after only appears in a test file; `get` is too short to check.
        assert_eq!(surface_missing(&names, &files), ["retry_after"]);
    }
}
