//! Canonical data model.
//!
//! These structs are the source of truth for both YAML on disk (Bruno format)
//! and the JSON that crosses the Tauri IPC boundary to the WASM frontend. Keep
//! `types.rs` in `src/` in shape-sync with this file.
//!
//! Runtime-only fields (`path`, `rel_path`, `id`) use
//! `skip_serializing_if = "String::is_empty"` rather than `#[serde(skip)]` so
//! they survive the trip to the frontend but are stripped before yaml write.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KV {
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Body {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthConfig {
    /// "none" | "inherit" | "bearer" | "api_key" | "basic"
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,
    /// bearer token or basic password; may contain {{vars}}
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
    /// api_key header/query name (e.g. "X-API-Key") or basic username
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// "header" | "query" — where to put the api_key
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub r#in: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HttpSpec {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<KV>,
    #[serde(default)]
    pub params: Vec<KV>,
    #[serde(default)]
    pub body: Body,
    /// Legacy Bruno string field — kept for round-trip compat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_config: Option<AuthConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Info {
    pub name: String,
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub seq: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub info: Info,
    pub http: HttpSpec,

    // runtime metadata — stripped before yaml write in loader
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rel_path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EnvVar {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Env {
    pub name: String,
    #[serde(default)]
    pub variables: Vec<EnvVar>,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Collection {
    pub root: String,
    pub requests: Vec<Request>,
    pub envs: Vec<Env>,
}
