//! Intermediate model: the single source of truth every other crate reads.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Api {
    pub name: String,
    pub version: String,
    pub base_url: Option<String>,
    pub resources: Vec<Resource>,
    pub types: Vec<TypeDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resource {
    pub name: String,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub method: String,
    pub path: String,
    pub params: Vec<Param>,
    pub request: Option<TypeRef>,
    pub responses: Vec<Response>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub location: ParamLocation,
    pub required: bool,
    pub ty: TypeRef,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ParamLocation {
    Path,
    Query,
    Header,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub status: u16,
    pub body: Option<TypeRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDef {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub ty: TypeRef,
    pub required: bool,
    pub nullable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeRef {
    String,
    Integer,
    Number,
    Boolean,
    Any,
    Array(Box<TypeRef>),
    Named(String),
}
