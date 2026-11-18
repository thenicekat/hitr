//! Local stdio MCP bridge. The desktop process owns the vault and HTTP client.

use crate::{mcp_inputs, model, vault::VaultState, AppState};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::{schemars, tool, tool_router, ServiceExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{watch, Semaphore};

const MAX_BODY_BYTES: usize = 128 * 1024;

#[derive(Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(super) enum Status {
    Disabled,
    Starting,
    Ready,
    Error { message: String },
}

pub(super) struct Access {
    enabled: Mutex<bool>,
    revoked: watch::Sender<()>,
}

impl Access {
    fn new(enabled: bool) -> Self {
        Self {
            enabled: Mutex::new(enabled),
            revoked: watch::channel(()).0,
        }
    }

    pub(super) fn load() -> Self {
        Self::new(enabled_path().is_ok_and(|path| path.is_file()))
    }

    pub(super) fn enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }

    pub(super) fn set_enabled(&self, enabled: bool) -> io::Result<()> {
        let mut current = self.enabled.lock().unwrap();
        if *current != enabled {
            let path = enabled_path()?;
            if enabled {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .open(path)?;
            } else {
                match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                self.revoke();
            }
            *current = enabled;
        }
        Ok(())
    }

    pub(super) fn revoke(&self) {
        self.revoked.send_modify(|_| {});
    }
}

#[derive(Clone)]
struct Broker {
    access: Arc<Access>,
    vault: Arc<VaultState>,
    collection: Arc<Mutex<Option<model::Collection>>>,
    client: reqwest::Client,
    requests: Arc<Semaphore>,
}

const LOCKED: &str = "Unlock the vault in the Hitr desktop app first.";
const NO_COLLECTION: &str = "Load a collection in the Hitr desktop app first.";
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct Search {
    #[schemars(
        description = "Words from the saved request name, ID or HTTP method. Empty lists all saved requests."
    )]
    query: String,
    #[serde(default)]
    #[schemars(description = "Search result offset. Use next_offset to retrieve the next page.")]
    offset: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendRequest {
    #[schemars(description = "Exact request ID from search_requests.")]
    request_id: String,
    #[schemars(
        description = "Exact environment name from search_requests, or null to send without an environment."
    )]
    environment: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Optional query and JSON body values for this call. Use the request's input_schema from search_requests."
    )]
    inputs: mcp_inputs::Inputs,
}

#[tool_router(server_handler)]
impl Broker {
    #[tool(
        description = "Find saved requests, their input schemas, and environments in Hitr’s active collection. Requires an unlocked vault."
    )]
    fn search_requests(&self, Parameters(input): Parameters<Search>) -> CallToolResult {
        tool_result(self.search(input))
    }

    #[tool(
        description = "Send a saved request with a chosen environment and optional typed inputs from its input_schema. Requires an unlocked vault and may change remote data."
    )]
    async fn send_request(&self, Parameters(input): Parameters<SendRequest>) -> CallToolResult {
        tool_result(self.send(input).await)
    }
}

fn tool_result(result: Result<Value, &'static str>) -> CallToolResult {
    match result {
        Ok(result) => CallToolResult::structured(result),
        Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
    }
}

impl Broker {
    fn check_access(&self) -> Result<(), &'static str> {
        if !self.access.enabled() {
            return Err("Enable MCP in the Hitr desktop app first.");
        }
        if self.vault.password.lock().unwrap().is_none() {
            return Err(LOCKED);
        }
        Ok(())
    }

    fn search(&self, input: Search) -> Result<Value, &'static str> {
        self.check_access()?;
        let collection = self.collection.lock().unwrap();
        let collection = collection.as_ref().ok_or(NO_COLLECTION)?;
        let query = input.query.to_lowercase();
        let words: Vec<_> = query.split_whitespace().collect();
        let mut found = collection
            .requests
            .iter()
            .filter(|req| {
                let text =
                    format!("{} {} {}", req.info.name, req.id, req.http.method).to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .skip(input.offset);
        let requests: Vec<_> = found
            .by_ref()
            .take(25)
            .map(|req| {
                json!({
                    "request_id": req.id,
                    "name": req.info.name,
                    "method": req.http.method,
                    "input_schema": mcp_inputs::describe(req),
                })
            })
            .collect();
        let next_offset = found.next().map(|_| input.offset + requests.len());
        let environments: Vec<_> = collection.envs.iter().map(|env| &env.name).collect();
        Ok(json!({
            "requests": requests,
            "environments": environments,
            "next_offset": next_offset,
            "more_matches": next_offset.is_some(),
        }))
    }

    fn selection(
        &self,
        input: &SendRequest,
    ) -> Result<(model::Request, Option<model::Env>), &'static str> {
        let collection = self.collection.lock().unwrap();
        let collection = collection.as_ref().ok_or(NO_COLLECTION)?;
        let req = collection
            .requests
            .iter()
            .find(|req| req.id == input.request_id)
            .ok_or("Saved request not found. Use search_requests.")?;
        let env = if let Some(name) = &input.environment {
            let mut matches = collection.envs.iter().filter(|env| env.name == *name);
            let env = matches
                .next()
                .filter(|_| matches.next().is_none())
                .ok_or("Environment is unknown or ambiguous. Use search_requests.")?;
            Some(env.clone())
        } else {
            None
        };
        Ok((req.clone(), env))
    }

    async fn send(&self, input: SendRequest) -> Result<Value, &'static str> {
        let mut revoked = self.access.revoked.subscribe();
        self.check_access()?;
        let _permit = self
            .requests
            .try_acquire()
            .map_err(|_| "Hitr is busy. Retry shortly.")?;
        let (req, env) = self.selection(&input)?;
        let vault = {
            let cache = self.vault.cache.lock().unwrap();
            if self.vault.password.lock().unwrap().is_none() {
                return Err(LOCKED);
            }
            cache.clone()
        };
        let request = mcp_inputs::prepare(
            &self.client,
            req,
            env.as_ref(),
            vault.as_ref(),
            input.inputs,
        )?;
        let secrets = response_secrets(&request, vault.as_ref());
        let send = async {
            let start = Instant::now();
            let response = self
                .client
                .execute(request)
                .await
                .map_err(|_| "Hitr could not reach the saved API.")?;
            let status = response.status().as_u16();
            let (body, omission_reason) = match read_body(response, &secrets).await? {
                Ok(body) => (Some(body), None),
                Err(reason) => (None, Some(reason)),
            };
            Ok(json!({
                "status": status,
                "latency_ms": start.elapsed().as_millis() as u64,
                "body": body,
                "body_omitted": body.is_none(),
                "body_omitted_reason": omission_reason,
            }))
        };
        tokio::select! {
            biased;
            _ = revoked.changed() => Err("MCP access changed while sending the request."),
            result = send => result,
        }
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
) -> Result<Result<String, &'static str>, &'static str> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Hitr could not read the API response.")?
    {
        if bytes.len() + chunk.len() > MAX_BODY_BYTES {
            return Ok(Err("size_limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let Ok(body) = String::from_utf8(bytes) else {
        return Ok(Err("invalid_utf8"));
    };
    Ok(filter_body(body, secrets).ok_or("credential_match"))
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

fn enabled_path() -> io::Result<PathBuf> {
    bridge_dir().map(|dir| dir.with_file_name("mcp-enabled"))
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
    {
        let state = app.state::<AppState>();
        let mut status = state.mcp_status.lock().unwrap();
        if !matches!(*status, Status::Disabled | Status::Error { .. }) {
            return;
        }
        *status = Status::Starting;
    }
    tauri::async_runtime::spawn(async move {
        if let Err(message) = serve(&app).await {
            eprintln!("Hitr MCP: {message}");
            *app.state::<AppState>().mcp_status.lock().unwrap() = Status::Error { message };
        }
    });
}

async fn serve(app: &AppHandle) -> Result<(), String> {
    let dir = bridge_dir().map_err(|e| e.to_string())?;
    let state = app.state::<AppState>();
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|err| err.to_string())?;
    let broker = Broker {
        access: state.mcp_access.clone(),
        vault: state.vault.clone(),
        collection: state.collection.clone(),
        client,
        requests: Arc::new(Semaphore::new(4)),
    };
    let (_lock, listener) = listen(&dir).map_err(|err| err.to_string())?;
    *state.mcp_status.lock().unwrap() = Status::Ready;
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
            io::Error::new(err.kind(), format!("Could not connect to Hitr MCP. Start the Hitr desktop app and turn on MCP help’s Enable MCP setting. ({err})"))
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
    use crate::http;
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
        let vault = Arc::new(VaultState::default());
        *vault.password.lock().unwrap() =
            Some(age::secrecy::SecretString::from("fixture".to_owned()));
        Broker {
            access: Arc::new(Access::new(true)),
            vault,
            collection: Arc::new(Mutex::new(Some(collection))),
            client: reqwest::Client::new(),
            requests: Arc::new(Semaphore::new(1)),
        }
    }

    #[tokio::test]
    async fn mcp_lists_the_collection_and_blocks_calls_when_locked() {
        let (client, server) = UnixStream::pair().unwrap();
        let broker = fixture();
        let server_broker = broker.clone();
        let session = tokio::spawn(async move {
            server_broker
                .serve(server)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let (incoming, mut outgoing) = client.into_split();
        let mut responses = BufReader::new(incoming).lines();

        let messages = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_requests","arguments":{"query":"get widgets"}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"send_request","arguments":{"request_id":"widgets/list.yml","environment":"other"}}}),
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"send_request","arguments":{"request_id":"widgets/list.yml","environment":"sandbox","inputs":{"query":{"limit":10}}}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"send_request","arguments":{"request_id":"widgets/list.yml","environment":"sandbox"}}}),
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"search_requests","arguments":{"query":""}}}),
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"search_requests","arguments":{"query":""}}}),
        ];
        for message in messages {
            if message["id"] == 5 {
                *broker.vault.password.lock().unwrap() = None;
                broker.access.revoke();
            }
            if message["id"] == 7 {
                *broker.access.enabled.lock().unwrap() = false;
            }
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
                    assert_eq!(requests[0]["input_schema"]["additionalProperties"], false);
                    assert_eq!(result["structuredContent"]["environments"][0], "sandbox");
                    assert!(!reply.to_string().contains("api.example.test"));
                }
                4 => {
                    assert_eq!(result["isError"], true);
                    assert!(result.to_string().contains("Environment is unknown"));
                }
                5 | 6 => {
                    assert_eq!(result["isError"], true);
                    assert!(result.to_string().contains("Unlock the vault"));
                }
                7 => {
                    assert_eq!(result["isError"], true);
                    assert!(result.to_string().contains("Enable MCP"));
                }
                8 => {
                    assert_eq!(result["isError"], true);
                    assert!(result
                        .to_string()
                        .contains("Unknown or protected query input"));
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

    #[tokio::test]
    async fn vault_lock_cancels_an_in_flight_mcp_call() {
        let broker = fixture();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        {
            let mut collection = broker.collection.lock().unwrap();
            collection.as_mut().unwrap().requests[0].http.url =
                format!("https://{}/widgets", listener.local_addr().unwrap());
        }
        let mut send = Box::pin(broker.send(SendRequest {
            request_id: "widgets/list.yml".into(),
            environment: None,
            inputs: mcp_inputs::Inputs::default(),
        }));
        // Hold the connection before completing TLS so access can be revoked
        // with an outbound call still pending.
        let (_connection, _) = tokio::select! {
            result = &mut send => panic!("request finished before cancellation: {result:?}"),
            accepted = tokio::time::timeout(Duration::from_secs(3), listener.accept()) =>
                accepted.unwrap().unwrap(),
        };
        *broker.vault.password.lock().unwrap() = None;
        broker.access.revoke();
        assert_eq!(
            send.await,
            Err("MCP access changed while sending the request.")
        );
    }

    #[test]
    fn request_origin_is_checked_before_using_the_vault() {
        let broker = fixture();
        let collection = broker.collection.lock().unwrap();
        let collection = collection.as_ref().unwrap();
        let req = &collection.requests[0];
        let env = &collection.envs[0];
        let mut vault = crate::vault::VaultData::default();
        crate::vault::set_in(
            &mut vault,
            "sandbox",
            "bearerToken",
            "example-fixture-token",
        );
        let request = http::prepare(
            &broker.client,
            req,
            Some(env),
            Some(&vault),
            http::RequestMode::Mcp,
        )
        .unwrap();
        assert_eq!(
            request.headers()["authorization"],
            "Bearer example-fixture-token"
        );

        let mut req = req.clone();
        req.http.url = "https://other.example.test/widgets".into();
        let err = http::prepare(
            &broker.client,
            &req,
            Some(env),
            Some(&vault),
            http::RequestMode::Mcp,
        )
        .unwrap_err();
        assert!(err.contains("environment’s HTTPS origin"));
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
