//! UI. Single file by design — every component, modal, and event handler
//! lives here so behavior is greppable.
//!
//! ## Component tree
//!
//! ```text
//! App
//! ├── UnlockModal          (blocks all interaction while vault locked)
//! ├── topbar (root picker, lock button, reload)
//! ├── envs pane
//! ├── requests pane        (search + list of ReqRow, capped at 300 rendered)
//! ├── response pane
//! │   ├── RequestEditor    (method, url, KVEditor for params/headers, body)
//! │   │   └── KVEditor
//! │   └── (response viewer inline)
//! ├── NewEnvModal
//! ├── EditEnvModal
//! ├── NewRequestModal
//! └── ImportCurlModal
//! ```
//!
//! ## Reactivity gotchas
//!
//! - `use_effect` re-runs whenever any signal it *reads* changes. If the
//!   effect also *writes* to signals it reads, you get an infinite loop.
//!   Use `use_hook` for one-shot side effects (like initial load) and
//!   `.peek()` inside the closure to read without subscribing.
//! - `use_callback` returns a stable `Callback<T>` — pass this to child
//!   components instead of raw closures, or prop-eq breaks and every child
//!   re-renders on every parent render.
//! - Child components are memoized by prop equality. `Signal<T>` is a stable
//!   `Copy` handle → passing it as a prop is cheap and doesn't invalidate.
//!
//! ## Render cap
//!
//! `RENDER_CAP = 300` limits how many request rows are actually mounted.
//! Search filters through the full collection first, then truncates for
//! display — matches still surface even if you have 1500 requests.

#![allow(non_snake_case)]

use crate::api;
use crate::types::*;
use dioxus::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// Split a URL string on the first `?`. Everything before is kept in the URL
/// input; everything after becomes params rows. Handles `{{baseUrl}}/path?a=b`
/// safely — templates in the pre-`?` part pass through unchanged.
///
/// Params list preserves order and doesn't URL-decode values (Bruno stores
/// raw). Rows are enabled=true by default.
fn split_url_and_params(url: &str) -> (String, Vec<KV>) {
    let Some(q_pos) = url.find('?') else {
        return (url.to_string(), Vec::new());
    };
    let base = url[..q_pos].to_string();
    let query = &url[q_pos + 1..];
    let mut out = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = match pair.find('=') {
            Some(eq) => (pair[..eq].to_string(), pair[eq + 1..].to_string()),
            None => (pair.to_string(), String::new()),
        };
        out.push(KV {
            name,
            value,
            kind: Some("query".into()),
            description: None,
            enabled: Some(true),
        });
    }
    (base, out)
}

/// Toggle `// ` comment on the line(s) covered by [start,end] in `text`.
/// Returns (new_text, new_caret_start, new_caret_end). If every line in the
/// range is already commented, uncomment; otherwise, comment all.
fn toggle_line_comment(text: &str, start: usize, end: usize) -> (String, usize, usize) {
    let bytes = text.as_bytes();
    // find line-start for `start`
    let mut ls = start.min(bytes.len());
    while ls > 0 && bytes[ls - 1] != b'\n' {
        ls -= 1;
    }
    // find line-end for `end`
    let mut le = end.min(bytes.len());
    while le < bytes.len() && bytes[le] != b'\n' {
        le += 1;
    }

    let block = &text[ls..le];
    let all_commented = !block.is_empty()
        && block
            .lines()
            .all(|l| l.is_empty() || l.trim_start().starts_with("//"));

    let mut new_lines: Vec<String> = Vec::new();
    let mut delta_start: isize = 0;
    let mut delta_end: isize = 0;
    let mut first = true;
    for line in block.split('\n') {
        if all_commented {
            // remove leading whitespace + `//` + optional single space
            if let Some(idx) = line.find("//") {
                let head = &line[..idx];
                let rest = &line[idx + 2..];
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                let new_line = format!("{}{}", head, rest);
                let removed = (line.len() as isize) - (new_line.len() as isize);
                if first {
                    delta_start -= removed;
                }
                delta_end -= removed;
                new_lines.push(new_line);
            } else {
                new_lines.push(line.to_string());
            }
        } else if !line.is_empty() {
            let new_line = format!("// {}", line);
            let added = (new_line.len() as isize) - (line.len() as isize);
            if first {
                delta_start += added;
            }
            delta_end += added;
            new_lines.push(new_line);
        } else {
            new_lines.push(line.to_string());
        }
        first = false;
    }

    let new_block = new_lines.join("\n");
    let mut out = String::with_capacity(text.len() + 32);
    out.push_str(&text[..ls]);
    out.push_str(&new_block);
    out.push_str(&text[le..]);

    let new_start = (start as isize + delta_start).max(0) as usize;
    let new_end = (end as isize + delta_end).max(0) as usize;
    (out, new_start, new_end)
}

/// Best-effort clipboard write. Silently drops errors — clipboard access can
/// fail if the WebView doesn't have focus, and there's nothing useful to say.
fn copy_to_clipboard(text: &str) {
    if let Some(win) = web_sys::window() {
        let clipboard = win.navigator().clipboard();
        let _ = clipboard.write_text(text);
    }
}

/// Global keyboard shortcuts: cmd/ctrl+Enter fires the selected request.
/// cmd/ctrl+S is intercepted only to prevent the browser's default save-page
/// dialog — the app already autosaves on every edit.
fn install_shortcuts(fire: Callback<()>) {
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };
    let cb = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |e: web_sys::KeyboardEvent| {
        let mod_key = e.meta_key() || e.ctrl_key();
        if !mod_key {
            return;
        }
        match e.key().as_str() {
            "Enter" => {
                e.prevent_default();
                fire.call(());
            }
            "s" | "S" => {
                e.prevent_default(); /* autosave already ran */
            }
            _ => {}
        }
    });
    let _ = doc.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
    cb.forget();
}

/// Stamp `autocapitalize=off / autocorrect=off / spellcheck=false` on every
/// `<input>` and `<textarea>` in the document, plus any inserted afterwards
/// via a MutationObserver. Runs once on App mount.
///
/// Cheaper than adding attrs to every `input {}` in the RSX and impossible
/// to forget for future inputs.
fn install_input_attrs() {
    let Some(win) = web_sys::window() else { return };
    let Some(doc) = win.document() else { return };

    let apply = |root: &web_sys::Node| {
        if let Ok(el) = root.clone().dyn_into::<web_sys::Element>() {
            for tag in ["input", "textarea"] {
                if let Ok(list) = el.query_selector_all(tag) {
                    for i in 0..list.length() {
                        if let Some(node) = list.item(i)
                            && let Ok(e) = node.dyn_into::<web_sys::Element>()
                        {
                            let _ = e.set_attribute("autocapitalize", "off");
                            let _ = e.set_attribute("autocorrect", "off");
                            let _ = e.set_attribute("spellcheck", "false");
                        }
                    }
                }
            }
        }
    };

    // patch existing
    if let Some(body) = doc.body() {
        apply(body.as_ref());
    }

    // patch on insert
    let cb = Closure::<dyn FnMut(js_sys::Array)>::new(move |records: js_sys::Array| {
        for i in 0..records.length() {
            let rec = records.get(i);
            if let Ok(mr) = rec.dyn_into::<web_sys::MutationRecord>() {
                let added = mr.added_nodes();
                for j in 0..added.length() {
                    if let Some(n) = added.item(j) {
                        apply(&n);
                    }
                }
            }
        }
    });

    if let Ok(observer) = web_sys::MutationObserver::new(cb.as_ref().unchecked_ref()) {
        let init = web_sys::MutationObserverInit::new();
        init.set_child_list(true);
        init.set_subtree(true);
        if let Some(body) = doc.body() {
            let _ = observer.observe_with_options(body.as_ref(), &init);
        }
    }
    cb.forget();
}

static CSS: Asset = asset!("/assets/styles.css");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Modal {
    None,
    NewEnv,
    EditEnv,
    NewRequest,
    ImportCurl,
    ImportOpenApi,
}

pub fn App() -> Element {
    let mut collection = use_signal(Collection::default);
    let mut selected_env = use_signal(|| None::<String>);
    let mut selected_req = use_signal(|| None::<String>);
    let mut filter = use_signal(String::new);
    let mut response = use_signal(|| None::<FiredResponse>);
    let mut firing = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut modal = use_signal(|| Modal::None);
    let mut root = use_signal(String::new);
    let mut vault_locked = use_signal(|| true);
    let mut vault_exists = use_signal(|| true);
    let mut editing_root = use_signal(|| false);
    let mut root_input = use_signal(String::new);
    let mut toast = use_signal(|| None::<String>);

    let load_all = use_callback(move |()| {
        spawn(async move {
            match api::vault_status().await {
                Ok(s) => {
                    vault_locked.set(!s.unlocked);
                    vault_exists.set(s.exists);
                }
                Err(e) => error.set(Some(format!("vault_status: {}", e))),
            }
            match api::get_root().await {
                Ok(r) => root.set(r),
                Err(e) => error.set(Some(format!("get_root: {}", e))),
            }
            match api::load().await {
                Ok(c) => {
                    if selected_env.peek().is_none()
                        && let Some(first) = c.envs.first()
                    {
                        selected_env.set(Some(first.name.clone()));
                    }
                    collection.set(c);
                    error.set(None);
                }
                Err(e) => error.set(Some(format!("load: {}", e))),
            }
        });
    });

    const RENDER_CAP: usize = 300;
    let filtered_reqs = use_memo(move || {
        let coll = collection.read();
        let q = filter.read().to_lowercase();
        let iter: Box<dyn Iterator<Item = &Request>> = if q.is_empty() {
            Box::new(coll.requests.iter())
        } else {
            Box::new(coll.requests.iter().filter(move |r| {
                r.info.name.to_lowercase().contains(&q)
                    || r.http.url.to_lowercase().contains(&q)
                    || r.rel_path.to_lowercase().contains(&q)
                    || r.http.method.to_lowercase().contains(&q)
            }))
        };
        iter.take(RENDER_CAP).cloned().collect::<Vec<_>>()
    });
    let total_reqs = use_memo(move || collection.read().requests.len());
    let shown_reqs = use_memo(move || filtered_reqs.read().len());

    // History for the currently-selected request. Reloaded from disk on
    // selection change and after every fire; never in-memory-only.
    let mut history = use_signal(Vec::<FiredResponse>::new);
    use_effect(move || {
        let rid = selected_req.read().clone();
        spawn(async move {
            let Some(rid) = rid else {
                history.set(Vec::new());
                return;
            };
            match api::load_history(&rid).await {
                Ok(h) => history.set(h),
                Err(_) => history.set(Vec::new()),
            }
        });
    });

    let fire = use_callback(move |()| {
        let req_id = selected_req.read().clone();
        let env_name = selected_env.read().clone();
        let Some(req_id) = req_id else { return };
        firing.set(true);
        response.set(None);
        error.set(None);
        spawn(async move {
            match api::fire_request(&req_id, env_name.as_deref()).await {
                Ok(r) => {
                    response.set(Some(r));
                    // Reload from disk to pick up the newly-appended entry
                    // (backend wrote it in fire_request).
                    if let Ok(h) = api::load_history(&req_id).await {
                        history.set(h);
                    }
                }
                Err(e) => error.set(Some(format!("fire: {}", e))),
            }
            firing.set(false);
        });
    });

    use_hook(|| {
        load_all.call(());
        install_input_attrs();
        install_shortcuts(fire);
    });

    // Auto-dismiss toast after 2s. Guards on Some so re-triggering a toast
    // while one is showing resets the timer rather than double-clearing.
    use_effect(move || {
        if toast.read().is_some() {
            spawn(async move {
                gloo_timers::future::TimeoutFuture::new(2000).await;
                toast.set(None);
            });
        }
    });

    let selected_req_obj = use_memo(move || {
        let id = selected_req.read().clone()?;
        collection
            .read()
            .requests
            .iter()
            .find(|r| r.id == id)
            .cloned()
    });

    let selected_env_obj = use_memo(move || {
        let name = selected_env.read().clone()?;
        collection
            .read()
            .envs
            .iter()
            .find(|e| e.name == name)
            .cloned()
    });

    rsx! {
        link { rel: "stylesheet", href: CSS }
        div { class: "app",
            // header
            div { class: "topbar",
                span { class: "brand", "hitr" }
                if *editing_root.read() {
                    input {
                        class: "root-input",
                        value: "{root_input}",
                        oninput: move |e| root_input.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter {
                                let p = root_input.read().clone();
                                spawn(async move {
                                    match api::set_root(&p).await {
                                        Ok(_) => {
                                            editing_root.set(false);
                                            selected_env.set(None);
                                            selected_req.set(None);
                                            load_all.call(());
                                        }
                                        Err(err) => error.set(Some(format!("set_root: {}", err))),
                                    }
                                });
                            } else if e.key() == Key::Escape {
                                editing_root.set(false);
                            }
                        },
                    }
                    button {
                        class: "btn small",
                        title: "browse for folder",
                        onclick: move |_| {
                            spawn(async move {
                                if let Ok(Some(picked)) = api::pick_folder().await {
                                    match api::set_root(&picked).await {
                                        Ok(_) => {
                                            editing_root.set(false);
                                            selected_env.set(None);
                                            selected_req.set(None);
                                            load_all.call(());
                                        }
                                        Err(err) => error.set(Some(format!("set_root: {}", err))),
                                    }
                                }
                            });
                        },
                        "browse"
                    }
                    button { class: "btn small", onclick: move |_| editing_root.set(false), "cancel" }
                } else {
                    span { class: "root",
                        onclick: move |_| { root_input.set(root.read().clone()); editing_root.set(true); },
                        title: "click to change",
                        "root: {root}"
                    }
                }
                span { class: "sep" }
                if let Some(e) = error.read().as_ref() {
                    span { class: "error", "{e}" }
                }
                if !*vault_locked.read() {
                    button {
                        class: "btn icon ghost",
                        title: "lock vault",
                        onclick: move |_| {
                            spawn(async move {
                                let _ = api::vault_lock().await;
                                vault_locked.set(true);
                            });
                        },
                        "🔒"
                    }
                }
                button {
                    class: "btn icon ghost",
                    title: "reload collection",
                    onclick: move |_| load_all.call(()),
                    "↻"
                }
            }

            div { class: "main",
                // envs pane
                div { class: "pane envs",
                    div { class: "pane-hdr",
                        span { "envs" }
                        if let Some(env) = selected_env_obj.read().as_ref() {
                            button { class: "btn icon", title: "edit env",
                                onclick: move |_| modal.set(Modal::EditEnv), "✎" }
                            {
                                let e = env.clone();
                                rsx! {
                                    button { class: "btn icon danger", title: "delete env",
                                        onclick: move |_| {
                                            let e = e.clone();
                                            spawn(async move {
                                                if let Err(err) = api::delete_env(&e).await {
                                                    error.set(Some(format!("delete_env: {}", err)));
                                                    return;
                                                }
                                                selected_env.set(None);
                                                load_all.call(());
                                            });
                                        },
                                        "🗑"
                                    }
                                }
                            }
                        }
                        button { class: "btn icon", title: "new env", onclick: move |_| modal.set(Modal::NewEnv), "+" }
                    }
                    div { class: "list",
                        for env in collection.read().envs.iter() {
                            {
                                let name = env.name.clone();
                                let selected = selected_env.read().as_deref() == Some(&name);
                                let n2 = name.clone();
                                rsx! {
                                    div {
                                        key: "{name}",
                                        class: if selected { "row selected" } else { "row" },
                                        onclick: move |_| selected_env.set(Some(n2.clone())),
                                        span { class: "row-title", "{name}" }
                                        span { class: "row-meta", "{env.variables.len()} vars" }
                                    }
                                }
                            }
                        }
                    }
                }

                // requests pane
                div { class: "pane requests",
                    div { class: "pane-hdr",
                        input {
                            class: "search",
                            placeholder: "search {total_reqs} requests…",
                            value: "{filter}",
                            oninput: move |e| filter.set(e.value()),
                        }
                        span { class: "muted small",
                            if *shown_reqs.read() < *total_reqs.read() {
                                "{shown_reqs}/{total_reqs}"
                            } else {
                                "{total_reqs}"
                            }
                        }
                        button { class: "btn small", title: "import curl", onclick: move |_| modal.set(Modal::ImportCurl), "curl" }
                        button { class: "btn small", title: "import openapi spec", onclick: move |_| modal.set(Modal::ImportOpenApi), "spec" }
                        button { class: "btn icon", title: "new request", onclick: move |_| modal.set(Modal::NewRequest), "+" }
                    }
                    div { class: "list",
                        {
                            let sel = selected_req.read().clone();
                            let reqs = filtered_reqs.read();
                            rsx! {
                                for req in reqs.iter() {
                                    ReqRow {
                                        key: "{req.id}",
                                        id: req.id.clone(),
                                        method: req.http.method.clone(),
                                        name: req.info.name.clone(),
                                        rel_path: req.rel_path.clone(),
                                        selected: sel.as_deref() == Some(&req.id),
                                        selected_req,
                                    }
                                }
                            }
                        }
                    }
                }

                // request editor + response pane
                div { class: "pane response",
                    if let Some(r) = selected_req_obj.read().as_ref() {
                        RequestEditor {
                            key: "{r.id}",
                            req: r.clone(),
                            firing: *firing.read(),
                            current_env_name: selected_env.read().clone(),
                            on_fire: move |_| fire.call(()),
                            on_saved: move |_| load_all.call(()),
                            on_toast: move |msg: String| toast.set(Some(msg)),
                        }
                    } else {
                        div { class: "pane-hdr",
                            span { class: "muted", "select a request" }
                        }
                    }
                    div { class: "response-body",
                        if let Some(resp) = response.read().as_ref() {
                            div { class: "resp-status",
                                span { class: "status-code s{resp.status / 100}xx", "{resp.status} {resp.status_text}" }
                                span { class: "latency", "{resp.latency_ms} ms" }
                                span { class: "final-url muted", "{resp.final_url}" }
                                {
                                    let body = resp.body.clone();
                                    rsx! {
                                        button {
                                            class: "btn icon",
                                            title: "copy response body",
                                            onclick: move |_| {
                                                copy_to_clipboard(&body);
                                                toast.set(Some("response copied".into()));
                                            },
                                            "📋"
                                        }
                                    }
                                }
                            }
                            {
                                let hist = history.read().clone();
                                if hist.len() > 1 {
                                    let rid_for_clear = selected_req.read().clone();
                                    rsx! {
                                        details { class: "headers",
                                            summary { "history ({hist.len()})" }
                                            if let Some(rid) = rid_for_clear {
                                                button {
                                                    class: "btn small ghost",
                                                    onclick: move |_| {
                                                        let rid = rid.clone();
                                                        spawn(async move {
                                                            if api::clear_history(&rid).await.is_ok() {
                                                                history.set(Vec::new());
                                                            }
                                                        });
                                                    },
                                                    "clear"
                                                }
                                            }
                                            for (i, h) in hist.iter().enumerate() {
                                                {
                                                    let h_clone = h.clone();
                                                    rsx! {
                                                        div { class: "hdr history-row", key: "{i}",
                                                            onclick: move |_| response.set(Some(h_clone.clone())),
                                                            span { class: "hdr-k", "#{i} · {h.status} {h.status_text}" }
                                                            span { class: "hdr-v", "{h.latency_ms} ms" }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else { rsx! {} }
                            }
                            details { class: "headers",
                                summary { "headers ({resp.headers.len()})" }
                                for (k, v) in resp.headers.iter() {
                                    div { class: "hdr", key: "{k}",
                                        span { class: "hdr-k", "{k}" }
                                        span { class: "hdr-v", "{v}" }
                                    }
                                }
                            }
                            if resp.is_json {
                                JsonTree { text: resp.body.clone() }
                            } else {
                                pre { class: "body-json", "{resp.body}" }
                            }
                        } else {
                            div { class: "muted center", "no response yet" }
                        }
                    }
                }
            }

            // modals
            match *modal.read() {
                Modal::None => rsx! {},
                Modal::NewEnv => rsx! { NewEnvModal { on_close: move |_| { modal.set(Modal::None); load_all.call(()); }, envs: collection.read().envs.clone() } },
                Modal::EditEnv => {
                    let env = selected_env_obj.read().clone();
                    rsx! {
                        if let Some(env) = env {
                            EditEnvModal { env, on_close: move |_| { modal.set(Modal::None); load_all.call(()); } }
                        }
                    }
                }
                Modal::NewRequest => {
                    let default_folder = selected_req_obj.read().as_ref().and_then(|r| {
                        std::path::Path::new(&r.rel_path).parent().and_then(|p| p.to_str()).map(String::from)
                    }).unwrap_or_default();
                    rsx! {
                        NewRequestModal { default_folder, on_close: move |_| { modal.set(Modal::None); load_all.call(()); } }
                    }
                }
                Modal::ImportOpenApi => rsx! {
                    ImportOpenApiModal { on_close: move |_| { modal.set(Modal::None); load_all.call(()); } }
                },
                Modal::ImportCurl => {
                    let default_folder = selected_req_obj.read().as_ref().and_then(|r| {
                        std::path::Path::new(&r.rel_path).parent().and_then(|p| p.to_str()).map(String::from)
                    }).unwrap_or_default();
                    rsx! {
                        ImportCurlModal { default_folder, on_close: move |_| { modal.set(Modal::None); load_all.call(()); } }
                    }
                }
            }

            if *vault_locked.read() {
                UnlockModal {
                    exists: *vault_exists.read(),
                    on_unlocked: move |_| {
                        vault_locked.set(false);
                        vault_exists.set(true);
                    },
                }
            }

            if let Some(msg) = toast.read().as_ref() {
                div { class: "toast", "{msg}" }
            }
        }
    }
}

#[component]
fn UnlockModal(exists: bool, on_unlocked: EventHandler<()>) -> Element {
    let mut password = use_signal(String::new);
    let mut confirm = use_signal(String::new);
    let mut err = use_signal(|| None::<String>);

    let mut submit = move || {
        let pw = password.read().clone();
        let cf = confirm.read().clone();
        if pw.is_empty() {
            err.set(Some("password required".into()));
            return;
        }
        if !exists && pw != cf {
            err.set(Some("passwords don't match".into()));
            return;
        }
        spawn(async move {
            match api::vault_unlock(&pw).await {
                Ok(_) => on_unlocked.call(()),
                Err(e) => err.set(Some(e)),
            }
        });
    };

    rsx! {
        div { class: "modal-bg",
            div { class: "modal",
                h3 { if exists { "unlock vault" } else { "create vault" } }
                div { class: "muted small",
                    if exists {
                        "enter master password to decrypt secrets"
                    } else {
                        "no vault yet — set a master password. cannot be recovered if lost."
                    }
                }
                label { "password" }
                input {
                    autofocus: true,
                    r#type: "password",
                    value: "{password}",
                    oninput: move |e| password.set(e.value()),
                    onkeydown: move |e| {
                        if e.key() == Key::Enter && exists { submit(); }
                    },
                }
                if !exists {
                    label { "confirm" }
                    input {
                        r#type: "password",
                        value: "{confirm}",
                        oninput: move |e| confirm.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter { submit(); }
                        },
                    }
                }
                if let Some(e) = err.read().as_ref() { div { class: "error", "{e}" } }
                div { class: "modal-actions",
                    button { class: "btn primary", onclick: move |_| submit(),
                        if exists { "unlock" } else { "create" }
                    }
                }
            }
        }
    }
}

#[component]
fn ReqRow(
    id: String,
    method: String,
    name: String,
    rel_path: String,
    selected: bool,
    mut selected_req: Signal<Option<String>>,
) -> Element {
    let folder = std::path::Path::new(&rel_path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .to_string();
    let method_lower = method.to_lowercase();
    let id_click = id.clone();
    rsx! {
        div {
            class: if selected { "row selected" } else { "row" },
            onclick: move |_| selected_req.set(Some(id_click.clone())),
            span { class: "method method-{method_lower}", "{method}" }
            span { class: "row-title", "{name}" }
            span { class: "row-meta", "{folder}" }
        }
    }
}

#[component]
fn NewEnvModal(on_close: EventHandler<()>, envs: Vec<Env>) -> Element {
    let mut name = use_signal(String::new);
    let mut template = use_signal(|| envs.first().map(|e| e.name.clone()));
    let mut err = use_signal(|| None::<String>);

    rsx! {
        div { class: "modal-bg",
            div { class: "modal",
                h3 { "new env" }
                label { "name" }
                input {
                    autofocus: true,
                    value: "{name}",
                    oninput: move |e| name.set(e.value()),
                }
                label { "template (copy vars from)" }
                select {
                    onchange: move |e| {
                        let v = e.value();
                        template.set(if v.is_empty() { None } else { Some(v) });
                    },
                    option { value: "", "— none —" }
                    for env in envs.iter() {
                        option {
                            value: "{env.name}",
                            selected: template.read().as_deref() == Some(env.name.as_str()),
                            "{env.name}"
                        }
                    }
                }
                if let Some(e) = err.read().as_ref() { div { class: "error", "{e}" } }
                div { class: "modal-actions",
                    button { class: "btn", onclick: move |_| on_close.call(()), "cancel" }
                    button {
                        class: "btn primary",
                        onclick: move |_| {
                            let n = name.read().clone();
                            let t = template.read().clone();
                            if n.trim().is_empty() { err.set(Some("name required".into())); return; }
                            spawn(async move {
                                match api::create_env(&n, t.as_deref()).await {
                                    Ok(_) => on_close.call(()),
                                    Err(e) => err.set(Some(e)),
                                }
                            });
                        },
                        "create"
                    }
                }
            }
        }
    }
}

#[component]
fn EditEnvModal(env: Env, on_close: EventHandler<()>) -> Element {
    let mut vars = use_signal(|| env.variables.clone());
    let mut secret_values = use_signal(std::collections::HashMap::<String, String>::new);
    let mut err = use_signal(|| None::<String>);
    let mut new_name = use_signal(|| env.name.clone());
    let env_name = env.name.clone();
    let env_path = env.path.clone();

    let en = env_name.clone();
    use_effect(move || {
        let en = en.clone();
        let vs = vars.read().clone();
        spawn(async move {
            let mut map = std::collections::HashMap::new();
            for v in vs.iter().filter(|v| v.secret) {
                if let Ok(Some(s)) = api::get_secret(&en, &v.name).await {
                    map.insert(v.name.clone(), s);
                }
            }
            secret_values.set(map);
        });
    });

    rsx! {
        div { class: "modal-bg",
            div { class: "modal wide",
                h3 { "edit env" }
                label { "name" }
                input {
                    value: "{new_name}",
                    oninput: move |e| new_name.set(e.value()),
                }
                div { class: "muted small", "{env_path}" }
                table { class: "vars",
                    thead { tr {
                        th { "name" } th { "value" } th { "secret" } th { "" }
                    } }
                    tbody {
                        for (idx, v) in vars.read().iter().enumerate() {
                            {
                                let vname = v.name.clone();
                                let is_secret = v.secret;
                                let value = if is_secret {
                                    secret_values.read().get(&vname).cloned().unwrap_or_default()
                                } else {
                                    v.value.clone()
                                };
                                rsx! {
                                    tr { key: "{idx}",
                                        td {
                                            input {
                                                value: "{v.name}",
                                                oninput: move |e| {
                                                    let mut cur = vars.read().clone();
                                                    cur[idx].name = e.value();
                                                    vars.set(cur);
                                                },
                                            }
                                        }
                                        td {
                                            input {
                                                r#type: if is_secret { "password" } else { "text" },
                                                value: "{value}",
                                                oninput: move |e| {
                                                    let val = e.value();
                                                    if is_secret {
                                                        let mut m = secret_values.read().clone();
                                                        m.insert(vname.clone(), val);
                                                        secret_values.set(m);
                                                    } else {
                                                        let mut cur = vars.read().clone();
                                                        cur[idx].value = val;
                                                        vars.set(cur);
                                                    }
                                                },
                                            }
                                        }
                                        td {
                                            input {
                                                r#type: "checkbox",
                                                checked: is_secret,
                                                onchange: move |e| {
                                                    let mut cur = vars.read().clone();
                                                    let now_secret = e.checked();
                                                    let vname = cur[idx].name.clone();
                                                    if now_secret {
                                                        // moving plain → secret: preserve the value
                                                        // by seeding it into the secret_values map,
                                                        // then clear the on-disk value field.
                                                        let existing = cur[idx].value.clone();
                                                        if !existing.is_empty() {
                                                            let mut m = secret_values.read().clone();
                                                            m.insert(vname, existing);
                                                            secret_values.set(m);
                                                        }
                                                        cur[idx].value = String::new();
                                                    } else {
                                                        // moving secret → plain: hoist the current
                                                        // secret value (if any) back into the on-disk
                                                        // field, and drop it from the secret map.
                                                        let mut m = secret_values.read().clone();
                                                        if let Some(v) = m.remove(&vname) {
                                                            cur[idx].value = v;
                                                        }
                                                        secret_values.set(m);
                                                    }
                                                    cur[idx].secret = now_secret;
                                                    vars.set(cur);
                                                },
                                            }
                                        }
                                        td {
                                            button {
                                                class: "btn small danger",
                                                onclick: move |_| {
                                                    let mut cur = vars.read().clone();
                                                    cur.remove(idx);
                                                    vars.set(cur);
                                                },
                                                "×"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                button {
                    class: "btn small",
                    onclick: move |_| {
                        let mut cur = vars.read().clone();
                        cur.push(EnvVar::default());
                        vars.set(cur);
                    },
                    "+ add var"
                }

                if let Some(e) = err.read().as_ref() { div { class: "error", "{e}" } }
                div { class: "modal-actions",
                    button { class: "btn", onclick: move |_| on_close.call(()), "cancel" }
                    button {
                        class: "btn primary",
                        onclick: move |_| {
                            let old_name = env_name.clone();
                            let target_name = new_name.read().trim().to_string();
                            let vs = vars.read().clone();
                            let secrets = secret_values.read().clone();
                            let path = env_path.clone();
                            spawn(async move {
                                if target_name.is_empty() {
                                    err.set(Some("name required".into()));
                                    return;
                                }
                                let (final_name, final_path) = if target_name != old_name {
                                    match api::rename_env(&old_name, &target_name).await {
                                        Ok(renamed) => (renamed.name, renamed.path),
                                        Err(e) => { err.set(Some(format!("rename: {}", e))); return; }
                                    }
                                } else {
                                    (old_name.clone(), path.clone())
                                };
                                let stripped: Vec<EnvVar> = vs.iter().map(|v| EnvVar {
                                    name: v.name.clone(),
                                    value: if v.secret { String::new() } else { v.value.clone() },
                                    secret: v.secret,
                                }).collect();
                                let env = Env { name: final_name.clone(), variables: stripped.clone(), path: final_path };
                                if let Err(e) = api::save_env(&env).await {
                                    err.set(Some(e));
                                    return;
                                }
                                for v in stripped.iter().filter(|v| v.secret) {
                                    if let Some(val) = secrets.get(&v.name) {
                                        if val.is_empty() {
                                            let _ = api::delete_secret(&final_name, &v.name).await;
                                        } else if let Err(e) = api::set_secret(&final_name, &v.name, val).await {
                                            err.set(Some(format!("secret {}: {}", v.name, e)));
                                            return;
                                        }
                                    }
                                }
                                on_close.call(());
                            });
                        },
                        "save"
                    }
                }
            }
        }
    }
}

#[component]
fn NewRequestModal(default_folder: String, on_close: EventHandler<()>) -> Element {
    let mut name = use_signal(String::new);
    let mut method = use_signal(|| "GET".to_string());
    let mut url = use_signal(String::new);
    let mut folder = use_signal(|| default_folder);
    let mut err = use_signal(|| None::<String>);

    rsx! {
        div { class: "modal-bg",
            div { class: "modal",
                h3 { "new request" }
                label { "name" }
                input {
                    autofocus: true,
                    value: "{name}",
                    oninput: move |e| name.set(e.value()),
                }
                label { "method" }
                select {
                    onchange: move |e| method.set(e.value()),
                    for m in ["GET","POST","PUT","PATCH","DELETE","HEAD","OPTIONS"].iter() {
                        option { value: "{m}", selected: method.read().as_str() == *m, "{m}" }
                    }
                }
                label { "url" }
                input {
                    value: "{url}",
                    placeholder: "{{baseUrl}}/path",
                    oninput: move |e| url.set(e.value()),
                }
                label { "folder (relative to collection root)" }
                input {
                    value: "{folder}",
                    oninput: move |e| folder.set(e.value()),
                }
                if let Some(e) = err.read().as_ref() { div { class: "error", "{e}" } }
                div { class: "modal-actions",
                    button { class: "btn", onclick: move |_| on_close.call(()), "cancel" }
                    button {
                        class: "btn primary",
                        onclick: move |_| {
                            let f = folder.read().clone();
                            let n = name.read().clone();
                            let m = method.read().clone();
                            let u = url.read().clone();
                            if n.trim().is_empty() { err.set(Some("name required".into())); return; }
                            if f.trim().is_empty() { err.set(Some("folder required".into())); return; }
                            spawn(async move {
                                match api::create_request(&f, &n, &m, &u).await {
                                    Ok(_) => on_close.call(()),
                                    Err(e) => err.set(Some(e)),
                                }
                            });
                        },
                        "create"
                    }
                }
            }
        }
    }
}

#[component]
fn ImportCurlModal(default_folder: String, on_close: EventHandler<()>) -> Element {
    let mut input = use_signal(String::new);
    let mut name = use_signal(String::new);
    let mut folder = use_signal(|| default_folder);
    let mut preview = use_signal(|| None::<Request>);
    let mut err = use_signal(|| None::<String>);

    let do_parse = move |_| {
        let raw = input.read().clone();
        if raw.trim().is_empty() {
            err.set(Some("paste a curl command".into()));
            return;
        }
        spawn(async move {
            match api::parse_curl(&raw).await {
                Ok(req) => {
                    preview.set(Some(req));
                    err.set(None);
                }
                Err(e) => {
                    preview.set(None);
                    err.set(Some(e));
                }
            }
        });
    };

    let do_import = move |_| {
        let raw = input.read().clone();
        let n = name.read().clone();
        let f = folder.read().clone();
        if n.trim().is_empty() {
            err.set(Some("name required".into()));
            return;
        }
        if f.trim().is_empty() {
            err.set(Some("folder required".into()));
            return;
        }
        spawn(async move {
            match api::import_curl(&raw, &f, &n).await {
                Ok(_) => on_close.call(()),
                Err(e) => err.set(Some(e)),
            }
        });
    };

    rsx! {
        div { class: "modal-bg",
            div { class: "modal wide",
                h3 { "import curl" }
                label { "curl command" }
                textarea {
                    class: "body-text",
                    style: "min-height: 140px",
                    spellcheck: false,
                    value: "{input}",
                    oninput: move |e| input.set(e.value()),
                    placeholder: "curl 'https://api.example.com/foo' -H 'Authorization: Bearer …' -d '{{\"key\":\"value\"}}'"
                }
                div { class: "modal-actions", style: "justify-content: flex-start",
                    button { class: "btn small", onclick: do_parse, "parse" }
                }
                if let Some(r) = preview.read().as_ref() {
                    div { class: "muted small",
                        "preview: {r.http.method} {r.http.url}  ·  {r.http.headers.len()} headers"
                    }
                }
                label { "name" }
                input {
                    value: "{name}",
                    oninput: move |e| name.set(e.value()),
                    placeholder: "my request"
                }
                label { "folder (relative to collection root)" }
                input {
                    value: "{folder}",
                    oninput: move |e| folder.set(e.value()),
                }
                if let Some(e) = err.read().as_ref() { div { class: "error", "{e}" } }
                div { class: "modal-actions",
                    button { class: "btn", onclick: move |_| on_close.call(()), "cancel" }
                    button { class: "btn primary", onclick: do_import, "import" }
                }
            }
        }
    }
}

#[component]
fn ImportOpenApiModal(on_close: EventHandler<()>) -> Element {
    let mut spec_path = use_signal(String::new);
    let mut folder_prefix = use_signal(|| "openapi".to_string());
    let mut env_name = use_signal(String::new);
    let mut create_env = use_signal(|| true);
    let mut preview = use_signal(|| None::<ImportPreview>);
    let mut err = use_signal(|| None::<String>);
    let mut busy = use_signal(|| false);

    let do_preview = move |_| {
        let p = spec_path.read().clone();
        if p.trim().is_empty() {
            err.set(Some("spec path required".into()));
            return;
        }
        busy.set(true);
        spawn(async move {
            match api::preview_openapi(&p).await {
                Ok(pv) => {
                    if env_name.peek().is_empty() {
                        env_name.set(slug_env(&pv.title));
                    }
                    preview.set(Some(pv));
                    err.set(None);
                }
                Err(e) => {
                    preview.set(None);
                    err.set(Some(e));
                }
            }
            busy.set(false);
        });
    };

    let do_import = move |_| {
        let p = spec_path.read().clone();
        let fp = folder_prefix.read().clone();
        let ce = *create_env.read();
        let en = env_name.read().clone();
        if preview.peek().is_none() {
            err.set(Some("preview first".into()));
            return;
        }
        busy.set(true);
        spawn(async move {
            match api::import_openapi(&p, &fp, ce, &en).await {
                Ok(_) => on_close.call(()),
                Err(e) => err.set(Some(e)),
            }
            busy.set(false);
        });
    };

    rsx! {
        div { class: "modal-bg",
            div { class: "modal wide",
                h3 { "import openapi spec" }
                label { "spec path (yaml or json, absolute)" }
                input {
                    autofocus: true,
                    value: "{spec_path}",
                    oninput: move |e| spec_path.set(e.value()),
                    placeholder: "/path/to/openapi.yaml"
                }
                div { class: "modal-actions", style: "justify-content: flex-start; margin-top: 8px;",
                    button {
                        class: if *busy.read() { "btn small disabled" } else { "btn small" },
                        disabled: *busy.read(),
                        onclick: do_preview,
                        "preview"
                    }
                }
                if let Some(pv) = preview.read().as_ref() {
                    div { class: "muted small",
                        "{pv.title} v{pv.version}  ·  {pv.op_count} ops across {pv.folder_count} folders"
                    }
                    if !pv.suggested_vars.is_empty() {
                        div { class: "muted small",
                            "vars: "
                            for (i, v) in pv.suggested_vars.iter().enumerate() {
                                if i > 0 { ", " }
                                span { "{v.name}" }
                                if v.secret { " (secret)" }
                            }
                        }
                    }
                    if !pv.sample_ops.is_empty() {
                        details { class: "headers",
                            summary { "sample operations ({pv.sample_ops.len()})" }
                            for op in pv.sample_ops.iter() {
                                div { class: "hdr",
                                    span { class: "hdr-k", "{op.method} {op.folder}" }
                                    span { class: "hdr-v", "{op.path}" }
                                }
                            }
                        }
                    }
                }

                label { "folder prefix (relative to collection root)" }
                input {
                    value: "{folder_prefix}",
                    oninput: move |e| folder_prefix.set(e.value()),
                    placeholder: "openapi"
                }

                div { style: "display: flex; align-items: center; gap: 8px; margin-top: 12px;",
                    input {
                        r#type: "checkbox",
                        checked: *create_env.read(),
                        onchange: move |e| create_env.set(e.checked()),
                    }
                    span { class: "small", "create env with suggested vars" }
                }
                if *create_env.read() {
                    label { "env name" }
                    input {
                        value: "{env_name}",
                        oninput: move |e| env_name.set(e.value()),
                    }
                }

                if let Some(e) = err.read().as_ref() { div { class: "error", "{e}" } }
                div { class: "modal-actions",
                    button { class: "btn", onclick: move |_| on_close.call(()), "cancel" }
                    button {
                        class: if *busy.read() { "btn primary disabled" } else { "btn primary" },
                        disabled: *busy.read(),
                        onclick: do_import,
                        if *busy.read() { "importing…" } else { "import" }
                    }
                }
            }
        }
    }
}

fn slug_env(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    Params,
    Headers,
    Body,
}

#[component]
fn RequestEditor(
    req: Request,
    firing: bool,
    current_env_name: Option<String>,
    on_fire: EventHandler<()>,
    on_saved: EventHandler<()>,
    on_toast: EventHandler<String>,
) -> Element {
    let mut method = use_signal(|| req.http.method.clone());
    let (init_url, init_extra_params) = split_url_and_params(&req.http.url);
    let mut url = use_signal(|| init_url);
    let headers = use_signal(|| req.http.headers.clone());
    let mut params = use_signal(|| {
        // Merge params from URL query into existing params tab.
        // URL-derived rows come first; skip any whose key already appears
        // in req.http.params to avoid duplicates (Bruno stores params in
        // both the url string and the params[] array for the same key).
        let existing_keys: std::collections::HashSet<&str> =
            req.http.params.iter().map(|p| p.name.as_str()).collect();
        let mut merged: Vec<KV> = init_extra_params
            .into_iter()
            .filter(|p| !existing_keys.contains(p.name.as_str()))
            .collect();
        for p in &req.http.params {
            merged.push(p.clone());
        }
        merged
    });
    let mut body_type = use_signal(|| req.http.body.kind.clone().unwrap_or_default());
    let mut body_text = use_signal(|| {
        // Bruno stores body under `data`; older hitr saves used `json`/`text`.
        // Prefer whichever is non-empty.
        let b = &req.http.body;
        let candidates = match body_type.peek().as_str() {
            "json" => [&b.json, &b.data, &b.text],
            "text" => [&b.text, &b.data, &b.json],
            _ => [&b.data, &b.json, &b.text],
        };
        candidates
            .iter()
            .find_map(|f| f.as_ref().filter(|s| !s.is_empty()).cloned())
            .unwrap_or_default()
    });
    let mut tab = use_signal(|| EditorTab::Body);
    let mut save_err = use_signal(|| None::<String>);
    // Bumped on every user edit. The debounced task snapshots the value on
    // spawn and re-checks after sleeping — if it changed, another edit
    // arrived and this task's save is stale, so it bails.
    let mut save_gen = use_signal(|| 0usize);
    // "saving..." indicator, on when a save is in flight.
    let mut saving = use_signal(|| false);

    let orig = req.clone();
    let mut bump = move || {
        let n = *save_gen.peek() + 1;
        save_gen.set(n);
    };

    // Autosave: react to save_gen. Wait 500ms. If save_gen changed during the
    // wait, another edit came in — do nothing (that later task will save).
    // Skip the very first fire (component mount, no user edit).
    let orig_for_effect = orig.clone();
    use_effect(move || {
        let stamp = *save_gen.read();
        if stamp == 0 {
            return;
        }
        let orig = orig_for_effect.clone();
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(500).await;
            if *save_gen.peek() != stamp {
                return;
            }
            let mut updated = orig;
            updated.http.method = method.peek().clone();
            updated.http.url = url.peek().clone();
            updated.http.headers = headers.peek().clone();
            updated.http.params = params.peek().clone();
            let bt = body_type.peek().clone();
            let bx = body_text.peek().clone();
            updated.http.body = Body {
                kind: if bt.is_empty() {
                    None
                } else {
                    Some(bt.clone())
                },
                json: None,
                text: None,
                data: if !bt.is_empty() && !bx.is_empty() {
                    Some(bx.clone())
                } else {
                    None
                },
            };
            saving.set(true);
            match api::save_request(&updated).await {
                Ok(_) => {
                    save_err.set(None);
                    on_saved.call(());
                }
                Err(e) => save_err.set(Some(e)),
            }
            saving.set(false);
        });
    });

    let mut name_input = use_signal(|| req.info.name.clone());
    let mut rename_pending = use_signal(|| false);
    let mut rename_err = use_signal(|| None::<String>);
    let orig_id = req.id.clone();

    rsx! {
        div { class: "req-editor",
            div { class: "req-line",
                input {
                    class: "name-input",
                    value: "{name_input}",
                    placeholder: "request name",
                    oninput: move |e| {
                        name_input.set(e.value());
                        rename_pending.set(true);
                    },
                    onblur: move |_| {
                        if !*rename_pending.peek() { return; }
                        let rid = orig_id.clone();
                        let new_name = name_input.read().trim().to_string();
                        if new_name.is_empty() {
                            rename_err.set(Some("name required".into()));
                            return;
                        }
                        rename_pending.set(false);
                        spawn(async move {
                            match api::rename_request(&rid, &new_name).await {
                                Ok(_) => { rename_err.set(None); on_saved.call(()); }
                                Err(e) => rename_err.set(Some(e)),
                            }
                        });
                    },
                }
            }
            if let Some(e) = rename_err.read().as_ref() {
                div { class: "error small", "{e}" }
            }
            div { class: "req-line",
                select {
                    class: "method-select method-{method.read().to_lowercase()}",
                    onchange: move |e| { method.set(e.value()); bump(); },
                    for m in ["GET","POST","PUT","PATCH","DELETE","HEAD","OPTIONS"].iter() {
                        option { value: "{m}", selected: method.read().as_str() == *m, "{m}" }
                    }
                }
                input {
                    class: "url-input",
                    value: "{url}",
                    oninput: move |e| {
                        let v = e.value();
                        // Auto-split query pairs into params tab as user types
                        // — but only when the input actually contains `?`, so
                        // templates like `{{baseUrl}}/path` don't get chopped.
                        if v.contains('?') {
                            let (base, extras) = split_url_and_params(&v);
                            let mut cur = params.read().clone();
                            let existing: std::collections::HashSet<String> =
                                cur.iter().map(|p| p.name.clone()).collect();
                            for e in extras {
                                if !existing.contains(&e.name) { cur.push(e); }
                            }
                            params.set(cur);
                            url.set(base);
                        } else {
                            url.set(v);
                        }
                        bump();
                    },
                    placeholder: "{{baseUrl}}/path"
                }
                if *saving.read() {
                    span { class: "muted small", "saving…" }
                }
                {
                    let rid = orig.id.clone();
                    let rid2 = orig.id.clone();
                    let orig_del = orig.clone();
                    let env_for_curl = current_env_name.clone();
                    rsx! {
                        button {
                            class: "btn icon",
                            title: "duplicate request",
                            onclick: move |_| {
                                let rid = rid.clone();
                                spawn(async move {
                                    if api::duplicate_request(&rid).await.is_ok() {
                                        on_toast.call("request duplicated".into());
                                        on_saved.call(());
                                    } else {
                                        on_toast.call("duplicate failed".into());
                                    }
                                });
                            },
                            "⧉"
                        }
                        button {
                            class: "btn icon wide",
                            title: "copy as curl",
                            onclick: move |_| {
                                let rid = rid2.clone();
                                let env = env_for_curl.clone();
                                spawn(async move {
                                    if let Ok(s) = api::to_curl(&rid, env.as_deref()).await {
                                        copy_to_clipboard(&s);
                                        on_toast.call("curl copied".into());
                                    }
                                });
                            },
                            "</>"
                        }
                        button {
                            class: "btn icon danger",
                            title: "delete request",
                            onclick: move |_| {
                                let r = orig_del.clone();
                                spawn(async move {
                                    if api::delete_request(&r).await.is_ok() {
                                        on_saved.call(());
                                    }
                                });
                            },
                            "🗑"
                        }
                    }
                }
                button {
                    class: if firing { "btn primary disabled" } else { "btn primary" },
                    disabled: firing,
                    onclick: move |_| on_fire.call(()),
                    if firing { "…" } else { "send" }
                }
            }
            div { class: "req-tabs",
                {
                    let cur = *tab.read();
                    rsx! {
                        button {
                            class: if cur == EditorTab::Params { "tab active" } else { "tab" },
                            onclick: move |_| tab.set(EditorTab::Params),
                            "params ({params.read().len()})"
                        }
                        button {
                            class: if cur == EditorTab::Headers { "tab active" } else { "tab" },
                            onclick: move |_| tab.set(EditorTab::Headers),
                            "headers ({headers.read().len()})"
                        }
                        button {
                            class: if cur == EditorTab::Body { "tab active" } else { "tab" },
                            onclick: move |_| tab.set(EditorTab::Body),
                            "body"
                        }
                    }
                }
            }
            div { class: "req-tab-body",
                match *tab.read() {
                    EditorTab::Params => rsx! { KVEditor { items: params, on_change: move |_| bump() } },
                    EditorTab::Headers => rsx! { KVEditor { items: headers, on_change: move |_| bump() } },
                    EditorTab::Body => rsx! {
                        div { class: "body-editor",
                            div { class: "body-type-row",
                                span { class: "muted small", "type" }
                                for t in ["none","json","text"].iter() {
                                    button {
                                        class: if body_type.read().as_str() == *t || (t == &"none" && body_type.read().is_empty()) { "tab active" } else { "tab" },
                                        onclick: move |_| {
                                            body_type.set(if *t == "none" { String::new() } else { t.to_string() });
                                            bump();
                                        },
                                        "{t}"
                                    }
                                }
                                if body_type.read().as_str() == "json" {
                                    button {
                                        class: "btn small ghost",
                                        onclick: move |_| {
                                            let cur = body_text.read().clone();
                                            match serde_json::from_str::<serde_json::Value>(&cur) {
                                                Ok(v) => {
                                                    if let Ok(pretty) = serde_json::to_string_pretty(&v) {
                                                        body_text.set(pretty);
                                                        bump();
                                                    }
                                                }
                                                Err(_) => save_err.set(Some("invalid JSON".into())),
                                            }
                                        },
                                        "format"
                                    }
                                }
                            }
                            if !body_type.read().is_empty() {
                                textarea {
                                    class: "body-text",
                                    spellcheck: false,
                                    value: "{body_text}",
                                    oninput: move |e| { body_text.set(e.value()); bump(); },
                                    onkeydown: move |e| {
                                        if (e.modifiers().meta() || e.modifiers().ctrl()) && e.key() == Key::Character("/".into()) {
                                            e.prevent_default();
                                            if let Some(win) = web_sys::window()
                                                && let Some(doc) = win.document()
                                                    && let Some(active) = doc.active_element()
                                                        && let Ok(ta) = active.dyn_into::<web_sys::HtmlTextAreaElement>() {
                                                            let val = ta.value();
                                                            let s = ta.selection_start().ok().flatten().unwrap_or(0) as usize;
                                                            let en = ta.selection_end().ok().flatten().unwrap_or(0) as usize;
                                                            let (new_val, ns, ne) = toggle_line_comment(&val, s, en);
                                                            ta.set_value(&new_val);
                                                            let _ = ta.set_selection_start(Some(ns as u32));
                                                            let _ = ta.set_selection_end(Some(ne as u32));
                                                            body_text.set(new_val);
                                                            bump();
                                                        }
                                        }
                                    },
                                    placeholder: "request body… (⌘/ to comment)"
                                }
                            } else {
                                div { class: "muted center", "no body" }
                            }
                        }
                    }
                }
            }
            if let Some(e) = save_err.read().as_ref() {
                div { class: "error small", "{e}" }
            }
        }
    }
}

#[component]
fn KVEditor(items: Signal<Vec<KV>>, on_change: EventHandler<()>) -> Element {
    rsx! {
        div { class: "kv-editor",
            table { class: "vars",
                thead { tr {
                    th { "" } th { "name" } th { "value" } th { "" }
                } }
                tbody {
                    for (idx, item) in items.read().iter().enumerate() {
                        {
                            let enabled = item.enabled.unwrap_or(true);
                            let name = item.name.clone();
                            let value = item.value.clone();
                            rsx! {
                                tr { key: "{idx}",
                                    td {
                                        input {
                                            r#type: "checkbox",
                                            checked: enabled,
                                            onchange: move |e| {
                                                let mut cur = items.read().clone();
                                                cur[idx].enabled = Some(e.checked());
                                                items.set(cur);
                                                on_change.call(());
                                            },
                                        }
                                    }
                                    td {
                                        input {
                                            value: "{name}",
                                            oninput: move |e| {
                                                let mut cur = items.read().clone();
                                                cur[idx].name = e.value();
                                                items.set(cur);
                                                on_change.call(());
                                            },
                                        }
                                    }
                                    td {
                                        input {
                                            value: "{value}",
                                            oninput: move |e| {
                                                let mut cur = items.read().clone();
                                                cur[idx].value = e.value();
                                                items.set(cur);
                                                on_change.call(());
                                            },
                                        }
                                    }
                                    td {
                                        button {
                                            class: "btn small danger",
                                            onclick: move |_| {
                                                let mut cur = items.read().clone();
                                                cur.remove(idx);
                                                items.set(cur);
                                                on_change.call(());
                                            },
                                            "×"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            button {
                class: "btn small",
                onclick: move |_| {
                    let mut cur = items.read().clone();
                    cur.push(KV::default());
                    items.set(cur);
                    on_change.call(());
                },
                "+ add"
            }
        }
    }
}

// -----------------------------------------------------------------------------
// JSON tree — virtual scroll edition.
//
// Instead of recursing into components per node, we flatten the JSON into a
// Vec<Line> once. Each Line is one visible row. The renderer keeps track of
// scroll position + viewport height, then mounts only the ~40 rows that fit
// on screen (plus a small overscan buffer). Collapsing a node hides its
// subtree by filtering the flat index list, which is O(n) once — not a re-diff
// of thousands of components.
// -----------------------------------------------------------------------------

const ROW_HEIGHT: f64 = 20.0;
const OVERSCAN: usize = 8;

#[derive(Clone, PartialEq)]
enum LineKind {
    /// Object/array opening bracket. `id` is the node id; `count` is # children.
    Open {
        id: usize,
        is_array: bool,
        count: usize,
        trailing_comma: bool,
    },
    /// Closing bracket. `id` matches the Open. `trailing_comma` = need "," after.
    Close {
        id: usize,
        is_array: bool,
        trailing_comma: bool,
    },
    /// A leaf key/value line inside an object.
    Kv {
        key: Option<String>,
        value: String,
        class: &'static str,
        trailing_comma: bool,
    },
    /// A leaf value line inside an array.
    Item {
        value: String,
        class: &'static str,
        trailing_comma: bool,
    },
}

#[derive(Clone, PartialEq)]
struct Line {
    depth: usize,
    /// Chain of parent node ids from root to this line. Used to test collapse.
    ancestors: Vec<usize>,
    kind: LineKind,
}

/// Flatten a JSON value into a linear sequence of visible lines.
fn flatten(v: &serde_json::Value) -> Vec<Line> {
    let mut out = Vec::new();
    let mut next_id = 0usize;
    walk(v, 0, &[], None, true, &mut next_id, &mut out);
    out
}

fn walk(
    v: &serde_json::Value,
    depth: usize,
    ancestors: &[usize],
    key: Option<&str>,
    is_last: bool,
    next_id: &mut usize,
    out: &mut Vec<Line>,
) {
    let key_str = key.map(|k| serde_json::to_string(k).unwrap_or_else(|_| format!("\"{}\"", k)));
    let trailing = !is_last;
    match v {
        serde_json::Value::Object(obj) => {
            let id = *next_id;
            *next_id += 1;
            let mut new_ancestors = ancestors.to_vec();
            out.push(Line {
                depth,
                ancestors: new_ancestors.clone(),
                kind: LineKind::Open {
                    id,
                    is_array: false,
                    count: obj.len(),
                    trailing_comma: false,
                },
            });
            // key label for the opening line lives inside Open render via `key_str`
            // but we render key separately in the row builder — encode it as an
            // Item-shaped extension? Simpler: prepend key by putting it in the
            // Kv line via a virtual approach. We instead flatten by rendering
            // "key: {" on the same line as the open; do that in build_row.
            // For that we need to stash the key on the Open — extend LineKind:
            //   ...already handled at row build time via ancestors trail.
            // Simpler: just push a Kv-style line marking open. Redo:
            //   pop the just-pushed Open and replace with a proper compound line.
            let _ = key_str.clone(); // key label handled in build_row via prev line lookup? too complex.
            new_ancestors.push(id);
            let entries: Vec<(&String, &serde_json::Value)> = obj.iter().collect();
            let n = entries.len();
            for (i, (k, val)) in entries.iter().enumerate() {
                walk(
                    val,
                    depth + 1,
                    &new_ancestors,
                    Some(k.as_str()),
                    i + 1 == n,
                    next_id,
                    out,
                );
            }
            out.push(Line {
                depth,
                ancestors: ancestors.to_vec(),
                kind: LineKind::Close {
                    id,
                    is_array: false,
                    trailing_comma: trailing,
                },
            });
        }
        serde_json::Value::Array(arr) => {
            let id = *next_id;
            *next_id += 1;
            let mut new_ancestors = ancestors.to_vec();
            out.push(Line {
                depth,
                ancestors: new_ancestors.clone(),
                kind: LineKind::Open {
                    id,
                    is_array: true,
                    count: arr.len(),
                    trailing_comma: false,
                },
            });
            new_ancestors.push(id);
            let n = arr.len();
            for (i, item) in arr.iter().enumerate() {
                walk(
                    item,
                    depth + 1,
                    &new_ancestors,
                    None,
                    i + 1 == n,
                    next_id,
                    out,
                );
            }
            out.push(Line {
                depth,
                ancestors: ancestors.to_vec(),
                kind: LineKind::Close {
                    id,
                    is_array: true,
                    trailing_comma: trailing,
                },
            });
        }
        serde_json::Value::Null => {
            out.push(Line {
                depth,
                ancestors: ancestors.to_vec(),
                kind: match key_str {
                    Some(k) => LineKind::Kv {
                        key: Some(k),
                        value: "null".into(),
                        class: "j-null",
                        trailing_comma: trailing,
                    },
                    None => LineKind::Item {
                        value: "null".into(),
                        class: "j-null",
                        trailing_comma: trailing,
                    },
                },
            });
        }
        serde_json::Value::Bool(b) => {
            let val = b.to_string();
            out.push(Line {
                depth,
                ancestors: ancestors.to_vec(),
                kind: match key_str {
                    Some(k) => LineKind::Kv {
                        key: Some(k),
                        value: val,
                        class: "j-bool",
                        trailing_comma: trailing,
                    },
                    None => LineKind::Item {
                        value: val,
                        class: "j-bool",
                        trailing_comma: trailing,
                    },
                },
            });
        }
        serde_json::Value::Number(n) => {
            let val = n.to_string();
            out.push(Line {
                depth,
                ancestors: ancestors.to_vec(),
                kind: match key_str {
                    Some(k) => LineKind::Kv {
                        key: Some(k),
                        value: val,
                        class: "j-num",
                        trailing_comma: trailing,
                    },
                    None => LineKind::Item {
                        value: val,
                        class: "j-num",
                        trailing_comma: trailing,
                    },
                },
            });
        }
        serde_json::Value::String(s) => {
            let val = serde_json::to_string(s).unwrap_or_else(|_| format!("\"{}\"", s));
            out.push(Line {
                depth,
                ancestors: ancestors.to_vec(),
                kind: match key_str {
                    Some(k) => LineKind::Kv {
                        key: Some(k),
                        value: val,
                        class: "j-str",
                        trailing_comma: trailing,
                    },
                    None => LineKind::Item {
                        value: val,
                        class: "j-str",
                        trailing_comma: trailing,
                    },
                },
            });
        }
    }
}

/// Compute the list of line indices visible under the current `collapsed` set.
/// A line is visible unless any of its ancestor ids is in collapsed. Skips
/// entire subtrees efficiently by tracking a hide-until-close counter.
fn line_matches(line: &Line, needle_lower: &str) -> bool {
    match &line.kind {
        LineKind::Kv { key, value, .. } => {
            key.as_deref()
                .map(|k| k.to_lowercase().contains(needle_lower))
                .unwrap_or(false)
                || value.to_lowercase().contains(needle_lower)
        }
        LineKind::Item { value, .. } => value.to_lowercase().contains(needle_lower),
        _ => false,
    }
}

fn compute_visible(
    lines: &[Line],
    collapsed: &std::collections::HashSet<usize>,
    query: &str,
) -> Vec<usize> {
    // Search mode: find matching lines, force-expand their ancestor chain, keep
    // only matches + their Open/Close bracket rows for context.
    if !query.is_empty() {
        let needle = query.to_lowercase();
        let mut keep_ancestors: std::collections::HashSet<usize> = Default::default();
        let mut matched_indices: Vec<usize> = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            if line_matches(line, &needle) {
                matched_indices.push(i);
                for a in &line.ancestors {
                    keep_ancestors.insert(*a);
                }
            }
        }
        // Include matched lines + Open/Close rows whose id is a kept ancestor
        // (so structure remains visible around matches).
        let mut out: Vec<usize> = Vec::with_capacity(matched_indices.len() * 3);
        for (i, line) in lines.iter().enumerate() {
            let keep = matched_indices.binary_search(&i).is_ok()
                || match &line.kind {
                    LineKind::Open { id, .. } | LineKind::Close { id, .. } => {
                        keep_ancestors.contains(id)
                    }
                    _ => false,
                };
            if keep {
                out.push(i);
            }
        }
        return out;
    }

    let mut out = Vec::with_capacity(lines.len());
    let mut hide_ids: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        // Are we inside a collapsed subtree? If any of this line's ancestors
        // is currently in the hide stack, skip. Except the Open line of a
        // collapsed node itself, which stays visible so user can toggle it.
        let inside_hidden = hide_ids.iter().any(|h| line.ancestors.contains(h));
        if !inside_hidden {
            out.push(i);
        }
        match &line.kind {
            LineKind::Open { id, .. } => {
                if collapsed.contains(id) {
                    hide_ids.push(*id);
                }
            }
            LineKind::Close { id, .. } => {
                if let Some(pos) = hide_ids.iter().position(|h| h == id) {
                    hide_ids.remove(pos);
                    // Also hide the Close row itself: rewind out if we just
                    // pushed a Close under a collapse trigger. Simpler: drop
                    // the last pushed index if it matches this Close.
                    if !inside_hidden && out.last() == Some(&i) {
                        out.pop();
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Compact summary label for a collapsed node.
fn collapsed_summary(kind: &LineKind) -> String {
    match kind {
        LineKind::Open {
            is_array: true,
            count,
            ..
        } => format!("[ …{} items ]", count),
        LineKind::Open {
            is_array: false,
            count,
            ..
        } => format!("{{ …{} keys }}", count),
        _ => String::new(),
    }
}

#[component]
fn JsonTree(text: String) -> Element {
    let parsed = serde_json::from_str::<serde_json::Value>(&text);
    let text_fallback = text.clone();
    let Ok(root) = parsed else {
        return rsx! { pre { class: "body-json", "{text_fallback}" } };
    };

    let flat = use_hook(|| flatten(&root));
    // Seed: collapse anything below depth 2, plus any array > 20.
    let initial_collapsed: std::collections::HashSet<usize> = flat
        .iter()
        .filter_map(|l| match &l.kind {
            LineKind::Open {
                id,
                count,
                is_array,
                ..
            } if l.depth >= 2 || (*is_array && *count > 20) => Some(*id),
            _ => None,
        })
        .collect();
    let lines = use_signal(move || flat.clone());
    let mut collapsed = use_signal(move || initial_collapsed.clone());
    let mut scroll_top = use_signal(|| 0.0f64);
    let mut viewport_h = use_signal(|| 400.0f64);
    let mut query = use_signal(String::new);

    let visible =
        use_memo(move || compute_visible(&lines.read(), &collapsed.read(), &query.read()));
    let match_count = use_memo(move || {
        let q = query.read();
        if q.is_empty() {
            return 0usize;
        }
        let needle = q.to_lowercase();
        lines
            .read()
            .iter()
            .filter(|l| line_matches(l, &needle))
            .count()
    });

    rsx! { div { class: "json-tree-wrap",
        div { class: "json-search",
            input {
                class: "json-search-input",
                placeholder: "search response…",
                value: "{query}",
                oninput: move |e| { query.set(e.value()); scroll_top.set(0.0); },
                onkeydown: move |e| {
                    if e.key() == Key::Escape { query.set(String::new()); }
                },
            }
            if !query.read().is_empty() {
                span { class: "muted small", "{match_count} matches" }
                button {
                    class: "btn small ghost",
                    onclick: move |_| query.set(String::new()),
                    "clear"
                }
            }
        }
        div {
            class: "json-viewport",
            onmounted: move |m| {
                spawn(async move {
                    if let Ok(rect) = m.get_client_rect().await {
                        viewport_h.set(rect.size.height);
                    }
                });
            },
            onscroll: move |e| {
                // Dioxus scroll event carries no delta by default; read from DOM.
                let _ = e;
                if let Some(el) = web_sys::window()
                    .and_then(|w| w.document())
                    .and_then(|d| d.query_selector(".json-viewport").ok().flatten())
                {
                    scroll_top.set(el.scroll_top() as f64);
                }
            },
            {
                // Snapshot only the window we need — cloning the entire lines
                // Vec on every scroll tick pegs the CPU on large responses.
                let vis_r = visible.read();
                let total_h = vis_r.len() as f64 * ROW_HEIGHT;
                let start = ((*scroll_top.read() / ROW_HEIGHT) as usize).saturating_sub(OVERSCAN);
                let visible_count = (*viewport_h.read() / ROW_HEIGHT).ceil() as usize + OVERSCAN * 2;
                let end = (start + visible_count).min(vis_r.len());
                let offset_y = start as f64 * ROW_HEIGHT;
                let window_indices: Vec<usize> = vis_r[start..end].to_vec();
                drop(vis_r);
                let lines_r = lines.read();
                let window: Vec<Line> = window_indices.iter().map(|&i| lines_r[i].clone()).collect();
                drop(lines_r);

                rsx! {
                    div {
                        class: "json-spacer",
                        style: "height: {total_h}px; position: relative;",
                        div {
                            class: "json-window",
                            style: "position: absolute; top: {offset_y}px; left: 0; right: 0;",
                            for line in window.iter() {
                                {
                                    let line = line.clone();
                                    let pad = line.depth * 16;
                                    let comma = |t: bool| if t { "," } else { "" };
                                    let row = match &line.kind {
                                        LineKind::Open { id, is_array, count: _, trailing_comma } => {
                                            let is_col = collapsed.read().contains(id);
                                            let node_id = *id;
                                            let arrow = if is_col { "▶" } else { "▼" };
                                            let brace = if *is_array { "[" } else { "{" };
                                            let summary = if is_col { collapsed_summary(&line.kind) } else { String::new() };
                                            let tail = comma(is_col && *trailing_comma);
                                            rsx! {
                                                div { class: "j-row", style: "padding-left: {pad}px",
                                                    span {
                                                        class: "j-toggle",
                                                        onclick: move |_| {
                                                            let mut s = collapsed.read().clone();
                                                            if !s.insert(node_id) { s.remove(&node_id); }
                                                            collapsed.set(s);
                                                        },
                                                        "{arrow} "
                                                    }
                                                    if is_col {
                                                        span { class: "j-summary", "{summary}{tail}" }
                                                    } else {
                                                        span { class: "j-punct", "{brace}" }
                                                    }
                                                }
                                            }
                                        }
                                        LineKind::Close { id: _, is_array, trailing_comma } => {
                                            let brace = if *is_array { "]" } else { "}" };
                                            let tail = comma(*trailing_comma);
                                            rsx! {
                                                div { class: "j-row", style: "padding-left: {pad}px",
                                                    span { class: "j-punct", "{brace}{tail}" }
                                                }
                                            }
                                        }
                                        LineKind::Kv { key, value, class, trailing_comma } => {
                                            let tail = comma(*trailing_comma);
                                            let k = key.clone().unwrap_or_default();
                                            let cls = *class;
                                            let v = value.clone();
                                            rsx! {
                                                div { class: "j-row", style: "padding-left: {pad}px",
                                                    span { class: "j-key", "{k}" }
                                                    span { class: "j-punct", ": " }
                                                    span { class: "{cls}", "{v}{tail}" }
                                                }
                                            }
                                        }
                                        LineKind::Item { value, class, trailing_comma } => {
                                            let tail = comma(*trailing_comma);
                                            let cls = *class;
                                            let v = value.clone();
                                            rsx! {
                                                div { class: "j-row", style: "padding-left: {pad}px",
                                                    span { class: "{cls}", "{v}{tail}" }
                                                }
                                            }
                                        }
                                    };
                                    rsx! { {row} }
                                }
                            }
                        }
                    }
                }
            }
        }
    }}
}
