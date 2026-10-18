//! Shared by the Postman and Bruno adapters: both describe an API as example requests,
//! so types are inferred deterministically from the example JSON.
use sdkforge_ir::{Api, Field, Operation, Param, ParamLocation, Resource, Response, TypeDef, TypeRef};
use serde_json::Value;

pub struct RawRequest {
    pub name: String,
    /// Folder the request lives in, if any.
    pub folder: Option<String>,
    pub method: String,
    pub url: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Value>,
    /// (status, example body)
    pub responses: Vec<(u16, Option<Value>)>,
}

pub fn pascal(s: &str) -> String {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default()
        })
        .collect()
}

fn camel(s: &str) -> String {
    let p = pascal(s);
    let mut c = p.chars();
    c.next().map(|f| f.to_ascii_lowercase().to_string() + c.as_str()).unwrap_or_default()
}

/// Strip `{{var}}` / scheme / host prefix, drop the query string, turn `:id` into `{id}`.
pub fn path_of(url: &str) -> String {
    let mut u = url.trim();
    if let Some(rest) = u.strip_prefix("{{") {
        u = rest.split_once("}}").map_or("", |(_, r)| r);
    } else if let Some((_, rest)) = u.split_once("://") {
        u = rest.find('/').map_or("", |i| &rest[i..]);
    }
    let u = u.split(['?', '#']).next().unwrap_or("");
    let path: Vec<String> = u
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| match s.strip_prefix(':') {
            Some(name) => format!("{{{name}}}"),
            None => s.to_string(),
        })
        .collect();
    format!("/{}", path.join("/"))
}

fn scalar_type(v: &str) -> TypeRef {
    if v.parse::<i64>().is_ok() {
        TypeRef::Integer
    } else if v.parse::<f64>().is_ok() {
        TypeRef::Number
    } else if matches!(v, "true" | "false") {
        TypeRef::Boolean
    } else {
        TypeRef::String
    }
}

/// Infer a type from an example value, registering named object types in `api`.
fn infer(api: &mut Api, name: &str, v: &Value) -> (TypeRef, bool) {
    match v {
        Value::Null => (TypeRef::Any, true),
        Value::Bool(_) => (TypeRef::Boolean, false),
        Value::Number(n) => (if n.is_i64() || n.is_u64() { TypeRef::Integer } else { TypeRef::Number }, false),
        Value::String(_) => (TypeRef::String, false),
        Value::Array(a) => {
            let inner = a.first().map_or(TypeRef::Any, |f| infer(api, name, f).0);
            (TypeRef::Array(Box::new(inner)), false)
        }
        Value::Object(o) => {
            let fields = o
                .iter()
                .map(|(k, fv)| {
                    let (ty, nullable) = infer(api, &format!("{name}{}", pascal(k)), fv);
                    // One example cannot prove optionality, so every seen field is required.
                    Field { name: k.clone(), ty, required: true, nullable }
                })
                .collect();
            if !api.types.iter().any(|t| t.name == name) {
                api.types.push(TypeDef { name: name.to_string(), fields });
            }
            (TypeRef::Named(name.to_string()), false)
        }
    }
}

pub fn build_api(name: &str, requests: Vec<RawRequest>) -> Api {
    let mut api = Api { name: name.to_string(), version: "0.0.0".into(), ..Api::default() };
    let mut seen_ids: Vec<String> = Vec::new();

    for r in requests {
        let path = path_of(&r.url);
        let mut id = camel(&r.name);
        if id.is_empty() {
            id = format!("{}{}", r.method.to_lowercase(), pascal(&path));
        }
        let base = id.clone();
        let mut n = 2;
        while seen_ids.contains(&id) {
            id = format!("{base}{n}");
            n += 1;
        }
        seen_ids.push(id.clone());

        let mut params: Vec<Param> = path
            .split('/')
            .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
            .map(|n| Param { name: n.to_string(), location: ParamLocation::Path, required: true, ty: TypeRef::String })
            .collect();
        for (k, v) in &r.query {
            if !params.iter().any(|p| p.name == *k) {
                params.push(Param { name: k.clone(), location: ParamLocation::Query, required: false, ty: scalar_type(v) });
            }
        }

        let request = r.body.as_ref().map(|b| infer(&mut api, &format!("{}Request", pascal(&id)), b).0);
        let responses = r
            .responses
            .iter()
            .map(|(status, body)| Response {
                status: *status,
                body: body.as_ref().map(|b| infer(&mut api, &format!("{}Response", pascal(&id)), b).0),
            })
            .collect();

        let group = r
            .folder
            .clone()
            .or_else(|| path.split('/').find(|s| !s.is_empty() && !s.starts_with('{')).map(str::to_string))
            .unwrap_or_else(|| "default".into());
        let op = Operation { id, method: r.method.to_uppercase(), path, params, request, responses };
        match api.resources.iter_mut().find(|x| x.name == group) {
            Some(x) => x.operations.push(op),
            None => api.resources.push(Resource { name: group, operations: vec![op] }),
        }
    }
    api
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_paths() {
        assert_eq!(path_of("{{baseUrl}}/users/:id?x=1"), "/users/{id}");
        assert_eq!(path_of("https://api.test.com/v1/pets"), "/v1/pets");
        assert_eq!(path_of("{{baseUrl}}"), "/");
    }

    #[test]
    fn names_are_cased() {
        assert_eq!(camel("Get User by ID"), "getUserByID");
        assert_eq!(pascal("create-user"), "CreateUser");
    }
}
