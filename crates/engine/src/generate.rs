use std::path::{Path, PathBuf};

use sdkforge_generate::{
    check_fidelity, chunk_prompt, core_prompt, fix_prompt, request_files, request_files_with,
    Claude, GeneratedFile,
};
use sdkforge_ingest::Lang;
use sdkforge_ir::Api;
use sdkforge_verify::{fix_loop, for_target, run_in_sandbox, Outcome, RetryPolicy, VerifyError};

use crate::deliver::{deliver, DeliverRequest, Package};
use crate::{merge, write_files, Ctx, EngineError, Host, Log, Result};

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
    out: &Path,
    log: &Log,
) -> std::result::Result<String, String> {
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
    for r in &api.resources {
        log.info(format!("generating {}...", r.name));
        merge(
            &mut files,
            request_files(&mut claude, &chunk_prompt(api, &plan, r, instructions))
                .map_err(|e| e.to_string())?,
        );
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
            let mut msg = format!(
                "verified after {attempts} attempt(s), {tokens} tokens: {}",
                out.display()
            );
            if let Some(d) = &req.deliver {
                let delivered = deliver(
                    log.host(),
                    &DeliverRequest {
                        dir: out.to_path_buf(),
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
        Ok((o, attempts)) => Err(format!(
            "not verified after {attempts} attempt(s); flagged for review, output left in {}\n{}",
            out.display(),
            o.output
        )),
        Err(e) => Err(e.to_string()),
    }
}
