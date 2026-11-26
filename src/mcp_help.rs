use crate::{api, types::*};
use dioxus::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[component]
pub fn McpHelpPanel(on_close: EventHandler<()>, vault_locked: bool, envs: Vec<Env>) -> Element {
    let mut status = use_signal(|| None::<Result<McpHelp, String>>);
    let mut config_open = use_signal(|| false);
    let mut copy_result = use_signal(|| None::<bool>);
    let mut saving = use_signal(|| false);
    let mut settings_error = use_signal(|| None::<String>);
    let refresh = use_callback(move |()| {
        status.set(None);
        spawn(async move {
            status.set(Some(api::mcp_help().await));
        });
    });
    use_hook(move || refresh.call(()));

    let result = status.read();
    let help = result.as_ref().and_then(|result| result.as_ref().ok());
    let enabled = help
        .filter(|help| !matches!(help.status, McpStatus::Unsupported))
        .map(|help| help.enabled);
    let (state, label) = match help.map(|help| &help.status) {
        Some(McpStatus::Ready) if vault_locked => ("paused", "Vault locked"),
        Some(McpStatus::Ready) => ("ready", "Ready"),
        Some(McpStatus::Starting) => ("paused", "Starting…"),
        Some(McpStatus::Disabled) => ("off", "Off"),
        Some(McpStatus::Error { .. }) => ("failed", "Couldn’t start"),
        Some(McpStatus::Unsupported) => ("off", "Unavailable on this platform"),
        None if result.is_none() => ("off", "Checking…"),
        None => ("failed", "Status unavailable"),
    };

    rsx! {
        aside {
            id: "mcp-help",
            class: "mcp-help",
            aria_labelledby: "mcp-help-title",
            onkeydown: move |event| {
                if event.key() == Key::Enter {
                    event.stop_propagation();
                }
                if event.key() == Key::Escape {
                    event.prevent_default();
                    on_close.call(());
                }
            },
            header { class: "mcp-panel-header",
                h2 { id: "mcp-help-title", "MCP" }
                button {
                    class: "btn icon mcp-close",
                    aria_label: "Close MCP",
                    title: "Close",
                    onmounted: move |element| async move {
                        let _ = element.data().set_focus(true).await;
                    },
                    onclick: move |_| on_close.call(()),
                    "×"
                }
            }
            div { class: "mcp-access",
                div { class: "mcp-access-row",
                    div {
                        if enabled.is_some() {
                            label { r#for: "mcp-enabled", "Enable MCP" }
                        }
                        div { class: "mcp-status mcp-status-{state}", role: "status",
                            span { class: "mcp-status-dot", aria_hidden: "true" }
                            if *saving.read() { "Saving…" } else { "{label}" }
                        }
                    }
                    if let Some(enabled) = enabled {
                        input {
                            id: "mcp-enabled",
                            class: "mcp-switch",
                            r#type: "checkbox",
                            role: "switch",
                            aria_describedby: "mcp-access-scope",
                            checked: enabled,
                            disabled: *saving.read(),
                            onchange: move |event| {
                                let enabled = event.checked();
                                saving.set(true);
                                settings_error.set(None);
                                spawn(async move {
                                    if let Err(error) = api::set_mcp_enabled(enabled).await {
                                        settings_error.set(Some(error));
                                    }
                                    status.set(Some(api::mcp_help().await));
                                    saving.set(false);
                                });
                            }
                        }
                    }
                }
                if let Some(McpStatus::Error { message }) = help.map(|help| &help.status) {
                    p { class: "mcp-error", "{message} Restart Hitr after fixing the issue." }
                }
                if state == "failed" || matches!(help.map(|help| &help.status), Some(McpStatus::Starting)) {
                    button { class: "btn small ghost", disabled: *saving.read(), onclick: move |_| refresh.call(()), "Refresh status" }
                }
                if let Some(error) = settings_error.read().as_ref() {
                    p { role: "alert", class: "mcp-error", "{error}" }
                }
                if enabled.is_some() {
                    p { id: "mcp-access-scope", class: "mcp-hint", "While the vault is unlocked, local clients can send saved requests in environments you allow below." }
                    if let Some(help) = help {
                        if let Some(error) = &help.allowed_envs_error {
                            p { role: "alert", class: "mcp-error", "MCP requests are blocked because restrictions could not be loaded: {error}" }
                        }
                        fieldset { class: "mcp-environments",
                            disabled: *saving.read() || help.allowed_envs_error.is_some(),
                            legend { "Allowed environments" }
                            p { class: "mcp-hint", "Choose which environments MCP clients can discover and use. None are allowed by default." }
                            if envs.is_empty() {
                                p { class: "mcp-hint", "Load a collection to choose environments." }
                            }
                            for env in envs.iter() {
                                {
                                    let name = env.name.clone();
                                    let path = env.path.clone();
                                    let allowed = help.allowed_envs.contains(&path);
                                    let mut names = help.allowed_envs.clone();
                                    rsx! {
                                        label { key: "{path}", class: "mcp-environment",
                                            input {
                                                r#type: "checkbox",
                                                checked: allowed,
                                                onchange: move |event| {
                                                    names.retain(|item| item != &path);
                                                    if event.checked() { names.push(path.clone()); }
                                                    let updated = names.clone();
                                                    saving.set(true);
                                                    settings_error.set(None);
                                                    spawn(async move {
                                                        if let Err(error) = api::set_mcp_allowed_envs(updated).await { settings_error.set(Some(error)); }
                                                        status.set(Some(api::mcp_help().await));
                                                        saving.set(false);
                                                    });
                                                }
                                            }
                                            "{name}"
                                        }
                                    }
                                }
                            }
                        }
                        p { class: "mcp-hint", "Choices apply to this collection. An allowed environment or a fixed URL may still point to another server." }
                    }
                }
            }
            if let Some(config) = help.and_then(|help| help.client_config.as_ref()) {
                section { class: "mcp-setup",
                    div { class: "mcp-config",
                        div { class: "mcp-config-bar",
                            button {
                                class: "btn mcp-disclosure",
                                aria_expanded: *config_open.read(),
                                aria_controls: "mcp-config-json",
                                onclick: move |_| config_open.toggle(),
                                span { class: "mcp-chevron", aria_hidden: "true" }
                                "Client configuration"
                            }
                            span { class: "mcp-copied", role: "status",
                                if *copy_result.read() == Some(true) { "Copied" }
                            }
                            button {
                                class: "btn mcp-copy",
                                aria_label: "Copy configuration",
                                title: "Copy configuration",
                                onclick: {
                                    let config = config.clone();
                                    move |_| {
                                        let Some(window) = web_sys::window() else {
                                            copy_result.set(Some(false));
                                            config_open.set(true);
                                            return;
                                        };
                                        let write = window.navigator().clipboard().write_text(&config);
                                        spawn(async move {
                                            let copied = JsFuture::from(write).await.is_ok();
                                            copy_result.set(Some(copied));
                                            if !copied {
                                                config_open.set(true);
                                            }
                                        });
                                    }
                                },
                                span { class: "copy-icon", aria_hidden: "true" }
                            }
                        }
                        pre {
                            id: "mcp-config-json",
                            class: "mcp-snippet",
                            hidden: !*config_open.read(),
                            tabindex: "0",
                            "{config}"
                        }
                    }
                    if *copy_result.read() == Some(false) {
                        p { role: "alert", class: "mcp-error", "Copy failed. Select the JSON to copy it manually." }
                    }
                    p { class: "mcp-hint", "Paste into your MCP client’s settings, then restart the client." }
                }
            }
        }
    }
}
