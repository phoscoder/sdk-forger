use std::cell::RefCell;
use std::path::{Path, PathBuf};

use sdkforge_generate::{extract_diff, patch_prompt, repro_prompt, Claude, DIFF_SYSTEM};
use sdkforge_ingest::{detect_lang, outline};
use sdkforge_repo as repo;
use sdkforge_verify::{
    detect, fix_loop, run_in_sandbox, Outcome, RetryPolicy, VerifyError, SANDBOX_ARTIFACTS,
};

use crate::{Ctx, EngineError, Host, Log, Result, Verdict};

pub struct MaintainRequest {
    /// Repo URL or local path to clone.
    pub repo: String,
    /// The error message or improvement to make.
    pub request: String,
    /// Treat the request as a bug: write a failing test first.
    pub bug: bool,
    pub workdir: Option<PathBuf>,
    /// Push the branch and open a PR after approval.
    pub push: bool,
    pub model: Option<String>,
    pub max_attempts: u32,
    pub token_budget: u64,
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().take(40) {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

/// First line of the request, cut to a commit-subject length.
fn subject(request: &str) -> String {
    let line = request.lines().next().unwrap_or("maintenance");
    match line.char_indices().nth(60) {
        Some((i, _)) => format!("{}...", line[..i].trim_end()),
        None => line.to_string(),
    }
}

/// Files that define a symbol the request names, found with the same tree-sitter outline Port uses.
/// Empty when the repo's language is not recognized.
fn symbol_files(dir: &Path, files: &[String], request: &str) -> Vec<String> {
    let Some(lang) = detect_lang(files) else {
        return Vec::new();
    };
    let flat = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_lowercase()
    };
    let tokens: Vec<String> = request
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() > 3)
        .map(str::to_lowercase)
        .collect();
    let request_flat = flat(request);
    files
        .iter()
        .filter(|f| lang.is_source_file(f))
        .take(600)
        .filter(|f| {
            let Some(src) = std::fs::read_to_string(dir.join(f))
                .ok()
                .filter(|s| s.len() < 300_000)
            else {
                return false;
            };
            let Ok(o) = outline(lang, f, &src) else {
                return false;
            };
            o.symbols.iter().any(|sym| {
                let name = flat(&sym.name);
                (name.len() >= 5 && request_flat.contains(&name))
                    || tokens.iter().any(|t| name.contains(t.as_str()))
            })
        })
        .cloned()
        .collect()
}

fn ask_diff(claude: &mut Claude, prompt: &str) -> std::result::Result<String, String> {
    let reply = claude
        .complete(DIFF_SYSTEM, prompt)
        .map_err(|e| e.to_string())?;
    extract_diff(&reply).ok_or_else(|| "the model did not return a diff".to_string())
}

/// Returns a summary line once a diff is approved and committed.
pub fn maintain(host: &dyn Host, a: &MaintainRequest) -> Result<String> {
    let log = Log::new(host, "");
    let work = a.workdir.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!(
            "sdkforge-{}-{}",
            slug(a.repo.rsplit('/').next().unwrap_or("repo")),
            std::process::id()
        ))
    });
    if work.exists() {
        return Err(EngineError(format!(
            "{} already exists; pass a different --workdir",
            work.display()
        )));
    }
    log.info(format!("cloning into {}...", work.display()));
    repo::clone(&a.repo, &work).ctx()?;
    repo::exclude_locally(&work, &SANDBOX_ARTIFACTS).ctx()?;

    let default_branch = repo::current_branch(&work).ctx()?;
    let tc = detect(&work).ok_or("could not detect the build/test setup; add a .sdkforge.toml with `image`, `install` and `test`")?;
    log.info(format!("toolchain: {} ({})", tc.image, tc.test));
    let branch = format!("sdkforge/{}", slug(&a.request));
    repo::create_branch(&work, &branch).ctx()?;

    let all = repo::tracked_files(&work).ctx()?;
    let mut claude = Claude::from_env(a.model.clone()).ctx()?;
    let symbol_hits = symbol_files(&work, &all, &a.request);
    if !symbol_hits.is_empty() {
        log.info(format!(
            "symbols in the request match {} file(s)",
            symbol_hits.len()
        ));
    }
    let relevant =
        |work: &Path| repo::select_files_boosted(work, &a.request, &all, &symbol_hits, 8, 60_000);
    if relevant(&work).is_empty() {
        log.warn("no files matched the request keywords; the model will only see the file list");
    }

    if a.bug {
        log.info("writing a failing test that reproduces the problem...");
        let diff = ask_diff(
            &mut claude,
            &repro_prompt(&a.request, &all, &relevant(&work)),
        )?;
        repo::apply_patch(&work, &diff)
            .map_err(|e| EngineError(format!("reproducing test did not apply: {e}")))?;
        match run_in_sandbox(&work, &tc) {
            Ok(o) if !o.success => log.info("reproduced: the new test fails as expected"),
            Ok(_) => {
                log.warn("the suite passes with the new test, so the problem may not be reproduced")
            }
            Err(e) => return Err(EngineError(e.to_string())),
        }
        repo::commit_all(&work, "test: reproduce reported problem").ctx()?;
    }

    let policy = RetryPolicy {
        max_attempts: a.max_attempts,
        token_budget: a.token_budget,
    };
    let claude = RefCell::new(claude);
    let mut feedback: Option<String> = None;
    for round in 1..=3 {
        log.info(format!("patching (round {round})..."));
        let apply_error: RefCell<Option<String>> = RefCell::new(None);
        let apply = |diff: std::result::Result<String, String>| {
            *apply_error.borrow_mut() = match diff {
                Ok(d) => repo::apply_patch(&work, &d)
                    .err()
                    .map(|e| format!("patch did not apply: {e}")),
                Err(e) => Some(e),
            };
        };
        apply(ask_diff(
            &mut claude.borrow_mut(),
            &patch_prompt(
                &a.request,
                &all,
                &relevant(&work),
                None,
                feedback.as_deref(),
            ),
        ));

        let result = fix_loop(
            &policy,
            || {
                if let Some(msg) = apply_error.borrow().clone() {
                    return Ok(Outcome {
                        success: false,
                        output: msg,
                    });
                }
                log.info("verifying in sandbox...");
                run_in_sandbox(&work, &tc)
            },
            |err| {
                log.warn("verification failed, asking for a new patch...");
                repo::reset_worktree(&work).map_err(|e| VerifyError::Sandbox(e.to_string()))?;
                let prompt = patch_prompt(
                    &a.request,
                    &all,
                    &relevant(&work),
                    Some(err),
                    feedback.as_deref(),
                );
                apply(ask_diff(&mut claude.borrow_mut(), &prompt));
                Ok(())
            },
            || claude.borrow().tokens_used,
        );
        match result {
            Ok((o, _)) if o.success => {}
            Ok((o, n)) => {
                log.detail(o.output);
                return Err(EngineError(format!(
                    "not verified after {n} attempt(s); flagged for review, repo left in {}",
                    work.display()
                )));
            }
            Err(e) => return Err(EngineError(e.to_string())),
        }

        let diff = repo::diff(&work).ctx()?;
        log.info("tests pass in the sandbox.");
        match host.approve(&diff) {
            Verdict::Approve => {
                repo::commit_all(&work, &format!("fix: {}", subject(&a.request))).ctx()?;
                log.info(format!(
                    "committed on branch {branch} in {}",
                    work.display()
                ));
                if a.push {
                    publish(&log, &work, &a.repo, &branch, &default_branch, &a.request)?;
                } else {
                    log.info("not pushed (pass --push to push the branch and open a PR)");
                }
                return Ok(format!("tokens used: {}", claude.borrow().tokens_used));
            }
            Verdict::Reject => {
                repo::reset_worktree(&work).ok();
                return Err("rejected; nothing committed".into());
            }
            Verdict::Feedback(f) => {
                repo::reset_worktree(&work).ctx()?;
                feedback = Some(f);
            }
        }
    }
    Err("no approved diff after 3 rounds".into())
}

fn publish(
    log: &Log,
    work: &Path,
    repo_url: &str,
    branch: &str,
    default_branch: &str,
    request: &str,
) -> Result<()> {
    let forge = repo::forge_for(repo_url);
    let remote = forge.prepare_push(work).ctx()?;
    if remote != "origin" {
        log.info(format!("no write access; pushing to your fork ({remote})"));
    }
    repo::push(work, &remote, branch, default_branch).ctx()?;
    let body = format!("{request}\n\nGenerated by SDK Forge. Tests passed in a network-isolated sandbox and the diff was approved before commit.");
    match forge.open_pull_request(work, branch, &format!("fix: {}", subject(request)), &body) {
        Ok(url) => log.info(format!("pull request: {url}")),
        Err(e) => log.warn(format!(
            "branch pushed, but the PR could not be opened: {e}"
        )),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_match_finds_the_defining_file_even_without_keyword_text() {
        let dir = std::env::temp_dir().join(format!("sdkforge-sym-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::write(
            dir.join("pkg/billing.py"),
            "class InvoiceClient:\n    def refund_invoice(self, id):\n        return id\n",
        )
        .unwrap();
        std::fs::write(dir.join("pkg/other.py"), "def helper():\n    return 1\n").unwrap();
        std::fs::write(dir.join("pyproject.toml"), "[project]\nname='x'\n").unwrap();
        let files: Vec<String> = ["pkg/billing.py", "pkg/other.py", "pyproject.toml"]
            .map(String::from)
            .to_vec();
        let hits = symbol_files(&dir, &files, "refundInvoice raises a KeyError");
        assert_eq!(hits, vec!["pkg/billing.py".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn slug_and_subject_are_short_and_safe() {
        assert_eq!(slug("Fix: the /pets endpoint!"), "fix-the-pets-endpoint");
        assert!(subject(&"x".repeat(200)).ends_with("..."));
    }
}
