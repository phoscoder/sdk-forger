use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use sdkforge_generate::{extract_diff, patch_prompt, repro_prompt, Claude, DIFF_SYSTEM};
use sdkforge_repo as repo;
use sdkforge_verify::{detect, fix_loop, run_in_sandbox, Outcome, RetryPolicy, VerifyError, SANDBOX_ARTIFACTS};

use crate::die;

pub struct Args {
    pub repo: String,
    pub request: String,
    pub bug: bool,
    pub workdir: Option<PathBuf>,
    pub push: bool,
    pub model: Option<String>,
    pub max_attempts: u32,
    pub token_budget: u64,
}

enum Verdict {
    Approve,
    Reject,
    Feedback(String),
}

fn ask_approval() -> Verdict {
    print!("Approve and commit to a new branch? [y = yes, n = abort, or type feedback]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    match line.trim() {
        "y" | "Y" | "yes" => Verdict::Approve,
        "" | "n" | "N" | "no" => Verdict::Reject,
        other => Verdict::Feedback(other.to_string()),
    }
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

fn ask_diff(claude: &mut Claude, prompt: &str) -> Result<String, String> {
    let reply = claude.complete(DIFF_SYSTEM, prompt).map_err(|e| e.to_string())?;
    extract_diff(&reply).ok_or_else(|| "the model did not return a diff".to_string())
}

pub fn run(a: Args) {
    let work = a.workdir.unwrap_or_else(|| std::env::temp_dir().join(format!("sdkforge-{}-{}", slug(&a.repo.rsplit('/').next().unwrap_or("repo")), std::process::id())));
    if work.exists() {
        die(&format!("{} already exists; pass a different --workdir", work.display()));
    }
    println!("cloning into {}...", work.display());
    repo::clone(&a.repo, &work).unwrap_or_else(|e| die(&e.to_string()));
    repo::exclude_locally(&work, &SANDBOX_ARTIFACTS).unwrap_or_else(|e| die(&e.to_string()));

    let default_branch = repo::current_branch(&work).unwrap_or_else(|e| die(&e.to_string()));
    let tc = detect(&work).unwrap_or_else(|| die("could not detect the build/test setup; add a .sdkforge.toml with `image`, `install` and `test`"));
    println!("toolchain: {} ({})", tc.image, tc.test);
    let branch = format!("sdkforge/{}", slug(&a.request));
    repo::create_branch(&work, &branch).unwrap_or_else(|e| die(&e.to_string()));

    let all = repo::tracked_files(&work).unwrap_or_else(|e| die(&e.to_string()));
    let mut claude = Claude::from_env(a.model).unwrap_or_else(|e| die(&e.to_string()));
    let relevant = |work: &Path| repo::select_files(work, &a.request, &all, 8, 60_000);
    if relevant(&work).is_empty() {
        println!("note: no files matched the request keywords; the model will only see the file list");
    }

    if a.bug {
        println!("writing a failing test that reproduces the problem...");
        let diff = ask_diff(&mut claude, &repro_prompt(&a.request, &all, &relevant(&work))).unwrap_or_else(|e| die(&e));
        repo::apply_patch(&work, &diff).unwrap_or_else(|e| die(&format!("reproducing test did not apply: {e}")));
        match run_in_sandbox(&work, &tc) {
            Ok(o) if !o.success => println!("reproduced: the new test fails as expected"),
            Ok(_) => println!("warning: the suite passes with the new test, so the problem may not be reproduced"),
            Err(e) => die(&e.to_string()),
        }
        repo::commit_all(&work, "test: reproduce reported problem").unwrap_or_else(|e| die(&e.to_string()));
    }

    let policy = RetryPolicy { max_attempts: a.max_attempts, token_budget: a.token_budget };
    let claude = RefCell::new(claude);
    let mut feedback: Option<String> = None;
    for round in 1..=3 {
        println!("patching (round {round})...");
        let apply_error: RefCell<Option<String>> = RefCell::new(None);
        let apply = |diff: Result<String, String>| {
            *apply_error.borrow_mut() = match diff {
                Ok(d) => repo::apply_patch(&work, &d).err().map(|e| format!("patch did not apply: {e}")),
                Err(e) => Some(e),
            };
        };
        apply(ask_diff(&mut claude.borrow_mut(), &patch_prompt(&a.request, &all, &relevant(&work), None, feedback.as_deref())));

        let result = fix_loop(
            &policy,
            || {
                if let Some(msg) = apply_error.borrow().clone() {
                    return Ok(Outcome { success: false, output: msg });
                }
                println!("verifying in sandbox...");
                run_in_sandbox(&work, &tc)
            },
            |err| {
                println!("verification failed, asking for a new patch...");
                repo::reset_worktree(&work).map_err(|e| VerifyError::Sandbox(e.to_string()))?;
                let prompt = patch_prompt(&a.request, &all, &relevant(&work), Some(err), feedback.as_deref());
                apply(ask_diff(&mut claude.borrow_mut(), &prompt));
                Ok(())
            },
            || claude.borrow().tokens_used,
        );
        match result {
            Ok((o, _)) if o.success => {}
            Ok((o, n)) => {
                eprintln!("{}", o.output);
                die(&format!("not verified after {n} attempt(s); flagged for review, repo left in {}", work.display()))
            }
            Err(e) => die(&e.to_string()),
        }

        let diff = repo::diff(&work).unwrap_or_else(|e| die(&e.to_string()));
        println!("\n{diff}\ntests pass in the sandbox.");
        match ask_approval() {
            Verdict::Approve => {
                repo::commit_all(&work, &format!("fix: {}", subject(&a.request))).unwrap_or_else(|e| die(&e.to_string()));
                println!("committed on branch {branch} in {}", work.display());
                if a.push {
                    publish(&work, &branch, &default_branch, &a.request);
                } else {
                    println!("not pushed (pass --push to push the branch and open a PR)");
                }
                println!("tokens used: {}", claude.borrow().tokens_used);
                return;
            }
            Verdict::Reject => {
                repo::reset_worktree(&work).ok();
                die("rejected; nothing committed")
            }
            Verdict::Feedback(f) => {
                repo::reset_worktree(&work).unwrap_or_else(|e| die(&e.to_string()));
                feedback = Some(f);
            }
        }
    }
    die("no approved diff after 3 rounds");
}

fn publish(work: &Path, branch: &str, default_branch: &str, request: &str) {
    use repo::ForgeProvider;
    repo::push(work, branch, default_branch).unwrap_or_else(|e| die(&e.to_string()));
    let body = format!("{request}\n\nGenerated by SDK Forge. Tests passed in a network-isolated sandbox and the diff was approved before commit.");
    match repo::GitHub.open_pull_request(work, branch, &format!("fix: {}", subject(request)), &body) {
        Ok(url) => println!("pull request: {url}"),
        Err(e) => println!("branch pushed, but the PR could not be opened: {e}"),
    }
}
