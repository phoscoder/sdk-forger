use serde_json::Value;
use sdkforge_ir::{
    Api, Field, Operation, Param, ParamLocation, Resource, Response, TypeDef, TypeRef,
};

use crate::{Ingest, IngestError};

const METHODS: [&str; 7] = ["get", "put", "post", "delete", "options", "head", "patch"];

/// OpenAPI 3.x adapter. Accepts JSON or YAML.
pub struct OpenApi;

impl Ingest for OpenApi {
    fn ingest(&self, input: &str) -> Result<Api, IngestError> {
        let doc: Value = serde_yaml::from_str(input).map_err(|e| IngestError::Invalid(e.to_string()))?;
        if !doc.get("openapi").and_then(Value::as_str).is_some_and(|v| v.starts_with('3')) {
            return Err(IngestError::Invalid("expected an OpenAPI 3.x document".into()));
        }
        let info = &doc["info"];
        let mut api = Api {
            name: info["title"].as_str().unwrap_or("Api").to_string(),
            version: info["version"].as_str().unwrap_or("0.0.0").to_string(),
            base_url: doc["servers"][0]["url"].as_str().map(str::to_string),
            ..Api::default()
        };

        if let Some(schemas) = doc["components"]["schemas"].as_object() {
            for (name, schema) in schemas {
                api.types.push(type_def(&doc, name, schema));
            }
        }

        let Some(paths) = doc["paths"].as_object() else { return Ok(api) };
        for (path, item) in paths {
            let item = resolve(&doc, item);
            for method in METHODS {
                let Some(op) = item.get(method) else { continue };
                let op = resolve(&doc, op);
                let operation = operation(&doc, path, method, item, op);
                let group = resource_name(path, op);
                match api.resources.iter_mut().find(|r| r.name == group) {
                    Some(r) => r.operations.push(operation),
                    None => api.resources.push(Resource { name: group, operations: vec![operation] }),
                }
            }
        }
        Ok(api)
    }
}

/// Follow a local `#/...` `$ref` (depth-limited against cycles).
fn resolve<'a>(doc: &'a Value, mut v: &'a Value) -> &'a Value {
    for _ in 0..16 {
        let Some(r) = v.get("$ref").and_then(Value::as_str) else { break };
        match r.strip_prefix('#').and_then(|p| doc.pointer(p)) {
            Some(t) => v = t,
            None => break,
        }
    }
    v
}

fn ref_name(v: &Value) -> Option<String> {
    v.get("$ref")?.as_str()?.rsplit('/').next().map(str::to_string)
}

fn type_ref(doc: &Value, schema: &Value) -> TypeRef {
    if let Some(name) = ref_name(schema) {
        return TypeRef::Named(name);
    }
    match schema["type"].as_str() {
        Some("string") => TypeRef::String,
        Some("integer") => TypeRef::Integer,
        Some("number") => TypeRef::Number,
        Some("boolean") => TypeRef::Boolean,
        Some("array") => TypeRef::Array(Box::new(type_ref(doc, &schema["items"]))),
        _ => TypeRef::Any,
    }
}

fn type_def(doc: &Value, name: &str, schema: &Value) -> TypeDef {
    let required: Vec<&str> = schema["required"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let fields = schema["properties"]
        .as_object()
        .map(|props| {
            props
                .iter()
                .map(|(n, s)| Field {
                    name: n.clone(),
                    ty: type_ref(doc, s),
                    required: required.contains(&n.as_str()),
                    nullable: resolve(doc, s)["nullable"].as_bool().unwrap_or(false),
                })
                .collect()
        })
        .unwrap_or_default();
    TypeDef { name: name.to_string(), fields }
}

fn json_schema<'a>(doc: &'a Value, container: &'a Value) -> Option<&'a Value> {
    let container = resolve(doc, container);
    container["content"]
        .as_object()?
        .iter()
        .find(|(k, _)| k.contains("json"))
        .map(|(_, v)| &v["schema"])
}

fn operation(doc: &Value, path: &str, method: &str, item: &Value, op: &Value) -> Operation {
    let id = op["operationId"].as_str().map(str::to_string).unwrap_or_else(|| {
        let slug: String = path
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("{method}{slug}")
    });

    let mut params = Vec::new();
    for list in [&item["parameters"], &op["parameters"]] {
        for p in list.as_array().into_iter().flatten() {
            let p = resolve(doc, p);
            let location = match p["in"].as_str() {
                Some("path") => ParamLocation::Path,
                Some("query") => ParamLocation::Query,
                Some("header") => ParamLocation::Header,
                _ => continue,
            };
            let name = p["name"].as_str().unwrap_or_default().to_string();
            // Operation-level params override path-level ones of the same name.
            params.retain(|x: &Param| !(x.name == name && std::mem::discriminant(&x.location) == std::mem::discriminant(&location)));
            params.push(Param {
                name,
                location,
                required: p["required"].as_bool().unwrap_or(false) || matches!(location, ParamLocation::Path),
                ty: type_ref(doc, &p["schema"]),
            });
        }
    }

    let request = json_schema(doc, &op["requestBody"]).map(|s| type_ref(doc, s));
    let responses = op["responses"]
        .as_object()
        .map(|rs| {
            rs.iter()
                .filter_map(|(code, r)| {
                    Some(Response {
                        status: code.parse().ok()?,
                        body: json_schema(doc, r).map(|s| type_ref(doc, s)),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Operation { id, method: method.to_uppercase(), path: path.to_string(), params, request, responses }
}

/// Group by first tag, else first non-parameter path segment.
fn resource_name(path: &str, op: &Value) -> String {
    if let Some(tag) = op["tags"][0].as_str() {
        return tag.to_string();
    }
    path.split('/')
        .find(|s| !s.is_empty() && !s.starts_with('{'))
        .unwrap_or("default")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r##"
openapi: 3.0.3
info: { title: Acme, version: "1.2.0" }
servers: [{ url: https://api.acme.test }]
paths:
  /users/{id}:
    parameters:
      - { name: id, in: path, required: true, schema: { type: string } }
    get:
      operationId: getUser
      tags: [Users]
      responses:
        "200":
          content: { application/json: { schema: { $ref: "#/components/schemas/User" } } }
        "404": { description: not found }
  /users:
    post:
      requestBody:
        content: { application/json: { schema: { $ref: "#/components/schemas/User" } } }
      responses: { "201": { description: created } }
components:
  schemas:
    User:
      type: object
      required: [email]
      properties:
        email: { type: string }
        age: { type: integer, nullable: true }
        tags: { type: array, items: { type: string } }
"##;

    #[test]
    fn parses_spec_into_ir() {
        let api = OpenApi.ingest(SPEC).unwrap();
        assert_eq!(api.name, "Acme");
        assert_eq!(api.base_url.as_deref(), Some("https://api.acme.test"));
        assert_eq!(api.resources.len(), 2);
        let users = api.resources.iter().find(|r| r.name == "Users").unwrap();
        let get = &users.operations[0];
        assert_eq!(get.id, "getUser");
        assert_eq!(get.params.len(), 1);
        assert!(get.params[0].required);
        assert_eq!(get.responses.len(), 2);
        let user = &api.types[0];
        assert!(user.fields.iter().find(|f| f.name == "email").unwrap().required);
        assert!(user.fields.iter().find(|f| f.name == "age").unwrap().nullable);
        let post = &api.resources.iter().find(|r| r.name == "users").unwrap().operations[0];
        assert_eq!(post.id, "post_users");
    }

    #[test]
    fn rejects_non_openapi() {
        assert!(OpenApi.ingest("hello: world").is_err());
    }
}
