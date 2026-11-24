use dioxus::prelude::*;

static CSS: Asset = asset!("/assets/onboarding.css");

#[component]
pub fn Onboarding(
    mut step: Signal<usize>,
    busy: bool,
    error: Option<String>,
    on_finish: EventHandler<()>,
    on_collapse: EventHandler<()>,
) -> Element {
    let mut checked = use_signal(|| false);
    let mut glowing = use_signal(|| false);
    let mut api_preview = use_signal(|| false);
    let current = step().min(2);
    let (heading, description) = match current {
        0 => (
            "Start with a thought",
            "Write freely. Use the checkbox in the toolbar to turn any line into something you can finish.",
        ),
        1 => (
            "A quiet way to return",
            "Set a reminder beside a task. When it’s due, the whole floating icon glows. Click it to return to that line.",
        ),
        _ => (
            "Keep it within reach",
            "Close your note to the floating icon. Click to reopen it, or drag it wherever you work.",
        ),
    };

    use_effect(move || {
        let _ = step();
        document::eval(
            "document.getElementById('onboarding-title')?.focus({preventScroll: true});",
        );
    });

    rsx! {
        link { rel: "stylesheet", href: CSS }
        div { class: "onboarding", role: "region", aria_label: "Getting started",
            header { class: "onboarding-header", "data-tauri-drag-region": "",
                span { class: "note-icon", aria_hidden: "true" }
                span { "Welcome to Hitr" }
                button {
                    class: "close",
                    title: "Collapse to the floating icon",
                    aria_label: "Collapse walkthrough",
                    disabled: busy,
                    onclick: move |_| on_collapse.call(()),
                    "×"
                }
            }
            div { class: "onboarding-content",
                div { class: "onboarding-progress", aria_label: format!("Step {} of 3", current + 1),
                    for index in 0..3 {
                        span {
                            class: if index <= current { "is-current" } else { "" },
                            aria_hidden: "true",
                        }
                    }
                    small { "{current + 1} / 3" }
                }
                h2 { id: "onboarding-title", tabindex: "-1", "{heading}" }
                p { class: "onboarding-description", "{description}" }

                if current == 0 {
                    div { class: "onboarding-example onboarding-note",
                        h3 { "A little progress" }
                        p { "Keep the first version simple." }
                        div { class: if checked() { "onboarding-task is-done" } else { "onboarding-task" },
                            button {
                                class: "onboarding-check",
                                r#type: "button",
                                role: "checkbox",
                                aria_checked: checked(),
                                aria_label: "Check off the example task",
                                onclick: move |_| checked.toggle(),
                                if checked() { "✓" }
                            }
                            span { "Send the draft" }
                        }
                    }
                    p { class: "onboarding-hint", aria_live: "polite",
                        if checked() { "One thing done. The context stays with it." }
                        else { "Try the checkbox. This is just an example." }
                    }
                } else if current == 1 {
                    div { class: "onboarding-example onboarding-reminder",
                        div { class: "onboarding-reminder-line",
                            span { "Send the draft" }
                            span { class: "onboarding-time", if glowing() { "Due" } else { "Remind" } }
                        }
                        div { class: "onboarding-glow-preview",
                            div {
                                class: if glowing() { "bubble is-due" } else { "bubble" },
                                role: "img",
                                aria_label: if glowing() { "Floating icon with a reminder glow" } else { "Floating notes icon" },
                                span { class: "note-icon", aria_hidden: "true" }
                            }
                            button {
                                class: "onboarding-preview-button",
                                aria_pressed: glowing(),
                                onclick: move |_| glowing.toggle(),
                                if glowing() { "Reset preview" } else { "Preview glow" }
                            }
                        }
                    }
                    p { class: "onboarding-hint", "Keep Hitr running to see reminder glows." }
                } else {
                    div { class: "onboarding-example onboarding-workspaces",
                        if api_preview() {
                            div { class: "onboarding-mini-header",
                                strong { "APIs" }
                                button {
                                    class: "onboarding-preview-button",
                                    onclick: move |_| api_preview.set(false),
                                    "Back to notes"
                                }
                            }
                            p { "Your requests have their own workspace." }
                        } else {
                            div { class: "onboarding-mini-header",
                                strong { "Your note" }
                                button {
                                    class: "open-apis",
                                    title: "Preview API access",
                                    aria_label: "Preview API access",
                                    onclick: move |_| api_preview.set(true),
                                    svg {
                                        width: "15", height: "15", view_box: "0 0 24 24",
                                        fill: "none", stroke: "currentColor", stroke_width: "1.6",
                                        stroke_linecap: "round", stroke_linejoin: "round",
                                        "aria-hidden": "true",
                                        path { d: "m8 7-5 5 5 5m8-10 5 5-5 5m-3-14-2 18" }
                                    }
                                }
                            }
                            p { "The code icon in your note header opens APIs." }
                        }
                    }
                    p { class: "onboarding-hint", "Try switching views here. Your notes save as you type." }
                }
                if let Some(message) = error {
                    p { class: "onboarding-error", role: "alert", "{message}" }
                }
            }
            div { class: "onboarding-actions",
                button {
                    class: "onboarding-skip",
                    disabled: busy,
                    onclick: move |_| on_finish.call(()),
                    "Skip intro"
                }
                div { class: "onboarding-navigation",
                    if current > 0 {
                        button {
                            class: "onboarding-back",
                            disabled: busy,
                            onclick: move |_| step.set(current - 1),
                            "Back"
                        }
                    }
                    button {
                        class: "onboarding-next",
                        disabled: busy,
                        onclick: move |_| {
                            if current == 2 { on_finish.call(()); }
                            else { step.set(current + 1); }
                        },
                        if busy { "Saving…" }
                        else if current == 2 { "Start writing" }
                        else { "Next" }
                    }
                }
            }
        }
    }
}
