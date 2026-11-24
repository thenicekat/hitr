//! Frontend entrypoint. Compiles to WASM, runs in the Tauri WebView.
//!
//! Modules:
//! - `app`   — the `App` component and every child component/modal
//! - `api`   — thin wrappers around Tauri `invoke`, one per backend command
//! - `types` — JSON DTOs mirroring `src-tauri/src/model.rs` (kept in sync manually)

mod api;
mod app;
mod mcp_help;
mod notes;
mod onboarding;
mod types;
mod update_notice;

use app::App;
use dioxus::prelude::*;
use dioxus_logger::tracing::Level;

fn main() {
    console_error_panic_hook::set_once();
    dioxus_logger::init(Level::INFO).expect("failed to init logger");
    if web_sys::window()
        .and_then(|w| w.location().search().ok())
        .is_some_and(|s| s.contains("notes=1"))
    {
        launch(notes::Notes);
    } else {
        launch(App);
    }
}
