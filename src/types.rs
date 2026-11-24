//! Frontend DTOs.
//!
//! Mirror of `src-tauri/src/model.rs`. Kept in sync manually — no code
//! generation. If you add a field on the backend, add it here too, or IPC
//! deserialization will drop the field silently on the frontend side.
//!
//! Watch out for numeric types: `u128` in Rust silently overflows JSON's
//! number range and comes back as a float that fails to deserialize back to
//! `u128`. Stick to `u64` at the IPC boundary.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct KV {
    pub name: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Body {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub json: Option<String>,
    pub text: Option<String>,
    pub data: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AuthConfig {
    pub mode: String,
    pub token: String,
    pub key: String,
    #[serde(rename = "in")]
    pub r#in: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HttpSpec {
    pub method: String,
    pub url: String,
    pub headers: Vec<KV>,
    pub params: Vec<KV>,
    pub body: Body,
    pub auth: Option<String>,
    pub auth_config: Option<AuthConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Info {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub seq: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Request {
    pub info: Info,
    pub http: HttpSpec,
    pub mcp_schema: Option<serde_json::Value>,
    pub path: String,
    pub rel_path: String,
    pub id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
    pub secret: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Env {
    pub name: String,
    pub variables: Vec<EnvVar>,
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Collection {
    pub root: String,
    pub requests: Vec<Request>,
    pub envs: Vec<Env>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SuggestedVar {
    pub name: String,
    pub secret: bool,
    pub value: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SampleOp {
    pub method: String,
    pub name: String,
    pub folder: String,
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ImportPreview {
    pub title: String,
    pub version: String,
    pub op_count: usize,
    pub folder_count: usize,
    pub suggested_vars: Vec<SuggestedVar>,
    pub sample_ops: Vec<SampleOp>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ImportStats {
    pub written: usize,
    pub skipped_existing: usize,
    pub filled_empty_bodies: usize,
    pub folder_prefix: String,
    pub env_created: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FiredResponse {
    pub status: u16,
    pub status_text: String,
    pub latency_ms: u64,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub is_json: bool,
    pub final_url: String,
}

#[derive(Clone, Deserialize)]
pub struct McpHelp {
    pub status: McpStatus,
    pub enabled: bool,
    pub client_config: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum McpStatus {
    Disabled,
    Starting,
    Ready,
    Error { message: String },
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NotesPreferences {
    pub dark: bool,
    pub text_size: u8,
    pub always_on_top: bool,
}
impl Default for NotesPreferences {
    fn default() -> Self {
        Self {
            dark: false,
            text_size: 15,
            always_on_top: true,
        }
    }
}

#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedNote {
    pub id: String,
    pub title: String,
    pub html: String,
}
#[derive(serde::Deserialize)]
pub struct DeletedNote {
    pub note: SavedNote,
}
#[derive(serde::Deserialize)]
pub struct NotesBook {
    pub active_id: String,
    pub notes: Vec<SavedNote>,
    #[serde(default = "already_onboarded")]
    pub onboarding_complete: bool,
    #[serde(default)]
    pub deleted_note: Option<DeletedNote>,
    #[serde(default)]
    pub warning: Option<String>,
}

fn already_onboarded() -> bool {
    true
}
