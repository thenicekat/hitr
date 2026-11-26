//! OpenAPI 3.x → Bruno YAML importer.
//!
//! Reads a spec file (yaml or json), walks `paths.<path>.<method>`, and emits
//! one Request per operation into `<root>/<folder_prefix>/<tag>/<name>.yml`.
//!
//! Path params are kept as literal `{param}` in the URL — Bruno-style. Body
//! comes from `content."application/json".example`; schema-driven body
//! generation is intentionally out of scope.

use crate::mcp_inputs::{ordinary_name, saved_url, FieldType, InputSchema, QueryField, Scalar};
use crate::model::*;
use openapiv3::{OpenAPI, Operation, Parameter, ReferenceOr, Schema, SchemaKind, Type};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    pub filled_empty_bodies: usize,
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
    let mut current = r;
    for _ in 0..16 {
        match current {
            ReferenceOr::Item(s) => return Some(s),
            ReferenceOr::Reference { reference } => {
                let name = reference
                    .strip_prefix("#/components/schemas/")?
                    .replace("~1", "/")
                    .replace("~0", "~");
                current = spec.components.as_ref()?.schemas.get(&name)?;
            }
        }
    }
    None
}

fn resolve_schema_boxed<'a>(
    spec: &'a OpenAPI,
    r: &'a ReferenceOr<Box<Schema>>,
) -> Option<&'a Schema> {
    match r {
        ReferenceOr::Item(boxed) => Some(boxed.as_ref()),
        ReferenceOr::Reference { reference } => {
            let name = reference
                .strip_prefix("#/components/schemas/")?
                .replace("~1", "/")
                .replace("~0", "~");
            resolve_schema(spec, spec.components.as_ref()?.schemas.get(&name)?)
        }
    }
}

fn resolve_request_body<'a>(
    spec: &'a OpenAPI,
    r: &'a ReferenceOr<openapiv3::RequestBody>,
) -> Option<&'a openapiv3::RequestBody> {
    let mut current = r;
    for _ in 0..16 {
        match current {
            ReferenceOr::Item(body) => return Some(body),
            ReferenceOr::Reference { reference } => {
                let name = reference
                    .strip_prefix("#/components/requestBodies/")?
                    .replace("~1", "/")
                    .replace("~0", "~");
                current = spec.components.as_ref()?.request_bodies.get(&name)?;
            }
        }
    }
    None
}

/// Build a skeleton JSON value from a schema. Bounded by `depth` to avoid
/// infinite recursion on circular refs (e.g. self-referential tree nodes).
fn schema_skeleton(spec: &OpenAPI, schema: &Schema, depth: u8) -> serde_json::Value {
    use openapiv3::{SchemaKind, Type as OAType};
    if depth == 0 {
        return serde_json::Value::Null;
    }
    match &schema.schema_kind {
        SchemaKind::Type(OAType::Object(obj)) => {
            let mut map = serde_json::Map::new();
            for (k, v) in &obj.properties {
                let child = resolve_schema_boxed(spec, v)
                    .map(|s| schema_skeleton(spec, s, depth - 1))
                    .unwrap_or(serde_json::Value::Null);
                map.insert(k.clone(), child);
            }
            serde_json::Value::Object(map)
        }
        SchemaKind::Type(OAType::Array(arr)) => {
            let item_val = arr
                .items
                .as_ref()
                .and_then(|r| resolve_schema_boxed(spec, r))
                .map(|s| schema_skeleton(spec, s, depth - 1))
                .unwrap_or(serde_json::Value::Null);
            serde_json::Value::Array(vec![item_val])
        }
        SchemaKind::Type(OAType::String(_)) => serde_json::Value::String(String::new()),
        SchemaKind::Type(OAType::Integer(_)) => serde_json::json!(0),
        SchemaKind::Type(OAType::Number(_)) => serde_json::json!(0.0),
        SchemaKind::Type(OAType::Boolean { .. }) => serde_json::Value::Bool(false),
        SchemaKind::AllOf { all_of } => {
            let mut map = serde_json::Map::new();
            for r in all_of {
                if let Some(s) = resolve_schema(spec, r) {
                    if let serde_json::Value::Object(m) = schema_skeleton(spec, s, depth - 1) {
                        map.extend(m);
                    }
                }
            }
            serde_json::Value::Object(map)
        }
        SchemaKind::OneOf { one_of } | SchemaKind::AnyOf { any_of: one_of } => one_of
            .first()
            .and_then(|r| resolve_schema(spec, r))
            .map(|s| schema_skeleton(spec, s, depth - 1))
            .unwrap_or(serde_json::Value::Null),
        _ => serde_json::Value::Null,
    }
}

fn extract_body(op: &Operation, spec: &OpenAPI) -> Body {
    let Some(rb) = op.request_body.as_ref() else {
        return Body::default();
    };
    let Some(rb) = resolve_request_body(spec, rb) else {
        return Body::default();
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
    if let Some((_, ReferenceOr::Item(ex))) = content.examples.iter().next() {
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
    // schema present but no example — generate skeleton
    if let Some(schema_ref) = &content.schema {
        if let Some(schema) = resolve_schema(spec, schema_ref) {
            let skeleton = schema_skeleton(spec, schema, 4);
            // only use if it produced something non-null
            if skeleton != serde_json::Value::Null {
                if let Ok(pretty) = serde_json::to_string_pretty(&skeleton) {
                    return Body {
                        r#type: Some("json".into()),
                        data: Some(pretty),
                        json: None,
                        text: None,
                    };
                }
            }
        }
        // schema exists but skeleton failed (e.g. unresolvable ref) — still mark json
        return Body {
            r#type: Some("json".into()),
            data: None,
            json: None,
            text: None,
        };
    }
    Body::default()
}

fn resolve_param<'a>(spec: &'a OpenAPI, r: &'a ReferenceOr<Parameter>) -> Option<&'a Parameter> {
    let mut current = r;
    for _ in 0..16 {
        match current {
            ReferenceOr::Item(p) => return Some(p),
            ReferenceOr::Reference { reference } => {
                let name = reference
                    .strip_prefix("#/components/parameters/")?
                    .replace("~1", "/")
                    .replace("~0", "~");
                current = spec.components.as_ref()?.parameters.get(&name)?;
            }
        }
    }
    None
}

fn parameter_key(p: &Parameter) -> (&str, &str) {
    let kind = match p {
        Parameter::Query { .. } => "query",
        Parameter::Header { .. } => "header",
        Parameter::Path { .. } => "path",
        Parameter::Cookie { .. } => "cookie",
    };
    (kind, &p.parameter_data_ref().name)
}

fn parameter_example(data: &openapiv3::ParameterData, spec: &OpenAPI) -> Option<String> {
    let schema = match &data.format {
        openapiv3::ParameterSchemaOrContent::Schema(s) => resolve_schema(spec, s),
        _ => None,
    };
    let value = data
        .example
        .as_ref()
        .or_else(|| {
            data.examples.values().find_map(|ex| match ex {
                ReferenceOr::Item(ex) => ex.value.as_ref(),
                _ => None,
            })
        })
        .or_else(|| schema.and_then(|s| s.schema_data.example.as_ref()))
        .or_else(|| schema.and_then(|s| s.schema_data.default.as_ref()))?;
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => Some(value.to_string()),
        _ => None,
    }
}

fn extract_params(op: &Operation, spec: &OpenAPI) -> (Vec<KV>, Vec<KV>) {
    let mut headers = Vec::new();
    let mut params = Vec::new();
    for p in &op.parameters {
        let Some(p) = resolve_param(spec, p) else {
            continue;
        };
        let (kind, name) = parameter_key(p);
        if kind != "query" && kind != "header" {
            continue;
        }
        let data = p.parameter_data_ref();
        let value = parameter_example(data, spec).unwrap_or_default();
        let enabled = data.required || !value.is_empty();
        let kv = KV {
            name: name.to_string(),
            value,
            r#type: Some(kind.to_string()),
            description: data.description.clone(),
            enabled: Some(enabled),
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
                    openapiv3::SecurityScheme::HTTP { scheme, .. }
                        if scheme.eq_ignore_ascii_case("bearer") =>
                    {
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
                let mut merged = op.clone();
                let mut params = item.parameters.clone();
                for parameter in &op.parameters {
                    if let Some(replacement) = resolve_param(spec, parameter) {
                        let (kind, name) = parameter_key(replacement);
                        params.retain(|existing| {
                            resolve_param(spec, existing).is_none_or(|p| {
                                let (existing_kind, existing_name) = parameter_key(p);
                                existing_kind != kind
                                    || if kind == "header" {
                                        !existing_name.eq_ignore_ascii_case(name)
                                    } else {
                                        existing_name != name
                                    }
                            })
                        });
                    }
                    params.push(parameter.clone());
                }
                merged.parameters = params;
                out.push((path.clone(), method.to_string(), merged));
            }
        }
    }
    out
}

fn input_type(spec: &OpenAPI, schema: &Schema) -> Option<FieldType> {
    fn scalar(schema: &Schema) -> Option<Scalar> {
        match &schema.schema_kind {
            SchemaKind::Type(Type::String(s))
                if !matches!(
                    s.format,
                    openapiv3::VariantOrUnknownOrEmpty::Item(openapiv3::StringFormat::Password)
                ) =>
            {
                Some(Scalar::String)
            }
            SchemaKind::Type(Type::Integer(_)) => Some(Scalar::Integer),
            SchemaKind::Type(Type::Number(_)) => Some(Scalar::Number),
            SchemaKind::Type(Type::Boolean(_)) => Some(Scalar::Boolean),
            _ => None,
        }
    }
    if schema.schema_data.read_only {
        return None;
    }
    let (scalar, array) = match &schema.schema_kind {
        SchemaKind::Type(Type::Array(array)) => (
            scalar(resolve_schema_boxed(spec, array.items.as_ref()?)?)?,
            true,
        ),
        _ => (scalar(schema)?, false),
    };
    Some(FieldType {
        scalar,
        array,
        properties: BTreeMap::new(),
    })
}

// Body schemas can contain nested objects; query parameters remain scalar or scalar arrays.
// Bound recursion prevents cyclic OpenAPI references from exhausting the stack.
fn body_input_type(spec: &OpenAPI, schema: &Schema, depth: usize) -> Option<FieldType> {
    if depth == 0 || schema.schema_data.read_only {
        return None;
    }
    match &schema.schema_kind {
        SchemaKind::Type(Type::Array(array)) => {
            let item = resolve_schema_boxed(spec, array.items.as_ref()?)?;
            let mut field = body_input_type(spec, item, depth - 1)?;
            if field.array {
                return None;
            }
            field.array = true;
            Some(field)
        }
        SchemaKind::Type(Type::Object(_)) | SchemaKind::AllOf { .. } => {
            let mut properties = BTreeMap::new();
            body_fields(spec, schema, depth - 1, &mut properties);
            (!properties.is_empty()).then_some(FieldType {
                scalar: Scalar::Object,
                array: false,
                properties,
            })
        }
        _ => input_type(spec, schema),
    }
}

fn body_fields(
    spec: &OpenAPI,
    schema: &Schema,
    depth: usize,
    fields: &mut BTreeMap<String, FieldType>,
) {
    if depth == 0 {
        return;
    }
    match &schema.schema_kind {
        SchemaKind::Type(Type::Object(object)) => {
            for (name, field) in &object.properties {
                if let Some(kind) = resolve_schema_boxed(spec, field)
                    .and_then(|field| body_input_type(spec, field, depth - 1))
                {
                    // An object named `key` (such as a selector) is structural, not an API key.
                    if ordinary_name(name)
                        || (name == "key" && matches!(kind.scalar, Scalar::Object))
                    {
                        fields.insert(name.clone(), kind);
                    }
                }
            }
        }
        SchemaKind::AllOf { all_of } => {
            for branch in all_of
                .iter()
                .filter_map(|branch| resolve_schema(spec, branch))
            {
                body_fields(spec, branch, depth - 1, fields);
            }
        }
        _ => {}
    }
}

fn mcp_schema(spec: &OpenAPI, method: &str, path: &str, op: &Operation) -> InputSchema {
    use openapiv3::{ParameterSchemaOrContent, QueryStyle, SecurityScheme};
    let api_keys: BTreeSet<_> = spec
        .components
        .iter()
        .flat_map(|components| components.security_schemes.values())
        .filter_map(|scheme| match scheme {
            ReferenceOr::Item(SecurityScheme::APIKey { name, .. }) => {
                Some(name.to_ascii_lowercase())
            }
            _ => None,
        })
        .collect();
    let mut schema = InputSchema {
        version: 1,
        method: method.to_owned(),
        url: build_url(path),
        query: BTreeMap::new(),
        omit_empty_query: BTreeSet::new(),
        body: BTreeMap::new(),
    };
    for param in op.parameters.iter().filter_map(|p| resolve_param(spec, p)) {
        let Parameter::Query {
            parameter_data,
            style,
            allow_empty_value,
            ..
        } = param
        else {
            continue;
        };
        let data = parameter_data;
        if !ordinary_name(&data.name) || api_keys.contains(&data.name.to_ascii_lowercase()) {
            continue;
        }
        if !data.required && *allow_empty_value != Some(true) {
            schema.omit_empty_query.insert(data.name.clone());
        }
        let ParameterSchemaOrContent::Schema(reference) = &data.format else {
            continue;
        };
        let Some(field) = resolve_schema(spec, reference).and_then(|s| input_type(spec, s)) else {
            continue;
        };
        let separator = match style {
            QueryStyle::Form => (field.array && data.explode == Some(false)).then_some(','),
            QueryStyle::SpaceDelimited if field.array && data.explode != Some(true) => Some(' '),
            QueryStyle::PipeDelimited if field.array && data.explode != Some(true) => Some('|'),
            _ => continue,
        };
        schema
            .query
            .insert(data.name.clone(), QueryField { field, separator });
    }
    if let Some(body) = op
        .request_body
        .as_ref()
        .and_then(|body| resolve_request_body(spec, body))
    {
        if let Some(body_schema) = body
            .content
            .get("application/json")
            .and_then(|content| content.schema.as_ref())
            .and_then(|reference| resolve_schema(spec, reference))
        {
            body_fields(spec, body_schema, 8, &mut schema.body);
            fn remove_api_keys(
                fields: &mut BTreeMap<String, FieldType>,
                api_keys: &BTreeSet<String>,
            ) {
                fields.retain(|name, field| {
                    remove_api_keys(&mut field.properties, api_keys);
                    (!api_keys.contains(&name.to_ascii_lowercase())
                        || matches!(field.scalar, Scalar::Object))
                        && (!matches!(field.scalar, Scalar::Object) || !field.properties.is_empty())
                });
            }
            remove_api_keys(&mut schema.body, &api_keys);
        }
    }
    schema
}

/// Recover metadata for older imports without rewriting their request files.
pub fn recover_mcp_schemas(root: &Path, requests: &mut [Request]) {
    let mut pending: HashMap<(String, String), Vec<&mut Request>> = HashMap::new();
    for req in requests {
        let valid = req
            .mcp_schema
            .as_ref()
            .is_some_and(|schema| schema.matches(req));
        if valid
            && req
                .mcp_schema
                .as_ref()
                .is_some_and(|schema| schema.version >= 1)
        {
            continue;
        }
        // Keep usable legacy metadata if the original OpenAPI document is unavailable.
        if !valid {
            req.mcp_schema = None;
        }
        if req.http.url.starts_with("{{baseUrl}}/") {
            pending
                .entry((
                    req.http.method.to_uppercase(),
                    saved_url(&req.http.url).to_owned(),
                ))
                .or_default()
                .push(req);
        }
    }
    for filename in [
        "openapi.yaml",
        "openapi.yml",
        "openapi.json",
        "spec.yaml",
        "spec.yml",
        "spec.json",
    ] {
        if pending.is_empty() {
            break;
        }
        let Ok(spec) = read_spec(&root.join(filename)) else {
            continue;
        };
        for (path, method, op) in iter_ops(&spec) {
            if let Some(requests) = pending.remove(&(method.clone(), build_url(&path))) {
                let schema = mcp_schema(&spec, &method, &path, &op);
                for req in requests {
                    req.mcp_schema = Some(schema.clone());
                }
            }
        }
    }
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

/// Fill only an empty JSON body on the matching request. Keep unknown YAML
/// fields, and leave edited or mismatched requests untouched.
fn fill_empty_body(file: &Path, method: &str, url: &str, body: &Body) -> Result<bool, String> {
    let Some(template) = body.data.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(false);
    };
    if body.r#type.as_deref() != Some("json") {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(file).map_err(|e| e.to_string())?;
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&raw).map_err(|e| e.to_string())?;
    let Some(http) = doc
        .get_mut("http")
        .and_then(serde_yaml::Value::as_mapping_mut)
    else {
        return Ok(false);
    };
    if http
        .get("method")
        .and_then(serde_yaml::Value::as_str)
        .is_none_or(|m| !m.eq_ignore_ascii_case(method))
        || http.get("url").and_then(serde_yaml::Value::as_str) != Some(url)
    {
        return Ok(false);
    }
    let Some(existing) = http
        .get_mut("body")
        .and_then(serde_yaml::Value::as_mapping_mut)
    else {
        return Ok(false);
    };
    if existing.get("type").and_then(serde_yaml::Value::as_str) != Some("json") {
        return Ok(false);
    }
    for key in ["data", "json", "text"] {
        if let Some(value) = existing.get(key) {
            if !value.is_null() && value.as_str().is_none_or(|s| !s.trim().is_empty()) {
                return Ok(false);
            }
        }
    }
    existing.insert(
        serde_yaml::Value::String("data".into()),
        serde_yaml::Value::String(template.into()),
    );
    let output = serde_yaml::to_string(&doc).map_err(|e| e.to_string())?;
    use std::io::Write;
    let parent = file
        .parent()
        .ok_or_else(|| "request has no parent directory".to_string())?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temp.as_file()
        .set_permissions(
            std::fs::metadata(file)
                .map_err(|e| e.to_string())?
                .permissions(),
        )
        .map_err(|e| e.to_string())?;
    temp.write_all(output.as_bytes())
        .map_err(|e| e.to_string())?;
    if std::fs::read_to_string(file).map_err(|e| e.to_string())? != raw {
        return Err(format!("request changed during import: {}", file.display()));
    }
    temp.persist(file).map_err(|e| e.to_string())?;
    Ok(true)
}

pub fn import(
    root: &Path,
    spec_path: &Path,
    folder_prefix: &str,
    create_env: bool,
    env_name: &str,
    fill_empty_bodies: bool,
) -> Result<ImportStats, String> {
    let spec = read_spec(spec_path)?;
    let mut written = 0usize;
    let mut skipped = 0usize;
    let mut filled_empty_bodies = 0usize;
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
            if fill_empty_bodies
                && fill_empty_body(&file, method, &build_url(path), &extract_body(op, &spec))?
            {
                filled_empty_bodies += 1;
            } else {
                skipped += 1;
            }
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
                auth_config: None,
            },
            mcp_schema: Some(mcp_schema(&spec, method, path, op)),
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
        filled_empty_bodies,
        folder_prefix: folder_prefix.to_string(),
        env_created,
    })
}

// silence unused-warn for internal helpers that could be used later
#[allow(dead_code)]
fn _unused_marker(_: SchemaKind, _: Type, _: PathBuf) {}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = r##"
openapi: 3.0.3
info: {title: Test, version: '1'}
paths:
  /widgets/search:
    post:
      operationId: search-widgets
      tags: [widgets]
      requestBody:
        content:
          application/json:
            schema: {$ref: '#/components/schemas/SearchRequest'}
      responses: {'200': {description: ok}}
components:
  schemas:
    SearchRequest:
      type: object
      properties:
        filter: {type: string}
        limit: {type: integer}
"##;

    #[test]
    fn reimport_fills_only_matching_empty_bodies() {
        let dir = std::env::temp_dir().join(format!(
            "hitr-openapi-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let spec = dir.join("spec.yaml");
        std::fs::write(&spec, SPEC).unwrap();
        let target = dir.join("out/widgets/search_widgets.yml");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "info:\n  name: search-widgets\n  type: http\nhttp:\n  method: POST\n  url: '{{baseUrl}}/widgets/search'\n  body:\n    type: json\ncustom_field: keep\n").unwrap();
        let skipped = import(&dir, &spec, "out", false, "", false).unwrap();
        assert_eq!(skipped.skipped_existing, 1);
        let filled = import(&dir, &spec, "out", false, "", true).unwrap();
        assert_eq!(filled.filled_empty_bodies, 1);
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(yaml["custom_field"].as_str(), Some("keep"));
        let body: serde_json::Value =
            serde_json::from_str(yaml["http"]["body"]["data"].as_str().unwrap()).unwrap();
        assert_eq!(body["filter"], "");
        assert_eq!(body["limit"], 0);
        let before = std::fs::read(&target).unwrap();
        let again = import(&dir, &spec, "out", false, "", true).unwrap();
        assert_eq!(again.skipped_existing, 1);
        assert_eq!(before, std::fs::read(&target).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn follows_local_request_body_and_schema_references() {
        let spec: OpenAPI = serde_yaml::from_str(
            r##"
openapi: 3.0.3
info: {title: Example, version: '1'}
paths:
  /widgets:
    post:
      requestBody: {$ref: '#/components/requestBodies/WidgetBody'}
      responses: {'200': {description: ok}}
components:
  requestBodies:
    WidgetBody:
      content:
        application/json:
          schema: {$ref: '#/components/schemas/Alias'}
  schemas:
    Alias: {$ref: '#/components/schemas/Widget'}
    Widget:
      type: object
      properties:
        title: {type: string}
"##,
        )
        .unwrap();
        let op = &iter_ops(&spec)[0].2;
        let body = extract_body(op, &spec);
        let value: serde_json::Value = serde_json::from_str(body.data.as_deref().unwrap()).unwrap();
        assert_eq!(value["title"], "");
    }

    #[test]
    fn imports_inherited_and_overridden_parameters_with_values() {
        let spec: OpenAPI = serde_yaml::from_str(
            r##"
openapi: 3.0.3
info: {title: Test, version: '1'}
paths:
  /items:
    parameters:
      - $ref: '#/components/parameters/Page'
      - {in: query, name: empty, schema: {type: string}}
      - {in: header, name: X-Mode, schema: {type: string}, example: old}
    get:
      parameters:
        - {in: header, name: x-mode, schema: {type: string}, example: new}
        - {in: query, name: enabled, schema: {type: boolean, default: false}}
      responses: {'200': {description: ok}}
components:
  parameters:
    Page: {in: query, name: page, schema: {type: integer, example: 3}}
"##,
        )
        .unwrap();
        let op = &iter_ops(&spec)[0].2;
        let (headers, params) = extract_params(op, &spec);
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].value, "new");
        let page = params.iter().find(|p| p.name == "page").unwrap();
        assert_eq!(page.value, "3");
        assert_eq!(page.enabled, Some(true));
        let empty = params.iter().find(|p| p.name == "empty").unwrap();
        assert_eq!(empty.enabled, Some(false));
        assert_eq!(
            params.iter().find(|p| p.name == "enabled").unwrap().value,
            "false"
        );
    }
}
