//! Mock server pieces. The recorder captures the HTTP traffic of an SDK driver so two SDKs
//! (an original and its port) can be compared request by request.
use std::collections::{BTreeMap, BTreeSet};

use sdkforge_ir::Api;
use serde::Deserialize;
use serde_json::Value;

/// Python (standard library only) recording server; run inside a container.
pub const RECORDER_PY: &str = include_str!("recorder.py");

pub struct MockServer {
    pub api: Api,
}

impl MockServer {
    pub fn new(api: Api) -> Self {
        Self { api }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Entry {
    pub scenario: Option<String>,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub body: Value,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Recording {
    #[serde(default)]
    pub requests: Vec<Entry>,
    #[serde(default)]
    pub outcomes: BTreeMap<String, String>,
}

fn describe(e: &Entry) -> String {
    let query: Vec<String> = e
        .query
        .iter()
        .map(|(k, v)| format!("{k}={}", v.join(",")))
        .collect();
    let q = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", query.join("&"))
    };
    let body = if e.body.is_null() {
        String::new()
    } else {
        format!(" body={}", e.body)
    };
    let headers = if e.headers.is_empty() {
        String::new()
    } else {
        format!(" headers={:?}", e.headers)
    };
    format!("{} {}{q}{body}{headers}", e.method, e.path)
}

/// Differences between the original SDK's traffic and the port's, per scenario.
/// Empty means: same requests in, same requests out, same success/error outcome.
pub fn compare(expected: &Recording, actual: &Recording) -> Vec<String> {
    let ids: BTreeSet<String> = expected
        .requests
        .iter()
        .chain(&actual.requests)
        .map(|r| r.scenario.clone().unwrap_or_else(|| "(none)".into()))
        .chain(expected.outcomes.keys().cloned())
        .chain(actual.outcomes.keys().cloned())
        .collect();
    let of = |rec: &Recording, id: &str| -> Vec<Entry> {
        rec.requests
            .iter()
            .filter(|r| r.scenario.as_deref().unwrap_or("(none)") == id)
            .cloned()
            .collect()
    };

    let mut problems = Vec::new();
    for id in ids {
        let (e, a) = (of(expected, &id), of(actual, &id));
        if e.len() != a.len() {
            problems.push(format!(
                "[{id}] the original sent {} request(s), the port sent {}",
                e.len(),
                a.len()
            ));
        }
        if let Some((i, (x, y))) = e.iter().zip(&a).enumerate().find(|(_, (x, y))| x != y) {
            problems.push(format!(
                "[{id}] request #{}:\n    original: {}\n    port:     {}",
                i + 1,
                describe(x),
                describe(y)
            ));
        }
        let (eo, ao) = (expected.outcomes.get(&id), actual.outcomes.get(&id));
        if eo != ao {
            problems.push(format!("[{id}] outcome: original {:?}, port {:?}", eo, ao));
        }
    }
    problems
}

/// Result of a comparison where some scenarios' differences are knowingly accepted.
#[derive(Debug, Default)]
pub struct Comparison {
    /// Differences that must be fixed.
    pub blocking: Vec<String>,
    /// Differences in accepted scenarios, kept so they can be documented.
    pub accepted: Vec<String>,
}

/// Like [`compare`], but differences in the scenarios listed in `accepted_ids` don't block.
pub fn compare_accepting(
    expected: &Recording,
    actual: &Recording,
    accepted_ids: &[String],
) -> Comparison {
    let mut out = Comparison::default();
    for problem in compare(expected, actual) {
        let accepted = accepted_ids
            .iter()
            .any(|id| problem.starts_with(&format!("[{id}]")));
        if accepted {
            out.accepted.push(problem);
        } else {
            out.blocking.push(problem);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(scenario: &str, method: &str, path: &str, body: Value) -> Entry {
        Entry {
            scenario: Some(scenario.into()),
            method: method.into(),
            path: path.into(),
            query: BTreeMap::new(),
            body,
            headers: BTreeMap::new(),
        }
    }

    fn rec(requests: Vec<Entry>, outcome: &str) -> Recording {
        Recording {
            requests,
            outcomes: BTreeMap::from([("s1".to_string(), outcome.to_string())]),
        }
    }

    #[test]
    fn identical_traffic_matches() {
        let a = rec(vec![entry("s1", "GET", "/pets", Value::Null)], "ok");
        assert!(compare(&a, &a.clone()).is_empty());
    }

    #[test]
    fn reports_differences() {
        let a = rec(
            vec![
                entry("s1", "GET", "/pets", Value::Null),
                entry("s1", "GET", "/pets", Value::Null),
            ],
            "ok",
        );
        let b = rec(vec![entry("s1", "GET", "/pets/", Value::Null)], "error");
        let p = compare(&a, &b);
        assert!(p
            .iter()
            .any(|m| m.contains("sent 2 request(s), the port sent 1")));
        assert!(p.iter().any(|m| m.contains("request #1")));
        assert!(p.iter().any(|m| m.contains("outcome")));
    }

    #[test]
    fn recorder_script_is_embedded() {
        assert!(RECORDER_PY.contains("/__scenario"));
    }

    #[test]
    fn accepted_scenarios_do_not_block() {
        let a = rec(vec![entry("s1", "GET", "/pets", Value::Null)], "ok");
        let b = rec(vec![entry("s1", "GET", "/pets", Value::Null)], "error");
        let c = compare_accepting(&a, &b, &["s1".to_string()]);
        assert!(c.blocking.is_empty() && c.accepted.len() == 1);
        let c = compare_accepting(&a, &b, &["other".to_string()]);
        assert_eq!(c.blocking.len(), 1);
    }
}
