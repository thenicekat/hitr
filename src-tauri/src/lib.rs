//! Tauri host process. Owns filesystem, network, and the encrypted vault.
//!
//! Every function marked `#[tauri::command]` is invocable from the Dioxus
//! frontend via `invoke("cmd_name", args)`. The dispatch table is registered
//! in `run()` at the bottom of this file — new commands must be added there.
//!
//! ## AppState
//!
//! - `root: PathBuf` — collection root, persisted to `config.json`
//! - `collection: Option<Collection>` — cached parse of the collection tree,
//!   populated by `load` and re-used by `fire_request`
//! - `vault: VaultState` — password + decrypted vault cache; see `vault.rs`
//!
//! All three are behind Mutexes; long-running work (`load`) is dispatched to
//! `spawn_blocking` so the IPC event loop stays responsive.

mod curl;
mod history;
mod http;
mod loader;
#[cfg(unix)]
pub mod mcp;
mod mcp_inputs;
mod model;
#[cfg(target_os = "macos")]
mod notes_panel;
mod notes_store;
mod notes_window;
mod openapi;
mod updates;
mod vault;

use crate::model::*;
use crate::vault::VaultState;
use age::secrecy::SecretString;
use directories::BaseDirs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::AppHandle;
use tauri::{Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

struct AppState {
    root: Mutex<PathBuf>,
    collection: Arc<Mutex<Option<Collection>>>,
    vault: Arc<VaultState>,
    #[cfg(unix)]
    mcp_status: Mutex<mcp::Status>,
    #[cfg(unix)]
    mcp_access: Arc<mcp::Access>,
}

fn require_password(state: &AppState) -> Result<SecretString, String> {
    let p = state.vault.password.lock().unwrap();
    p.clone().ok_or_else(|| "vault_locked".to_string())
}

fn ensure_cache(state: &AppState) -> Result<(), String> {
    let mut cache = state.vault.cache.lock().unwrap();
    if cache.is_some() {
        return Ok(());
    }
    let pw = require_password(state)?;
    *cache = Some(vault::read_encrypted(&pw)?);
    Ok(())
}

fn default_root() -> PathBuf {
    if let Some(bd) = BaseDirs::new() {
        return bd.home_dir().join("collections/default");
    }
    PathBuf::from(".")
}

fn config_path() -> Option<PathBuf> {
    let bd = BaseDirs::new()?;
    Some(bd.config_dir().join("hitr").join("config.json"))
}

fn read_root_from_config() -> Option<PathBuf> {
    let p = config_path()?;
    let s = std::fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("root").and_then(|r| r.as_str()).map(PathBuf::from)
}

fn write_root_to_config(root: &std::path::Path) -> Result<(), String> {
    let p = config_path().ok_or_else(|| "no config dir".to_string())?;
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let v = serde_json::json!({ "root": root.to_string_lossy() });
    std::fs::write(p, serde_json::to_string_pretty(&v).unwrap()).map_err(|e| e.to_string())
}

#[tauri::command]
fn get_root(state: State<AppState>) -> String {
    state.root.lock().unwrap().to_string_lossy().to_string()
}

#[tauri::command]
fn set_root(state: State<AppState>, root: String) -> Result<(), String> {
    let p = PathBuf::from(&root);
    if !p.is_dir() {
        return Err(format!("not a directory: {}", root));
    }
    write_root_to_config(&p)?;
    let mut active_root = state.root.lock().unwrap();
    let mut collection = state.collection.lock().unwrap();
    *active_root = p;
    *collection = None;
    #[cfg(unix)]
    state.mcp_access.revoke();
    Ok(())
}

#[tauri::command]
async fn load(state: State<'_, AppState>) -> Result<Collection, String> {
    let root = state.root.lock().unwrap().clone();
    let load_root = root.clone();
    let c = tokio::task::spawn_blocking(move || loader::load_collection(&load_root))
        .await
        .map_err(|e| e.to_string())??;
    let active_root = state.root.lock().unwrap();
    if *active_root != root {
        return Err("Collection changed while loading. Refresh and try again.".into());
    }
    *state.collection.lock().unwrap() = Some(c.clone());
    Ok(c)
}

#[tauri::command]
fn vault_status(state: State<AppState>) -> serde_json::Value {
    let unlocked = state.vault.password.lock().unwrap().is_some();
    let exists = vault::vault_exists();
    serde_json::json!({ "unlocked": unlocked, "exists": exists })
}

#[tauri::command]
fn mcp_help(state: State<'_, AppState>) -> serde_json::Value {
    #[cfg(unix)]
    {
        let enabled = state.mcp_access.enabled();
        let status = if enabled {
            state.mcp_status.lock().unwrap().clone()
        } else {
            mcp::Status::Disabled
        };
        let executable = std::env::current_exe()
            .ok()
            .and_then(|path| path.into_os_string().into_string().ok());
        let client_config = executable.map(|command| {
            serde_json::to_string_pretty(&serde_json::json!({
                "mcpServers": {
                    "hitr": { "command": command, "args": ["mcp"] }
                }
            }))
            .expect("Hitr MCP client configuration is serializable")
        });
        serde_json::json!({
            "status": status,
            "enabled": enabled,
            "full_response": state.mcp_access.full_response(),
            "allowed_envs": state.mcp_access.allowed_envs().unwrap_or_default(),
            "allowed_envs_error": state.mcp_access.allowed_envs().err(),
            "client_config": client_config
        })
    }
    #[cfg(not(unix))]
    {
        let _ = state;
        serde_json::json!({"status": {"state": "unsupported"}, "enabled": false})
    }
}

#[tauri::command]
fn set_mcp_full_response(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    #[cfg(unix)]
    {
        state
            .mcp_access
            .set_full_response(enabled)
            .map_err(|e| format!("Could not save the MCP response setting: {e}"))
    }
    #[cfg(not(unix))]
    {
        let _ = (state, enabled);
        Err("MCP is unavailable on this platform.".into())
    }
}

#[tauri::command]
fn set_mcp_allowed_envs(state: State<'_, AppState>, names: Vec<String>) -> Result<(), String> {
    #[cfg(unix)]
    {
        state
            .mcp_access
            .set_allowed_envs(names)
            .map_err(|e| format!("Could not save MCP restrictions: {e}"))
    }
    #[cfg(not(unix))]
    {
        let _ = (state, names);
        Err("MCP is unavailable on this platform.".into())
    }
}

#[tauri::command]
fn set_mcp_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        state
            .mcp_access
            .set_enabled(enabled)
            .map_err(|error| format!("Could not save the MCP setting: {error}"))?;
        if enabled {
            mcp::start(app);
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (app, state, enabled);
        Err("MCP is unavailable on this platform.".into())
    }
}

#[tauri::command]
async fn vault_unlock(state: State<'_, AppState>, password: String) -> Result<(), String> {
    let secret = SecretString::from(password);
    let password_for_worker = secret.clone();
    let data = tokio::task::spawn_blocking(move || {
        if vault::vault_exists() {
            vault::read_encrypted(&password_for_worker).map(Some)
        } else {
            Ok(None)
        }
    })
    .await
    .map_err(|_| "Vault unlock task failed.".to_string())??;
    let mut cache = state.vault.cache.lock().unwrap();
    *state.vault.password.lock().unwrap() = Some(secret);
    *cache = data;
    Ok(())
}

#[tauri::command]
fn vault_lock(state: State<AppState>) {
    let mut cache = state.vault.cache.lock().unwrap();
    *state.vault.password.lock().unwrap() = None;
    *cache = None;
    #[cfg(unix)]
    state.mcp_access.revoke();
}

#[tauri::command]
fn get_secret(state: State<AppState>, env: String, name: String) -> Result<Option<String>, String> {
    ensure_cache(&state)?;
    let cache = state.vault.cache.lock().unwrap();
    Ok(cache.as_ref().and_then(|d| vault::get_from(d, &env, &name)))
}

#[tauri::command]
fn set_secret(
    state: State<AppState>,
    env: String,
    name: String,
    value: String,
) -> Result<(), String> {
    ensure_cache(&state)?;
    let pw = require_password(&state)?;
    let mut cache = state.vault.cache.lock().unwrap();
    let data = cache.as_mut().unwrap();
    vault::set_in(data, &env, &name, &value);
    vault::write_encrypted(&pw, data)?;
    Ok(())
}

#[tauri::command]
fn delete_secret(state: State<AppState>, env: String, name: String) -> Result<(), String> {
    ensure_cache(&state)?;
    let pw = require_password(&state)?;
    let mut cache = state.vault.cache.lock().unwrap();
    let data = cache.as_mut().unwrap();
    vault::delete_in(data, &env, &name);
    vault::write_encrypted(&pw, data)?;
    Ok(())
}

#[tauri::command]
fn save_env(env: Env) -> Result<(), String> {
    loader::write_env(&env)
}

#[tauri::command]
fn rename_env(state: State<AppState>, old_name: String, new_name: String) -> Result<Env, String> {
    if new_name.trim().is_empty() {
        return Err("new name is empty".into());
    }
    if new_name.contains('/') || new_name.contains('\\') {
        return Err("name cannot contain slashes".into());
    }
    let root = state.root.lock().unwrap().clone();
    let dir = root.join("environments");
    let old_path = dir.join(format!("{}.yml", old_name));
    let new_path = dir.join(format!("{}.yml", new_name));
    if !old_path.exists() {
        return Err(format!("not found: {}", old_path.display()));
    }
    // same underlying file (case-insensitive fs) → still allow rename
    let same_file = std::fs::canonicalize(&old_path)
        .ok()
        .zip(std::fs::canonicalize(&new_path).ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    if new_path.exists() && !same_file {
        return Err(format!("already exists: {}", new_path.display()));
    }

    let raw = std::fs::read_to_string(&old_path).map_err(|e| e.to_string())?;
    let mut env: Env = serde_yaml::from_str(&raw).map_err(|e| e.to_string())?;
    env.name = new_name.clone();
    env.path = new_path.to_string_lossy().to_string();

    if state.vault.password.lock().unwrap().is_some() {
        ensure_cache(&state)?;
        let pw = require_password(&state)?;
        let mut cache = state.vault.cache.lock().unwrap();
        let data = cache.as_mut().unwrap();
        vault::rename_env_in(data, &old_name, &new_name);
        vault::write_encrypted(&pw, data)?;
    }

    loader::write_env(&env)?;
    if !same_file {
        std::fs::remove_file(&old_path).map_err(|e| e.to_string())?;
    }
    Ok(env)
}

#[tauri::command]
fn create_env(
    state: State<AppState>,
    name: String,
    template_from: Option<String>,
) -> Result<Env, String> {
    let root = state.root.lock().unwrap().clone();
    let dir = root.join("environments");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.yml", name));
    if path.exists() {
        return Err(format!("already exists: {}", path.display()));
    }
    let mut new_env = Env {
        name: name.clone(),
        variables: vec![],
        path: path.to_string_lossy().to_string(),
    };
    if let Some(tpl) = template_from {
        let coll = state.collection.lock().unwrap();
        if let Some(c) = coll.as_ref() {
            if let Some(t) = c.envs.iter().find(|e| e.name == tpl) {
                new_env.variables = t
                    .variables
                    .iter()
                    .map(|v| EnvVar {
                        name: v.name.clone(),
                        value: String::new(),
                        secret: v.secret,
                    })
                    .collect();
            }
        }
    }
    loader::write_env(&new_env)?;
    Ok(new_env)
}

#[tauri::command]
fn delete_env(state: State<AppState>, env: Env) -> Result<(), String> {
    if state.vault.password.lock().unwrap().is_some() {
        ensure_cache(&state)?;
        let pw = require_password(&state)?;
        let mut cache = state.vault.cache.lock().unwrap();
        let data = cache.as_mut().unwrap();
        vault::delete_env_in(data, &env.name);
        vault::write_encrypted(&pw, data)?;
    }
    loader::delete_env_file(&env)
}

#[tauri::command]
fn save_request(req: Request) -> Result<(), String> {
    loader::write_request(&req)
}

#[tauri::command]
fn create_request(
    state: State<AppState>,
    folder: String,
    name: String,
    method: String,
    url: String,
) -> Result<String, String> {
    let root = state.root.lock().unwrap().clone();
    let path = loader::create_request_file(&root, &folder, &name, &method, &url)?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
fn rename_request(
    state: State<AppState>,
    request_id: String,
    new_name: String,
) -> Result<String, String> {
    if new_name.trim().is_empty() {
        return Err("new name is empty".into());
    }
    if new_name.contains('/') || new_name.contains('\\') {
        return Err("name cannot contain slashes".into());
    }
    let old_req = {
        let coll = state.collection.lock().unwrap();
        let c = coll
            .as_ref()
            .ok_or_else(|| "collection not loaded".to_string())?;
        c.requests
            .iter()
            .find(|r| r.id == request_id)
            .cloned()
            .ok_or_else(|| format!("request not found: {}", request_id))?
    };
    let old_path = std::path::PathBuf::from(&old_req.path);
    let dir = old_path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?;
    let new_path = dir.join(format!("{}.yml", new_name));

    let same_file = std::fs::canonicalize(&old_path)
        .ok()
        .zip(std::fs::canonicalize(&new_path).ok())
        .map(|(a, b)| a == b)
        .unwrap_or(false);
    if new_path.exists() && !same_file {
        return Err(format!("already exists: {}", new_path.display()));
    }

    let mut updated = old_req.clone();
    updated.info.name = new_name.clone();
    updated.path = new_path.to_string_lossy().to_string();
    loader::write_request(&updated)?;
    if !same_file {
        std::fs::remove_file(&old_path).map_err(|e| e.to_string())?;
    }
    Ok(new_path.to_string_lossy().to_string())
}

#[tauri::command]
fn delete_request(req: Request) -> Result<(), String> {
    loader::delete_request_file(&req)
}

#[tauri::command]
fn parse_curl(input: String) -> Result<Request, String> {
    curl::parse_curl(&input)
}

#[tauri::command]
fn duplicate_request(state: State<AppState>, request_id: String) -> Result<String, String> {
    let coll = state.collection.lock().unwrap();
    let c = coll
        .as_ref()
        .ok_or_else(|| "collection not loaded".to_string())?;
    let src = c
        .requests
        .iter()
        .find(|r| r.id == request_id)
        .ok_or_else(|| format!("not found: {}", request_id))?
        .clone();
    drop(coll);

    let src_path = std::path::PathBuf::from(&src.path);
    let dir = src_path
        .parent()
        .ok_or_else(|| "no parent dir".to_string())?;
    let stem = src_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("copy");
    let mut n = 1;
    let new_path = loop {
        let candidate = dir.join(format!(
            "{}-copy{}.yml",
            stem,
            if n == 1 { String::new() } else { n.to_string() }
        ));
        if !candidate.exists() {
            break candidate;
        }
        n += 1;
        if n > 100 {
            return Err("too many copies".into());
        }
    };
    let mut new_req = src.clone();
    new_req.info.name = format!("{} (copy)", src.info.name);
    new_req.path = new_path.to_string_lossy().to_string();
    new_req.rel_path = String::new();
    new_req.id = String::new();
    loader::write_request(&new_req)?;
    Ok(new_path.to_string_lossy().to_string())
}

#[tauri::command]
fn to_curl(
    state: State<AppState>,
    request_id: String,
    env_name: Option<String>,
) -> Result<String, String> {
    let (req, env) = {
        let coll = state.collection.lock().unwrap();
        let c = coll
            .as_ref()
            .ok_or_else(|| "collection not loaded".to_string())?;
        let req = c
            .requests
            .iter()
            .find(|r| r.id == request_id)
            .cloned()
            .ok_or_else(|| format!("not found: {}", request_id))?;
        let env = env_name.and_then(|n| c.envs.iter().find(|e| e.name == n).cloned());
        (req, env)
    };
    let vault_data = if state.vault.password.lock().unwrap().is_some() {
        ensure_cache(&state)?;
        state.vault.cache.lock().unwrap().clone()
    } else {
        None
    };
    let vars = env
        .as_ref()
        .map(|e| http::resolve_env_vars(e, vault_data.as_ref()))
        .unwrap_or_default();
    Ok(build_curl(&req, &vars))
}

fn shell_escape(s: &str) -> String {
    // wrap in single quotes; embed literal quotes via '\''
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn build_curl(req: &Request, vars: &std::collections::HashMap<String, String>) -> String {
    let (url, _) = http::substitute(&req.http.url, vars);
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "curl -X {} {}",
        req.http.method.to_uppercase(),
        shell_escape(&url)
    ));
    for h in &req.http.headers {
        if h.enabled == Some(false) {
            continue;
        }
        let (v, _) = http::substitute(&h.value, vars);
        lines.push(format!(
            "  -H {}",
            shell_escape(&format!("{}: {}", h.name, v))
        ));
    }
    if let Some(body_type) = req.http.body.r#type.as_deref() {
        let raw = req
            .http
            .body
            .data
            .as_deref()
            .or(req.http.body.json.as_deref())
            .or(req.http.body.text.as_deref())
            .unwrap_or("");
        if !raw.is_empty() {
            let (b, _) = http::substitute(raw, vars);
            let _ = body_type;
            lines.push(format!("  --data {}", shell_escape(&b)));
        }
    }
    lines.join(" \\\n")
}

#[tauri::command]
fn preview_openapi(spec_path: String) -> Result<openapi::ImportPreview, String> {
    openapi::preview(std::path::Path::new(&spec_path))
}

#[tauri::command]
fn import_openapi(
    state: State<AppState>,
    spec_path: String,
    folder_prefix: String,
    create_env: bool,
    env_name: String,
    fill_empty_bodies: bool,
) -> Result<openapi::ImportStats, String> {
    let root = state.root.lock().unwrap().clone();
    openapi::import(
        &root,
        std::path::Path::new(&spec_path),
        &folder_prefix,
        create_env,
        &env_name,
        fill_empty_bodies,
    )
}

#[tauri::command]
fn import_curl(
    state: State<AppState>,
    input: String,
    folder: String,
    name: String,
) -> Result<String, String> {
    let mut req = curl::parse_curl(&input)?;
    req.info.name = name.clone();
    let root = state.root.lock().unwrap().clone();
    let dir = root.join(&folder);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.yml", name));
    if path.exists() {
        return Err(format!("already exists: {}", path.display()));
    }
    req.path = path.to_string_lossy().to_string();
    loader::write_request(&req)?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
async fn fire_request(
    state: State<'_, AppState>,
    request_id: String,
    env_name: Option<String>,
) -> Result<http::FiredResponse, String> {
    let (req, env) = {
        let coll = state.collection.lock().unwrap();
        let c = coll
            .as_ref()
            .ok_or_else(|| "collection not loaded".to_string())?;
        let req = c
            .requests
            .iter()
            .find(|r| r.id == request_id)
            .cloned()
            .ok_or_else(|| format!("request not found: {}", request_id))?;
        let env = env_name.and_then(|n| c.envs.iter().find(|e| e.name == n).cloned());
        (req, env)
    };
    let vault_data = if state.vault.password.lock().unwrap().is_some() {
        ensure_cache(&state)?;
        state.vault.cache.lock().unwrap().clone()
    } else {
        None
    };
    let resp = http::fire(&req, env.as_ref(), vault_data.as_ref()).await?;
    let root = state.root.lock().unwrap().clone();
    let _ = history::append(&root, &request_id, &resp);
    Ok(resp)
}

#[tauri::command]
fn load_history(
    state: State<AppState>,
    request_id: String,
) -> Result<Vec<http::FiredResponse>, String> {
    let root = state.root.lock().unwrap().clone();
    history::load(&root, &request_id)
}

#[tauri::command]
fn clear_history(state: State<AppState>, request_id: String) -> Result<(), String> {
    let root = state.root.lock().unwrap().clone();
    history::clear(&root, &request_id)
}

#[tauri::command]
async fn pick_folder(app: AppHandle) -> Result<Option<String>, String> {
    // Plugin's blocking builder deadlocks on the main thread; use the async
    // variant with a oneshot channel.
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_folder(move |path| {
        let _ = tx.send(path);
    });
    match rx.await {
        Ok(Some(p)) => Ok(Some(p.to_string())),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
async fn pick_openapi_file(app: AppHandle) -> Result<Option<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("Choose an OpenAPI file")
        .add_filter("OpenAPI files", &["yaml", "yml", "json"])
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    rx.await
        .map(|path| path.map(|file| file.to_string()))
        .map_err(|error| error.to_string())
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct NotesPreferences {
    dark: bool,
    text_size: u8,
    always_on_top: bool,
    #[serde(default)]
    hide_completed: bool,
}
impl Default for NotesPreferences {
    fn default() -> Self {
        Self {
            dark: false,
            text_size: 15,
            always_on_top: true,
            hide_completed: false,
        }
    }
}

#[tauri::command]
fn load_notes_preferences() -> Result<NotesPreferences, String> {
    let path = note_path()?.with_file_name("notes-preferences.json");
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(NotesPreferences::default()),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
fn save_notes_preferences(
    window: tauri::WebviewWindow,
    preferences: NotesPreferences,
) -> Result<(), String> {
    use std::io::Write;
    if !matches!(window.label(), "notes" | "main") {
        return Err("Invalid window".into());
    }
    if !(13..=19).contains(&preferences.text_size) {
        return Err("Text size must be between 13 and 19".into());
    }
    let path = note_path()?.with_file_name("notes-preferences.json");
    let dir = path.parent().ok_or("Invalid preferences path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(&preferences).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if let Some(notes) = window.app_handle().get_webview_window("notes") {
        set_notes_always_on_top(&notes, preferences.always_on_top)?;
    }
    file.persist(path).map_err(|e| e.to_string())?;
    window
        .app_handle()
        .emit("notes-preferences-changed", &preferences)
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn note_path() -> Result<PathBuf, String> {
    let dirs = BaseDirs::new().ok_or("Cannot find the application data directory")?;
    Ok(dirs.data_local_dir().join("hitr").join("quick-note.txt"))
}

fn read_note(path: &std::path::Path) -> Result<String, String> {
    match std::fs::read_to_string(path.with_extension("html")) {
        Ok(html) => return Ok(html),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Could not load note: {e}")),
    }
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text
            .lines()
            .map(|line| {
                format!(
                    "<div>{}</div>",
                    line.replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;")
                )
            })
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("Could not load note: {e}")),
    }
}

#[cfg(test)]
fn write_note(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let path = path.with_extension("html");
    let dir = path.parent().ok_or("Invalid note path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|e| e.to_string())?;
    file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn notes_drag_origin(window: tauri::WebviewWindow) -> Result<(i32, i32, f64), String> {
    if window.label() != "notes" {
        return Err("Invalid window".into());
    }
    let position = window.outer_position().map_err(|e| e.to_string())?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    Ok((position.x, position.y, scale))
}

#[tauri::command]
fn move_notes(window: tauri::WebviewWindow, x: i32, y: i32) -> Result<(), String> {
    if window.label() != "notes" {
        return Err("Invalid window".into());
    }
    notes_window::move_icon(&window, x, y)
}

fn workspace_window(app: &AppHandle, label: &str) -> Result<tauri::WebviewWindow, String> {
    match app.get_webview_window(label) {
        Some(window) => Ok(window),
        None => {
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|window| window.label == label)
                .ok_or("Window configuration not found")?;
            let window = tauri::WebviewWindowBuilder::from_config(app, config)
                .map_err(|e| e.to_string())?
                .visible(false)
                .focused(false)
                .build()
                .map_err(|e| e.to_string())?;
            if label == "notes" {
                notes_window::reset(app);
            }
            Ok(window)
        }
    }
}

fn set_notes_always_on_top(notes: &tauri::WebviewWindow, enabled: bool) -> Result<(), String> {
    // Tao schedules its own level change asynchronously on macOS. Using both
    // setters can overwrite the panel's status level with the floating level.
    #[cfg(not(target_os = "macos"))]
    notes
        .set_always_on_top(enabled)
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    {
        let window = notes.clone();
        notes
            .run_on_main_thread(move || {
                use objc2_app_kit::{NSNormalWindowLevel, NSStatusWindowLevel, NSWindow};
                if let Ok(handle) = window.ns_window() {
                    // The panel stays a utility window; this preference controls its level.
                    let native = unsafe { &*(handle as *const NSWindow) };
                    native.setLevel(if enabled {
                        NSStatusWindowLevel
                    } else {
                        NSNormalWindowLevel
                    });
                }
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn configure_notes_window(notes: &tauri::WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    notes_panel::configure(notes)?;
    let preferences = load_notes_preferences().unwrap_or_default();
    set_notes_always_on_top(notes, preferences.always_on_top)
}

#[tauri::command]
async fn open_api_workspace(app: AppHandle) -> Result<(), String> {
    let window = workspace_window(&app, "main")?;
    window.show().map_err(|e| e.to_string())?;
    window.unminimize().map_err(|e| e.to_string())?;
    window.set_focus().map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_notes_workspace(app: AppHandle) -> Result<(), String> {
    let notes = workspace_window(&app, "notes")?;
    configure_notes_window(&notes)?;
    resize_notes(notes.clone(), true)?;
    notes.show().map_err(|e| e.to_string())?;
    notes.unminimize().map_err(|e| e.to_string())?;
    notes.set_focus().map_err(|e| e.to_string())?;
    // Hiding keeps the selected request, response, and edits available on return.
    if let Some(apis) = app.get_webview_window("main") {
        apis.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn resize_notes(window: tauri::WebviewWindow, expanded: bool) -> Result<(), String> {
    if window.label() != "notes" {
        return Err("Only the notes window can be resized".into());
    }
    notes_window::resize(&window, expanded)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let root = read_root_from_config().unwrap_or_else(default_root);

    tauri::Builder::default()
        .manage(notes_window::NotesPlacement::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.handle().plugin(tauri_nspanel::init())?;
            if let Some(notes) = app.get_webview_window("notes") {
                configure_notes_window(&notes)?;
                notes.show()?;
            }
            #[cfg(target_os = "macos")]
            {
                app.handle()
                    .plugin(tauri_plugin_updater::Builder::new().build())?;
                let menu = tauri::menu::Menu::default(app.handle())?;
                let items = menu.items()?;
                let app_menu = items
                    .first()
                    .and_then(|item| item.as_submenu())
                    .ok_or("macOS application menu not found")?;
                let check = tauri::menu::MenuItem::with_id(
                    app,
                    "check-for-updates",
                    "Check for Updates…",
                    true,
                    None::<&str>,
                )?;
                app_menu.insert(&check, 1)?;
                app.set_menu(menu)?;
                app.on_menu_event(|app, event| {
                    if event.id().as_ref() == "check-for-updates" {
                        updates::check_from_menu(app.clone());
                    }
                });
            }
            app.manage(updates::Installer::default());
            #[cfg(target_os = "macos")]
            updates::start_automatic_checks(app.handle().clone());
            app.manage(AppState {
                root: Mutex::new(root.clone()),
                collection: Arc::new(Mutex::new(None)),
                vault: Arc::new(VaultState::default()),
                #[cfg(unix)]
                mcp_status: Mutex::new(mcp::Status::Disabled),
                #[cfg(unix)]
                mcp_access: Arc::new(mcp::Access::load()),
            });
            #[cfg(unix)]
            if app.state::<AppState>().mcp_access.enabled() {
                mcp::start(app.handle().clone());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            load_notes_preferences,
            save_notes_preferences,
            notes_store::load_notes,
            notes_store::finish_onboarding,
            notes_store::save_note,
            notes_store::create_note,
            notes_store::select_note,
            notes_store::delete_note,
            notes_store::restore_deleted_note,
            notes_store::dismiss_deleted_note,
            resize_notes,
            open_api_workspace,
            open_notes_workspace,
            notes_drag_origin,
            move_notes,
            get_root,
            set_root,
            load,
            vault_status,
            mcp_help,
            set_mcp_enabled,
            set_mcp_allowed_envs,
            set_mcp_full_response,
            updates::check_update,
            updates::install_update,
            vault_unlock,
            vault_lock,
            get_secret,
            set_secret,
            delete_secret,
            save_env,
            create_env,
            rename_env,
            delete_env,
            save_request,
            create_request,
            delete_request,
            rename_request,
            parse_curl,
            import_curl,
            preview_openapi,
            import_openapi,
            duplicate_request,
            to_curl,
            fire_request,
            load_history,
            clear_history,
            pick_folder,
            pick_openapi_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod notes_tests {
    use super::{read_note, write_note};

    #[test]
    fn migrates_plain_text_without_interpreting_markup() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("quick-note.txt");
        std::fs::write(&path, "<example> & café\nNext line").unwrap();
        assert_eq!(
            read_note(&path).unwrap(),
            "<div>&lt;example&gt; &amp; café</div><div>Next line</div>"
        );
        let html = "<h2>Plan</h2><ul><li><b>Write</b> this</li></ul>";
        write_note(&path, html).unwrap();
        assert_eq!(read_note(&path).unwrap(), html);
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "<example> & café\nNext line"
        );
    }
}
