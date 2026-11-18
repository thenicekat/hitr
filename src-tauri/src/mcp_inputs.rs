//! Typed, per-call inputs derived from a saved request's OpenAPI operation.

use crate::{http, model::Request, vault::VaultData};
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputSchema {
    pub method: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, QueryField>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub omit_empty_query: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub body: BTreeMap<String, FieldType>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryField {
    #[serde(flatten)]
    pub field: FieldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub separator: Option<char>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldType {
    #[serde(rename = "type")]
    pub scalar: Scalar,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub array: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scalar {
    String,
    Integer,
    Number,
    Boolean,
}

impl FieldType {
    fn scalar_matches(&self, value: &Value) -> bool {
        match self.scalar {
            Scalar::String => value.is_string(),
            Scalar::Integer => value.is_i64() || value.is_u64(),
            Scalar::Number => value.is_number(),
            Scalar::Boolean => value.is_boolean(),
        }
    }

    fn accepts(&self, value: &Value) -> bool {
        if self.array {
            value
                .as_array()
                .is_some_and(|items| items.iter().all(|item| self.scalar_matches(item)))
        } else {
            self.scalar_matches(value)
        }
    }

    fn json_schema(&self) -> Value {
        let item = json!({ "type": self.scalar });
        if self.array {
            json!({ "type": "array", "items": item })
        } else {
            item
        }
    }
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inputs {
    #[serde(default)]
    pub query: BTreeMap<String, Value>,
    #[serde(default)]
    pub body: BTreeMap<String, Value>,
}

#[derive(Default)]
struct Available<'a> {
    query: BTreeMap<&'a str, &'a QueryField>,
    body: BTreeMap<&'a str, &'a FieldType>,
}

pub fn saved_url(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

pub fn ordinary_name(name: &str) -> bool {
    if name.is_empty() || name.contains("{{") {
        return false;
    }
    let normalized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    !normalized.is_empty()
        && ![
            "token",
            "secret",
            "password",
            "apikey",
            "credential",
            "authorization",
            "cookie",
        ]
        .iter()
        .any(|part| normalized.contains(part))
        && !matches!(normalized.as_str(), "auth" | "key" | "signature")
}

impl InputSchema {
    pub fn matches(&self, req: &Request) -> bool {
        self.method.eq_ignore_ascii_case(&req.http.method) && self.url == saved_url(&req.http.url)
    }

    fn available(&self, req: &Request) -> Available<'_> {
        let mut available = Available::default();
        let query_in_url = req.http.url.split_once('?').map_or("", |(_, q)| q);
        for (name, field) in &self.query {
            let is_api_key =
                req.http.auth_config.as_ref().is_some_and(|auth| {
                    auth.mode == "api_key" && auth.key.eq_ignore_ascii_case(name)
                });
            let in_url = url::form_urlencoded::parse(query_in_url.as_bytes())
                .any(|(key, _)| key.eq_ignore_ascii_case(name));
            let is_bound = req
                .http
                .params
                .iter()
                .any(|p| p.name.eq_ignore_ascii_case(name) && p.value.contains("{{"));
            if ordinary_name(name) && !is_api_key && !in_url && !is_bound {
                available.query.insert(name.as_str(), field);
            }
        }

        // Keep saved JSON fields bound to the environment under desktop control.
        let body = &req.http.body;
        if body.r#type.as_deref().is_none_or(|kind| kind == "json") {
            let raw = body.json_template().unwrap_or_default();
            let saved = if raw.trim().is_empty() {
                Some(Map::new())
            } else {
                serde_json::from_str::<Map<String, Value>>(&http::strip_json_comments(raw)).ok()
            };
            if let Some(saved) = saved.filter(|values| values.keys().all(|key| !key.contains("{{")))
            {
                for (name, field) in &self.body {
                    let protected = saved.get(name).is_some_and(|value| {
                        value.is_object()
                            || value
                                .as_array()
                                .is_some_and(|items| items.iter().any(Value::is_object))
                            || value.to_string().contains("{{")
                    });
                    if ordinary_name(name) && !protected {
                        available.body.insert(name.as_str(), field);
                    }
                }
            }
        }
        available
    }
}

pub fn describe(req: &Request) -> Value {
    let available = req
        .mcp_schema
        .as_ref()
        .filter(|schema| schema.matches(req))
        .map_or_else(Available::default, |schema| schema.available(req));
    let query = available
        .query
        .into_iter()
        .map(|(name, field)| (name.to_owned(), field.field.json_schema()))
        .collect();
    let body = available
        .body
        .into_iter()
        .map(|(name, field)| (name.to_owned(), field.json_schema()))
        .collect();
    let mut sections = Map::new();
    for (name, properties) in [("query", query), ("body", body)] {
        if !Map::is_empty(&properties) {
            sections.insert(name.to_owned(), object_schema(properties));
        }
    }
    object_schema(sections)
}

fn object_schema(properties: Map<String, Value>) -> Value {
    json!({"type": "object", "properties": properties, "additionalProperties": false})
}

pub fn prepare(
    client: &reqwest::Client,
    mut req: Request,
    env: Option<&crate::model::Env>,
    vault: Option<&VaultData>,
    inputs: Inputs,
) -> Result<reqwest::Request, &'static str> {
    let schema = req.mcp_schema.take().filter(|schema| schema.matches(&req));
    let available = schema
        .as_ref()
        .map_or_else(Available::default, |schema| schema.available(&req));

    let mut query = Vec::new();
    for (name, value) in &inputs.query {
        let field = available
            .query
            .get(name.as_str())
            .ok_or("Unknown or protected query input. Check input_schema in search_requests.")?;
        if !field.field.accepts(value) {
            return Err("Query input has the wrong type. Check input_schema in search_requests.");
        }
        let to_text = |value: &Value| {
            value
                .as_str()
                .map_or_else(|| value.to_string(), |s| s.to_owned())
        };
        let values = if let Some(items) = value.as_array() {
            let items: Vec<_> = items.iter().map(to_text).collect();
            if let Some(separator) = field.separator.filter(|_| !items.is_empty()) {
                vec![items.join(&separator.to_string())]
            } else {
                items
            }
        } else {
            vec![to_text(value)]
        };
        query.push((name.as_str(), values));
    }
    for (name, value) in &inputs.body {
        let field = available
            .body
            .get(name.as_str())
            .ok_or("Unknown or protected JSON input. Check input_schema in search_requests.")?;
        if !field.accepts(value) {
            return Err("JSON input has the wrong type. Check input_schema in search_requests.");
        }
    }

    req.http.params.retain(|param| {
        let empty_optional = param.value.is_empty()
            && schema
                .as_ref()
                .is_some_and(|schema| schema.omit_empty_query.contains(&param.name));
        !(empty_optional || inputs.query.contains_key(&param.name))
    });

    let mut prepared = http::prepare(client, &req, env, vault, http::RequestMode::Mcp)
        .map_err(|_| "Hitr could not prepare this request. Check its HTTPS URL, environment origin and variables.")?;
    // Add agent values after template resolution so they are sent literally.
    for (name, values) in query {
        if prepared
            .url()
            .query_pairs()
            .any(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            return Err("Query input conflicts with a saved URL value or credential.");
        }
        if !values.is_empty() {
            let mut pairs = prepared.url_mut().query_pairs_mut();
            for value in values {
                pairs.append_pair(name, &value);
            }
        }
    }
    if !inputs.body.is_empty() {
        let mut body: Map<String, Value> = match prepared.body().and_then(reqwest::Body::as_bytes) {
            Some(bytes) if !bytes.is_empty() => serde_json::from_slice(bytes)
                .map_err(|_| "Hitr could not merge inputs into the saved JSON body.")?,
            _ => Map::new(),
        };
        body.extend(inputs.body);
        prepared
            .headers_mut()
            .entry(reqwest::header::CONTENT_TYPE)
            .or_insert(reqwest::header::HeaderValue::from_static(
                "application/json",
            ));
        // Let reqwest compute the size for the new body.
        prepared
            .headers_mut()
            .remove(reqwest::header::CONTENT_LENGTH);
        *prepared.body_mut() = Some(Value::Object(body).to_string().into());
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{loader, model};

    #[test]
    fn recovers_and_applies_inputs_without_rewriting_saved_requests() {
        let dir = tempfile::Builder::new()
            .prefix("hitr-inputs-")
            .tempdir()
            .unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("spec.yaml"),
            r#"
openapi: 3.0.3
info: {title: Widgets, version: "1"}
components:
  securitySchemes:
    custom: {type: apiKey, in: query, name: region}
  schemas:
    Labels:
      type: array
      items: {type: string}
paths:
  /widgets:
    get:
      parameters:
        - {in: query, name: labels, schema: {$ref: '#/components/schemas/Labels'}}
        - {in: query, name: limit, schema: {type: integer}}
        - {in: query, name: mode, schema: {type: string}}
        - {in: query, name: region, schema: {type: string}}
        - {in: header, name: Authorization, schema: {type: string}}
      responses: {'200': {description: OK}}
    post:
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                labels: {$ref: '#/components/schemas/Labels'}
                limit: {type: integer}
                workspace: {type: string}
                password: {type: string}
      responses: {'200': {description: OK}}
"#,
        )
        .unwrap();
        let get = r#"
info: {name: List widgets, type: http}
http:
  method: GET
  url: '{{baseUrl}}/widgets'
  params:
    - {name: mode, value: '', enabled: true}
    - {name: limit, value: '50', enabled: true}
    - {name: region, value: '{{region}}', enabled: true}
"#;
        let post = r#"
info: {name: Search widgets, type: http}
http:
  method: POST
  url: '{{baseUrl}}/widgets'
  body:
    type: json
    data: '{"workspace":"{{region}}","limit":50}'
"#;
        std::fs::write(root.join("get.yml"), get).unwrap();
        std::fs::write(root.join("post.yml"), post).unwrap();

        let collection = loader::load_collection(root).unwrap();
        let get_req = &collection.requests[0];
        let post_req = &collection.requests[1];
        let schema = describe(get_req);
        let query = &schema["properties"]["query"]["properties"];
        assert_eq!(query["labels"]["items"]["type"], "string");
        assert_eq!(query["limit"]["type"], "integer");
        assert!(query.get("region").is_none());
        assert!(query.get("Authorization").is_none());
        let schema = describe(post_req);
        let body = &schema["properties"]["body"]["properties"];
        assert!(body.get("workspace").is_none());
        assert!(body.get("password").is_none());
        assert_eq!(body["labels"]["type"], "array");

        let client = reqwest::Client::new();
        let env = model::Env {
            variables: vec![
                model::EnvVar {
                    name: "baseUrl".into(),
                    value: "https://api.example.test".into(),
                    secret: false,
                },
                model::EnvVar {
                    name: "region".into(),
                    value: "fixture-value".into(),
                    secret: false,
                },
            ],
            ..Default::default()
        };

        let inputs = |value| serde_json::from_value::<Inputs>(value).unwrap();
        let get_http = prepare(
            &client,
            get_req.clone(),
            Some(&env),
            None,
            inputs(json!({"query":{"labels":["a&b", "{{region}}"],"limit":10}})),
        )
        .unwrap();
        let pairs: Vec<_> = get_http.url().query_pairs().into_owned().collect();
        assert_eq!(
            pairs,
            [
                ("region".into(), "fixture-value".into()),
                ("labels".into(), "a&b".into()),
                ("labels".into(), "{{region}}".into()),
                ("limit".into(), "10".into()),
            ]
        );

        let post_http = prepare(
            &client,
            post_req.clone(),
            Some(&env),
            None,
            inputs(json!({"body":{"labels":["{{region}}"],"limit":10}})),
        )
        .unwrap();
        let bytes = post_http.body().unwrap().as_bytes().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(bytes).unwrap(),
            json!({"labels":["{{region}}"], "limit":10, "workspace":"fixture-value"})
        );
        for (req, input) in [
            (get_req, json!({"query":{"limit":"10"}})),
            (get_req, json!({"query":{"region":"new"}})),
            (post_req, json!({"body":{"workspace":"new"}})),
        ] {
            assert!(prepare(&client, req.clone(), Some(&env), None, inputs(input)).is_err());
        }
        assert_eq!(std::fs::read_to_string(root.join("get.yml")).unwrap(), get);
        assert_eq!(
            std::fs::read_to_string(root.join("post.yml")).unwrap(),
            post
        );

        let spec = root.join("spec.yaml");
        crate::openapi::import(root, &spec, "imported", false, "", false).unwrap();
        std::fs::remove_file(spec).unwrap();
        let collection = loader::load_collection(root).unwrap();
        let imported = collection
            .requests
            .iter()
            .find(|req| req.id.starts_with("imported/") && req.http.method == "GET")
            .unwrap();
        assert_eq!(describe(imported), describe(get_req));
    }
}
