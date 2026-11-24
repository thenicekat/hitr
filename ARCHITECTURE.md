# Architecture

Two Rust crates in one workspace:

- `hitr-ui` (root `Cargo.toml`) — Dioxus frontend, compiles to WASM, runs in the WebView.
- `hitr` (`src-tauri/`) — Tauri v2 backend, native Rust, runs on the host.

They communicate via Tauri's IPC bridge — the frontend calls Rust functions marked `#[tauri::command]` through `invoke("cmd_name", args)`.

## Notes and reminders

The floating window is `src/notes.rs`. A note is still rich-text HTML, with
stable `data-task-id` attributes on actionable blocks and optional
`data-done` / `data-remind-at` attributes. There is no separate task database:
editing or deleting the line also edits or deletes its reminder.

`assets/note-content.js` sanitizes the HTML and derives an outline of action
lines. The editor and the notebook-wide reminder index use that same parser.
`assets/note-editor.js` adds transient checkbox/reminder controls, which the
sanitizer excludes from saved HTML. It disposes document listeners and timers
when the editor unmounts.

The frontend compares persisted deadlines with the current wall clock. A due
line in any note lights the whole floating icon. Opening it selects the source
note and reveals the line; only completion, clearing, or rescheduling handles
the reminder. Hitr has to be running to display the cue. No system alarm is
registered, and no per-second disk writes are needed.

`src-tauri/src/notes_store.rs` retains atomic note saves and imports the earlier
Focus queue into a note once, leaving the source file intact. A failed optional
import returns a visible warning without preventing access to existing notes.

```
┌────────────────── window (WKWebView on macOS) ─────────────────┐
│  ┌──── Dioxus WASM ────────────────────────────────────┐       │
│  │  App component, signals, RSX layout, event handlers │       │
│  │              │                                      │       │
│  │              │  invoke("fire_request", {…})         │       │
│  └──────────────┼──────────────────────────────────────┘       │
│                 │                                              │
└─────────────────┼──────────────────────────────────────────────┘
                  ▼   Tauri IPC (JSON over local socket)
┌─────────────── Tauri host (native Rust) ─────────────────────┐
│  #[tauri::command] fns → loader / http / vault / curl        │
│  filesystem, reqwest, age encryption, keychain (deprecated)  │
└──────────────────────────────────────────────────────────────┘
```

## File layout

```
hitr/
├── Cargo.toml                # frontend crate + workspace root
├── Dioxus.toml               # dioxus-cli config (dev port, watch paths)
├── src/                      # frontend (WASM)
│   ├── main.rs               # panic hook, launch(App)
│   ├── app.rs                # main component + all UI
│   ├── api.rs                # thin Tauri invoke wrappers, one per command
│   └── types.rs              # JSON DTOs mirroring backend model
├── assets/
│   └── styles.css            # single-file CSS, tokyo-night palette
├── src-tauri/
│   ├── Cargo.toml            # backend crate
│   ├── tauri.conf.json       # window size, bundle id, CSP
│   ├── build.rs              # tauri-build codegen entrypoint
│   └── src/
│       ├── main.rs           # `hitr_lib::run()` shim
│       ├── lib.rs            # command handlers + AppState + Tauri Builder
│       ├── model.rs          # canonical structs (Env, Request, HttpSpec…)
│       ├── loader.rs         # walk collection dir, parse request/env YAML, write back
│       ├── http.rs           # reqwest-based fire, {{var}} substitution
│       ├── vault.rs          # age-encrypted secrets file
│       └── curl.rs           # tokenizer + parser for `curl` command strings
├── ARCHITECTURE.md           # you are here
├── README.md
└── LICENSE
```

## Startup sequence

1. Tauri host launches → `run()` in `lib.rs`:
   - reads `~/Library/Application Support/hitr/config.json` for last-known collection root (falls back to `~/collections/default`)
   - constructs `AppState { root, collection: None, vault: locked }`
   - starts the notes window at icon size and keeps the API window hidden → serves `dist/` (built by `dx serve` in dev, `dx bundle` in release)
2. Each WebView loads the WASM bundle. `?notes=1` mounts `Notes`; the API window mounts `App`.
3. `App::use_hook` fires once, including while its window is hidden:
   - `install_input_attrs()` — installs a `MutationObserver` that stamps `autocapitalize=off` on every input as it mounts
   - `load_all()` — Tauri IPC calls: `vault_status` → `get_root` → `load`
4. Opening APIs from the note saves pending edits, shows/focuses the API window, and collapses the note to its icon. Save failures leave the draft open.
5. If the vault is locked, `UnlockModal` blocks API interaction. Its **Back to notes** action works without a password, as does the API toolbar's action once unlocked. `open_notes_workspace` expands/focuses notes and hides APIs, preserving API view state. Both navigation commands recreate their window if it was closed.

The notes component observes the WebView's actual viewport size to synchronize
its expanded state. A native request to open notes therefore works even if it
arrives before the frontend mounts; it does not rely on a one-time event.

## Data flow — firing a request

```
user click [send]
    │
    ▼
fire callback in App                         (src/app.rs)
    │  invoke("fire_request", {requestId, envName})
    ▼
lib.rs::fire_request                         (src-tauri/src/lib.rs)
    │  looks up Request + Env from AppState.collection
    │  reads vault cache (unlocks/decrypts if needed)
    ▼
http.rs::fire                                (src-tauri/src/http.rs)
    │  resolve_env_vars(env, vault)          // secrets from vault, non-secrets inline
    │  substitute {{var}} in url + headers + body
    │  early-fail with named vars if any {{…}} left in url
    │  reqwest::Client → ..send().await
    ▼
FiredResponse { status, latency_ms, headers, body, is_json }
    │  (returned as JSON over IPC)
    ▼
App::response signal set → view re-renders
```

## Storage — how each type of data lives

| Kind | Format | Location | Notes |
|---|---|---|---|
| Requests | YAML | `<root>/<folder>/<name>.yml` | We write these back via `loader::write_request` when the user saves edits. Runtime fields (`path`, `rel_path`, `id`) stripped before write. Format matches Bruno's for round-trip compat. |
| Envs | YAML | `<root>/environments/<name>.yml` | Contains variable *names* only. Non-secret values inline. Secret values are absent — they're in the vault. |
| Secrets | age-encrypted JSON | `~/Library/Application Support/hitr/vault.age` | Passphrase-derived key, ChaCha20-Poly1305. Written atomically via tmp+rename. |
| Config | JSON | `~/Library/Application Support/hitr/config.json` | Just the collection root path. |

## Reactivity model

Dioxus 0.7 signals. Rules that matter:

- `.read()` inside a component subscribes it to that signal.
- `.peek()` reads without subscribing — use inside `use_hook` / event handlers to avoid re-render loops.
- `use_hook(|| …)` runs once per component instance (like React `useRef` init).
- `use_effect(|| …)` runs on every render where a subscribed signal changed — dangerous when the effect writes to signals it also reads. Prefer `use_hook` + explicit callback for one-shot side effects.
- `use_callback(|arg| …)` returns a stable `Callback<T>` — pass to child components to avoid prop-eq breakage.
- `use_memo(|| …)` recomputes only when subscribed inputs change.

The 300-row render cap (`RENDER_CAP` in `App`) exists because rendering all ~1500 rows in a large collection freezes the WebView on click. Filter narrows through the full list; only the display truncates.

## Tauri IPC contract

Every backend command is a `#[tauri::command]` fn. Argument names in Rust must match the JS-side keys sent by `invoke`. Since we go from Rust → JS, we use `serde(rename = "camelCase")` on frontend `Args` structs to match Rust snake_case params. See `src/api.rs`.

Return types must be JSON-serializable. `u128` looks fine but overflows JSON's number range — use `u64`. This is a real trap. See `FiredResponse::latency_ms`.

`#[serde(skip)]` on struct fields drops the field for **all** serde targets, including Tauri's IPC serializer. If you need runtime-only fields on a struct that also crosses IPC, use `#[serde(default, skip_serializing_if = "String::is_empty")]` and strip in the writer function instead.

## Vault

Secrets live in `vault.age`, written using [age](https://age-encryption.org/) with a passphrase-derived key.

- Password lives in memory as `Arc<Mutex<Option<SecretString>>>` for the session — never persisted.
- Cache lives alongside password: `Option<VaultData>`. Populated on first read, invalidated on lock.
- Every write re-encrypts and rewrites the whole file (there's ~10 secrets, atomicity beats performance here).

## Curl parser

`src-tauri/src/curl.rs`:

1. `tokenize(input)` — shell-like tokenizer that respects `'…'`, `"…"`, `\` line continuations, and escape sequences inside double-quoted strings. Not a full shell parser — handles the shape people actually paste.
2. `parse_curl(input)` — walks tokens, extracts flags into a Request. Infers `POST` if `-d` present without `-X`. Sets body kind = `json` if `--json` flag or Content-Type header matches. Ignores flags that don't affect the request semantics (`-i`, `-s`, `-v`, `--compressed`, etc.).

Not implemented: `-F/--form` (multipart), `-b/--cookie`, `-u/--user` (silently dropped, not applied).

## Adding a new Tauri command

1. Add fn in `src-tauri/src/lib.rs` with `#[tauri::command]` attr.
2. Register it in the `invoke_handler` list in `run()`.
3. Add matching wrapper + `Args` struct in `src/api.rs` on the frontend.
4. Call from a component via `spawn(async move { api::my_command(…).await })`.

## Adding a new UI feature

The API workspace lives in `src/app.rs`. Signals live at the top of `App()`. Modals are their own `#[component]`s with an `on_close: EventHandler<()>` prop that reloads the collection. The floating workspace lives in `src/notes.rs`; note editing and inline reminder controls live in `assets/note-editor.js`.

For anything performance-sensitive (long lists), extract into a component with `PartialEq`-cheap props (Signals, primitives) so Dioxus can prop-eq skip re-renders.

## Verification

CI runs `cargo fmt --all -- --check`, backend tests, and native/WASM checks and
Clippy with warnings denied. Run the backend tests with
`cargo test --manifest-path src-tauri/Cargo.toml`. Note-storage tests use temporary
directories to check reminder preservation and one-time task migration without
touching personal data. Editor and rendered-UI smoke checks cover line identity,
reminder scheduling, expiry, completion, and returning to a due line.

## What's *not* here (and why)

- No Node / no npm. Rust → WASM through the Dioxus CLI.
- No CSS framework, no component library. Shared theme tokens and hand-written styles.

The design bias throughout is **cost the feature honestly** — every abstraction, dependency, or config option is one more thing to explain, break, and remove later.
