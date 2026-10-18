use std::io::{BufRead, Write};
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use sdkforge_engine::{
    Delivery, Event, GenerateRequest, Host, Level, MaintainRequest, Package, PortRequest, Verdict,
};

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
        /// After a verified run, add README, metadata and CI, then package: none, zip or repo
        #[arg(long, value_parser = ["none", "zip", "repo"])]
        deliver: Option<String>,
        /// Run the language formatter in the sandbox during delivery
        #[arg(long)]
        format: bool,
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

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1)
}

/// Terminal host: events to stdout/stderr, approval from stdin.
struct Terminal;

impl Host for Terminal {
    fn event(&self, e: Event) {
        let tag = if e.scope.is_empty() {
            String::new()
        } else {
            format!("[{}] ", e.scope)
        };
        match e.level {
            Level::Info => println!("{tag}{}", e.message),
            Level::Warn | Level::Detail => eprintln!("{tag}{}", e.message),
        }
    }

    fn approve(&self, diff: &str) -> Verdict {
        println!("\n{diff}");
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
}

fn main() {
    let host = Terminal;
    match Cli::parse().command {
        Command::Generate {
            spec,
            instructions,
            lang,
            out,
            dry_run,
            deliver,
            format,
            model,
            max_attempts,
            token_budget,
        } => {
            let package = match deliver.as_deref() {
                Some("zip") => Some(Package::Zip),
                Some("repo") => Some(Package::Repo),
                Some(_) => Some(Package::None),
                None => None,
            };
            let req = GenerateRequest {
                spec,
                instructions,
                langs: lang,
                out,
                dry_run,
                model,
                max_attempts,
                token_budget,
                deliver: package.map(|package| Delivery { format, package }),
            };
            let results = sdkforge_engine::generate(&host, &req).unwrap_or_else(|e| die(&e.0));
            let mut failed = false;
            for r in results {
                match r.result {
                    Ok(msg) => println!("{}: {msg}", r.lang.name()),
                    Err(msg) => {
                        failed = true;
                        eprintln!("{}: FAILED: {msg}", r.lang.name());
                    }
                }
            }
            if failed {
                std::process::exit(1);
            }
        }
        Command::Maintain {
            repo,
            request,
            bug,
            workdir,
            push,
            model,
            max_attempts,
            token_budget,
        } => {
            let req = MaintainRequest {
                repo,
                request,
                bug,
                workdir,
                push,
                model,
                max_attempts,
                token_budget,
            };
            println!(
                "{}",
                sdkforge_engine::maintain(&host, &req).unwrap_or_else(|e| die(&e.0))
            );
        }
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
        } => {
            let req = PortRequest {
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
            };
            println!(
                "{}",
                sdkforge_engine::port(&host, &req).unwrap_or_else(|e| die(&e.0))
            );
        }
    }
}
