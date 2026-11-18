//! Local stdio MCP bridge. The desktop process owns the vault and HTTP client.

use crate::{http, loader, model, vault::VaultState, AppState};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router, ServiceExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use url::{Origin, Url};

const MAX_BODY_BYTES: usize = 128 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    grants: Vec<Grant>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    request_id: String,
    environment: Option<String>,
    origin: Url,
    #[serde(default)]
    return_body: bool,
}

struct ApprovedRequest {
    request: model::Request,
    environment: Option<model::Env>,
    origin: Origin,
    return_body: bool,
}

impl ApprovedRequest {
    fn environment_name(&self) -> Option<&str> {
        self.environment.as_ref().map(|env| env.name.as_str())
    }
}

fn resolve_grants(
    grants: Vec<Grant>,
    collection: &model::Collection,
) -> Result<Vec<ApprovedRequest>, String> {
    let mut approved = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for grant in grants {
        if grant.origin.scheme() != "https"
            || grant.origin.path() != "/"
            || grant.origin.query().is_some()
            || grant.origin.fragment().is_some()
            || !grant.origin.username().is_empty()
            || grant.origin.password().is_some()
        {
            return Err(
                "Each MCP grant needs an HTTPS origin without a path or credentials.".into(),
            );
        }
        if !seen.insert((grant.request_id.clone(), grant.environment.clone())) {
            return Err("An MCP request and environment pair is listed more than once.".into());
        }
        let request = collection
            .requests
            .iter()
            .find(|req| req.id == grant.request_id)
            .ok_or_else(|| format!("Unknown MCP request: {}", grant.request_id))?;
        let environment = if let Some(name) = &grant.environment {
            let mut matches = collection.envs.iter().filter(|env| env.name == *name);
            let env = matches
                .next()
                .filter(|_| matches.next().is_none())
                .ok_or_else(|| format!("Unknown or ambiguous MCP environment: {name}"))?;
            Some(env.clone())
        } else {
            None
        };
        approved.push(ApprovedRequest {
            request: request.clone(),
            environment,
            origin: grant.origin.origin(),
            return_body: grant.return_body,
        });
    }
    Ok(approved)
}

#[derive(Clone)]
struct Broker {
    vault: Arc<VaultState>,
    approved: Arc<[ApprovedRequest]>,
    client: reqwest::Client,
    requests: Arc<Semaphore>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Search {
    #[schemars(
        description = "Words from the saved request name, ID or HTTP method. Empty lists approved requests."
    )]
    query: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendRequest {
    #[schemars(description = "Exact request ID from search_requests.")]
    request_id: String,
    #[schemars(
        description = "Environment from search_requests, or null for a request approved without one."
    )]
    environment: Option<String>,
}

#[tool_router(server_handler)]
impl Broker {
    #[tool(
        description = "Search saved Hitr requests approved for MCP and see their allowed environments."
    )]
    fn search_requests(&self, Parameters(input): Parameters<Search>) -> CallToolResult {
        let query = input.query.to_lowercase();
        let mut found = self.approved.iter().filter(|item| {
            let req = &item.request;
            let matches =
                format!("{} {} {}", req.info.name, req.id, req.http.method).to_lowercase();
            query.split_whitespace().all(|word| matches.contains(word))
        });
        let requests: Vec<_> = found
            .by_ref()
            .take(25)
            .map(|item| {
                json!({
                    "request_id": item.request.id,
                    "name": item.request.info.name,
                    "method": item.request.http.method,
                    "environment": item.environment_name(),
                })
            })
            .collect();
        CallToolResult::structured(json!({
            "requests": requests,
            "more_matches": found.next().is_some(),
        }))
    }

    #[tool(
        description = "Execute an approved saved request with its chosen environment. The request can change remote data."
    )]
    async fn send_request(&self, Parameters(input): Parameters<SendRequest>) -> CallToolResult {
        match self.send(input).await {
            Ok(result) => CallToolResult::structured(result),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        }
    }
}

impl Broker {
    async fn send(&self, input: SendRequest) -> Result<Value, &'static str> {
        let item = self
            .approved
            .iter()
            .find(|item| {
                item.request.id == input.request_id
                    && item.environment_name() == input.environment.as_deref()
            })
            .ok_or("Request and environment are not approved. Use search_requests.")?;
        let _permit = self
            .requests
            .try_acquire()
            .map_err(|_| "Hitr is busy. Retry shortly.")?;
        let uses_vault = item
            .environment
            .as_ref()
            .is_some_and(|env| env.variables.iter().any(|v| v.secret));
        let vault = {
            let cache = self.vault.cache.lock().unwrap();
            if uses_vault && self.vault.password.lock().unwrap().is_none() {
                return Err("Unlock the vault in the Hitr desktop app first.");
            }
            cache.clone()
        };

        let request = http::prepare(&self.client, &item.request, item.environment.as_ref(), vault.as_ref(), Some(&item.origin))
            .map_err(|_| "Hitr could not prepare this request. Check the saved request, environment values and approved origin.")?;
        let secrets = if item.return_body {
            response_secrets(&request, vault.as_ref())
        } else {
            Vec::new()
        };
        let start = Instant::now();
        let response = self
            .client
            .execute(request)
            .await
            .map_err(|_| "Hitr could not reach the approved API.")?;
        let status = response.status().as_u16();
        let body = if item.return_body {
            read_body(response, &secrets).await?
        } else {
            None
        };
        Ok(json!({
            "status": status,
            "latency_ms": start.elapsed().as_millis() as u64,
            "body": body,
            "body_omitted": body.is_none(),
        }))
    }
}

fn response_secrets(
    request: &reqwest::Request,
    vault: Option<&crate::vault::VaultData>,
) -> Vec<String> {
    let mut values: Vec<String> = vault
        .into_iter()
        .flat_map(|vault| vault.secrets.values())
        .cloned()
        .collect();
    // Requests may also contain literal credentials, including base64 Basic auth.
    for (name, header) in request.headers() {
        if matches!(name.as_str(), "accept" | "content-type" | "user-agent") {
            continue;
        }
        if let Ok(value) = header.to_str() {
            values.push(value.to_owned());
            if name == "authorization" {
                if let Some((_, credential)) = value.split_once(' ') {
                    values.push(credential.to_owned());
                }
            }
        }
    }
    if let Some(query) = request.url().query() {
        for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if ["key", "token", "secret", "password", "credential"]
                .iter()
                .any(|part| name.to_ascii_lowercase().contains(part))
            {
                values.push(value.into_owned());
            }
        }
    }
    values.retain(|value| !value.is_empty());
    values
}

async fn read_body(
    mut response: reqwest::Response,
    secrets: &[String],
) -> Result<Option<String>, &'static str> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Hitr could not read the API response.")?
    {
        if bytes.len() + chunk.len() > MAX_BODY_BYTES {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }
    let Ok(body) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    Ok(filter_body(body, secrets))
}

fn filter_body(body: String, secrets: &[String]) -> Option<String> {
    fn has_secret(text: &str, secrets: &[String]) -> bool {
        secrets.iter().any(|secret| {
            let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
            text.contains(secret) || text.contains(&encoded)
        })
    }

    fn contains_secret(value: &Value, secrets: &[String]) -> bool {
        match value {
            Value::String(s) => has_secret(s, secrets),
            Value::Array(values) => values.iter().any(|v| contains_secret(v, secrets)),
            Value::Object(obj) => obj
                .iter()
                .any(|(k, v)| has_secret(k, secrets) || contains_secret(v, secrets)),
            _ => false,
        }
    }

    if has_secret(&body, secrets)
        || serde_json::from_str::<Value>(&body).is_ok_and(|json| contains_secret(&json, secrets))
    {
        None
    } else {
        Some(body)
    }
}

fn bridge_dir() -> io::Result<PathBuf> {
    let config = super::config_path()
        .ok_or_else(|| io::Error::other("Hitr config directory is unavailable."))?;
    Ok(config.with_file_name("mcp"))
}

fn listen(dir: &Path) -> io::Result<(File, UnixListener)> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    let meta = dir.symlink_metadata()?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "Hitr MCP needs a private socket directory.",
        ));
    }
    let lock = OpenOptions::new()
        .mode(0o600)
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))?;
    lock.try_lock()
        .map_err(|_| io::Error::other("A Hitr MCP instance is already running."))?;
    let socket = dir.join("bridge.sock");
    match socket.symlink_metadata() {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(&socket)?,
        Ok(_) => return Err(io::Error::other("Hitr MCP socket path is occupied.")),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    Ok((lock, listener))
}

pub(super) fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = serve(app).await {
            eprintln!("Hitr MCP: {error}");
        }
    });
}

async fn serve(app: AppHandle) -> Result<(), String> {
    let dir = bridge_dir().map_err(|e| e.to_string())?;
    let config_file = dir.with_extension("json");
    let state = app.state::<AppState>();
    let root = state.root.lock().unwrap().clone();
    let vault = state.vault.clone();
    let Some(approved) = tokio::task::spawn_blocking(move || {
        let raw = match std::fs::read(config_file) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.to_string()),
        };
        let config: Config = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        let collection = loader::load_collection(&root)?;
        resolve_grants(config.grants, &collection).map(Some)
    })
    .await
    .map_err(|err| err.to_string())??
    else {
        return Ok(());
    };
    if approved.is_empty() {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|err| err.to_string())?;
    let broker = Broker {
        vault,
        approved: approved.into(),
        client,
        requests: Arc::new(Semaphore::new(4)),
    };
    let (_lock, listener) = listen(&dir).map_err(|err| err.to_string())?;
    let connections = Arc::new(Semaphore::new(8));
    loop {
        let (stream, _) = listener.accept().await.map_err(|err| err.to_string())?;
        let Ok(permit) = connections.clone().try_acquire_owned() else {
            continue;
        };
        let broker = broker.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let initialized =
                tokio::time::timeout(Duration::from_secs(10), broker.serve(stream)).await;
            if let Ok(Ok(service)) = initialized {
                let _ = service.waiting().await;
            }
        });
    }
}

pub fn stdio_bridge() -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let stream = UnixStream::connect(bridge_dir()?.join("bridge.sock")).await.map_err(|err| {
            io::Error::new(err.kind(), format!("Could not connect to Hitr MCP. Configure grants and start the Hitr desktop app. ({err})"))
        })?;
        let (mut incoming, mut outgoing) = stream.into_split();
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let upstream = async {
            tokio::io::copy(&mut stdin, &mut outgoing).await?;
            outgoing.shutdown().await
        };
        let downstream = async {
            tokio::io::copy(&mut incoming, &mut stdout).await?;
            stdout.flush().await
        };
        tokio::pin!(downstream);
        tokio::select! {
            result = upstream => {
                result?;
                downstream.await
            }
            result = &mut downstream => result,
        }
    });
    // A pending read on local stdin cannot be cancelled by Tokio. Let the
    // process exit when the desktop socket closes, even if stdin stays open.
    runtime.shutdown_background();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, BufReader};

    fn fixture() -> Broker {
        let collection = model::Collection {
            requests: vec![model::Request {
                id: "widgets/list.yml".into(),
                info: model::Info {
                    name: "List widgets".into(),
                    ..Default::default()
                },
                http: model::HttpSpec {
                    method: "GET".into(),
                    url: "{{baseUrl}}/widgets".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
            envs: vec![model::Env {
                name: "sandbox".into(),
                variables: vec![
                    model::EnvVar {
                        name: "baseUrl".into(),
                        value: "https://api.example.test".into(),
                        secret: false,
                    },
                    model::EnvVar {
                        name: "bearerToken".into(),
                        value: String::new(),
                        secret: true,
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let approved = resolve_grants(
            vec![Grant {
                request_id: "widgets/list.yml".into(),
                environment: Some("sandbox".into()),
                origin: Url::parse("https://api.example.test").unwrap(),
                return_body: false,
            }],
            &collection,
        )
        .unwrap();
        Broker {
            vault: Arc::new(VaultState::default()),
            approved: approved.into(),
            client: reqwest::Client::new(),
            requests: Arc::new(Semaphore::new(1)),
        }
    }

    #[tokio::test]
    async fn mcp_advertises_two_tools_and_blocks_unapproved_or_locked_requests() {
        let (client, server) = UnixStream::pair().unwrap();
        let broker = fixture();
        let session = tokio::spawn(async move {
            broker.serve(server).await.unwrap().waiting().await.unwrap();
        });
        let (incoming, mut outgoing) = client.into_split();
        let mut responses = BufReader::new(incoming).lines();

        let messages = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_requests","arguments":{"query":"get widgets"}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"send_request","arguments":{"request_id":"widgets/list.yml","environment":"other"}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"send_request","arguments":{"request_id":"widgets/list.yml","environment":"sandbox"}}}),
        ];
        for message in messages {
            outgoing
                .write_all(format!("{message}\n").as_bytes())
                .await
                .unwrap();
            let Some(id) = message.get("id").and_then(Value::as_u64) else {
                continue;
            };
            let reply = tokio::time::timeout(Duration::from_secs(3), responses.next_line())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let reply: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(reply["id"], id);
            let result = &reply["result"];
            match id {
                1 => assert!(result["capabilities"]["tools"].is_object()),
                2 => {
                    let tools = result["tools"].as_array().unwrap();
                    assert_eq!(tools.len(), 2);
                    assert!(tools.iter().any(|tool| tool["name"] == "search_requests"));
                    assert!(tools.iter().any(|tool| tool["name"] == "send_request"));
                }
                3 => {
                    let requests = &result["structuredContent"]["requests"];
                    assert_eq!(requests[0]["request_id"], "widgets/list.yml");
                    assert_eq!(requests[0]["environment"], "sandbox");
                    assert!(!reply.to_string().contains("api.example.test"));
                }
                4 => {
                    assert_eq!(result["isError"], true);
                    assert!(result.to_string().contains("not approved"));
                }
                5 => {
                    assert_eq!(result["isError"], true);
                    assert!(result.to_string().contains("Unlock the vault"));
                }
                _ => unreachable!(),
            }
        }
        drop(outgoing);
        drop(responses);
        tokio::time::timeout(Duration::from_secs(3), session)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn request_origin_is_checked_before_using_the_vault() {
        let broker = fixture();
        let item = &broker.approved[0];
        let env = item.environment.as_ref().unwrap();
        let mut vault = crate::vault::VaultData::default();
        crate::vault::set_in(
            &mut vault,
            "sandbox",
            "bearerToken",
            "example-fixture-token",
        );
        let request = http::prepare(
            &broker.client,
            &item.request,
            Some(env),
            Some(&vault),
            Some(&item.origin),
        )
        .unwrap();
        assert_eq!(
            request.headers()["authorization"],
            "Bearer example-fixture-token"
        );

        let mut env = env.clone();
        env.variables[0].value = "https://other.example.test".into();
        let err = http::prepare(
            &broker.client,
            &item.request,
            Some(&env),
            Some(&vault),
            Some(&item.origin),
        )
        .unwrap_err();
        assert!(err.contains("approved HTTPS origin"));
        assert!(!err.contains("example-fixture-token"));
    }

    #[test]
    fn response_body_suppresses_reflected_secrets() {
        let secrets = vec!["fixture/token".to_string()];
        assert!(filter_body(r#"{"value":"fixture/token"}"#.into(), &secrets).is_none());
        assert!(filter_body(r#"{"value":"fixture%2Ftoken"}"#.into(), &secrets).is_none());
        assert!(filter_body(r#"{"value":"fixture\u002ftoken"}"#.into(), &secrets).is_none());
        assert_eq!(
            filter_body(r#"{"ok":true}"#.into(), &secrets).as_deref(),
            Some(r#"{"ok":true}"#)
        );
    }
}
