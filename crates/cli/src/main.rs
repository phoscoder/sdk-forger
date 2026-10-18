mod maintain;
mod port;

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use sdkforge_generate::{
    check_fidelity, chunk_prompt, core_prompt, fix_prompt, request_files, request_files_with,
    Claude, GeneratedFile,
};
use sdkforge_ingest::Lang;
use sdkforge_ir::Api;
use sdkforge_verify::{fix_loop, for_target, run_in_sandbox, Outcome, RetryPolicy, VerifyError};

#[derive(Parser)]
#[command(
    name = "sdkforge",
    version,
    about = "AI-built, sandbox-verified API SDKs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Generate an SDK from an API description
    Generate {
        spec: String,
        /// Free-text instructions for the model
        #[arg(short, long, default_value = "")]
        instructions: String,
        /// Target language(s): python, node, php, rust, go. Repeat or comma-separate for several;
        /// several languages run in parallel into <out>/<lang>
        #[arg(short, long, default_value = "python", value_delimiter = ',')]
        lang: Vec<String>,
        /// Output directory
        #[arg(short, long, default_value = "out")]
        out: PathBuf,
        /// Parse and plan only; no model calls, no Docker
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 5)]
        max_attempts: u32,
        #[arg(long, default_value_t = 500_000)]
        token_budget: u64,
    },
    /// Patch an existing SDK repo
    Maintain {
        /// Repo URL or local path to clone
        repo: String,
        /// The error message or improvement to make
        #[arg(short, long)]
        request: String,
        /// Treat the request as a bug: write a failing test first
        #[arg(long)]
        bug: bool,
        #[arg(long)]
        workdir: Option<PathBuf>,
        /// Push the branch and open a PR after approval
        #[arg(long)]
        push: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 5)]
        max_attempts: u32,
        #[arg(long, default_value_t = 500_000)]
        token_budget: u64,
    },
    /// Port an SDK to another language
    Port {
        /// Source SDK repo URL or local path
        repo: String,
        /// Target language: python, node, php, rust or go
        #[arg(short, long)]
        to: String,
        /// Source language (detected from the repo when omitted): python, node, php, rust or go
        #[arg(short, long)]
        from: Option<String>,
        #[arg(short, long, default_value = "")]
        instructions: String,
        #[arg(short, long, default_value = "port-out")]
        out: PathBuf,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value_t = 5)]
        max_attempts: u32,
        #[arg(long, default_value_t = 800_000)]
        token_budget: u64,
        /// Skip the HTTP traffic parity check against the original SDK
        #[arg(long)]
        no_parity: bool,
        /// Scenario ids whose traffic differs from the original on purpose (comma-separated)
        #[arg(long, value_delimiter = ',')]
        accept_diff: Vec<String>,
    },
}

pub(crate) fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1)
}

pub(crate) fn write_files(root: &Path, files: &[GeneratedFile]) {
    for f in files {
        let p = root.join(&f.path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| die(&e.to_string()));
        }
        std::fs::write(&p, &f.content).unwrap_or_else(|e| die(&e.to_string()));
    }
}

/// Later files replace earlier ones with the same path.
pub(crate) fn merge(all: &mut Vec<GeneratedFile>, new: Vec<GeneratedFile>) {
    for f in new {
        match all.iter_mut().find(|x| x.path == f.path) {
            Some(x) => *x = f,
            None => all.push(f),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate(
    spec: String,
    instructions: String,
    langs: Vec<String>,
    out: PathBuf,
    dry_run: bool,
    model: Option<String>,
    max_attempts: u32,
    token_budget: u64,
) {
    let mut targets: Vec<Lang> = Vec::new();
    for name in &langs {
        let l = Lang::parse(name).unwrap_or_else(|| {
            die(&format!(
                "unknown language `{name}`; use python, node, php, rust or go"
            ))
        });
        if !targets.contains(&l) {
            targets.push(l);
        }
    }
    let api =
        sdkforge_ingest::ingest_path(Path::new(&spec)).unwrap_or_else(|e| die(&e.to_string()));

    println!(
        "{} v{} ({} resources, {} types)",
        api.name,
        api.version,
        api.resources.len(),
        api.types.len()
    );
    for r in &api.resources {
        println!("  {}", r.name);
        for o in &r.operations {
            println!("    {:<6} {}  [{}]", o.method, o.path, o.id);
        }
    }
    if dry_run {
        for l in &targets {
            let plan = sdkforge_planner::plan(&api, l.name());
            println!("plan ({}): {} files", l.name(), plan.files.len());
            plan.files.iter().for_each(|f| println!("  {f}"));
        }
        return;
    }

    let many = targets.len() > 1;
    let results: Vec<(Lang, Result<String, String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = targets
            .iter()
            .map(|&lang| {
                let (api, instructions, model) = (&api, &instructions, model.clone());
                let dir = if many {
                    out.join(lang.name())
                } else {
                    out.clone()
                };
                let tag = if many {
                    format!("[{}] ", lang.name())
                } else {
                    String::new()
                };
                scope.spawn(move || {
                    (
                        lang,
                        generate_one(
                            api,
                            lang,
                            instructions,
                            &dir,
                            model,
                            max_attempts,
                            token_budget,
                            &tag,
                        ),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| die("a generator thread panicked"))
            })
            .collect()
    });

    let mut failed = false;
    for (lang, r) in results {
        match r {
            Ok(msg) => println!("{}: {msg}", lang.name()),
            Err(msg) => {
                failed = true;
                eprintln!("{}: FAILED: {msg}", lang.name());
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
}

/// One language, start to finish. Errors are returned (not fatal) so other languages can finish.
#[allow(clippy::too_many_arguments)]
fn generate_one(
    api: &Api,
    lang: Lang,
    instructions: &str,
    out: &Path,
    model: Option<String>,
    max_attempts: u32,
    token_budget: u64,
    tag: &str,
) -> Result<String, String> {
    let log = |m: &str| println!("{tag}{m}");
    let plan = sdkforge_planner::plan(api, lang.name());
    let tc = for_target(lang.name()).ok_or("no sandbox recipe for that language")?;
    let mut claude = Claude::from_env(model).map_err(|e| e.to_string())?;
    let mut files: Vec<GeneratedFile> = Vec::new();

    log("generating core...");
    merge(
        &mut files,
        request_files(&mut claude, &core_prompt(api, &plan, instructions))
            .map_err(|e| e.to_string())?,
    );
    for r in &api.resources {
        log(&format!("generating {}...", r.name));
        merge(
            &mut files,
            request_files(&mut claude, &chunk_prompt(api, &plan, r, instructions))
                .map_err(|e| e.to_string())?,
        );
    }
    if files.is_empty() {
        return Err("the model returned no files".into());
    }
    write_files(out, &files);

    let policy = RetryPolicy {
        max_attempts,
        token_budget,
    };
    let claude = std::cell::RefCell::new(claude);
    let files = std::cell::RefCell::new(files);
    let result = fix_loop(
        &policy,
        || {
            let problems = check_fidelity(api, lang.name(), &files.borrow());
            if !problems.is_empty() {
                log("spec fidelity check failed");
                let output = format!(
                    "Spec fidelity check failed. Remove or fix paths not in the API model:\n{}",
                    problems.join("\n")
                );
                return Ok(Outcome {
                    success: false,
                    output,
                });
            }
            log("verifying in sandbox...");
            run_in_sandbox(out, &tc)
        },
        |err| {
            log("verification failed, asking the model for a fix...");
            let fixed = request_files_with(
                &mut claude.borrow_mut(),
                &fix_prompt(&plan, &files.borrow(), err),
            )
            .map_err(|e| VerifyError::Sandbox(e.to_string()))?;
            write_files(out, &fixed);
            merge(&mut files.borrow_mut(), fixed);
            Ok(())
        },
        || claude.borrow().tokens_used,
    );
    let tokens = claude.borrow().tokens_used;
    match result {
        Ok((o, attempts)) if o.success => Ok(format!(
            "verified after {attempts} attempt(s), {tokens} tokens: {}",
            out.display()
        )),
        Ok((o, attempts)) => Err(format!(
            "not verified after {attempts} attempt(s); flagged for review, output left in {}\n{}",
            out.display(),
            o.output
        )),
        Err(e) => Err(e.to_string()),
    }
}

fn main() {
    match Cli::parse().command {
        Command::Generate {
            spec,
            instructions,
            lang,
            out,
            dry_run,
            model,
            max_attempts,
            token_budget,
        } => generate(
            spec,
            instructions,
            lang,
            out,
            dry_run,
            model,
            max_attempts,
            token_budget,
        ),
        Command::Maintain {
            repo,
            request,
            bug,
            workdir,
            push,
            model,
            max_attempts,
            token_budget,
        } => maintain::run(maintain::Args {
            repo,
            request,
            bug,
            workdir,
            push,
            model,
            max_attempts,
            token_budget,
        }),
        Command::Port {
            repo,
            to,
            from,
            instructions,
            out,
            model,
            max_attempts,
            token_budget,
            no_parity,
            accept_diff,
        } => port::run(port::Args {
            repo,
            to,
            from,
            instructions,
            out,
            model,
            max_attempts,
            token_budget,
            parity: !no_parity,
            accept: accept_diff,
        }),
    }
}
