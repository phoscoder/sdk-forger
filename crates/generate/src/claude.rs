use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
pub enum ClaudeError {
    #[error("ANTHROPIC_API_KEY is not set")]
    MissingKey,
    #[error("request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
}

/// Minimal blocking client for the Claude Messages API.
pub struct Claude {
    http: reqwest::blocking::Client,
    key: String,
    pub model: String,
    pub max_tokens: u32,
    /// Running total of input+output tokens, checked against the run's budget.
    pub tokens_used: u64,
}

impl Claude {
    pub fn from_env(model: Option<String>) -> Result<Self, ClaudeError> {
        let key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| ClaudeError::MissingKey)?;
        Ok(Self {
            http: reqwest::blocking::Client::builder().timeout(std::time::Duration::from_secs(600)).build()?,
            key,
            model: model.unwrap_or_else(|| "claude-sonnet-5-5".into()),
            max_tokens: 16_000,
            tokens_used: 0,
        })
    }

    /// One request, retried on transport errors, 429 and 5xx (a long run must not die on a blip).
    pub fn complete(&mut self, system: &str, user: &str) -> Result<String, ClaudeError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.complete_once(system, user) {
                Err(e) if attempt < 4 && is_transient(&e) => {
                    std::thread::sleep(std::time::Duration::from_secs(2u64.pow(attempt)));
                }
                other => return other,
            }
        }
    }

    fn complete_once(&mut self, system: &str, user: &str) -> Result<String, ClaudeError> {
        let resp = self
            .http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.key)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model": self.model,
                "max_tokens": self.max_tokens,
                "system": system,
                "messages": [{ "role": "user", "content": user }],
            }))
            .send()?;
        let status = resp.status();
        let body: Value = resp.json()?;
        if !status.is_success() {
            return Err(ClaudeError::Api { status: status.as_u16(), body: body.to_string() });
        }
        self.tokens_used += body["usage"]["input_tokens"].as_u64().unwrap_or(0)
            + body["usage"]["output_tokens"].as_u64().unwrap_or(0);
        Ok(body["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["text"].as_str())
            .collect::<String>())
    }
}



fn is_transient(e: &ClaudeError) -> bool {
    match e {
        ClaudeError::Http(err) => err.is_timeout() || err.is_connect() || err.is_request(),
        ClaudeError::Api { status, .. } => *status == 429 || *status >= 500,
        ClaudeError::MissingKey => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_transient_errors() {
        assert!(is_transient(&ClaudeError::Api { status: 429, body: String::new() }));
        assert!(is_transient(&ClaudeError::Api { status: 529, body: String::new() }));
        assert!(!is_transient(&ClaudeError::Api { status: 400, body: String::new() }));
        assert!(!is_transient(&ClaudeError::MissingKey));
    }
}
