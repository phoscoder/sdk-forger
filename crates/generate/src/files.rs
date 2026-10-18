use std::path::{Component, Path};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
}

/// Parse `=== FILE: path ===` / `=== END ===` blocks. A block also ends at the next FILE header or
/// the end of the reply, since models sometimes forget the END marker. Unsafe paths are dropped.
pub fn parse_files(text: &str) -> Vec<GeneratedFile> {
    fn flush(cur: &mut Option<(String, Vec<&str>)>, out: &mut Vec<GeneratedFile>) {
        if let Some((path, mut lines)) = cur.take() {
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            if is_safe(&path) && !lines.is_empty() {
                out.push(GeneratedFile { path, content: lines.join("\n") + "\n" });
            }
        }
    }
    let mut out = Vec::new();
    let mut cur: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("=== FILE:") {
            flush(&mut cur, &mut out);
            let path = rest.trim().trim_end_matches('=').trim().to_string();
            cur = Some((path, Vec::new()));
        } else if line.trim() == "=== END ===" {
            flush(&mut cur, &mut out);
        } else if let Some((_, lines)) = cur.as_mut() {
            lines.push(line);
        }
    }
    flush(&mut cur, &mut out);
    out
}

fn is_safe(path: &str) -> bool {
    !path.is_empty() && Path::new(path).components().all(|c| matches!(c, Component::Normal(_)))
}

/// Mask values of credential-looking keys and bearer tokens before text reaches the model.
pub fn redact(text: &str) -> String {
    const KEYS: [&str; 7] = ["api_key", "apikey", "token", "secret", "password", "authorization", "x-api-key"];
    text.lines()
        .map(|line| {
            let lower = line.to_lowercase();
            for k in KEYS {
                if let Some(i) = lower.find(k) {
                    if let Some(sep) = line[i..].find([':', '=']) {
                        return format!("{} [REDACTED]", &line[..i + sep + 1]);
                    }
                }
            }
            if let Some(i) = lower.find("bearer ") {
                return format!("{}Bearer [REDACTED]", &line[..i]);
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

pub fn pascal(name: &str) -> String {
    snake(name)
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect()
}

pub fn camel(name: &str) -> String {
    let p = pascal(name);
    let mut c = p.chars();
    c.next().map(|f| f.to_ascii_lowercase().to_string() + c.as_str()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_blocks_and_drops_traversal() {
        let t = "=== FILE: a/b.py ===\nx = 1\n=== END ===\n=== FILE: ../evil ===\nno\n=== END ===\n";
        let f = parse_files(t);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].path, "a/b.py");
        assert_eq!(f[0].content, "x = 1\n");
    }

    #[test]
    fn tolerates_missing_end_markers() {
        let t = "=== FILE: a.php ===\n<?php\n\n=== FILE: b.php ===\n<?php echo 1;\n=== END ===\n=== FILE: c.php ===\nlast\n";
        let f = parse_files(t);
        assert_eq!(f.iter().map(|x| x.path.as_str()).collect::<Vec<_>>(), ["a.php", "b.php", "c.php"]);
        assert_eq!(f[0].content, "<?php\n");
        assert_eq!(f[2].content, "last\n");
    }

    #[test]
    fn redacts_secrets() {
        let r = redact("api_key: sk-123\nname: ok\nAuthorization: Bearer abc");
        assert!(!r.contains("sk-123") && !r.contains("abc"));
        assert!(r.contains("name: ok"));
    }

    #[test]
    fn snake_cases() {
        assert_eq!(snake("UserAccounts"), "user_accounts");
        assert_eq!(snake("pets"), "pets");
        assert_eq!(pascal("user-accounts"), "UserAccounts");
        assert_eq!(camel("UserAccounts"), "userAccounts");
    }
}
