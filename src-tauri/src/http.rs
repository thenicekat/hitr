//! Request firing.
//!
//! Resolves `{{var}}` templates against the currently-selected env, pulls
//! secret values from the vault at fire-time (never earlier — so keychain
//! prompts happen only when actually needed), sends via reqwest, and returns
//! a JSON-serializable `FiredResponse` for the frontend to render.

use crate::model::*;
use crate::vault::{self, VaultData};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FiredResponse {
    pub status: u16,
    pub status_text: String,
    pub latency_ms: u64,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub is_json: bool,
    pub final_url: String,
}

/// Merge non-secret env values with secret values pulled from the vault.
/// Non-secret vars come from the yml directly; secret vars come from vault
/// keyed under `<envName>/<varName>`. Missing secrets simply don't appear in
/// the output map — substitution then leaves `{{name}}` intact.
pub fn resolve_env_vars(env: &Env, vault_data: Option<&VaultData>) -> HashMap<String, String> {
    let mut out = HashMap::with_capacity(env.variables.len());
    for v in &env.variables {
        if v.secret {
            if let Some(data) = vault_data {
                if let Some(val) = vault::get_from(data, &env.name, &v.name) {
                    out.insert(v.name.clone(), val);
                    continue;
                }
            }
        }
        if !v.value.is_empty() {
            out.insert(v.name.clone(), v.value.clone());
        }
    }
    out
}

/// Strip `//` line comments and `/*…*/` block comments from a JSON string,
/// leaving strings intact. Comments inside string literals are preserved.
/// State machine: track "inside-string" vs "inside-comment" flags, respect
/// backslash escapes inside strings.
pub fn strip_json_comments(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_str = false;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_str {
            out.push(c as char);
            if escape {
                escape = false;
            } else if c == b'\\' {
                escape = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'/' => {
                    // line comment: skip until newline (keep the newline)
                    let mut j = i + 2;
                    while j < bytes.len() && bytes[j] != b'\n' {
                        j += 1;
                    }
                    i = j;
                    continue;
                }
                b'*' => {
                    let mut j = i + 2;
                    while j + 1 < bytes.len() && !(bytes[j] == b'*' && bytes[j + 1] == b'/') {
                        j += 1;
                    }
                    i = (j + 2).min(bytes.len());
                    continue;
                }
                _ => {}
            }
        }
        out.push(c as char);
        i += 1;
    }
    out
}

/// Replace `{{name}}` occurrences in `s` with values from `vars`. Returns
/// the substituted string plus the list of var names that were unresolved
/// (so the caller can surface them in an error message).
///
/// Unresolved vars are left as literal `{{name}}` in the output — visible
/// so the user sees what's missing.
pub fn substitute(s: &str, vars: &HashMap<String, String>) -> (String, Vec<String>) {
    let mut out = String::with_capacity(s.len());
    let mut missing = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        if let Some(close) = after.find("}}") {
            let key = after[..close].trim();
            if let Some(v) = vars.get(key) {
                out.push_str(v);
            } else {
                missing.push(key.to_string());
                out.push_str("{{");
                out.push_str(&after[..close]);
                out.push_str("}}");
            }
            rest = &after[close + 2..];
        } else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    (out, missing)
}

/// Fire a request. Substitutes vars, adds bearer auth if `bearerToken` is
/// present in the env and the request lacks an `Authorization` header,
/// follows up to 5 redirects, pretty-prints JSON responses, returns
/// status/latency/headers/body wrapped in a `FiredResponse`.
///
/// Fails early with a specific message if the URL contains any unresolved
/// `{{var}}` — reqwest's own "builder error" is opaque and unhelpful.
pub async fn fire(
    req: &Request,
    env: Option<&Env>,
    vault_data: Option<&VaultData>,
) -> Result<FiredResponse, String> {
    let vars = env
        .map(|e| resolve_env_vars(e, vault_data))
        .unwrap_or_default();
    let mut all_missing = std::collections::BTreeSet::new();

    let (url, miss) = substitute(&req.http.url, &vars);
    for m in miss {
        all_missing.insert(m);
    }

    if url.contains("{{") {
        return Err(format!(
            "unresolved var(s) in url: {} — set values via env editor",
            all_missing.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }

    let method = req.http.method.to_uppercase();

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| e.to_string())?;

    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let mut parsed =
        reqwest::Url::parse(&url).map_err(|e| format!("invalid url `{}`: {}", url, e))?;
    // Append enabled params from the params tab. Existing query keys in the
    // URL are left alone; params tab rows are added, so URL wins on conflict.
    // Substitute {{var}} in each key/value first.
    for p in &req.http.params {
        if p.enabled == Some(false) {
            continue;
        }
        if p.name.is_empty() {
            continue;
        }
        let (k, mk) = substitute(&p.name, &vars);
        let (v, mv) = substitute(&p.value, &vars);
        for m in mk {
            all_missing.insert(m);
        }
        for m in mv {
            all_missing.insert(m);
        }
        parsed.query_pairs_mut().append_pair(&k, &v);
    }
    // api_key-in-query must be appended before parsed is consumed by request()
    let mode = req
        .http
        .auth_config
        .as_ref()
        .map(|a| a.mode.as_str())
        .unwrap_or("inherit");
    if mode == "api_key" {
        if let Some(ac) = &req.http.auth_config {
            if ac.r#in == "query" || ac.r#in.is_empty() {
                let (val, _) = substitute(&ac.token, &vars);
                let key = ac.key.as_str();
                if !key.is_empty() && !val.is_empty() {
                    parsed.query_pairs_mut().append_pair(key, &val);
                }
            }
        }
    }

    let mut rb = client.request(m, parsed);

    for h in &req.http.headers {
        if h.enabled == Some(false) {
            continue;
        }
        let (val, miss) = substitute(&h.value, &vars);
        for m in miss {
            all_missing.insert(m);
        }
        rb = rb.header(&h.name, val);
    }
    // Apply auth. Explicit auth_config wins; fall back to legacy env bearerToken.
    let has_auth_header = req
        .http
        .headers
        .iter()
        .any(|h| h.name.eq_ignore_ascii_case("authorization"));
    match mode {
        "none" => {}
        "bearer" => {
            if !has_auth_header {
                if let Some(ac) = &req.http.auth_config {
                    let (tok, _) = substitute(&ac.token, &vars);
                    if !tok.is_empty() {
                        rb = rb.header("Authorization", format!("Bearer {}", tok));
                    }
                }
            }
        }
        "api_key" => {
            if let Some(ac) = &req.http.auth_config {
                // header placement only — query was handled above
                if ac.r#in == "header" {
                    let (val, _) = substitute(&ac.token, &vars);
                    let key = ac.key.as_str();
                    if !key.is_empty() && !val.is_empty() {
                        rb = rb.header(key, val);
                    }
                }
            }
        }
        "basic" => {
            if !has_auth_header {
                if let Some(ac) = &req.http.auth_config {
                    let (user, _) = substitute(&ac.key, &vars);
                    let (pass, _) = substitute(&ac.token, &vars);
                    if !user.is_empty() {
                        let pass_opt: Option<&str> =
                            if pass.is_empty() { None } else { Some(&pass) };
                        rb = rb.basic_auth(&user, pass_opt);
                    }
                }
            }
        }
        // "inherit" or anything else — fall back to env bearerToken
        _ => {
            if !has_auth_header {
                if let Some(bearer) = vars.get("bearerToken") {
                    rb = rb.header("Authorization", format!("Bearer {}", bearer));
                }
            }
        }
    }

    if let Some(body_type) = req.http.body.r#type.as_deref() {
        // Bruno's on-disk yml stores the body under `body.data` regardless of
        // type. Some newer variants use `body.json` / `body.text`. Take
        // whichever field is populated.
        let raw = match body_type {
            "json" => req
                .http
                .body
                .json
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| req.http.body.data.clone())
                .unwrap_or_default(),
            "text" => req
                .http
                .body
                .text
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| req.http.body.data.clone())
                .unwrap_or_default(),
            _ => req.http.body.data.clone().unwrap_or_default(),
        };
        if !raw.is_empty() {
            let cleaned = if body_type == "json" {
                strip_json_comments(&raw)
            } else {
                raw
            };
            let (sub, miss) = substitute(&cleaned, &vars);
            for m in miss {
                all_missing.insert(m);
            }
            let has_ct =
                req.http.headers.iter().any(|h| {
                    h.enabled != Some(false) && h.name.eq_ignore_ascii_case("content-type")
                });
            if body_type == "json" && !has_ct {
                rb = rb.header("Content-Type", "application/json");
            }
            rb = rb.body(sub);
        }
    }

    let started = Instant::now();
    let resp = rb.send().await.map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let status_text = resp.status().canonical_reason().unwrap_or("").to_string();
    let final_url = resp.url().to_string();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let is_json = headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.contains("json"));
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let body = if is_json {
        serde_json::from_str::<serde_json::Value>(&text)
            .and_then(|v| serde_json::to_string_pretty(&v))
            .unwrap_or(text)
    } else {
        text
    };
    let latency_ms = started.elapsed().as_millis() as u64;
    Ok(FiredResponse {
        status,
        status_text,
        latency_ms,
        headers,
        body,
        is_json,
        final_url,
    })
}
