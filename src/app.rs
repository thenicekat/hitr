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
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

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
                        if let Some(node) = list.item(i) {
                            if let Ok(e) = node.dyn_into::<web_sys::Element>() {
                                let _ = e.set_attribute("autocapitalize", "off");
                                let _ = e.set_attribute("autocorrect", "off");
                                let _ = e.set_attribute("spellcheck", "false");
                            }
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
        let mut init = web_sys::MutationObserverInit::new();
        init.child_list(true).subtree(true);
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
    let mut collection = use_signal(|| Collection::default());
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

    let load_all = use_callback(move |()| {
        spawn(async move {
            match api::vault_status().await {
                Ok(s) => { vault_locked.set(!s.unlocked); vault_exists.set(s.exists); }
                Err(e) => error.set(Some(format!("vault_status: {}", e))),
            }
            match api::get_root().await {
                Ok(r) => root.set(r),
                Err(e) => error.set(Some(format!("get_root: {}", e))),
            }
            match api::load().await {
                Ok(c) => {
                    if selected_env.peek().is_none() {
                        if let Some(first) = c.envs.first() {
                            selected_env.set(Some(first.name.clone()));
                        }
                    }
                    collection.set(c);
                    error.set(None);
                }
                Err(e) => error.set(Some(format!("load: {}", e))),
            }
        });
    });

    use_hook(|| {
        load_all.call(());
        install_input_attrs();
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

    let fire = use_callback(move |()| {
        let req_id = selected_req.read().clone();
        let env_name = selected_env.read().clone();
        let Some(req_id) = req_id else { return };
        firing.set(true);
        response.set(None);
        error.set(None);
        spawn(async move {
            match api::fire_request(&req_id, env_name.as_deref()).await {
                Ok(r) => response.set(Some(r)),
                Err(e) => error.set(Some(format!("fire: {}", e))),
            }
            firing.set(false);
        });
    });

    let selected_req_obj = use_memo(move || {
        let id = selected_req.read().clone()?;
        collection.read().requests.iter().find(|r| r.id == id).cloned()
    });

    let selected_env_obj = use_memo(move || {
        let name = selected_env.read().clone()?;
        collection.read().envs.iter().find(|e| e.name == name).cloned()
    });

    rsx! {
        link { rel: "stylesheet", href: CSS }
        div { class: "app",
            // header
            div { class: "topbar",
                span { class: "brand", "aptui" }
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
                        class: "btn small ghost",
                        onclick: move |_| {
                            spawn(async move {
                                let _ = api::vault_lock().await;
                                vault_locked.set(true);
                            });
                        },
                        "🔒 lock"
                    }
                }
                button { class: "btn ghost", onclick: move |_| load_all.call(()), "reload" }
            }

            div { class: "main",
                // envs pane
                div { class: "pane envs",
                    div { class: "pane-hdr",
                        span { "envs" }
                        button { class: "btn small", onclick: move |_| modal.set(Modal::NewEnv), "+" }
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
                    if let Some(env) = selected_env_obj.read().as_ref() {
                        div { class: "env-actions",
                            button { class: "btn small",
                                onclick: move |_| modal.set(Modal::EditEnv),
                                "edit"
                            }
                            {
                                let e = env.clone();
                                rsx! {
                                    button { class: "btn small danger",
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
                                        "delete"
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
                        button { class: "btn small", onclick: move |_| modal.set(Modal::ImportCurl), "curl" }
                        button { class: "btn small", onclick: move |_| modal.set(Modal::ImportOpenApi), "openapi" }
                        button { class: "btn small", onclick: move |_| modal.set(Modal::NewRequest), "+" }
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
                            on_fire: move |_| fire.call(()),
                            on_saved: move |_| load_all.call(()),
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
                            pre { class: "body-json", "{resp.body}" }
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
        if pw.is_empty() { err.set(Some("password required".into())); return; }
        if !exists && pw != cf { err.set(Some("passwords don't match".into())); return; }
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
    let mut secret_values = use_signal(|| std::collections::HashMap::<String, String>::new());
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
                                                    cur[idx].secret = e.checked();
                                                    if e.checked() { cur[idx].value = String::new(); }
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
        if raw.trim().is_empty() { err.set(Some("paste a curl command".into())); return; }
        spawn(async move {
            match api::parse_curl(&raw).await {
                Ok(req) => { preview.set(Some(req)); err.set(None); }
                Err(e) => { preview.set(None); err.set(Some(e)); }
            }
        });
    };

    let do_import = move |_| {
        let raw = input.read().clone();
        let n = name.read().clone();
        let f = folder.read().clone();
        if n.trim().is_empty() { err.set(Some("name required".into())); return; }
        if f.trim().is_empty() { err.set(Some("folder required".into())); return; }
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
        if p.trim().is_empty() { err.set(Some("spec path required".into())); return; }
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
                Err(e) => { preview.set(None); err.set(Some(e)); }
            }
            busy.set(false);
        });
    };

    let do_import = move |_| {
        let p = spec_path.read().clone();
        let fp = folder_prefix.read().clone();
        let ce = *create_env.read();
        let en = env_name.read().clone();
        if preview.peek().is_none() { err.set(Some("preview first".into())); return; }
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
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
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
    on_fire: EventHandler<()>,
    on_saved: EventHandler<()>,
) -> Element {
    let mut method = use_signal(|| req.http.method.clone());
    let mut url = use_signal(|| req.http.url.clone());
    let mut headers = use_signal(|| req.http.headers.clone());
    let mut params = use_signal(|| req.http.params.clone());
    let mut body_type = use_signal(|| req.http.body.kind.clone().unwrap_or_default());
    let mut body_text = use_signal(|| {
        // Bruno stores body under `data`; older aptui saves used `json`/`text`.
        // Prefer whichever is non-empty.
        let b = &req.http.body;
        let candidates = match body_type.peek().as_str() {
            "json" => [&b.json, &b.data, &b.text],
            "text" => [&b.text, &b.data, &b.json],
            _ => [&b.data, &b.json, &b.text],
        };
        candidates.iter()
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
    let mut bump = move || { let n = *save_gen.peek() + 1; save_gen.set(n); };

    // Autosave: react to save_gen. Wait 500ms. If save_gen changed during the
    // wait, another edit came in — do nothing (that later task will save).
    // Skip the very first fire (component mount, no user edit).
    let orig_for_effect = orig.clone();
    use_effect(move || {
        let stamp = *save_gen.read();
        if stamp == 0 { return; }
        let orig = orig_for_effect.clone();
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(500).await;
            if *save_gen.peek() != stamp { return; }
            let mut updated = orig;
            updated.http.method = method.peek().clone();
            updated.http.url = url.peek().clone();
            updated.http.headers = headers.peek().clone();
            updated.http.params = params.peek().clone();
            let bt = body_type.peek().clone();
            let bx = body_text.peek().clone();
            updated.http.body = Body {
                kind: if bt.is_empty() { None } else { Some(bt.clone()) },
                json: None,
                text: None,
                data: if !bt.is_empty() && !bx.is_empty() { Some(bx.clone()) } else { None },
            };
            saving.set(true);
            match api::save_request(&updated).await {
                Ok(_) => { save_err.set(None); on_saved.call(()); }
                Err(e) => save_err.set(Some(e)),
            }
            saving.set(false);
        });
    });

    rsx! {
        div { class: "req-editor",
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
                    oninput: move |e| { url.set(e.value()); bump(); },
                    placeholder: "{{baseUrl}}/path"
                }
                if *saving.read() {
                    span { class: "muted small", "saving…" }
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
                                    placeholder: "request body…"
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
