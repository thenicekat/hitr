# hitr

Hitr is a native macOS workspace for API requests and notes with quiet reminders. Its API client supports plain-text YAML collections, cURL and OpenAPI import, and a local MCP server for agents. Built with Rust, Tauri, and Dioxus.

Built because heavyweight Electron clients are slow and cloud-first tools want your data on their servers. Keep API work, quick notes, and your next task close at hand, with local storage and no cloud account.

## Status

**MVP.** macOS-tested. Cross-platform in principle (age-encrypted vault, no OS-specific deps) but only tested on macOS.

## Notes with quiet reminders

Hitr opens a small floating notes button. Click to open it or drag the whole
button to move it. Write freely, then turn any actionable line into a checkbox
using the toolbar or **⌘⇧L**. Notes, context, and things to do share the same page.

On a new installation, opening the icon first shows a short, three-step
walkthrough: try a checkbox, preview the reminder glow, and explore how notes
and APIs connect. Choose **Start writing** or **Skip intro** to open your first
note. Completion is remembered. Existing users go straight to their notes;
anyone can replay the walkthrough from **··· → Quick tour**.

Use the bell in the toolbar, or **Remind** beside a checkbox, to set a reminder
for that line. Choose 5, 15, 30, or 60 minutes, or a specific local date and time.
When a reminder is due, the **whole floating icon glows softly**. There is no
flashing, sound, or notification popup. Clicking the icon opens the relevant
note and highlights the due line. The footer also links to due reminders.

Open **Remind** again to move the reminder to a later time or **Clear reminder**.
Checking off a line clears its reminder. Simply viewing a reminder does not
complete the task or dismiss it. Reminders from every saved note contribute to
the glow, including notes you are not currently viewing.

The note switcher selects a saved note; **+** saves the current note and creates
a new one. Notes autosave and support bold, italic, underline, headings, and
lists. Titles come from the first non-empty line. Use **···** for appearance,
text size, and stay-on-top preferences. Appearance is shared with the API
workspace.

Choose **··· → Delete note** to remove the current note and open the next one
(or the previous one at the end of the list). Deleting your only note opens a
fresh blank. **Undo** restores the most recently deleted note, including its
latest edits and reminders, without losing work in other notes. Undo survives
restarts until you dismiss it or delete another note.

Hitr starts with only the floating icon. Use the existing **`</>`** button in
the note header to open APIs; the note saves and collapses as the API window
opens. **Back to notes** returns to your note and keeps the API workspace ready
for next time. You can return from the vault unlock screen without unlocking it.
Reminder times sit beside their task as a quiet, clickable time label.

Notes, line IDs, completion state, and reminder times are stored together in
`notes.json` under the local Hitr application data directory. Notes are plain
HTML in JSON, separate from the encrypted secrets vault. Reminder times survive
restarts; overdue reminders glow when Hitr is running again. Hitr must be
running for the icon to show a reminder—it does not schedule system alarms.

Existing quick-note files are imported on first use and retained. Tasks from
the earlier separate Focus experiment are imported once into a **Things to do**
note, preserving completion state and leaving the original `focus.json` intact.

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

Grab the Apple Silicon `.dmg` from the [latest release](https://github.com/thenicekat/hitr/releases/latest):

- Apple Silicon (M1/M2/M3/M4): `hitr_<version>_aarch64.dmg`

Current prebuilt releases support Apple Silicon only.

Open the DMG and drag `hitr.app` to `/Applications`.

**Gatekeeper will complain** because the build isn't notarized (no Apple Developer cert — costs $99/yr, skipped for now). Strip the quarantine flag once:

```bash
xattr -dr com.apple.quarantine /Applications/hitr.app
```

Then launch normally. Alternative: right-click `hitr.app` → **Open** → **Open** in the confirmation dialog. Both work; `xattr` is one-shot and the app opens cleanly on every subsequent launch.

If macOS still refuses ("hitr is damaged"), the download attribute wasn't cleared — re-run `xattr` targeting the actual path, or try the same command on the `.dmg` before opening it.

### App updates

Hitr checks for new releases at launch and every 24 hours while running. You can also choose **Check for Updates** from the Hitr menu. When an update is available, choose **Update and restart** to download the package, verify its signature, and install it. Older builds without the updater need a one-time manual install from GitHub Releases.

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

1. Open **MCP** in Hitr and turn on **Enable MCP**. The setting is off by default and is remembered across launches. Previous per-request grant files do not enable this setting.
2. Under **Allowed environments**, explicitly select the environments MCP may use. None are allowed by default; selections are saved for each environment file.
3. Select the **Copy configuration** icon and add the copied configuration to your MCP client. Set the server command to the installed Hitr executable and pass `mcp` as its argument.
4. Keep Hitr open and unlock its vault. Both MCP tools pause when the vault locks or MCP is disabled.

| Tool | What it does |
| --- | --- |
| `search_requests` | Finds saved requests by name, ID, or method and returns each request's `input_schema` and the available environments. Send an empty query to browse. Pass back `next_offset` as `offset` to get another page. |
| `send_request` | Runs a saved request by its exact ID with an allowed environment and optional `inputs`. |

Use the schema's `query` and `body` objects to set filters for one call. For example, a saved list request might accept `"inputs": { "query": { "state": ["active", "pending"], "limit": 10 } }`. Available inputs are OpenAPI query parameters and typed JSON body fields, including nested objects and arrays of objects. Unknown and protected fields are rejected at every level. Hitr keeps saved values for fields you leave alone and omits optional empty query parameters unless the spec permits empty values. Saved nested objects and environment-bound values remain under desktop control.

Hitr saves input schemas on OpenAPI import. For older imports, keep the original spec as `openapi.yaml` or `spec.yaml` at the collection root (`.yml` and `.json` also work) and reload the collection. Hitr matches the saved method and URL to the spec.

MCP can use every saved request with an allowed environment. Saved requests can change remote data, and a fixed URL or an allowed environment can point to the same server as a disallowed environment. Keep MCP enabled only if you want local clients to have that access while Hitr is unlocked.

Hitr uses the saved method, destination, headers, and auth, and applies typed filter values for the current call only. It attaches credentials in the desktop process, checks the selected environment’s `baseUrl` origin when configured, and uses HTTPS, following up to five redirects within the original origin. Redirects to another origin or to HTTP are not followed. Results include HTTP status, latency, and up to 128 KiB of response data. `body_omitted_reason` reports `size_limit`, `invalid_utf8`, or `credential_match`. An API response can still contain sensitive data. Turn on **Share full response** in the MCP panel to send the entire UTF-8 response body to your MCP client and LLM, lifting the 128 KiB limit and replacing known credentials with `[REDACTED]` instead of omitting the body. Redaction covers saved vault secrets and request credentials, including URL-encoded and JSON-escaped forms, and common JSON credential fields such as `access_token` and `password`. Unknown credentials and other sensitive data may still be included, and large responses increase client context usage. The toggle is off by default, remembered across launches, and applies to all allowed environments. Invalid UTF-8 bodies are still omitted; vault and environment restrictions still apply.

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
