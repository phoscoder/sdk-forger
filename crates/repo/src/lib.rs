//! Git operations (via the `git` CLI) and forge providers.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("forge error: {0}")]
    Forge(String),
    #[error("git {cmd}: {msg}")]
    Git { cmd: String, msg: String },
}

pub trait ForgeProvider {
    fn open_pull_request(
        &self,
        dir: &Path,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String, RepoError>;
}

/// GitHub through the `gh` CLI, which owns the user's credentials; tokens never pass through us.
pub struct GitHub;

impl ForgeProvider for GitHub {
    fn open_pull_request(
        &self,
        dir: &Path,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String, RepoError> {
        let out = Command::new("gh")
            .current_dir(dir)
            .args([
                "pr", "create", "--head", branch, "--title", title, "--body", body,
            ])
            .output()
            .map_err(|e| RepoError::Forge(format!("cannot run gh: {e}")))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(RepoError::Forge(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ))
        }
    }
}

fn run(dir: Option<&Path>, args: &[&str], stdin: Option<&str>) -> Result<String, RepoError> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let err = |msg: String| RepoError::Git {
        cmd: args.join(" "),
        msg,
    };
    let mut child = cmd.spawn().map_err(|e| err(e.to_string()))?;
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(input.as_bytes())
            .map_err(|e| err(e.to_string()))?;
    }
    let out = child.wait_with_output().map_err(|e| err(e.to_string()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(err(String::from_utf8_lossy(&out.stderr).trim().to_string()))
    }
}

pub fn clone(url: &str, dest: &Path) -> Result<(), RepoError> {
    run(
        None,
        &["clone", "--quiet", "--", url, &dest.to_string_lossy()],
        None,
    )
    .map(|_| ())
}

pub fn current_branch(dir: &Path) -> Result<String, RepoError> {
    Ok(
        run(Some(dir), &["rev-parse", "--abbrev-ref", "HEAD"], None)?
            .trim()
            .to_string(),
    )
}

pub fn tracked_files(dir: &Path) -> Result<Vec<String>, RepoError> {
    Ok(run(Some(dir), &["ls-files"], None)?
        .lines()
        .map(str::to_string)
        .collect())
}

/// Keep sandbox artifacts out of diffs and commits without touching the repo's own .gitignore.
pub fn exclude_locally(dir: &Path, patterns: &[&str]) -> std::io::Result<()> {
    let path = dir.join(".git/info/exclude");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    for p in patterns {
        text.push_str(&format!("{p}\n"));
    }
    std::fs::write(path, text)
}

pub fn apply_patch(dir: &Path, diff: &str) -> Result<(), RepoError> {
    run(
        Some(dir),
        &["apply", "--recount", "--whitespace=nowarn", "-"],
        Some(diff),
    )
    .map(|_| ())
}

/// Discard uncommitted changes, including new files (ignored sandbox artifacts stay).
pub fn reset_worktree(dir: &Path) -> Result<(), RepoError> {
    // `diff` marks new files intent-to-add; unstage so `clean` sees them as untracked.
    run(Some(dir), &["reset", "-q"], None)?;
    run(Some(dir), &["checkout", "--", "."], None)?;
    run(Some(dir), &["clean", "-fdq"], None).map(|_| ())
}

/// Uncommitted changes, new files included.
pub fn diff(dir: &Path) -> Result<String, RepoError> {
    run(Some(dir), &["add", "-N", "."], None)?;
    run(Some(dir), &["diff"], None)
}

pub fn create_branch(dir: &Path, name: &str) -> Result<(), RepoError> {
    run(Some(dir), &["checkout", "-q", "-b", name], None).map(|_| ())
}

pub fn commit_all(dir: &Path, message: &str) -> Result<(), RepoError> {
    run(Some(dir), &["add", "-A"], None)?;
    run(Some(dir), &["commit", "-q", "-m", message], None).map(|_| ())
}

/// Refuses to push the repo's default branch; Maintain mode always works on its own branch.
pub fn push(dir: &Path, branch: &str, default_branch: &str) -> Result<(), RepoError> {
    if branch == default_branch || branch == "main" || branch == "master" {
        return Err(RepoError::Git {
            cmd: "push".into(),
            msg: format!("refusing to push default branch `{branch}`"),
        });
    }
    run(Some(dir), &["push", "-u", "origin", branch], None).map(|_| ())
}

/// Pick the files most relevant to a request: keyword hits in path (weighted) and content,
/// within a total byte budget, so the model never sees the whole codebase.
pub fn select_files(
    dir: &Path,
    request: &str,
    files: &[String],
    max_files: usize,
    byte_budget: usize,
) -> Vec<(String, String)> {
    let words: Vec<String> = request
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| w.len() > 3)
        .map(str::to_lowercase)
        .collect();
    let mut scored: Vec<(usize, &String, String)> = files
        .iter()
        .filter_map(|f| {
            let text = std::fs::read_to_string(dir.join(f)).ok()?; // skips binaries
            let lower = text.to_lowercase();
            let name = f.to_lowercase();
            let score: usize = words
                .iter()
                .map(|w| {
                    lower.matches(w.as_str()).count().min(10)
                        + 5 * name.contains(w.as_str()) as usize
                })
                .sum();
            (score > 0).then_some((score, f, text))
        })
        .collect();
    scored.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut used = 0;
    let mut out = Vec::new();
    for (_, f, text) in scored {
        if out.len() >= max_files || used + text.len() > byte_budget {
            continue;
        }
        used += text.len();
        out.push((f.clone(), text));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_default_branch_push() {
        assert!(push(Path::new("."), "main", "main").is_err());
        assert!(push(Path::new("."), "develop", "develop").is_err());
    }

    #[test]
    fn selects_relevant_files_first() {
        let d = std::env::temp_dir().join(format!("sdkforge-sel-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("pagination.py"), "def next_cursor(): pass").unwrap();
        std::fs::write(d.join("other.py"), "x = 1").unwrap();
        let files = vec!["pagination.py".to_string(), "other.py".to_string()];
        let sel = select_files(&d, "next_cursor crashes in pagination", &files, 5, 10_000);
        assert_eq!(sel.len(), 1);
        assert_eq!(sel[0].0, "pagination.py");
        std::fs::remove_dir_all(d).ok();
    }

    #[test]
    fn apply_diff_reset_roundtrip() {
        let d = std::env::temp_dir().join(format!("sdkforge-git-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let g = |args: &[&str]| run(Some(&d), args, None).unwrap();
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        std::fs::write(d.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-qm", "init"]);

        // Wrong hunk counts (as models often write) plus a new file.
        let patch = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,9 +1,9 @@\n one\n-two\n+2\n three\n\
diff --git a/b.txt b/b.txt\nnew file mode 100644\n--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1 @@\n+new\n";
        apply_patch(&d, patch).unwrap();
        let shown = diff(&d).unwrap();
        assert!(shown.contains("+2") && shown.contains("b.txt"));

        reset_worktree(&d).unwrap();
        assert_eq!(
            std::fs::read_to_string(d.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
        assert!(!d.join("b.txt").exists());
        std::fs::remove_dir_all(d).ok();
    }
}
