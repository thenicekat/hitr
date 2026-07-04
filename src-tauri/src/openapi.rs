//! OpenAPI 3.x → Bruno YAML importer.
//!
//! Reads a spec file (yaml or json), walks `paths.<path>.<method>`, and emits
//! one Request per operation into `<root>/<folder_prefix>/<tag>/<name>.yml`.
//!
//! Path params are kept as literal `{param}` in the URL — Bruno-style. Body
//! comes from `content."application/json".example`; schema-driven body
//! generation is intentionally out of scope.

use crate::model::*;
use openapiv3::{OpenAPI, Operation, Parameter, ReferenceOr, Schema, SchemaKind, Type};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
pub struct SuggestedVar {
    pub name: String,
    pub secret: bool,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SampleOp {
    pub method: String,
    pub name: String,
    pub folder: String,
    pub path: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImportPreview {
    pub title: String,
    pub version: String,
    pub op_count: usize,
    pub folder_count: usize,
    pub suggested_vars: Vec<SuggestedVar>,
    pub sample_ops: Vec<SampleOp>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ImportStats {
    pub written: usize,
    pub skipped_existing: usize,
    pub folder_prefix: String,
    pub env_created: Option<String>,
}

fn read_spec(spec_path: &Path) -> Result<OpenAPI, String> {
    let raw = std::fs::read_to_string(spec_path).map_err(|e| e.to_string())?;
    let ext = spec_path.extension().and_then(|s| s.to_str()).unwrap_or("");
    if ext == "json" {
        serde_json::from_str(&raw).map_err(|e| format!("json: {}", e))
    } else {
        serde_yaml::from_str(&raw).map_err(|e| format!("yaml: {}", e))
    }
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn op_name(op: &Operation, method: &str, path: &str) -> String {
    if let Some(id) = &op.operation_id {
        return id.clone();
    }
    if let Some(sum) = op.summary.as_deref().filter(|s| !s.is_empty()) {
        return sum.chars().take(80).collect();
    }
    let path_slug = slug(path.trim_start_matches('/'));
    format!("{}_{}", method.to_lowercase(), path_slug)
}

fn folder_of(op: &Operation) -> String {
    op.tags.first().cloned().unwrap_or_else(|| "default".into())
}

fn resolve_schema<'a>(spec: &'a OpenAPI, r: &'a ReferenceOr<Schema>) -> Option<&'a Schema> {
    match r {
        ReferenceOr::Item(s) => Some(s),
        ReferenceOr::Reference { reference } => {
            let name = reference.rsplit('/').next()?;
            spec.components
                .as_ref()?
                .schemas
                .get(name)
                .and_then(|r| match r {
                    ReferenceOr::Item(s) => Some(s),
                    _ => None,
                })
        }
    }
}

fn extract_body(op: &Operation, spec: &OpenAPI) -> Body {
    let Some(rb) = op.request_body.as_ref() else {
        return Body::default();
    };
    let rb = match rb {
        ReferenceOr::Item(v) => v,
        _ => return Body::default(),
    };
    let Some(content) = rb.content.get("application/json") else {
        return Body::default();
    };
    // prefer explicit example
    if let Some(ex) = &content.example {
        if let Ok(pretty) = serde_json::to_string_pretty(ex) {
            return Body {
                r#type: Some("json".into()),
                data: Some(pretty),
                json: None,
                text: None,
            };
        }
    }
    // else first example in `examples` map
    if let Some((_, ex)) = content.examples.iter().next() {
        if let ReferenceOr::Item(ex) = ex {
            if let Some(v) = &ex.value {
                if let Ok(pretty) = serde_json::to_string_pretty(v) {
                    return Body {
                        r#type: Some("json".into()),
                        data: Some(pretty),
                        json: None,
                        text: None,
                    };
                }
            }
        }
    }
    // schema present but no example — mark type=json, empty body
    if let Some(schema_ref) = &content.schema {
        if resolve_schema(spec, schema_ref).is_some() {
            return Body {
                r#type: Some("json".into()),
                data: None,
                json: None,
                text: None,
            };
        }
    }
    Body::default()
}

fn resolve_param<'a>(spec: &'a OpenAPI, r: &'a ReferenceOr<Parameter>) -> Option<&'a Parameter> {
    match r {
        ReferenceOr::Item(p) => Some(p),
        ReferenceOr::Reference { reference } => {
            let name = reference.rsplit('/').next()?;
            spec.components
                .as_ref()?
                .parameters
                .get(name)
                .and_then(|r| match r {
                    ReferenceOr::Item(p) => Some(p),
                    _ => None,
                })
        }
    }
}

fn extract_params(op: &Operation, spec: &OpenAPI) -> (Vec<KV>, Vec<KV>) {
    let mut headers = Vec::new();
    let mut params = Vec::new();
    for p in &op.parameters {
        let Some(p) = resolve_param(spec, p) else { continue };
        let (name, kind) = match p {
            Parameter::Query { parameter_data, .. } => (&parameter_data.name, "query"),
            Parameter::Header { parameter_data, .. } => (&parameter_data.name, "header"),
            _ => continue,
        };
        let default_val = match p {
            Parameter::Query { parameter_data, .. }
            | Parameter::Header { parameter_data, .. }
            | Parameter::Path { parameter_data, .. }
            | Parameter::Cookie { parameter_data, .. } => parameter_data
                .example
                .as_ref()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
        };
        let kv = KV {
            name: name.clone(),
            value: default_val,
            r#type: Some(kind.to_string()),
            description: match p {
                Parameter::Query { parameter_data, .. }
                | Parameter::Header { parameter_data, .. }
                | Parameter::Path { parameter_data, .. }
                | Parameter::Cookie { parameter_data, .. } => parameter_data.description.clone(),
            },
            enabled: Some(true),
        };
        if kind == "header" {
            headers.push(kv);
        } else {
            params.push(kv);
        }
    }
    (headers, params)
}

fn build_url(path: &str) -> String {
    format!("{{{{baseUrl}}}}{}", path)
}

fn suggested_vars(spec: &OpenAPI) -> Vec<SuggestedVar> {
    let mut out = Vec::new();
    let base = spec
        .servers
        .first()
        .map(|s| s.url.clone())
        .unwrap_or_default();
    out.push(SuggestedVar {
        name: "baseUrl".into(),
        secret: false,
        value: base,
    });

    if let Some(components) = &spec.components {
        for (_, scheme) in &components.security_schemes {
            if let ReferenceOr::Item(s) = scheme {
                match s {
                    openapiv3::SecurityScheme::HTTP { scheme, .. } if scheme.eq_ignore_ascii_case("bearer") => {
                        out.push(SuggestedVar {
                            name: "bearerToken".into(),
                            secret: true,
                            value: String::new(),
                        });
                    }
                    openapiv3::SecurityScheme::APIKey { name, .. } => {
                        out.push(SuggestedVar {
                            name: slug(name),
                            secret: true,
                            value: String::new(),
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    // dedupe by name
    let mut seen = std::collections::HashSet::new();
    out.retain(|v| seen.insert(v.name.clone()));
    out
}

fn iter_ops(spec: &OpenAPI) -> Vec<(String, String, Operation)> {
    let mut out = Vec::new();
    for (path, item_ref) in &spec.paths.paths {
        let item = match item_ref {
            ReferenceOr::Item(i) => i,
            _ => continue,
        };
        for (method, op) in [
            ("GET", &item.get),
            ("POST", &item.post),
            ("PUT", &item.put),
            ("PATCH", &item.patch),
            ("DELETE", &item.delete),
            ("HEAD", &item.head),
            ("OPTIONS", &item.options),
            ("TRACE", &item.trace),
        ] {
            if let Some(op) = op {
                out.push((path.clone(), method.to_string(), op.clone()));
            }
        }
    }
    out
}

pub fn preview(spec_path: &Path) -> Result<ImportPreview, String> {
    let spec = read_spec(spec_path)?;
    let ops = iter_ops(&spec);
    let mut folders = std::collections::HashSet::new();
    let mut samples = Vec::new();
    for (path, method, op) in &ops {
        let f = folder_of(op);
        folders.insert(f.clone());
        if samples.len() < 8 {
            samples.push(SampleOp {
                method: method.clone(),
                name: op_name(op, method, path),
                folder: f,
                path: path.clone(),
            });
        }
    }
    Ok(ImportPreview {
        title: spec.info.title.clone(),
        version: spec.info.version.clone(),
        op_count: ops.len(),
        folder_count: folders.len(),
        suggested_vars: suggested_vars(&spec),
        sample_ops: samples,
    })
}

pub fn import(
    root: &Path,
    spec_path: &Path,
    folder_prefix: &str,
    create_env: bool,
    env_name: &str,
) -> Result<ImportStats, String> {
    let spec = read_spec(spec_path)?;
    let mut written = 0usize;
    let mut skipped = 0usize;
    let ops = iter_ops(&spec);

    let base_dir = if folder_prefix.is_empty() {
        root.to_path_buf()
    } else {
        root.join(folder_prefix)
    };

    for (idx, (path, method, op)) in ops.iter().enumerate() {
        let folder = folder_of(op);
        let name = op_name(op, method, path);
        // safe filename
        let file_name = format!("{}.yml", slug(&name));
        let dir = base_dir.join(slug(&folder));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(&file_name);
        if file.exists() {
            skipped += 1;
            continue;
        }
        let (headers, params) = extract_params(op, &spec);
        let body = extract_body(op, &spec);
        let req = Request {
            info: Info {
                name: name.clone(),
                r#type: "http".into(),
                seq: (idx + 1) as i64,
            },
            http: HttpSpec {
                method: method.clone(),
                url: build_url(path),
                headers,
                params,
                body,
                auth: Some("inherit".into()),
            },
            path: file.to_string_lossy().to_string(),
            rel_path: String::new(),
            id: String::new(),
        };
        crate::loader::write_request(&req)?;
        written += 1;
    }

    let env_created = if create_env && !env_name.trim().is_empty() {
        let env_dir = root.join("environments");
        std::fs::create_dir_all(&env_dir).map_err(|e| e.to_string())?;
        let env_path = env_dir.join(format!("{}.yml", env_name));
        if !env_path.exists() {
            let vars: Vec<EnvVar> = suggested_vars(&spec)
                .into_iter()
                .map(|v| EnvVar {
                    name: v.name,
                    value: if v.secret { String::new() } else { v.value },
                    secret: v.secret,
                })
                .collect();
            let env = Env {
                name: env_name.to_string(),
                variables: vars,
                path: env_path.to_string_lossy().to_string(),
            };
            crate::loader::write_env(&env)?;
            Some(env_name.to_string())
        } else {
            None
        }
    } else {
        None
    };

    Ok(ImportStats {
        written,
        skipped_existing: skipped,
        folder_prefix: folder_prefix.to_string(),
        env_created,
    })
}

// silence unused-warn for internal helpers that could be used later
#[allow(dead_code)]
fn _unused_marker(_: SchemaKind, _: Type, _: PathBuf) {}
