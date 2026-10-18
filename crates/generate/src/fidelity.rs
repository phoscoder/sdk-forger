//! Spec fidelity check: every API path in generated code must match an operation in the IR.
//!
//! Language-agnostic: it scans string literals that look like API paths ("/pets/{id}",
//! `/pets/${id}`, "/pets/%s", "/pets/" + id) instead of parsing call syntax, so invented
//! endpoints are caught in any language.
use sdkforge_ir::Api;

use crate::GeneratedFile;

/// `/pets/{petId}`, `/pets/${id}`, `/pets/{$id}` and `/pets/%s` are all the same route.
fn normalize(path: &str) -> String {
    let chars: Vec<char> = path.chars().collect();
    let mut out = String::new();
    let mut depth = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '{' => {
                if depth == 0 {
                    out.push_str("{}");
                }
                depth += 1;
            }
            '}' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            // `${id}` (JS) and `{$id}` (PHP): the `$` belongs to the placeholder.
            '$' if chars.get(i + 1) == Some(&'{') => {}
            '%' if chars.get(i + 1).is_some_and(|n| "sdvq".contains(*n)) => {
                out.push_str("{}");
                i += 1;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

fn quotes(lang: &str) -> &'static [char] {
    match lang {
        "python" | "php" => &['"', '\''],
        "node" => &['"', '\'', '`'],
        // `'` is a lifetime marker in Rust and a rune in Go, never a path string.
        "go" => &['"', '`'],
        _ => &['"'],
    }
}

/// String literal bodies. Single-line quotes that never close are not literals (apostrophes in comments).
fn literals(code: &str, lang: &str) -> Vec<String> {
    let quotes = quotes(lang);
    let chars: Vec<char> = code.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let q = chars[i];
        if !quotes.contains(&q) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut body = String::new();
        let mut closed = false;
        while j < chars.len() {
            match chars[j] {
                '\\' if q != '`' => {
                    body.push(chars.get(j + 1).copied().unwrap_or(' '));
                    j += 2;
                    continue;
                }
                c if c == q => {
                    closed = true;
                    break;
                }
                '\n' if q != '`' => break,
                c => body.push(c),
            }
            j += 1;
        }
        if closed {
            out.push(body);
            i = j + 1;
        } else {
            i += 1;
        }
    }
    out
}

fn looks_like_path(norm: &str) -> bool {
    norm.len() > 1
        && norm.starts_with('/')
        && !norm.contains("//")
        && norm
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-.{}/~".contains(c))
}

/// Problems found, one per invented path. Empty means the code only uses paths from the IR.
pub fn check(api: &Api, lang: &str, files: &[GeneratedFile]) -> Vec<String> {
    let known: Vec<String> = api
        .resources
        .iter()
        .flat_map(|r| &r.operations)
        .map(|o| normalize(&o.path))
        .collect();
    let mut problems = Vec::new();
    for f in files
        .iter()
        .filter(|f| !crate::port::is_test_path(&f.path) && is_code(&f.path))
    {
        for lit in literals(&f.content, lang) {
            let n = normalize(&lit);
            if !looks_like_path(&n) {
                continue;
            }
            // Equal, a leading piece of a longer path ("/pets" or "/pets/" + id),
            // or a trailing piece ("/items" appended after the id).
            let ok = known.iter().any(|p| {
                *p == n
                    || (p.starts_with(&n) && (n.ends_with('/') || p[n.len()..].starts_with('/')))
                    || p.ends_with(&n)
            });
            if !ok {
                problems.push(format!(
                    "{}: uses the path \"{lit}\", which is not in the API model",
                    f.path
                ));
            }
        }
    }
    problems.dedup();
    problems
}

fn is_code(path: &str) -> bool {
    matches!(
        path.rsplit_once('.').map(|(_, e)| e),
        Some("py" | "js" | "mjs" | "cjs" | "ts" | "php" | "rs" | "go")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdkforge_ir::{Operation, Resource};

    fn api() -> Api {
        let op = |p: &str| Operation {
            id: "x".into(),
            method: "GET".into(),
            path: p.into(),
            params: vec![],
            request: None,
            responses: vec![],
        };
        Api {
            resources: vec![Resource {
                name: "pets".into(),
                operations: vec![op("/pets"), op("/pets/{id}"), op("/pets/{id}/toys")],
            }],
            ..Api::default()
        }
    }

    fn file(path: &str, code: &str) -> Vec<GeneratedFile> {
        vec![GeneratedFile {
            path: path.into(),
            content: code.into(),
        }]
    }

    #[test]
    fn accepts_known_paths_in_every_language() {
        let cases = [
            ("python", "a.py", "self._client.request(\"GET\", \"/pets/\" + quote(id))\nself._client.request('GET', f'/pets/{pet_id}/toys')"),
            ("node", "a.js", "await this.client.request('GET', `/pets/${id}`);\nthis.request(\"GET\", \"/pets\")"),
            ("go", "a.go", "c.request(ctx, \"GET\", fmt.Sprintf(\"/pets/%s\", id), nil)\n r := 'x'"),
            ("php", "a.php", "$this->client->request('GET', '/pets/' . rawurlencode($id));\n$this->request('GET', \"/pets/{$id}/toys\");"),
            ("rust", "a.rs", "self.client.request(\"GET\", &format!(\"/pets/{}\", id))?;\nfn f<'a>(x: &'a str) {}"),
        ];
        for (lang, path, code) in cases {
            assert_eq!(
                check(&api(), lang, &file(path, code)),
                Vec::<String>::new(),
                "{lang}"
            );
        }
    }

    #[test]
    fn rejects_invented_endpoints() {
        let code = "self._client.request(\"GET\", \"/owners\")\nself._client.request(\"GET\", f\"/pets/{id}/vaccines\")";
        assert_eq!(check(&api(), "python", &file("pets.py", code)).len(), 2);
    }

    #[test]
    fn ignores_tests_prose_and_apostrophes() {
        let code = "# don't do this\nurl = \"https://api.x.com/anything\"\nmsg = \"not a /path with spaces\"";
        assert!(check(&api(), "python", &file("client.py", code)).is_empty());
        assert!(check(
            &api(),
            "python",
            &file("tests/test_x.py", "x = \"/owners\"")
        )
        .is_empty());
    }
}
