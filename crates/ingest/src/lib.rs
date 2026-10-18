//! Deterministic parsers that turn API descriptions into the IR.
use sdkforge_ir::Api;

mod bruno;
mod collection;
mod openapi;
mod postman;
mod source;
pub use bruno::Bruno;
pub use openapi::OpenApi;
pub use source::{detect_lang, outline, outline_python, Lang, public_names, render_outline, FileOutline, Kind, Symbol};
pub use postman::{looks_like as looks_like_postman, Postman};

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("unsupported input: {0}")]
    Unsupported(String),
    #[error("invalid spec: {0}")]
    Invalid(String),
}

pub trait Ingest {
    fn ingest(&self, input: &str) -> Result<Api, IngestError>;
}

/// Pick the adapter from what `path` is (a Bruno folder, a Postman collection, or OpenAPI) and parse it.
pub fn ingest_path(path: &std::path::Path) -> Result<Api, IngestError> {
    if path.is_dir() {
        return Bruno.ingest_dir(path);
    }
    let text = std::fs::read_to_string(path).map_err(|e| IngestError::Invalid(format!("{}: {e}", path.display())))?;
    if looks_like_postman(&text) {
        Postman.ingest(&text)
    } else {
        OpenApi.ingest(&text)
    }
}
