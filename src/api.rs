//! Tauri IPC wrappers.
//!
//! Every backend command in `src-tauri/src/lib.rs` gets a Rust-typed wrapper
//! here. The wrappers serialize args to JS via `serde-wasm-bindgen`, invoke
//! the command, and deserialize the response.
//!
//! Argument struct fields must match Rust snake_case backend params, but
//! Tauri v2 converts them to camelCase over the wire — hence the
//! `#[serde(rename = "…")]` attrs on multi-word arg fields.

use crate::types::*;
use serde::Serialize;
use serde_wasm_bindgen::{from_value, to_value};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = ["window", "__TAURI__", "core"], catch)]
    async fn invoke(cmd: &str, args: JsValue) -> Result<JsValue, JsValue>;
}

async fn call<T: for<'de> serde::Deserialize<'de>>(
    cmd: &str,
    args: impl Serialize,
) -> Result<T, String> {
    let a = to_value(&args).map_err(|e| e.to_string())?;
    match invoke(cmd, a).await {
        Ok(v) => from_value(v).map_err(|e| e.to_string()),
        Err(e) => Err(js_err(e)),
    }
}

async fn call_unit(cmd: &str, args: impl Serialize) -> Result<(), String> {
    let a = to_value(&args).map_err(|e| e.to_string())?;
    match invoke(cmd, a).await {
        Ok(_) => Ok(()),
        Err(e) => Err(js_err(e)),
    }
}

fn js_err(e: JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    format!("{:?}", e)
}

#[derive(Serialize)]
struct Empty {}

#[derive(Serialize)]
struct SetRootArgs<'a> {
    root: &'a str,
}
#[derive(Serialize)]
struct EnvVarArgs<'a> {
    env: &'a str,
    name: &'a str,
}
#[derive(Serialize)]
struct SetSecretArgs<'a> {
    env: &'a str,
    name: &'a str,
    value: &'a str,
}
#[derive(Serialize)]
struct EnvArg<'a> {
    env: &'a Env,
}
#[derive(Serialize)]
struct RequestArg<'a> {
    req: &'a Request,
}
#[derive(Serialize)]
struct CreateEnvArgs<'a> {
    name: &'a str,
    #[serde(rename = "templateFrom", skip_serializing_if = "Option::is_none")]
    template_from: Option<&'a str>,
}
#[derive(Serialize)]
struct CreateRequestArgs<'a> {
    folder: &'a str,
    name: &'a str,
    method: &'a str,
    url: &'a str,
}
#[derive(Serialize)]
struct FireArgs<'a> {
    #[serde(rename = "requestId")]
    request_id: &'a str,
    #[serde(rename = "envName", skip_serializing_if = "Option::is_none")]
    env_name: Option<&'a str>,
}

#[derive(Serialize)]
struct UnlockArgs<'a> {
    password: &'a str,
}

#[derive(serde::Deserialize)]
pub struct VaultStatus {
    pub unlocked: bool,
    pub exists: bool,
}

pub async fn vault_status() -> Result<VaultStatus, String> {
    call("vault_status", Empty {}).await
}
pub async fn vault_unlock(password: &str) -> Result<(), String> {
    call_unit("vault_unlock", UnlockArgs { password }).await
}
pub async fn vault_lock() -> Result<(), String> {
    call_unit("vault_lock", Empty {}).await
}

pub async fn get_root() -> Result<String, String> {
    call("get_root", Empty {}).await
}
pub async fn set_root(root: &str) -> Result<(), String> {
    call_unit("set_root", SetRootArgs { root }).await
}
pub async fn load() -> Result<Collection, String> {
    call("load", Empty {}).await
}
pub async fn get_secret(env: &str, name: &str) -> Result<Option<String>, String> {
    call("get_secret", EnvVarArgs { env, name }).await
}
pub async fn set_secret(env: &str, name: &str, value: &str) -> Result<(), String> {
    call_unit("set_secret", SetSecretArgs { env, name, value }).await
}
pub async fn delete_secret(env: &str, name: &str) -> Result<(), String> {
    call_unit("delete_secret", EnvVarArgs { env, name }).await
}
pub async fn save_env(env: &Env) -> Result<(), String> {
    call_unit("save_env", EnvArg { env }).await
}
pub async fn create_env(name: &str, template_from: Option<&str>) -> Result<Env, String> {
    call(
        "create_env",
        CreateEnvArgs {
            name,
            template_from,
        },
    )
    .await
}
#[derive(Serialize)]
struct RenameEnvArgs<'a> {
    #[serde(rename = "oldName")]
    old_name: &'a str,
    #[serde(rename = "newName")]
    new_name: &'a str,
}
pub async fn rename_env(old_name: &str, new_name: &str) -> Result<Env, String> {
    call("rename_env", RenameEnvArgs { old_name, new_name }).await
}
pub async fn delete_env(env: &Env) -> Result<(), String> {
    call_unit("delete_env", EnvArg { env }).await
}
pub async fn save_request(req: &Request) -> Result<(), String> {
    call_unit("save_request", RequestArg { req }).await
}
pub async fn create_request(
    folder: &str,
    name: &str,
    method: &str,
    url: &str,
) -> Result<String, String> {
    call(
        "create_request",
        CreateRequestArgs {
            folder,
            name,
            method,
            url,
        },
    )
    .await
}
#[derive(Serialize)]
struct ImportCurlArgs<'a> {
    input: &'a str,
    folder: &'a str,
    name: &'a str,
}
pub async fn import_curl(input: &str, folder: &str, name: &str) -> Result<String, String> {
    call(
        "import_curl",
        ImportCurlArgs {
            input,
            folder,
            name,
        },
    )
    .await
}
#[derive(Serialize)]
struct ParseCurlArgs<'a> {
    input: &'a str,
}
pub async fn parse_curl(input: &str) -> Result<Request, String> {
    call("parse_curl", ParseCurlArgs { input }).await
}

#[derive(Serialize)]
struct PreviewOpenApiArgs<'a> {
    #[serde(rename = "specPath")]
    spec_path: &'a str,
}
pub async fn preview_openapi(spec_path: &str) -> Result<ImportPreview, String> {
    call("preview_openapi", PreviewOpenApiArgs { spec_path }).await
}

#[derive(Serialize)]
struct ImportOpenApiArgs<'a> {
    #[serde(rename = "specPath")]
    spec_path: &'a str,
    #[serde(rename = "folderPrefix")]
    folder_prefix: &'a str,
    #[serde(rename = "createEnv")]
    create_env: bool,
    #[serde(rename = "envName")]
    env_name: &'a str,
}
pub async fn import_openapi(
    spec_path: &str,
    folder_prefix: &str,
    create_env: bool,
    env_name: &str,
) -> Result<ImportStats, String> {
    call(
        "import_openapi",
        ImportOpenApiArgs {
            spec_path,
            folder_prefix,
            create_env,
            env_name,
        },
    )
    .await
}
pub async fn fire_request(
    request_id: &str,
    env_name: Option<&str>,
) -> Result<FiredResponse, String> {
    call(
        "fire_request",
        FireArgs {
            request_id,
            env_name,
        },
    )
    .await
}

#[derive(Serialize)]
struct RequestIdArg<'a> {
    #[serde(rename = "requestId")]
    request_id: &'a str,
}
pub async fn duplicate_request(request_id: &str) -> Result<String, String> {
    call("duplicate_request", RequestIdArg { request_id }).await
}
pub async fn delete_request(req: &Request) -> Result<(), String> {
    call_unit("delete_request", RequestArg { req }).await
}
#[derive(Serialize)]
struct RenameRequestArgs<'a> {
    #[serde(rename = "requestId")]
    request_id: &'a str,
    #[serde(rename = "newName")]
    new_name: &'a str,
}
pub async fn rename_request(request_id: &str, new_name: &str) -> Result<String, String> {
    call(
        "rename_request",
        RenameRequestArgs {
            request_id,
            new_name,
        },
    )
    .await
}
pub async fn to_curl(request_id: &str, env_name: Option<&str>) -> Result<String, String> {
    call(
        "to_curl",
        FireArgs {
            request_id,
            env_name,
        },
    )
    .await
}
pub async fn load_history(request_id: &str) -> Result<Vec<FiredResponse>, String> {
    call("load_history", RequestIdArg { request_id }).await
}
pub async fn clear_history(request_id: &str) -> Result<(), String> {
    call_unit("clear_history", RequestIdArg { request_id }).await
}

/// Native OS folder picker via a custom Tauri command wrapper.
/// Frontend can't reliably reach plugin JS bindings from WASM without the
/// plugin's own JS shim, so we call our own #[tauri::command] that thin-wraps
/// the plugin's Rust API.
pub async fn pick_folder() -> Result<Option<String>, String> {
    call("pick_folder", Empty {}).await
}
