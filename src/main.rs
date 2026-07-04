//! Frontend entrypoint. Compiles to WASM, runs in the Tauri WebView.
//!
//! Modules:
//! - `app`   — the `App` component and every child component/modal
//! - `api`   — thin wrappers around Tauri `invoke`, one per backend command
//! - `types` — JSON DTOs mirroring `src-tauri/src/model.rs` (kept in sync manually)

mod api;
mod app;
mod types;

use app::App;
use dioxus::prelude::*;
use dioxus_logger::tracing::Level;

fn main() {
    console_error_panic_hook::set_once();
    dioxus_logger::init(Level::INFO).expect("failed to init logger");
    launch(App);
}
