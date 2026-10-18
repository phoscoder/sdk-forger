use std::cell::RefCell;
use std::path::{Path, PathBuf};

use sdkforge_generate::{
    driver_fix_prompt, driver_prompt, fix_prompt, module_prompt, parse_files, plan_prompt,
    scenarios_prompt, surface_missing, tests_prompt, Claude, GeneratedFile, Source, PLAN_SYSTEM,
    SYSTEM,
};
use sdkforge_ingest::{detect_lang, outline, public_names, render_outline, FileOutline, Lang};
use sdkforge_mock::{compare_accepting, Recording};
use sdkforge_planner::SdkPlan;
use sdkforge_repo as repo;
use sdkforge_verify::{
    detect_as, driver, fix_loop, for_target, install, record_traffic, run_in_sandbox, Outcome,
    RetryPolicy, VerifyError,
};

use crate::{die, merge, write_files};

pub struct Args {
    pub repo: String,
    pub to: String,
    /// Source language; detected from the repo when None.
    pub from: Option<String>,
    pub instructions: String,
    pub out: PathBuf,
    pub model: Option<String>,
    pub max_attempts: u32,
    pub token_budget: u64,
    pub parity: bool,
    /// Scenario ids whose traffic differences are known and accepted.
    pub accept: Vec<String>,
}

const SOURCE_BUDGET: usize = 150_000;
const TEST_BUDGET: usize = 100_000;

/// Files in size order until the byte budget is spent; returns (kept, skipped paths).
fn within_budget(
    dir: &Path,
    paths: Vec<String>,
    budget: usize,
) -> (Vec<(String, String)>, Vec<String>) {
    let mut loaded: Vec<(String, String)> = paths
        .into_iter()
        .filter_map(|p| Some((p.clone(), std::fs::read_to_string(dir.join(&p)).ok()?)))
        .collect();
    loaded.sort_by_key(|(_, c)| c.len());
    let (mut kept, mut skipped, mut used) = (Vec::new(), Vec::new(), 0);
    for (p, c) in loaded {
        if used + c.len() <= budget {
            used += c.len();
            kept.push((p, c));
        } else {
            skipped.push(p);
        }
    }
    kept.sort();
    (kept, skipped)
}

fn complete(claude: &RefCell<Claude>, system: &str, prompt: &str) -> String {
    claude
        .borrow_mut()
        .complete(system, prompt)
        .unwrap_or_else(|e| die(&e.to_string()))
}

/// Record the original SDK's traffic for shared scenarios and add a driver for the port.
/// Returns the baseline recording, or None (with a printed reason) if parity cannot be set up.
#[allow(clippy::too_many_arguments)]
fn setup_parity(
    claude: &RefCell<Claude>,
    src: &Source,
    plan_text: &str,
    work: &Path,
    tests: &[(String, String)],
    sources: &[(String, String)],
    files: &RefCell<Vec<GeneratedFile>>,
    from: Lang,
    target: &str,
    out: &Path,
) -> Option<Recording> {
    let skip = |why: &str| {
        println!("parity check skipped: {why}");
        None
    };
    println!("designing traffic scenarios...");
    let reply = ask_files(claude, &scenarios_prompt(src, tests));
    let Some(scen) = reply
        .into_iter()
        .find(|f| f.path == "parity/scenarios.json")
    else {
        return skip("the model returned no scenarios");
    };
    if !serde_json::from_str::<serde_json::Value>(&scen.content).is_ok_and(|v| v.is_array()) {
        return skip("scenarios.json is not a JSON array");
    }

    // 1. Baseline: drive the original SDK.
    let Some(tc) = detect_as(work, from.name()) else {
        return skip("no sandbox recipe for the original's language");
    };
    match install(work, &tc) {
        Ok(o) if o.success => {}
        Ok(o) => {
            return skip(&format!(
                "could not install the original's dependencies:\n{}",
                o.output
            ))
        }
        Err(e) => return skip(&e.to_string()),
    }
    let (src_path, src_cmd) = driver(from.name());
    write_files(work, std::slice::from_ref(&scen));
    let mut baseline = None;
    let mut driver_file = GeneratedFile {
        path: src_path.into(),
        content: String::new(),
    };
    let mut last_output = String::new();
    for attempt in 1..=3 {
        let prompt = if attempt == 1 {
            driver_prompt(src, from.name(), src_path, &scen.content, None, &[], tests)
        } else {
            driver_fix_prompt(src_path, &driver_file.content, &last_output)
        };
        let Some(f) = ask_files(claude, &prompt)
            .into_iter()
            .find(|f| f.path == src_path)
        else {
            continue;
        };
        driver_file = f;
        write_files(work, std::slice::from_ref(&driver_file));
        println!("recording the original SDK's traffic (attempt {attempt})...");
        match record_traffic(work, &tc, src_cmd) {
            Ok((o, rec)) if o.success && !rec.requests.is_empty() => {
                baseline = Some(rec);
                break;
            }
            Ok((o, rec)) => {
                last_output = if o.success {
                    format!("exit 0 but recorded {} requests", rec.requests.len())
                } else {
                    o.output
                }
            }
            Err(e) => return skip(&e.to_string()),
        }
    }
    let baseline = match baseline {
        Some(b) => b,
        None => {
            return skip(&format!(
                "the original's driver never ran cleanly:\n{last_output}"
            ))
        }
    };
    println!(
        "baseline: {} requests across {} scenarios",
        baseline.requests.len(),
        baseline.outcomes.len()
    );

    // 2. Driver for the port, written against the plan and the ported code.
    let (dst_path, _) = driver(target);
    let code: Vec<GeneratedFile> = files
        .borrow()
        .iter()
        .filter(|f| !f.path.starts_with("parity/"))
        .cloned()
        .collect();
    let tgt = ask_files(
        claude,
        &driver_prompt(
            src,
            target,
            dst_path,
            &scen.content,
            Some(plan_text),
            &code,
            tests,
        ),
    );
    let Some(df) = tgt.into_iter().find(|f| f.path == dst_path) else {
        return skip("the model returned no driver for the port");
    };
    merge(&mut files.borrow_mut(), vec![scen, df]);
    write_files(out, &files.borrow());
    let _ = sources;
    Some(baseline)
}

/// Ask for files; if the reply has none (format slip or truncation), say what came back and retry once.
fn ask_files(claude: &RefCell<Claude>, prompt: &str) -> Vec<GeneratedFile> {
    let reply = complete(claude, SYSTEM, prompt);
    let files = parse_files(&reply);
    if !files.is_empty() {
        return files;
    }
    let head: String = reply.chars().take(200).collect();
    println!(
        "the reply had no file blocks ({} chars, starts {head:?}); retrying",
        reply.len()
    );
    let retry = format!("{prompt}\n\nYour previous reply contained no `=== FILE: path ===` ... `=== END ===` blocks. Reply with ONLY files in exactly that format.");
    parse_files(&complete(claude, SYSTEM, &retry))
}

pub fn run(a: Args) {
    let to = Lang::parse(&a.to).unwrap_or_else(|| {
        die(&format!(
            "unsupported target `{}`; use python, node, php, rust or go",
            a.to
        ))
    });
    let tc = for_target(to.name()).unwrap_or_else(|| die("no sandbox recipe for that target"));
    let work = std::env::temp_dir().join(format!("sdkforge-port-{}", std::process::id()));
    println!("cloning source...");
    repo::clone(&a.repo, &work).unwrap_or_else(|e| die(&e.to_string()));
    let all = repo::tracked_files(&work).unwrap_or_else(|e| die(&e.to_string()));

    let from = match &a.from {
        Some(name) => Lang::parse(name).unwrap_or_else(|| {
            die(&format!(
                "unknown source language `{name}`; use python, node, php, rust or go"
            ))
        }),
        None => detect_lang(&all)
            .unwrap_or_else(|| die("could not detect the source language; pass --from")),
    };
    println!("source language: {}", from.display());
    let test_paths: Vec<String> = all
        .iter()
        .filter(|p| from.is_code_file(p) && from.is_test_file(p))
        .cloned()
        .collect();
    let src_paths: Vec<String> = all
        .iter()
        .filter(|p| from.is_source_file(p))
        .cloned()
        .collect();
    if src_paths.is_empty() {
        die(&format!("no {} source files found", from.display()));
    }

    let (sources, skipped_src) = within_budget(&work, src_paths, SOURCE_BUDGET);
    let (mut tests, skipped_tests) = within_budget(&work, test_paths, TEST_BUDGET);
    // Rust keeps unit tests inline next to the code.
    if from == Lang::Rust {
        for (p, c) in sources.iter().filter(|(_, c)| c.contains("#[cfg(test)]")) {
            tests.push((p.clone(), c.clone()));
        }
    }
    for s in skipped_src.iter().chain(&skipped_tests) {
        println!("note: skipped {s} (over the size budget)");
    }
    if tests.is_empty() {
        die("the source repo has no tests; Port needs them as the definition of done");
    }

    let outlines: Vec<FileOutline> = sources
        .iter()
        .map(|(p, c)| outline(from, p, c).unwrap_or_else(|e| die(&e.to_string())))
        .collect();
    let names = public_names(&outlines);
    println!(
        "analyzed {} source files ({} public symbols), {} test files",
        sources.len(),
        names.len(),
        tests.len()
    );

    let readme: String = ["README.md", "README.rst", "README"]
        .iter()
        .find_map(|n| std::fs::read_to_string(work.join(n)).ok())
        .map(|t| t.chars().take(8_000).collect())
        .unwrap_or_default();
    let outline = render_outline(&outlines);
    let manifest: String = [
        "Cargo.toml",
        "go.mod",
        "package.json",
        "composer.json",
        "pyproject.toml",
    ]
    .iter()
    .find_map(|n| std::fs::read_to_string(work.join(n)).ok())
    .map(|t| t.chars().take(3_000).collect())
    .unwrap_or_default();
    let target = to.name();
    let src = Source {
        from: from.display(),
        manifest: &manifest,
        outline: &outline,
        readme: &readme,
        instructions: &a.instructions,
        target,
    };

    let claude = RefCell::new(Claude::from_env(a.model).unwrap_or_else(|e| die(&e.to_string())));

    println!("planning the port...");
    let plan_text = complete(&claude, PLAN_SYSTEM, &plan_prompt(&src));

    println!("porting tests first...");
    let ported_tests = ask_files(&claude, &tests_prompt(&src, &plan_text, &tests));
    if ported_tests.is_empty() {
        die("the model returned no ported tests");
    }

    let mut files: Vec<GeneratedFile> = ported_tests.clone();
    for module in sources.iter().filter(|(p, _)| {
        outlines
            .iter()
            .any(|o| &o.path == p && !o.symbols.is_empty())
    }) {
        println!("porting {}...", module.0);
        merge(
            &mut files,
            ask_files(
                &claude,
                &module_prompt(&src, &plan_text, &ported_tests, module),
            ),
        );
    }
    write_files(&a.out, &files);

    let files = RefCell::new(files);
    let baseline = if a.parity {
        setup_parity(
            &claude, &src, &plan_text, &work, &tests, &sources, &files, from, target, &a.out,
        )
    } else {
        None
    };

    let plan = SdkPlan {
        language: target.to_string(),
        ..SdkPlan::default()
    };
    let policy = RetryPolicy {
        max_attempts: a.max_attempts,
        token_budget: a.token_budget,
    };
    let accepted_notes: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let result = fix_loop(
        &policy,
        || {
            let missing = surface_missing(&names, &files.borrow());
            if !missing.is_empty() {
                println!("surface check failed");
                let output = format!(
                    "These public symbols of the source SDK have no counterpart in the port:\n{}",
                    missing.join("\n")
                );
                return Ok(Outcome {
                    success: false,
                    output,
                });
            }
            println!("verifying in sandbox...");
            let tests_run = run_in_sandbox(&a.out, &tc)?;
            let Some(base) = &baseline else {
                return Ok(tests_run);
            };
            if !tests_run.success {
                return Ok(tests_run);
            }
            println!("checking traffic parity against the original...");
            let (cmd_outcome, actual) = record_traffic(&a.out, &tc, driver(target).1)?;
            if !cmd_outcome.success || actual.requests.is_empty() {
                let output = format!("The parity driver `{}` failed to run or recorded nothing. Fix the driver or the port:\n{}", driver(target).0, cmd_outcome.output);
                return Ok(Outcome {
                    success: false,
                    output,
                });
            }
            let cmp = compare_accepting(base, &actual, &a.accept);
            *accepted_notes.borrow_mut() = cmp.accepted;
            let problems = cmp.blocking;
            if problems.is_empty() {
                return Ok(tests_run);
            }
            println!(
                "traffic differs from the original in {} place(s)",
                problems.len()
            );
            let output = format!(
                "Traffic parity failed: for the same scripted scenarios (parity/scenarios.json) the original SDK and the port sent different HTTP traffic. \
Fix the PORT so its requests match the original's exactly (the original is correct; do not change scenarios.json):\n{}",
                problems.join("\n")
            );
            Ok(Outcome {
                success: false,
                output,
            })
        },
        |err| {
            println!("verification failed, asking the model for a fix...");
            let text = claude
                .borrow_mut()
                .complete(SYSTEM, &fix_prompt(&plan, &files.borrow(), err))
                .map_err(|e| VerifyError::Sandbox(e.to_string()))?;
            let fixed = parse_files(&text);
            write_files(&a.out, &fixed);
            merge(&mut files.borrow_mut(), fixed);
            Ok(())
        },
        || claude.borrow().tokens_used,
    );
    // Verification scaffolding is never part of the SDK, whether or not the run succeeded.
    std::fs::remove_dir_all(a.out.join("parity")).ok();
    let driver_path = a.out.join(driver(target).0);
    if driver_path.exists() {
        std::fs::remove_file(&driver_path).ok();
        if let Some(dir) = driver_path.parent() {
            std::fs::remove_dir(dir).ok(); // only if now empty (e.g. examples/)
        }
    }
    match result {
        Ok((o, n)) if o.success => {
            let parity = if baseline.is_some() {
                "traffic matches the original"
            } else {
                "traffic parity not checked"
            };
            println!(
                "ported tests pass after {n} attempt(s), {parity}: {}",
                a.out.display()
            );
            let notes = accepted_notes.borrow();
            if !notes.is_empty() {
                println!(
                    "accepted differences from the original:\n{}",
                    notes.join("\n")
                );
                write_notes(&a.out, &notes);
            }
        }
        Ok((o, n)) => {
            eprintln!("{}", o.output);
            let ids = scenario_ids(&o.output);
            if o.output.contains("Traffic parity failed") && !ids.is_empty() {
                eprintln!(
                    "if these differences are intended, re-run with --accept-diff {}",
                    ids.join(",")
                );
            }
            die(&format!(
                "not verified after {n} attempt(s); flagged for review, output left in {}",
                a.out.display()
            ))
        }
        Err(e) => die(&e.to_string()),
    }
    println!("tokens used: {}", claude.borrow().tokens_used);
}

/// Scenario ids named in `[id] ...` lines of a parity report.
fn scenario_ids(report: &str) -> Vec<String> {
    let mut ids: Vec<String> = report
        .lines()
        .filter_map(|l| {
            l.strip_prefix('[')?
                .split_once(']')
                .map(|(id, _)| id.to_string())
        })
        .collect();
    ids.dedup();
    ids
}

/// Document accepted differences next to the SDK so reviewers see them.
fn write_notes(out: &Path, notes: &[String]) {
    let path = out.join("PORTING_NOTES.md");
    let body = format!(
        "# Porting notes\n\nThis port knowingly differs from the original in these traffic scenarios (accepted with `--accept-diff`):\n\n{}\n",
        notes.iter().map(|n| format!("- {}", n.replace('\n', " "))).collect::<Vec<_>>().join("\n")
    );
    if !path.exists() {
        std::fs::write(path, body).ok();
    }
}
