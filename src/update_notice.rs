use crate::api;
use dioxus::prelude::*;

#[component]
pub fn UpdateNotice() -> Element {
    let mut available = use_signal(|| None::<String>);
    let mut installing = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);

    use_hook(move || {
        spawn(async move {
            if let Ok(version) = api::check_update().await {
                available.set(version);
            }
        });
    });

    let version = available.read().clone();
    rsx! {
        if let Some(version) = version {
            div { class: "update-notice",
                span { role: "status",
                    if *installing.read() {
                        "Installing Hitr {version}…"
                    } else {
                        "Hitr {version} available"
                    }
                }
                button {
                    class: "btn primary",
                    disabled: *installing.read(),
                    onclick: move |_| {
                        let version = version.clone();
                        installing.set(true);
                        error.set(None);
                        spawn(async move {
                            if let Err(message) = api::install_update(&version).await {
                                error.set(Some(message));
                            }
                            installing.set(false);
                        });
                    },
                    "Update and restart"
                }
                button {
                    class: "btn ghost",
                    disabled: *installing.read(),
                    onclick: move |_| available.set(None),
                    "Later"
                }
                if let Some(message) = error.read().as_ref() {
                    p { role: "alert", "Update failed. {message}" }
                }
            }
        }
    }
}
