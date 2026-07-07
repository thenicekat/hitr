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
mod http;
mod loader;
mod model;
mod openapi;
mod vault;

use crate::model::*;
use crate::vault::VaultState;
use age::secrecy::SecretString;
use directories::BaseDirs;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{Manager, State};

struct AppState {
    root: Mutex<PathBuf>,
    collection: Mutex<Option<Collection>>,
    vault: VaultState,
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
    *state.root.lock().unwrap() = p;
    *state.collection.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
async fn load(state: State<'_, AppState>) -> Result<Collection, String> {
    let root = state.root.lock().unwrap().clone();
    let c = tokio::task::spawn_blocking(move || loader::load_collection(&root))
        .await
        .map_err(|e| e.to_string())??;
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
fn vault_unlock(state: State<AppState>, password: String) -> Result<(), String> {
    let secret = SecretString::from(password);
    // verify: try decrypt if vault exists; else accept and let first save create it
    if vault::vault_exists() {
        vault::read_encrypted(&secret)?;
    }
    *state.vault.password.lock().unwrap() = Some(secret);
    *state.vault.cache.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
fn vault_lock(state: State<AppState>) {
    *state.vault.password.lock().unwrap() = None;
    *state.vault.cache.lock().unwrap() = None;
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
) -> Result<openapi::ImportStats, String> {
    let root = state.root.lock().unwrap().clone();
    openapi::import(
        &root,
        std::path::Path::new(&spec_path),
        &folder_prefix,
        create_env,
        &env_name,
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
    http::fire(&req, env.as_ref(), vault_data.as_ref()).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let root = read_root_from_config().unwrap_or_else(default_root);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            app.manage(AppState {
                root: Mutex::new(root.clone()),
                collection: Mutex::new(None),
                vault: VaultState::default(),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_root,
            set_root,
            load,
            vault_status,
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
            parse_curl,
            import_curl,
            preview_openapi,
            import_openapi,
            duplicate_request,
            to_curl,
            fire_request,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
