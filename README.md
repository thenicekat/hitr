# hitr

Hitr is a native REST API client for macOS with plain-text YAML collections, cURL and OpenAPI import, and a local MCP server for agents. Built with Rust, Tauri, and Dioxus.

Built because heavyweight Electron clients are slow and cloud-first tools want your data on their servers. This does one thing: read a collection off disk, edit envs, fire requests. Nothing else.

## Status

**MVP.** macOS-tested. Cross-platform in principle (age-encrypted vault, no OS-specific deps) but only tested on macOS.

## Features

**Collections**
- Plain YAML on disk — one file per request, editable in any text editor
- Import from curl paste (Chrome DevTools "Copy as cURL" works)
- Import from OpenAPI 3.x spec (yaml or json) — one request per operation, foldered by tag
- Import existing Bruno collections directly (same YAML shape)
- Create / duplicate / delete / rename requests
- Fuzzy search over 1000+ requests

**Editing**
- Full request editor: method, URL, params, headers, body
- Autosave on every edit (500ms debounce, no save button)
- URL query auto-splits into params tab (Postman-style)
- `⌘/` toggles `//` comments on JSON body — stripped before send so wire stays valid
- `⌘Enter` fires selected request

**Envs & secrets**
- Create / rename / delete envs; edit variables inline
- `{{var}}` substitution from selected env at fire time
- Fails loudly with named vars when any are unresolved
- Secrets encrypted with [age](https://age-encryption.org/) + master password — never on disk unencrypted
- Bearer helper: env's `bearerToken` becomes `Authorization: Bearer …` automatically

**Response pane**
- Status, latency (ms), headers, pretty-printed JSON body
- Local history: last 20 responses per request, stored in the collection's `.hitr/history` directory
- Copy response body or copy request as curl (with resolved vars) to clipboard

**Ops**
- ~15 MB single binary, ~50 ms warm start
- No Node runtime, no cloud account, no plugins, no telemetry

## Install

### macOS (prebuilt)

Grab the `.dmg` for your arch from the [latest release](https://github.com/thenicekat/hitr/releases/latest):

- Apple Silicon (M1/M2/M3/M4): `hitr_<version>_aarch64.dmg`
- Intel: `hitr_<version>_x64.dmg`

Open the DMG and drag `hitr.app` to `/Applications`.

**Gatekeeper will complain** because the build isn't notarized (no Apple Developer cert — costs $99/yr, skipped for now). Strip the quarantine flag once:

```bash
xattr -dr com.apple.quarantine /Applications/hitr.app
```

Then launch normally. Alternative: right-click `hitr.app` → **Open** → **Open** in the confirmation dialog. Both work; `xattr` is one-shot and the app opens cleanly on every subsequent launch.

If macOS still refuses ("hitr is damaged"), the download attribute wasn't cleared — re-run `xattr` targeting the actual path, or try the same command on the `.dmg` before opening it.

### App updates

Hitr checks for new releases at launch. When an update is available, choose **Update and restart** to download the package, verify its signature, and install it. Older builds without the updater need a one-time manual install from GitHub Releases.

Release builds publish signed update archives and a `latest.json` manifest alongside the DMGs. The release workflow needs the repository’s `TAURI_SIGNING_PRIVATE_KEY` secret. Keep a secure backup of the matching private key outside the repository.

### From source

```bash
rustup target add wasm32-unknown-unknown
cargo install dioxus-cli --locked
cargo install tauri-cli --version '^2.0.0' --locked

git clone https://github.com/thenicekat/hitr
cd hitr
cargo tauri dev              # dev mode with hot reload
cargo tauri build            # release bundle in src-tauri/target/release/bundle/macos
```

Point hitr at your collection root: click `root: /...` in the topbar, paste a path, Enter.

## MCP for agents

1. Open **MCP help** in Hitr and turn on **Enable MCP**. The setting is off by default and is remembered across launches. Previous per-request grant files do not enable this setting.
2. Copy the client configuration from MCP help and add it to your MCP client. Set the server command to the installed Hitr executable and pass `mcp` as its argument.
3. Keep Hitr open and unlock its vault. Both MCP tools pause when the vault locks or MCP is disabled.

| Tool | What it does |
| --- | --- |
| `search_requests` | Finds saved requests by name, ID, or method and lists the available environments. Send an empty query to browse. Pass back `next_offset` as `offset` to get another page. |
| `send_request` | Runs a saved request by its exact ID with the selected environment. Pass `null` to run without an environment. |

MCP can use every saved request and environment in the open collection, including production. Saved requests can change remote data. Keep MCP enabled only if you want local clients to have that access while Hitr is unlocked.

Hitr attaches credentials in the desktop process, checks the selected environment’s `baseUrl` origin when it is configured, and sends MCP requests over HTTPS without following redirects. Clients select the saved request and environment and cannot provide request overrides. Results include the HTTP status, latency, and bounded response body. Hitr suppresses oversized, non-UTF-8, and known credential echoes. An API response can still contain sensitive data.

MCP uses a private local socket and the [stdio transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports). The socket limits access to the local user account.

## Layout

```
┌ envs ──┐┌ requests (search)  ──┐┌ [POST] {{baseUrl}}/foo    [save] [send] │
│ dev    ││ [GET]  Get Account   │├ params (2) · headers (5) · body ────────┤
│ qa     ││ [POST] Update User   ││ {                                       │
│ prod   ││ [GET]  List Accounts ││   "id": "..."                           │
│  [+]   ││ [+][curl]            ││ }                                       │
│[edit]  ││                      │├─────────────────────────────────────────┤
│[del]   ││                      ││ 200 OK  142ms                           │
│        ││                      ││ headers ▸                               │
│        ││                      ││ { "account": {...} }                    │
└────────┘└──────────────────────┘└─────────────────────────────────────────┘
```

## Storage model

| Data                | Where                                                             |
|---------------------|-------------------------------------------------------------------|
| Requests            | YAML at `<root>/<folder>/<name>.yml`                              |
| Envs                | YAML at `<root>/environments/<name>.yml` (variable *names* + non-secret values) |
| Secret values       | age-encrypted vault at `<config>/hitr/vault.age`                 |
| Config              | `<config>/hitr/config.json` (just the collection root path)      |

`<config>` = `~/Library/Application Support` on macOS, `~/.config` on Linux, `%APPDATA%` on Windows.

### Where things live

**Envs** — one YAML per env in your collection root:

```
<collection-root>/environments/
├── dev.yml
├── qa.yml
└── prod.yml
```

Each yml lists variable names and their non-secret values. Secret variables have no value in the yml — the value lives in the vault under key `<envName>/<varName>`.

**Vault** — one encrypted file:

```
~/Library/Application Support/hitr/vault.age
```

Passphrase-derived key, [age](https://age-encryption.org/) format (ChaCha20-Poly1305). Written atomically via tmp+rename. Holds all secret values across all envs. If the vault file is deleted, non-secret values are untouched but every secret needs re-entering.

**Rename semantics** — renaming an env rewrites the yml at the new path AND migrates the vault entries under the new env name in a single transaction. Delete an env → yml deleted + vault entries for that env purged.

Response history is stored unencrypted in `<collection-root>/.hitr/history/`. Responses can contain sensitive data; exclude `.hitr/` from version control and protect the collection directory accordingly.

**What's safe to commit to git:**
- Request `.yml` files ✅
- Env `.yml` files ✅ (contain names + non-secret values, but never secrets)
- `vault.age` ❌ (encrypted, but keep out of shared repos anyway)
- `config.json` ❌ (has your local absolute path)

## Import notes

**Bruno collections** work directly — hitr's YAML shape matches [Bruno's](https://www.usebruno.com/) format (`info`, `http.method`, `http.url`, `http.headers`, `http.body.data`, plus `environments/*.yml`). Point hitr at a Bruno collection root and it just opens. Fields hitr writes are round-trip stable with Bruno.

**curl**: handles `-X`, `-H`, `-d/--data*`, `--json`, quoted args, backslash line-continuation. Ignores `-F/--form` (multipart), cookies, auth flags.

**OpenAPI**: reads YAML or JSON and creates a request per operation, grouped by the first tag. Query and header parameters are taken from both the path and operation; optional parameters without a value start disabled. JSON bodies use an example when available, or a schema-based template. Existing requests are skipped by default. To fill empty JSON bodies from a spec, select **Fill empty JSON bodies** during re-import and use the same folder prefix as the original import. Only matching requests with empty JSON bodies are changed; YAML formatting and comments in those files will be rewritten. Existing nonempty bodies are preserved.

## Deliberate non-features

Pre/post scripting. Chained requests. Team sync. GraphQL builder. Cookie jar. WebSocket. gRPC. File upload / multipart.

If it's in Postman and not here, it's on purpose.

## Contributing

PRs welcome. Keep the ethos: minimum surface area, no features that require a cloud account, no plugins.

## License

MIT — see [LICENSE](LICENSE).
