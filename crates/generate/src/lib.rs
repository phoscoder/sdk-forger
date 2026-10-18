//! LLM prompts, per-resource chunking and the Claude Messages client.
use sdkforge_ir::{Api, Operation, Resource};
use sdkforge_planner::SdkPlan;

mod claude;
mod fidelity;
mod files;
mod maintain;
mod port;

pub use claude::{Claude, ClaudeError};
pub use fidelity::check as check_fidelity;
pub use files::{camel, pascal, snake};
pub use files::{parse_files, redact, GeneratedFile};
pub use maintain::{extract_diff, patch_prompt, repro_prompt, DIFF_SYSTEM};
pub use port::{
    driver_fix_prompt, driver_prompt, module_prompt, plan_prompt, scenarios_prompt,
    surface_missing, target_rules, tests_prompt, Source, PLAN_SYSTEM,
};

pub const SYSTEM: &str = "You are an expert SDK engineer. Output ONLY files, each as:\n\
=== FILE: relative/path ===\n<file contents>\n=== END ===\n\
No prose, no markdown fences. Only implement endpoints present in the provided API model; never invent endpoints.";

fn common(plan: &SdkPlan, instructions: &str) -> String {
    format!(
        "Language: {}\nPackage/module name: {}\nClient class: {}\nTarget constraints: {}\nUser instructions: {}\n",
        plan.language,
        plan.package,
        plan.client_name,
        target_rules(&plan.language),
        redact(instructions)
    )
}

/// Where a resource's implementation and tests live, and what its type is called.
fn resource_files(lang: &str, pkg: &str, name: &str) -> (String, String, String) {
    let (snake, pascal, camel) = (files::snake(name), files::pascal(name), files::camel(name));
    match lang {
        "node" => (
            format!("src/resources/{snake}.js"),
            format!("test/{snake}.test.js"),
            format!("{pascal}Resource"),
        ),
        "go" => (
            format!("{snake}.go"),
            format!("{snake}_test.go"),
            format!("{pascal}Service"),
        ),
        "php" => (
            format!("src/Resources/{pascal}Resource.php"),
            format!("tests/{pascal}ResourceTest.php"),
            format!("{pascal}Resource"),
        ),
        "rust" => (
            format!("src/resources/{snake}.rs"),
            format!("tests/{snake}.rs"),
            format!("{pascal}Resource"),
        ),
        _ => {
            let _ = camel;
            (
                format!("{pkg}/resources/{snake}.py"),
                format!("tests/test_{snake}.py"),
                format!("{pascal}Resource"),
            )
        }
    }
}

/// Naming and file-layout contract shared by the core call and every resource call, so separately
/// generated files fit together.
fn conventions(plan: &SdkPlan) -> String {
    let (pkg, client) = (&plan.package, &plan.client_name);
    match plan.language.as_str() {
        "node" => format!(
            "Layout: package.json (\"type\": \"module\"), src/index.js (re-exports the client and errors), src/client.js (class {client}), src/errors.js (ApiError, ValidationError), \
src/models.js, src/resources/<snake>.js (class <Pascal>Resource taking the client). Each resource is exposed as `client.<camelCase>`. \
Internal `async request(method, path, {{ query, body }} = {{}})` returns decoded JSON."
        ),
        "go" => format!(
            "Layout: go.mod (module sdk), package `sdk` at the module root: client.go (struct {client}, `New{client}(baseURL string, opts ...Option) (*{client}, error)`), errors.go (*APIError, *ValidationError), \
models.go (structs with json tags), <snake>.go per resource (type <Pascal>Service). Each service is an exported field `client.<Pascal>`. Every public method takes context.Context first. \
Internal `func (c *{client}) request(ctx context.Context, method, path string, query url.Values, body, out any) error`."
        ),
        "php" => format!(
            "Layout: composer.json, phpunit.xml, src/{client}.php (namespace Acme\\Sdk), src/Exception/ApiException.php and ValidationException.php, src/Models/*.php (readonly classes with fromArray), \
src/Resources/<Pascal>Resource.php. Each resource is a public readonly property `$client-><camelCase>`. Protected `request(string $method, string $path, array $query = [], mixed $body = null): mixed`."
        ),
        "rust" => format!(
            "Layout: Cargo.toml, src/lib.rs (declares modules, re-exports {client} and Error), src/client.rs (struct {client}; `{client}::new(base_url: &str, api_key: &str) -> Self` plus setters for timeout and max_retries), \
src/error.rs (enum Error), src/models.rs (serde structs), src/resources/mod.rs (declares every resource module) and src/resources/<snake>.rs (struct <Pascal>Resource<'a>). \
Each resource is reached through a method `client.<snake>()`. Crate-visible `fn request<T: DeserializeOwned>(&self, method: &str, path: &str, query: &[(&str, String)], body: Option<serde_json::Value>) -> Result<T, Error>`."
        ),
        _ => format!(
            "Layout: {pkg}/__init__.py (exports {client}, errors, models), {pkg}/client.py (class {client}), {pkg}/errors.py (ApiError, ValidationError), {pkg}/models.py (dataclasses with from_dict), \
{pkg}/resources/__init__.py and {pkg}/resources/<snake>.py (class <Pascal>Resource taking the client). Each resource is an attribute `client.<snake>`. \
Internal `request(method, path, params=None, json=None)` returns decoded JSON."
        ),
    }
}

/// Ask for files; if the reply has none (format slip), retry once with a reminder.
pub fn request_files(claude: &mut Claude, prompt: &str) -> Result<Vec<GeneratedFile>, ClaudeError> {
    request_files_with(claude, prompt)
}

pub fn request_files_with(
    claude: &mut Claude,
    prompt: &str,
) -> Result<Vec<GeneratedFile>, ClaudeError> {
    let files = parse_files(&claude.complete(SYSTEM, prompt)?);
    if !files.is_empty() {
        return Ok(files);
    }
    let retry = format!("{prompt}\n\nYour previous reply contained no `=== FILE: path ===` ... `=== END ===` blocks. Reply with ONLY files in exactly that format.");
    Ok(parse_files(&claude.complete(SYSTEM, &retry)?))
}

/// The slice of the IR a resource needs: the resource plus the types it names.
fn slice(api: &Api, resource: &Resource) -> String {
    let res = serde_json::to_string_pretty(resource).unwrap_or_default();
    let types: Vec<_> = api
        .types
        .iter()
        .filter(|t| res.contains(&format!("\"{}\"", t.name)))
        .collect();
    format!(
        "Resource:\n{res}\nTypes:\n{}",
        serde_json::to_string_pretty(&types).unwrap_or_default()
    )
}

/// Test cases derived mechanically from the IR, not left to the model's imagination.
pub fn derive_cases(op: &Operation) -> Vec<String> {
    let mut cases = vec![format!(
        "happy path: {} {} returns the declared success body",
        op.method, op.path
    )];
    for p in op.params.iter().filter(|p| p.required) {
        cases.push(format!(
            "missing required parameter `{}` raises a validation error before any request",
            p.name
        ));
    }
    for r in op.responses.iter().filter(|r| r.status >= 400) {
        cases.push(format!("HTTP {} maps to a typed SDK error", r.status));
    }
    cases.push("HTTP 429 then 200 is retried and succeeds".into());
    cases
}

pub fn core_prompt(api: &Api, plan: &SdkPlan, instructions: &str) -> String {
    let resources: Vec<String> = api
        .resources
        .iter()
        .map(|r| {
            let (path, _, ty) = resource_files(&plan.language, &plan.package, &r.name);
            format!("- `{}` implemented in {path} as {ty}", r.name)
        })
        .collect();
    format!(
        "{}{}\nGenerate the SDK core: project/package files, the client (API key auth, base URL {:?}, timeout, max retries), typed errors for 4xx/5xx and for client-side validation, and models for the types below. \
ALL HTTP goes through ONE internal method named `request` that takes the HTTP method and the path (e.g. request(\"GET\", \"/pets/\" + id)); that path convention is checked automatically. \
It retries with exponential backoff on 429 and 503, honouring Retry-After. Wire in these resources (other calls implement them; do NOT implement them here):\n{}\n\nTypes:\n{}",
        common(plan, instructions),
        conventions(plan),
        api.base_url,
        resources.join("\n"),
        serde_json::to_string_pretty(&api.types).unwrap_or_default(),
    )
}

pub fn chunk_prompt(api: &Api, plan: &SdkPlan, resource: &Resource, instructions: &str) -> String {
    let (impl_path, test_path, ty) = resource_files(&plan.language, &plan.package, &resource.name);
    let cases: Vec<String> = resource
        .operations
        .iter()
        .flat_map(|o| {
            derive_cases(o)
                .into_iter()
                .map(move |c| format!("- {}: {c}", o.id))
        })
        .collect();
    format!(
        "{}{}\nGenerate `{impl_path}` (type `{ty}`) implementing every operation of this resource through the client's `request` method, and `{test_path}` \
which runs against a local test server per the target constraints. Tests must cover exactly these cases:\n{}\n\n{}",
        common(plan, instructions),
        conventions(plan),
        cases.join("\n"),
        slice(api, resource),
    )
}

pub fn fix_prompt(plan: &SdkPlan, current: &[GeneratedFile], error: &str) -> String {
    let code: String = current
        .iter()
        .map(|f| format!("=== FILE: {} ===\n{}\n=== END ===\n", f.path, f.content))
        .collect();
    format!(
        "The {} SDK failed its build/tests. Fix the code (not the test expectations unless they contradict the API model). \
Return only the files you change, in full.\n\nCurrent files:\n{code}\nFailure output:\n{}",
        plan.language,
        redact(error)
    )
}
