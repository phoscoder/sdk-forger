use crate::redact;

pub const DIFF_SYSTEM: &str = "You are a careful maintainer of an API client SDK. Reply with ONE unified diff in git format \
(`diff --git a/x b/x`, `--- a/x`, `+++ b/x`, `@@` hunks; use `--- /dev/null` for new files) and nothing else: no prose, no markdown fences. \
Keep the change minimal and match the surrounding code style.";

fn render(files: &[(String, String)]) -> String {
    files
        .iter()
        .map(|(p, c)| format!("=== {p} ===\n{c}\n"))
        .collect()
}

fn tree(all: &[String]) -> String {
    all.iter().take(300).cloned().collect::<Vec<_>>().join("\n")
}

/// Step 1 for bugs: a failing test that triggers the reported error, before any code changes.
pub fn repro_prompt(request: &str, all: &[String], files: &[(String, String)]) -> String {
    format!(
        "Write a NEW failing test (a diff that only adds or edits test files) reproducing this problem. \
The test must fail now for the reason described, and pass once the problem is fixed.\n\nProblem:\n{}\n\nRepo files:\n{}\n\nRelevant files:\n{}",
        redact(request),
        tree(all),
        render(files)
    )
}

pub fn patch_prompt(
    request: &str,
    all: &[String],
    files: &[(String, String)],
    failure: Option<&str>,
    feedback: Option<&str>,
) -> String {
    let mut p = format!(
        "Fix or implement the following in the SDK. If a reproducing test exists, make it pass without breaking other tests.\n\nRequest:\n{}\n\nRepo files:\n{}\n\nCurrent contents of relevant files:\n{}",
        redact(request),
        tree(all),
        render(files)
    );
    if let Some(f) = failure {
        p.push_str(&format!(
            "\nYour previous patch failed verification (it has been reverted). Output:\n{}\n",
            redact(f)
        ));
    }
    if let Some(f) = feedback {
        p.push_str(&format!(
            "\nThe reviewer rejected the previous diff with this feedback:\n{f}\n"
        ));
    }
    p
}

/// Pull the diff out of a reply, tolerating stray fences or leading prose.
pub fn extract_diff(reply: &str) -> Option<String> {
    let start = reply.find("diff --git").or_else(|| reply.find("--- "))?;
    let mut diff: Vec<&str> = reply[start..]
        .lines()
        .take_while(|l| !l.trim_start().starts_with("```"))
        .collect();
    while diff.last().is_some_and(|l| l.trim().is_empty()) {
        diff.pop();
    }
    Some(diff.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_fenced_diff() {
        let r =
            "Here:\n```diff\ndiff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n```\nDone";
        let d = extract_diff(r).unwrap();
        assert!(d.starts_with("diff --git") && d.ends_with("+b\n") && !d.contains("```"));
        assert!(extract_diff("no diff here").is_none());
    }
}
