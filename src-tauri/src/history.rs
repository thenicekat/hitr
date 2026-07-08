//! Per-request response history, persisted to disk.
//!
//! One JSONL file per request under `<collection-root>/.hitr/history/`.
//! Filename is a hex SHA-1 of the request's rel_path — flat, unambiguous,
//! survives request rename (breaks — but that's expected, no orphan cleanup).
//!
//! Each line is a JSON-encoded `FiredResponse`. Newest last (append-only).
//! Kept at MAX_ENTRIES via read-truncate-write on each append. Not the most
//! efficient shape — every append re-reads the file. But 20 entries × few KB
//! is trivial and simplifies the code (no separate "compact" pass).
//!
//! Directory is created on first write. `.hitr/` sits alongside `environments/`
//! under the collection root; users can add it to `.gitignore`.

use crate::http::FiredResponse;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

const MAX_ENTRIES: usize = 20;

fn history_dir(root: &Path) -> PathBuf {
    root.join(".hitr").join("history")
}

fn history_file(root: &Path, request_id: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(request_id.as_bytes());
    let digest = hex::encode(hasher.finalize());
    history_dir(root).join(format!("{}.jsonl", &digest[..16]))
}

pub fn append(root: &Path, request_id: &str, resp: &FiredResponse) -> Result<(), String> {
    let dir = history_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = history_file(root, request_id);

    // read existing (if any), keep last MAX_ENTRIES-1, then append new
    let mut lines: Vec<String> = if path.exists() {
        let f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        BufReader::new(f).lines().map_while(Result::ok).collect()
    } else {
        Vec::new()
    };

    let new_line = serde_json::to_string(resp).map_err(|e| e.to_string())?;
    lines.push(new_line);
    let overflow = lines.len().saturating_sub(MAX_ENTRIES);
    if overflow > 0 {
        lines.drain(..overflow);
    }

    let tmp = path.with_extension("jsonl.tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    for l in &lines {
        writeln!(f, "{}", l).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn load(root: &Path, request_id: &str) -> Result<Vec<FiredResponse>, String> {
    let path = history_file(root, request_id);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    let mut out: Vec<FiredResponse> = Vec::new();
    for line in BufReader::new(f).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(r) = serde_json::from_str::<FiredResponse>(&line) {
            out.push(r);
        }
    }
    out.reverse(); // newest first for UI
    Ok(out)
}

pub fn clear(root: &Path, request_id: &str) -> Result<(), String> {
    let path = history_file(root, request_id);
    if path.exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}
