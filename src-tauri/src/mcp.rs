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
    full_response: Mutex<bool>,
    allowed_envs: Mutex<Result<Vec<String>, String>>,
    revoked: watch::Sender<()>,
}

impl Access {
    fn new(enabled: bool) -> Self {
        Self {
            enabled: Mutex::new(enabled),
            full_response: Mutex::new(false),
            allowed_envs: Mutex::new(Ok(Vec::new())),
            revoked: watch::channel(()).0,
        }
    }

    pub(super) fn load() -> Self {
        let access = Self::new(enabled_path().is_ok_and(|path| path.is_file()));
        *access.full_response.lock().unwrap() =
            full_response_path().is_ok_and(|path| path.is_file());
        *access.allowed_envs.lock().unwrap() = (|| -> io::Result<Vec<String>> {
            let path = allowed_envs_path()?;
            match std::fs::read(path) {
                Ok(data) => serde_json::from_slice(&data).map_err(io::Error::other),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(e) => Err(e),
            }
        })()
        .map_err(|e| e.to_string());
        access
    }

    pub(super) fn full_response(&self) -> bool {
        *self.full_response.lock().unwrap()
    }

    pub(super) fn set_full_response(&self, enabled: bool) -> io::Result<()> {
        let mut current = self.full_response.lock().unwrap();
        let path = full_response_path()?;
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
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        *current = enabled;
        // Cancel in-flight requests so a response cannot use the previous policy.
        self.revoke();
        Ok(())
    }

    pub(super) fn allowed_envs(&self) -> Result<Vec<String>, String> {
        self.allowed_envs.lock().unwrap().clone()
    }

    pub(super) fn set_allowed_envs(&self, names: Vec<String>) -> io::Result<()> {
        let mut current = self.allowed_envs.lock().unwrap();
        let path = allowed_envs_path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("tmp");
        let data = serde_json::to_vec(&names).map_err(io::Error::other)?;
        std::fs::write(&temp, data)?;
        std::fs::rename(temp, path)?;
        *current = Ok(names);
        self.revoke();
        Ok(())
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
    #[schemars(description = "Exact allowed environment name from search_requests.")]
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
        let allowed = self
            .access
            .allowed_envs()
            .map_err(|_| "MCP environment restrictions could not be loaded.")?;
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
        let environments: Vec<_> = collection
            .envs
            .iter()
            .filter(|env| !env.path.is_empty() && allowed.contains(&env.path))
            .map(|env| &env.name)
            .collect();
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
        let allowed = self
            .access
            .allowed_envs()
            .map_err(|_| "MCP environment restrictions could not be loaded.")?;
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
            if env.path.is_empty() || !allowed.contains(&env.path) {
                return Err("Environment is unavailable.");
            }
            Some(env.clone())
        } else {
            return Err("Choose an allowed environment.");
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
        let full_response = self.access.full_response();
        let send = async {
            let start = Instant::now();
            let response = self
                .client
                .execute(request)
                .await
                .map_err(|_| "Hitr could not reach the saved API.")?;
            let status = response.status().as_u16();
            let (body, omission_reason) = match read_body(response, &secrets, full_response).await?
            {
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
    full_response: bool,
) -> Result<Result<String, &'static str>, &'static str> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Hitr could not read the API response.")?
    {
        if !full_response && bytes.len() + chunk.len() > MAX_BODY_BYTES {
            return Ok(Err("size_limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let Ok(body) = String::from_utf8(bytes) else {
        return Ok(Err("invalid_utf8"));
    };
    if full_response {
        Ok(Ok(redact_body(body, secrets)))
    } else {
        Ok(filter_body(body, secrets).ok_or("credential_match"))
    }
}

fn redact_body(body: String, secrets: &[String]) -> String {
    let mut patterns: Vec<String> = secrets
        .iter()
        .filter(|s| !s.is_empty())
        .flat_map(|secret| {
            let encoded: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
            [secret.clone(), encoded]
        })
        .collect();
    // Match longest first so overlapping credentials cannot leave a suffix exposed.
    patterns.sort_by_key(|s| std::cmp::Reverse(s.len()));
    patterns.dedup();
    fn redact(text: &str, patterns: &[String]) -> String {
        let mut output = String::new();
        let mut rest = text;
        while !rest.is_empty() {
            if let Some(pattern) = patterns.iter().find(|p| rest.starts_with(p.as_str())) {
                output.push_str("[REDACTED]");
                rest = &rest[pattern.len()..];
            } else {
                let c = rest.chars().next().unwrap();
                output.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
        output
    }
    fn walk(value: &mut Value, patterns: &[String]) {
        match value {
            Value::String(s) => *s = redact(s, patterns),
            Value::Array(items) => items.iter_mut().for_each(|v| walk(v, patterns)),
            Value::Object(map) => {
                for (key, mut value) in std::mem::take(map) {
                    let normalized: String = key
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric())
                        .flat_map(char::to_lowercase)
                        .collect();
                    if matches!(
                        normalized.as_str(),
                        "token"
                            | "accesstoken"
                            | "refreshtoken"
                            | "idtoken"
                            | "apikey"
                            | "password"
                            | "secret"
                            | "authorization"
                            | "cookie"
                            | "setcookie"
                    ) {
                        value = Value::String("[REDACTED]".into());
                    } else {
                        walk(&mut value, patterns);
                    }
                    let redacted_key = redact(&key, patterns);
                    let mut unique_key = redacted_key.clone();
                    let mut suffix = 2;
                    while map.contains_key(&unique_key) {
                        unique_key = format!("{redacted_key} ({suffix})");
                        suffix += 1;
                    }
                    map.insert(unique_key, value);
                }
            }
            Value::Number(_) | Value::Bool(_) => {
                let text = value.to_string();
                let redacted = redact(&text, patterns);
                if redacted != text {
                    *value = Value::String(redacted);
                }
            }
            _ => {}
        }
    }
    if let Ok(mut json) = serde_json::from_str::<Value>(&body) {
        walk(&mut json, &patterns);
        json.to_string()
    } else {
        redact(&body, &patterns)
    }
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

fn full_response_path() -> io::Result<PathBuf> {
    bridge_dir().map(|dir| dir.with_file_name("mcp-full-response"))
}

fn allowed_envs_path() -> io::Result<PathBuf> {
    bridge_dir().map(|dir| dir.with_file_name("mcp-allowed-environments.json"))
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

fn http_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let target = attempt.url();
            let same_origin = attempt
                .previous()
                .first()
                .is_some_and(|initial| initial.origin() == target.origin());
            // Keep credentials within the original HTTPS origin on every hop.
            if !same_origin
                || target.scheme() != "https"
                || !target.username().is_empty()
                || target.password().is_some()
            {
                return attempt.stop();
            }
            reqwest::redirect::Policy::limited(5).redirect(attempt)
        }))
        .no_proxy()
        .timeout(Duration::from_secs(20))
}

async fn serve(app: &AppHandle) -> Result<(), String> {
    let dir = bridge_dir().map_err(|e| e.to_string())?;
    let state = app.state::<AppState>();
    let client = http_client_builder()
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

    fn https_server(
        responses: impl FnOnce(&str) -> Vec<String>,
    ) -> (
        String,
        reqwest::Certificate,
        std::thread::JoinHandle<Vec<String>>,
    ) {
        use std::io::{BufRead, BufReader, Read, Write};
        let certified = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let certificate = reqwest::Certificate::from_der(certified.cert.der()).unwrap();
        let key =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certified.signing_key.serialize_der());
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certified.cert.der().clone()], key.into())
        .unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("https://{}", listener.local_addr().unwrap());
        let responses = responses(&base);
        let thread = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let deadline = Instant::now() + Duration::from_secs(5);
                let socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "expected HTTPS request");
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept failed: {error}"),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let connection = rustls::ServerConnection::new(Arc::new(config.clone())).unwrap();
                let mut stream = rustls::StreamOwned::new(connection, socket);
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                let mut content_length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    request.push_str(&line);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.trim().parse::<usize>().unwrap();
                        }
                    }
                }
                let mut body = vec![0; content_length];
                reader.read_exact(&mut body).unwrap();
                request.push_str(std::str::from_utf8(&body).unwrap());
                requests.push(request);
                stream.write_all(response.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
            requests
        });
        (base, certificate, thread)
    }

    fn redirect_response(status: &str, location: &str) -> String {
        format!("HTTP/1.1 {status}\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }

    fn ok_response() -> String {
        let body = r#"{"widgets":[]}"#;
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    #[tokio::test]
    async fn mcp_follows_same_origin_https_redirects_with_auth() {
        let (base, certificate, server) = https_server(|_| {
            let mut responses: Vec<_> = (1..=5)
                .map(|hop| redirect_response("301 Moved Permanently", &format!("/hop-{hop}")))
                .collect();
            responses.push(ok_response());
            responses
        });
        let mut broker = fixture();
        broker.client = http_client_builder()
            .add_root_certificate(certificate)
            .build()
            .unwrap();
        {
            let mut collection = broker.collection.lock().unwrap();
            let env = &mut collection.as_mut().unwrap().envs[0];
            env.variables[0].value = base;
            env.variables[1].secret = false;
            env.variables[1].value = "fixture-token".into();
        }
        let result = broker
            .send(SendRequest {
                request_id: "widgets/list.yml".into(),
                environment: Some("sandbox".into()),
                inputs: Default::default(),
            })
            .await
            .unwrap();
        assert_eq!(result["status"], 200);
        assert_eq!(result["body"], r#"{"widgets":[]}"#);
        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("GET /widgets "));
        assert_eq!(requests.len(), 6);
        assert!(requests[5].starts_with("GET /hop-5 "));
        for request in requests {
            assert!(request
                .to_lowercase()
                .contains("authorization: bearer fixture-token\r\n"));
        }
    }

    #[tokio::test]
    async fn mcp_redirects_preserve_post_bodies_for_307_and_308() {
        for status in ["307 Temporary Redirect", "308 Permanent Redirect"] {
            let (base, certificate, server) =
                https_server(|_| vec![redirect_response(status, "/final"), ok_response()]);
            let client = http_client_builder()
                .add_root_certificate(certificate)
                .build()
                .unwrap();
            let response = client
                .post(format!("{base}/widgets"))
                .json(&json!({"limit": 1}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let requests = server.join().unwrap();
            assert!(requests[0].starts_with("POST /widgets "));
            assert!(requests[1].starts_with("POST /final "));
            for request in requests {
                assert!(request.ends_with(r#"{"limit":1}"#));
            }
        }
    }

    #[tokio::test]
    async fn mcp_does_not_follow_redirects_outside_the_https_origin() {
        for destination in [
            "http://127.0.0.1:9/final",
            "https://localhost:9/final",
            "https://127.0.0.1:9/final",
            "{base}/final-with-userinfo",
        ] {
            let (base, certificate, server) = https_server(|base| {
                let location = if destination.contains("{base}") {
                    format!("{}/final", base.replace("https://", "https://user@"))
                } else {
                    destination.to_owned()
                };
                vec![redirect_response("301 Moved Permanently", &location)]
            });
            let client = http_client_builder()
                .add_root_certificate(certificate)
                .build()
                .unwrap();
            let response = client
                .get(format!("{base}/widgets"))
                .bearer_auth("fixture-token")
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 301, "destination: {destination}");
            assert_eq!(server.join().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn mcp_stops_redirect_loops_after_five_hops() {
        let (base, certificate, server) =
            https_server(|_| vec![redirect_response("301 Moved Permanently", "/widgets"); 6]);
        let client = http_client_builder()
            .add_root_certificate(certificate)
            .build()
            .unwrap();
        let error = client
            .get(format!("{base}/widgets"))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_redirect());
        assert_eq!(server.join().unwrap().len(), 6);
    }

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
                path: "/test/environments/sandbox.yml".into(),
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
        let access = Access::new(true);
        *access.allowed_envs.lock().unwrap() = Ok(vec!["/test/environments/sandbox.yml".into()]);
        Broker {
            access: Arc::new(access),
            vault,
            collection: Arc::new(Mutex::new(Some(collection))),
            client: http_client_builder().build().unwrap(),
            requests: Arc::new(Semaphore::new(1)),
        }
    }

    #[tokio::test]
    async fn mcp_requires_an_explicitly_allowed_environment() {
        let broker = fixture();
        *broker.access.allowed_envs.lock().unwrap() = Ok(vec![]);
        let search = || Search {
            query: String::new(),
            offset: 0,
        };
        assert_eq!(broker.search(search()).unwrap()["environments"], json!([]));
        let input = |environment| SendRequest {
            request_id: "widgets/list.yml".into(),
            environment,
            inputs: Default::default(),
        };
        assert!(broker.send(input(Some("sandbox".into()))).await.is_err());
        assert!(broker.send(input(None)).await.is_err());
        *broker.access.allowed_envs.lock().unwrap() =
            Ok(vec!["/test/environments/sandbox.yml".into()]);
        assert_eq!(
            broker.search(search()).unwrap()["environments"],
            json!(["sandbox"])
        );
        assert!(broker.selection(&input(Some("sandbox".into()))).is_ok());
        *broker.access.allowed_envs.lock().unwrap() = Err("bad settings".into());
        assert!(broker.search(search()).is_err());
        assert!(broker.send(input(Some("sandbox".into()))).await.is_err());
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
            collection.as_mut().unwrap().envs[0].variables[0].value =
                format!("https://{}", listener.local_addr().unwrap());
            collection.as_mut().unwrap().envs[0].variables.truncate(1);
        }
        let mut send = Box::pin(broker.send(SendRequest {
            request_id: "widgets/list.yml".into(),
            environment: Some("sandbox".into()),
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

    #[tokio::test]
    async fn full_response_returns_large_body_with_credentials_redacted() {
        let body = format!(
            r#"{{"token":"fixture/token","data":"{}"}}"#,
            "x".repeat(MAX_BODY_BYTES)
        );
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (base, certificate, server) = https_server(|_| vec![reply.clone(), reply]);
        let client = http_client_builder()
            .add_root_certificate(certificate)
            .build()
            .unwrap();
        let secrets = vec!["fixture/token".into()];
        let full = read_body(client.get(&base).send().await.unwrap(), &secrets, true)
            .await
            .unwrap()
            .unwrap();
        let json: Value = serde_json::from_str(&full).unwrap();
        assert_eq!(json["token"], "[REDACTED]");
        assert_eq!(json["data"].as_str().unwrap().len(), MAX_BODY_BYTES);
        assert!(!full.contains("fixture/token"));
        assert_eq!(
            read_body(client.get(&base).send().await.unwrap(), &secrets, false)
                .await
                .unwrap(),
            Err("size_limit")
        );
        server.join().unwrap();
        assert!(!Access::new(true).full_response());
    }

    #[test]
    fn full_response_redacts_encoded_nested_and_overlapping_credentials() {
        let secrets = vec!["fixture".into(), "fixture/token".into()];
        let body =
            r#"{"nested":["fixture/token","fixture%2Ftoken","fixture\u002ftoken"],"ok":true}"#;
        let redacted: Value = serde_json::from_str(&redact_body(body.into(), &secrets)).unwrap();
        assert_eq!(
            redacted,
            json!({"nested":["[REDACTED]","[REDACTED]","[REDACTED]"],"ok":true})
        );
        assert_eq!(
            redact_body("fixture/token fixture%2Ftoken".into(), &secrets),
            "[REDACTED] [REDACTED]"
        );
        assert_eq!(redact_body("unchanged".into(), &secrets), "unchanged");
        let body =
            r#"{"access_token":"new-token","count":12,"nested":{"refresh_token":"another-token"}}"#;
        let redacted: Value = serde_json::from_str(&redact_body(body.into(), &[])).unwrap();
        assert_eq!(
            redacted,
            json!({"access_token":"[REDACTED]", "count":12,"nested":{"refresh_token":"[REDACTED]"}})
        );
        let body = r#"{"first-token":1,"second-token":2,"value":12345}"#;
        let redacted: Value = serde_json::from_str(&redact_body(
            body.into(),
            &["first-token".into(), "second-token".into(), "12345".into()],
        ))
        .unwrap();
        assert_eq!(redacted.as_object().unwrap().len(), 3);
        assert_eq!(redacted["value"], "[REDACTED]");
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
