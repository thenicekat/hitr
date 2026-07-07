//! Filesystem I/O for Bruno collections.
//!
//! Walks a collection root, parses request/env yml files, and writes them
//! back on save. Bruno request files carry a fat `examples:` block of response
//! fixtures; we elide it before parse (~90% of file bytes, 0% of runtime
//! value) to keep cold load fast on large collections.

use crate::model::*;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

// elide top-level `examples:` block; parse cost dominated by fixture bodies
fn elide_examples(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut skip = false;
    for line in s.split_inclusive('\n') {
        let stripped = line.trim_end_matches('\n');
        if !skip && stripped.starts_with("examples:") {
            skip = true;
            continue;
        }
        if skip {
            let first = stripped.chars().next();
            match first {
                None => {
                    out.push_str(line);
                    continue;
                }
                Some(c) if c == ' ' || c == '\t' || c == '#' => continue,
                _ => skip = false,
            }
        }
        out.push_str(line);
    }
    out
}

/// Recursively walks `root`, parses every `*.yml` that looks like a Bruno
/// request or an environment, and returns a fully-populated `Collection`.
///
/// Skipped: hidden dirs, `folder.yml`, `opencollection.yml`, files that fail
/// to parse (silently — a broken yml doesn't block loading the rest).
pub fn load_collection(root: &Path) -> Result<Collection, String> {
    let mut c = Collection {
        root: root.to_string_lossy().to_string(),
        ..Default::default()
    };

    let env_dir = root.join("environments");
    if env_dir.is_dir() {
        for entry in std::fs::read_dir(&env_dir)
            .map_err(|e| e.to_string())?
            .flatten()
        {
            let p = entry.path();
            if p.extension().is_none_or(|x| x != "yml") {
                continue;
            }
            let raw = match std::fs::read_to_string(&p) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut env: Env = match serde_yaml::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if env.name.is_empty() {
                env.name = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
            }
            env.path = p.to_string_lossy().to_string();
            c.envs.push(env);
        }
    }
    c.envs.sort_by(|a, b| a.name.cmp(&b.name));

    for entry in WalkDir::new(root).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        !(name == "environments" || name.starts_with('.'))
    }) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if entry.file_type().is_dir() {
            continue;
        }
        let p = entry.path();
        if p.extension().is_none_or(|x| x != "yml") {
            continue;
        }
        let base = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if base == "folder.yml" || base == "opencollection.yml" {
            continue;
        }
        let raw = match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let elided = elide_examples(&raw);
        let mut req: Request = match serde_yaml::from_str(&elided) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if req.info.r#type != "http" && req.http.method.is_empty() {
            continue;
        }
        req.path = p.to_string_lossy().to_string();
        req.rel_path = p
            .strip_prefix(root)
            .unwrap_or(p)
            .to_string_lossy()
            .to_string();
        req.id = req.rel_path.clone();
        c.requests.push(req);
    }

    c.requests.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(c)
}

/// Write an env struct back to its `path`. Strips runtime metadata before
/// serialization so the on-disk yml stays clean.
pub fn write_env(env: &Env) -> Result<(), String> {
    let mut clean = env.clone();
    let path = std::mem::take(&mut clean.path);
    if path.is_empty() {
        return Err("env has no path".into());
    }
    let s = serde_yaml::to_string(&clean).map_err(|e| e.to_string())?;
    std::fs::write(path, s).map_err(|e| e.to_string())
}

/// Write a request struct back to its `path`. Strips `path`, `rel_path`, `id`
/// (runtime-only fields) before serialization.
pub fn write_request(req: &Request) -> Result<(), String> {
    let mut clean = req.clone();
    let path = std::mem::take(&mut clean.path);
    clean.rel_path = String::new();
    clean.id = String::new();
    if path.is_empty() {
        return Err("request has no path".into());
    }
    let s = serde_yaml::to_string(&clean).map_err(|e| e.to_string())?;
    std::fs::write(path, s).map_err(|e| e.to_string())
}

/// Create a new request yml at `<root>/<folder>/<name>.yml`. Errors if the
/// file already exists — we never overwrite silently.
pub fn create_request_file(
    root: &Path,
    folder: &str,
    name: &str,
    method: &str,
    url: &str,
) -> Result<PathBuf, String> {
    let dir = root.join(folder);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join(format!("{}.yml", name));
    if file.exists() {
        return Err(format!("already exists: {}", file.display()));
    }
    let req = Request {
        info: Info {
            name: name.to_string(),
            r#type: "http".to_string(),
            seq: 1,
        },
        http: HttpSpec {
            method: method.to_string(),
            url: url.to_string(),
            ..Default::default()
        },
        path: file.to_string_lossy().to_string(),
        rel_path: String::new(),
        id: String::new(),
    };
    let s = serde_yaml::to_string(&req).map_err(|e| e.to_string())?;
    std::fs::write(&file, s).map_err(|e| e.to_string())?;
    Ok(file)
}

pub fn delete_env_file(env: &Env) -> Result<(), String> {
    std::fs::remove_file(&env.path).map_err(|e| e.to_string())
}

pub fn delete_request_file(req: &Request) -> Result<(), String> {
    std::fs::remove_file(&req.path).map_err(|e| e.to_string())
}
