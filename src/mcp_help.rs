use crate::{api, types::*};
use dioxus::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[component]
pub fn McpHelpPanel(on_close: EventHandler<()>, vault_locked: bool) -> Element {
    let mut status = use_signal(|| None::<Result<McpHelp, String>>);
    let mut copy_feedback = use_signal(|| None::<&'static str>);
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
            header { class: "mcp-help-heading",
                h2 { id: "mcp-help-title", "MCP for agents" }
                button {
                    class: "btn ghost",
                    onmounted: move |element| async move {
                        let _ = element.data().set_focus(true).await;
                    },
                    onclick: move |_| on_close.call(()),
                    "Close"
                }
            }
            p { "While the vault is unlocked, local MCP clients can search and send every saved request using any environment in the open collection, including production." }
            section {
                div { class: "mcp-help-heading",
                    h3 { "Status" }
                    button { class: "btn small ghost", disabled: result.is_none() || *saving.read(), onclick: move |_| refresh.call(()), "Refresh" }
                }
                div { role: "status",
                    match result.as_ref() {
                        None => rsx! { p { "Checking MCP status…" } },
                        Some(Err(_)) => rsx! { p { class: "error", "Couldn’t read MCP status. Try refreshing." } },
                        Some(Ok(help)) => rsx! {
                            match &help.status {
                                McpStatus::Starting => rsx! { p { "MCP is starting. Refresh in a moment." } },
                                McpStatus::Disabled => rsx! { p { "MCP is disabled." } },
                                McpStatus::Ready if vault_locked => rsx! { p { "Vault locked. MCP search and send are paused." } },
                                McpStatus::Ready => rsx! { p { "Vault unlocked. MCP clients can search and send saved requests." } },
                                McpStatus::Error { message } => rsx! {
                                    p { class: "error", "MCP could not start." }
                                    p { "{message} Restart Hitr after fixing the issue." }
                                },
                                McpStatus::Unsupported => rsx! { p { "MCP is unavailable on this platform." } },
                            }
                        }
                    }
                }
                if let Some(Ok(help)) = result.as_ref() {
                    if !matches!(help.status, McpStatus::Unsupported) {
                        label { class: "mcp-enable",
                            input {
                                r#type: "checkbox",
                                checked: help.enabled,
                                disabled: *saving.read(),
                                onchange: move |event| {
                                    let enabled = event.checked();
                                    saving.set(true);
                                    settings_error.set(None);
                                    spawn(async move {
                                        if let Err(error) = api::set_mcp_enabled(enabled).await {
                                            settings_error.set(Some(error));
                                        }
                                        saving.set(false);
                                        refresh.call(());
                                    });
                                }
                            }
                            "Enable MCP"
                        }
                    }
                }
                if let Some(error) = settings_error.read().as_ref() {
                    p { role: "alert", class: "error", "{error}" }
                }
            }
            if let Some(Ok(help)) = result.as_ref() {
                if let Some(config) = help.client_config.as_ref() {
                    section {
                        h3 { "Connect an MCP client" }
                        p { "Add Hitr to your MCP client’s configuration and restart the client. Keep Hitr open with the vault unlocked, then try searching for a saved request." }
                        button {
                            class: "btn",
                            onclick: {
                                let config = config.clone();
                                move |_| {
                                    let Some(window) = web_sys::window() else {
                                        copy_feedback.set(Some("Copy unavailable. Select the configuration text to copy it."));
                                        return;
                                    };
                                    let write = window.navigator().clipboard().write_text(&config);
                                    spawn(async move {
                                        let message = if JsFuture::from(write).await.is_ok() {
                                            "Client configuration copied."
                                        } else {
                                            "Copy unavailable. Select the configuration text to copy it."
                                        };
                                        copy_feedback.set(Some(message));
                                    });
                                }
                            },
                            "Copy client configuration"
                        }
                        if let Some(message) = *copy_feedback.read() {
                            p { role: "status", "{message}" }
                        }
                        details {
                            summary { "Client configuration" }
                            pre { class: "mcp-snippet", "{config}" }
                        }
                    }
                }
            }
        }
    }
}
