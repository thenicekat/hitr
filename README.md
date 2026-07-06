# hitr

Fast, native REST client for [Bruno](https://www.usebruno.com/)-format collections. Rust + Tauri + Dioxus. No Electron, no Node runtime.

Built because Bruno's Electron shell is slow and Postman requires a cloud account. This does one thing: read a collection, edit envs, fire requests. Nothing else.

## Status

**MVP.** macOS-tested. Cross-platform in principle (age-encrypted vault, no OS-specific deps) but only tested on macOS.

## Features

- Reads Bruno-format `.yml` collections directly — no import step
- Edit / rename / create / delete envs
- Create requests (from scratch, from a curl paste, or via `+ new` form)
- Full request editor: method, URL, params, headers, body (JSON / text)
- `{{var}}` substitution from selected env at fire time
- Secrets encrypted with [age](https://age-encryption.org/) + master password (never touch disk unencrypted)
- Response pane: status, latency, headers, pretty-printed JSON body
- Fuzzy search over 1000+ requests
- ~5 MB binary, ~50 ms warm start

## Install

```bash
# prereqs
rustup target add wasm32-unknown-unknown
cargo install dioxus-cli --locked
cargo install tauri-cli --version '^2.0.0' --locked

# build & run
git clone https://github.com/<you>/hitr
cd hitr
cargo tauri dev              # dev mode with hot reload
cargo tauri build            # release .app bundle in src-tauri/target/release/bundle/macos
```

Point hitr at your collection root via the topbar path (or edit `~/Library/Application Support/hitr/config.json`).

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

## Var substitution

`{{name}}` in URL, headers, params, or body resolves from the currently-selected env. Secret vars pull from the vault at fire-time. Unresolved vars stay as `{{name}}` in the output — the fire fails loudly and tells you which vars are missing.

**Bearer helper:** if the env has a `bearerToken` var and the request has no `Authorization` header, hitr sends `Authorization: Bearer <val>` automatically.

## Curl import

Requests pane → **curl** → paste any curl command (Chrome DevTools "Copy as cURL" works). Handles `-X`, `-H`, `-d/--data*`, `--json`, quoted args, backslash line-continuation.

Doesn't handle: `-F/--form` (multipart), cookies, auth flags. Add if you need.

## Deliberate non-features

Request history. Pre/post scripting. Chained requests. Team sync. GraphQL builder. Cookie jar. WebSocket. gRPC.

If it's in Postman and not here, it's on purpose. The value is what's *not* built.

## Contributing

PRs welcome. Keep the ethos: minimum surface area, no features that require a cloud account, no plugins.

## License

MIT — see [LICENSE](LICENSE).
