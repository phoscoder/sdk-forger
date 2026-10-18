use std::path::Path;

use crate::collection::{build_api, RawRequest};
use crate::IngestError;

const VERBS: [&str; 7] = ["get", "post", "put", "delete", "patch", "head", "options"];

/// Bruno collection folder of `.bru` files.
pub struct Bruno;

impl Bruno {
    pub fn ingest_dir(&self, root: &Path) -> Result<sdkforge_ir::Api, IngestError> {
        let mut requests = Vec::new();
        walk(root, root, &mut requests)?;
        if requests.is_empty() {
            return Err(IngestError::Invalid("no .bru requests found".into()));
        }
        // Stable output regardless of directory order.
        requests.sort_by(|a, b| (&a.folder, &a.name).cmp(&(&b.folder, &b.name)));
        let name = std::fs::read_to_string(root.join("bruno.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v["name"].as_str().map(str::to_string))
            .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "Api".into());
        Ok(build_api(&name, requests))
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<RawRequest>) -> Result<(), IngestError> {
    let io = |e: std::io::Error| IngestError::Invalid(format!("{}: {e}", dir.display()));
    for entry in std::fs::read_dir(dir).map_err(io)? {
        let path = entry.map_err(io)?.path();
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if path.is_dir() {
            if file != "environments" && file != "node_modules" && !file.starts_with('.') {
                walk(root, &path, out)?;
            }
        } else if file.ends_with(".bru") && file != "collection.bru" && file != "folder.bru" {
            let text = std::fs::read_to_string(&path).map_err(io)?;
            let folder = path
                .parent()
                .and_then(|p| p.strip_prefix(root).ok())
                .and_then(|p| p.components().next())
                .map(|c| c.as_os_str().to_string_lossy().into_owned());
            if let Some(req) = parse(&text, folder, &file) {
                out.push(req);
            }
        }
    }
    Ok(())
}

/// Split into `name { ... }` blocks; a block ends at a `}` in column 0.
fn blocks(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur: Option<(String, Vec<&str>)> = None;
    for line in text.lines() {
        match &mut cur {
            None => {
                if let Some(head) = line.strip_suffix('{') {
                    cur = Some((head.trim().to_string(), Vec::new()));
                }
            }
            Some((name, body)) => {
                if line.trim_end() == "}" {
                    out.push((name.clone(), body.join("\n")));
                    cur = None;
                } else {
                    body.push(line);
                }
            }
        }
    }
    out
}

fn pairs(body: &str) -> Vec<(String, String)> {
    body.lines()
        .filter(|l| !l.trim_start().starts_with('~')) // `~` marks a disabled entry
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn parse(text: &str, folder: Option<String>, file: &str) -> Option<RawRequest> {
    let bs = blocks(text);
    let get = |n: &str| bs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
    let (verb, vbody) = bs.iter().find(|(k, _)| VERBS.contains(&k.as_str()))?;
    let url = pairs(vbody).into_iter().find(|(k, _)| k == "url")?.1;
    let name = pairs(get("meta").unwrap_or(""))
        .into_iter()
        .find(|(k, _)| k == "name")
        .map(|(_, v)| v)
        .unwrap_or_else(|| file.trim_end_matches(".bru").to_string());
    let body = get("body:json").and_then(|b| serde_json::from_str(b).ok());
    Some(RawRequest {
        name,
        folder,
        method: verb.to_uppercase(),
        url,
        query: pairs(get("params:query").unwrap_or("")),
        body,
        responses: Vec::new(), // Bruno stores no response examples; types come from the request side only
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_bru_file() {
        let t = "meta {\n  name: Create Pet\n  type: http\n}\n\npost {\n  url: {{baseUrl}}/pets/:owner\n  body: json\n}\n\nparams:query {\n  dry: true\n  ~skip: 1\n}\n\nbody:json {\n  {\n    \"name\": \"Rex\"\n  }\n}\n";
        let r = parse(t, Some("pets".into()), "create.bru").unwrap();
        assert_eq!(r.name, "Create Pet");
        assert_eq!(r.method, "POST");
        assert_eq!(r.url, "{{baseUrl}}/pets/:owner");
        assert_eq!(r.query, vec![("dry".to_string(), "true".to_string())]);
        assert_eq!(r.body.unwrap()["name"], "Rex");
    }

    #[test]
    fn ingests_a_folder() {
        let d = std::env::temp_dir().join(format!("sdkforge-bru-{}", std::process::id()));
        std::fs::create_dir_all(d.join("pets")).unwrap();
        std::fs::create_dir_all(d.join("environments")).unwrap();
        std::fs::write(d.join("bruno.json"), r#"{"name": "Pet API"}"#).unwrap();
        std::fs::write(d.join("environments/local.bru"), "vars {\n  baseUrl: http://x\n}\n").unwrap();
        std::fs::write(d.join("pets/list.bru"), "meta {\n  name: List Pets\n}\n\nget {\n  url: {{baseUrl}}/pets\n}\n").unwrap();
        let api = Bruno.ingest_dir(&d).unwrap();
        assert_eq!(api.name, "Pet API");
        assert_eq!(api.resources[0].name, "pets");
        assert_eq!(api.resources[0].operations[0].id, "listPets");
        std::fs::remove_dir_all(d).ok();
    }
}
