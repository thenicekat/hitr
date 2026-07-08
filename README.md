# hitr

Fast, native REST client for [Bruno](https://www.usebruno.com/)-format collections. Rust + Tauri + Dioxus. No Electron, no Node runtime.

Built because Bruno's Electron shell is slow and Postman requires a cloud account. This does one thing: read a collection, edit envs, fire requests. Nothing else.

## Status

**MVP.** macOS-tested. Cross-platform in principle (age-encrypted vault, no OS-specific deps) but only tested on macOS.

## Features

**Collections**
- Reads Bruno-format `.yml` collections directly — no import step
- Import from curl paste (Chrome DevTools "Copy as cURL" works)
- Import from OpenAPI 3.x spec (yaml or json) — one request per operation, foldered by tag
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
- In-memory history: last 10 fires per request
- Copy response body or copy request as curl (with resolved vars) to clipboard

**Ops**
- ~15 MB single binary, ~50 ms warm start (vs Bruno's 3-5 s Electron boot)
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
| Requests            | Bruno YAML at `<root>/<folder>/<name>.yml`                        |
| Envs                | Bruno YAML at `<root>/environments/<name>.yml` (variable *names* + non-secret values) |
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

**What's safe to commit to git:**
- Request `.yml` files ✅
- Env `.yml` files ✅ (contain names + non-secret values, but never secrets)
- `vault.age` ❌ (encrypted, but keep out of shared repos anyway)
- `config.json` ❌ (has your local absolute path)

## Import notes

**curl**: handles `-X`, `-H`, `-d/--data*`, `--json`, quoted args, backslash line-continuation. Ignores `-F/--form` (multipart), cookies, auth flags.

**OpenAPI**: reads yaml or json, walks `paths.<path>.<method>`, one request per operation. Folder = first tag. Extracts query + header params. Body from `application/json` example (no schema-driven generation). Suggested env vars from `servers[0]` + `securitySchemes`. Skips existing files — re-import is idempotent.

## Deliberate non-features

Persistent request history. Pre/post scripting. Chained requests. Team sync. GraphQL builder. Cookie jar. WebSocket. gRPC. File upload / multipart.

If it's in Postman and not here, it's on purpose.

## Contributing

PRs welcome. Keep the ethos: minimum surface area, no features that require a cloud account, no plugins.

## License

MIT — see [LICENSE](LICENSE).
