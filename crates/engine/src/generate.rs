use std::path::{Path, PathBuf};

use sdkforge_generate::{
    check_fidelity, chunk_prompt, core_prompt, fix_prompt, request_files, request_files_with,
    Claude, GeneratedFile,
};
use sdkforge_ingest::Lang;
use sdkforge_ir::Api;
use sdkforge_verify::{fix_loop, for_target, run_in_sandbox, Outcome, RetryPolicy, VerifyError};

use crate::deliver::{deliver, DeliverRequest, Package};
use crate::{approve_and_copy, merge, write_files, Ctx, EngineError, Host, Log, Result, Staging};

pub struct GenerateRequest {
    pub spec: String,
    pub instructions: String,
    /// Language names; several run in parallel into `<out>/<lang>`.
    pub langs: Vec<String>,
    pub out: PathBuf,
    /// Parse and plan only; no model calls, no Docker.
    pub dry_run: bool,
    pub model: Option<String>,
    pub max_attempts: u32,
    pub token_budget: u64,
    /// Verify in a scratch directory and ask the host before writing to `out`.
    pub require_approval: bool,
    /// Reuse the model's earlier output for resources whose prompt has not changed.
    pub use_cache: bool,
    /// After a verified run: add README/metadata/CI and package the result. None skips delivery.
    pub deliver: Option<Delivery>,
}

pub struct Delivery {
    pub format: bool,
    pub package: Package,
}

/// The outcome for one language: a summary on success, or why it failed.
pub struct LangResult {
    pub lang: Lang,
    pub result: std::result::Result<String, String>,
}

pub fn generate(host: &dyn Host, req: &GenerateRequest) -> Result<Vec<LangResult>> {
    let log = Log::new(host, "");
    let mut targets: Vec<Lang> = Vec::new();
    for name in &req.langs {
        let l = Lang::parse(name).ok_or_else(|| {
            EngineError(format!(
                "unknown language `{name}`; use python, node, php, rust or go"
            ))
        })?;
        if !targets.contains(&l) {
            targets.push(l);
        }
    }
    let api = sdkforge_ingest::ingest_path(Path::new(&req.spec)).ctx()?;

    log.info(format!(
        "{} v{} ({} resources, {} types)",
        api.name,
        api.version,
        api.resources.len(),
        api.types.len()
    ));
    for r in &api.resources {
        log.info(format!("  {}", r.name));
        for o in &r.operations {
            log.info(format!("    {:<6} {}  [{}]", o.method, o.path, o.id));
        }
    }
    if req.dry_run {
        for l in &targets {
            let plan = sdkforge_planner::plan(&api, l.name());
            log.info(format!("plan ({}): {} files", l.name(), plan.files.len()));
            plan.files.iter().for_each(|f| log.info(format!("  {f}")));
        }
        return Ok(Vec::new());
    }

    let many = targets.len() > 1;
    std::thread::scope(|scope| {
        let handles: Vec<_> = targets
            .iter()
            .map(|&lang| {
                let api = &api;
                let dir = if many {
                    req.out.join(lang.name())
                } else {
                    req.out.clone()
                };
                let log = Log::new(host, if many { lang.name() } else { "" });
                scope.spawn(move || LangResult {
                    lang,
                    result: generate_one(api, lang, req, &dir, &log),
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .map_err(|_| EngineError("a generator thread panicked".into()))
            })
            .collect()
    })
}

/// One language, start to finish. Errors are returned (not fatal) so other languages can finish.
fn generate_one(
    api: &Api,
    lang: Lang,
    req: &GenerateRequest,
    final_out: &Path,
    log: &Log,
) -> std::result::Result<String, String> {
    let stage = if req.require_approval {
        Some(Staging::new(lang.name()).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let out: &Path = stage.as_ref().map_or(final_out, Staging::path);
    let instructions = req.instructions.as_str();
    let plan = sdkforge_planner::plan(api, lang.name());
    let tc = for_target(lang.name()).ok_or("no sandbox recipe for that language")?;
    let mut claude = Claude::from_env(req.model.clone()).map_err(|e| e.to_string())?;
    let mut files: Vec<GeneratedFile> = Vec::new();

    log.info("generating core...");
    merge(
        &mut files,
        request_files(&mut claude, &core_prompt(api, &plan, instructions))
            .map_err(|e| e.to_string())?,
    );
    let chunks = generate_resources(api, &plan, req, instructions, log, &mut claude)?;
    for chunk in chunks {
        merge(&mut files, chunk);
    }
    if files.is_empty() {
        return Err("the model returned no files".into());
    }
    write_files(out, &files).map_err(|e| e.to_string())?;

    let policy = RetryPolicy {
        max_attempts: req.max_attempts,
        token_budget: req.token_budget,
    };
    let claude = std::cell::RefCell::new(claude);
    let files = std::cell::RefCell::new(files);
    let result = fix_loop(
        &policy,
        || {
            let problems = check_fidelity(api, lang.name(), &files.borrow());
            if !problems.is_empty() {
                log.warn("spec fidelity check failed");
                let output = format!(
                    "Spec fidelity check failed. Remove or fix paths not in the API model:\n{}",
                    problems.join("\n")
                );
                return Ok(Outcome {
                    success: false,
                    output,
                });
            }
            log.info("verifying in sandbox...");
            run_in_sandbox(out, &tc)
        },
        |err| {
            log.warn("verification failed, asking the model for a fix...");
            let fixed = request_files_with(
                &mut claude.borrow_mut(),
                &fix_prompt(&plan, &files.borrow(), err),
            )
            .map_err(|e| VerifyError::Sandbox(e.to_string()))?;
            write_files(out, &fixed).map_err(|e| VerifyError::Sandbox(e.to_string()))?;
            merge(&mut files.borrow_mut(), fixed);
            Ok(())
        },
        || claude.borrow().tokens_used,
    );
    let tokens = claude.borrow().tokens_used;
    match result {
        Ok((o, attempts)) if o.success => {
            if stage.is_some() {
                approve_and_copy(log.host(), out, final_out).map_err(|e| e.to_string())?;
            }
            let mut msg = format!(
                "verified after {attempts} attempt(s), {tokens} tokens: {}",
                final_out.display()
            );
            if let Some(d) = &req.deliver {
                let delivered = deliver(
                    log.host(),
                    &DeliverRequest {
                        dir: final_out.to_path_buf(),
                        lang: lang.name().to_string(),
                        api: api.clone(),
                        format: d.format,
                        package: d.package.clone(),
                    },
                )
                .map_err(|e| format!("verified, but delivery failed: {e}"))?;
                msg.push_str(&format!("\n{delivered}"));
            }
            Ok(msg)
        }
        Ok((o, attempts)) => {
            // Unverified work is never silently dropped: leave it where the user can review it.
            if stage.is_some() {
                crate::copy_tree(out, final_out).map_err(|e| e.to_string())?;
            }
            Err(format!(
            "not verified after {attempts} attempt(s); flagged for review, output left in {}\n{}",
            final_out.display(),
            o.output
        ))
        }
        Err(e) => Err(e.to_string()),
    }
}

type Chunk = std::result::Result<Vec<GeneratedFile>, String>;

/// How many resources are requested from the model at once.
const PARALLEL_RESOURCES: usize = 4;

/// Stable 64-bit FNV-1a, so cache keys survive compiler upgrades.
fn fnv(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0xff)) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

fn cache_path(prompt: &str, model: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("HOME")?).join(".cache")))?;
    Some(
        base.join("sdkforge")
            .join(format!("{:016x}.json", fnv(&[prompt, model]))),
    )
}

fn cache_read(path: &Path) -> Option<Vec<GeneratedFile>> {
    let pairs: Vec<(String, String)> = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some(
        pairs
            .into_iter()
            .map(|(path, content)| GeneratedFile { path, content })
            .collect(),
    )
}

fn cache_write(path: &Path, files: &[GeneratedFile]) {
    let pairs: Vec<(&str, &str)> = files
        .iter()
        .map(|f| (f.path.as_str(), f.content.as_str()))
        .collect();
    if let (Some(dir), Ok(json)) = (path.parent(), serde_json::to_vec(&pairs)) {
        std::fs::create_dir_all(dir).ok();
        std::fs::write(path, json).ok();
    }
}

/// One model call per resource, up to [`PARALLEL_RESOURCES`] at a time, in resource order.
/// Unchanged resources come from the cache. Tokens spent are added to `claude`.
fn generate_resources(
    api: &Api,
    plan: &sdkforge_planner::SdkPlan,
    req: &GenerateRequest,
    instructions: &str,
    log: &Log,
    claude: &mut Claude,
) -> std::result::Result<Vec<Vec<GeneratedFile>>, String> {
    use std::sync::Mutex;
    let next = Mutex::new(0usize);
    let results: Mutex<Vec<Option<Chunk>>> =
        Mutex::new((0..api.resources.len()).map(|_| None).collect());
    let spent = std::sync::atomic::AtomicU64::new(0);
    let workers = PARALLEL_RESOURCES.min(api.resources.len().max(1));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = {
                    let mut n = next.lock().unwrap();
                    let i = *n;
                    *n += 1;
                    i
                };
                let Some(r) = api.resources.get(i) else {
                    break;
                };
                let prompt = chunk_prompt(api, plan, r, instructions);
                let model = req.model.clone().unwrap_or_default();
                let cache = req.use_cache.then(|| cache_path(&prompt, &model)).flatten();
                let outcome = match cache.as_deref().and_then(cache_read) {
                    Some(files) => {
                        log.info(format!("{}: unchanged, reusing earlier output", r.name));
                        Ok(files)
                    }
                    None => {
                        log.info(format!("generating {}...", r.name));
                        Claude::from_env(req.model.clone())
                            .map_err(|e| e.to_string())
                            .and_then(|mut c| {
                                let files =
                                    request_files(&mut c, &prompt).map_err(|e| e.to_string());
                                spent
                                    .fetch_add(c.tokens_used, std::sync::atomic::Ordering::Relaxed);
                                files
                            })
                            .inspect(|files| {
                                if let Some(p) = &cache {
                                    cache_write(p, files);
                                }
                            })
                    }
                };
                results.lock().unwrap()[i] = Some(outcome);
            });
        }
    });
    claude.tokens_used += spent.into_inner();
    results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err("a resource was not generated".into())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_changes_with_prompt_and_model() {
        assert_eq!(fnv(&["a", "b"]), fnv(&["a", "b"]));
        assert_ne!(fnv(&["a", "b"]), fnv(&["a", "c"]));
        assert_ne!(fnv(&["ab", ""]), fnv(&["a", "b"]));
    }

    #[test]
    fn cache_round_trips_files() {
        let path = std::env::temp_dir()
            .join(format!("sdkforge-cache-{}", std::process::id()))
            .join("k.json");
        let files = vec![GeneratedFile {
            path: "a/b.py".into(),
            content: "x = 1\n".into(),
        }];
        cache_write(&path, &files);
        let back = cache_read(&path).unwrap();
        assert_eq!(
            (back[0].path.as_str(), back[0].content.as_str()),
            ("a/b.py", "x = 1\n")
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
        assert!(cache_read(&path).is_none());
    }
}
