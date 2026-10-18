use serde_json::Value;

use crate::collection::{build_api, RawRequest};
use crate::{Ingest, IngestError};

/// Postman Collection v2.x (JSON).
pub struct Postman;

impl Ingest for Postman {
    fn ingest(&self, input: &str) -> Result<sdkforge_ir::Api, IngestError> {
        let doc: Value =
            serde_json::from_str(input).map_err(|e| IngestError::Invalid(e.to_string()))?;
        if doc["info"].is_null() || !doc["item"].is_array() {
            return Err(IngestError::Invalid(
                "expected a Postman collection (`info` and `item`)".into(),
            ));
        }
        let mut out = Vec::new();
        walk(&doc["item"], None, &mut out);
        let name = doc["info"]["name"].as_str().unwrap_or("Api");
        Ok(build_api(name, out))
    }
}

/// True for JSON that looks like a Postman collection.
pub fn looks_like(input: &str) -> bool {
    serde_json::from_str::<Value>(input).is_ok_and(|d| {
        d["info"]["schema"]
            .as_str()
            .is_some_and(|s| s.contains("postman"))
            || d["info"]["_postman_id"].is_string()
    })
}

fn walk(items: &Value, folder: Option<&str>, out: &mut Vec<RawRequest>) {
    for item in items.as_array().into_iter().flatten() {
        if item["item"].is_array() {
            // Only the top-level folder names the resource; deeper folders flatten into it.
            let name = folder.or_else(|| item["name"].as_str());
            walk(&item["item"], name, out);
        } else if item["request"].is_object() {
            out.push(request(item, folder));
        }
    }
}

fn request(item: &Value, folder: Option<&str>) -> RawRequest {
    let req = &item["request"];
    let url = match &req["url"] {
        Value::String(s) => s.clone(),
        u => u["raw"].as_str().map(str::to_string).unwrap_or_else(|| {
            let parts: Vec<&str> = u["path"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            format!("/{}", parts.join("/"))
        }),
    };
    let query = req["url"]["query"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|q| !q["disabled"].as_bool().unwrap_or(false))
        .filter_map(|q| {
            Some((
                q["key"].as_str()?.to_string(),
                q["value"].as_str().unwrap_or("").to_string(),
            ))
        })
        .collect();
    let body = (req["body"]["mode"] == "raw")
        .then(|| req["body"]["raw"].as_str())
        .flatten()
        .and_then(|raw| serde_json::from_str(raw).ok());
    let responses = item["response"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let code = r["code"].as_u64()? as u16;
            Some((
                code,
                r["body"]
                    .as_str()
                    .and_then(|b| serde_json::from_str(b).ok()),
            ))
        })
        .collect();
    RawRequest {
        name: item["name"].as_str().unwrap_or("").to_string(),
        folder: folder.map(str::to_string),
        method: req["method"].as_str().unwrap_or("GET").to_string(),
        url,
        query,
        body,
        responses,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdkforge_ir::TypeRef;

    const COLLECTION: &str = r#"{
      "info": {"name": "Acme", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"},
      "item": [{
        "name": "Users",
        "item": [
          {"name": "Get User", "request": {"method": "GET", "url": {"raw": "{{baseUrl}}/users/:id?verbose=true"}},
           "response": [{"code": 200, "body": "{\"id\": 1, \"email\": \"a@b.c\", \"address\": {\"city\": \"X\"}, \"tags\": [\"a\"], \"nick\": null}"}]},
          {"name": "Create User", "request": {"method": "POST", "url": "{{baseUrl}}/users",
           "body": {"mode": "raw", "raw": "{\"email\": \"a@b.c\"}"}}, "response": [{"code": 201, "body": ""}]}
        ]
      }]
    }"#;

    #[test]
    fn parses_collection() {
        assert!(looks_like(COLLECTION));
        let api = Postman.ingest(COLLECTION).unwrap();
        assert_eq!(api.name, "Acme");
        assert_eq!(api.resources.len(), 1);
        let ops = &api.resources[0].operations;
        assert_eq!(ops[0].id, "getUser");
        assert_eq!(ops[0].path, "/users/{id}");
        assert_eq!(ops[0].params.len(), 1); // path param; `verbose` comes from url.query, not the raw string
        assert_eq!(ops[1].method, "POST");
        assert!(matches!(&ops[1].request, Some(TypeRef::Named(n)) if n == "CreateUserRequest"));
        let resp = api
            .types
            .iter()
            .find(|t| t.name == "GetUserResponse")
            .unwrap();
        assert!(resp.fields.iter().any(|f| f.name == "nick" && f.nullable));
        assert!(api.types.iter().any(|t| t.name == "GetUserResponseAddress"));
    }

    #[test]
    fn rejects_other_json() {
        assert!(Postman.ingest("{\"openapi\": \"3.0.0\"}").is_err());
    }
}
