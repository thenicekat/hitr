use crate::api;
use dioxus::prelude::*;

static THEME: Asset = asset!("/assets/theme.css");
static CSS: Asset = asset!("/assets/notes.css");

#[component]
pub fn Notes() -> Element {
    let mut notes = use_signal(Vec::<crate::types::SavedNote>::new);
    let mut active_id = use_signal(String::new);
    let mut preferences = use_signal(crate::types::NotesPreferences::default);
    let mut preferences_ready = use_signal(|| false);
    let mut preferences_busy = use_signal(|| false);
    let mut settings = use_signal(|| false);
    let mut expanded = use_signal(|| false);
    let mut plain_text = use_signal(String::new);
    let mut text = use_signal(String::new);
    let mut saved = use_signal(String::new);
    let mut status = use_signal(|| "Loading…".to_string());
    let mut ready = use_signal(|| false);
    let mut busy = use_signal(|| false);

    let apply_book = use_callback(move |book: crate::types::NotesBook| {
        if let Some(note) = book.notes.iter().find(|n| n.id == book.active_id) {
            active_id.set(note.id.clone());
            text.set(note.html.clone());
            saved.set(note.html.clone());
            plain_text.set(String::new());
            notes.set(book.notes);
            ready.set(true);
            status.set("Saved".into());
        } else {
            status.set("Could not find the selected note".into());
        }
    });

    let change_note = use_callback(move |target: Option<String>| {
        if *busy.peek() || !*ready.peek() {
            return;
        }
        if target.as_ref() == Some(&*active_id.peek()) {
            return;
        }
        busy.set(true);
        ready.set(false);
        let id = active_id();
        let html = text();
        let title = note_title(&plain_text());
        spawn(async move {
            let result = async {
                api::save_note(&id, &html, &title).await?;
                match target {
                    Some(id) => api::select_note(&id).await,
                    None => api::create_note().await,
                }
            }
            .await;
            match result {
                Ok(book) => apply_book.call(book),
                Err(e) => {
                    status.set(format!("Couldn't switch notes: {e}"));
                    ready.set(true);
                }
            }
            busy.set(false);
        });
    });

    use_hook(move || {
        document::eval(include_str!("../assets/notes-drag.js"));
        spawn(async move {
            match api::load_notes_preferences().await {
                Ok(value) => {
                    preferences.set(value);
                    preferences_ready.set(true);
                }
                Err(e) => status.set(format!("Couldn't load preferences: {e}")),
            }
            match api::load_notes().await {
                Ok(book) => apply_book.call(book),
                Err(e) => status.set(e),
            }
        });
    });

    use_effect(move || {
        let theme = if preferences().dark { "dark" } else { "light" };
        if let Some(root) = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.document_element())
        {
            let _ = root.set_attribute("data-theme", theme);
        }
    });

    use_hook(move || {
        spawn(async move {
            let mut events = document::eval(
                "await window.__TAURI__.event.listen('notes-preferences-changed', event => dioxus.send(event.payload)); await new Promise(() => {});",
            );
            while let Ok(value) = events.recv::<crate::types::NotesPreferences>().await {
                preferences.set(value);
            }
        });
    });

    // Save only the latest quiet edit. Saved changes retrigger this effect if
    // typing continued while the previous write was in flight.
    use_effect(move || {
        let value = text();
        let id = active_id();
        if !ready() || value == saved() {
            return;
        }
        spawn(async move {
            gloo_timers::future::TimeoutFuture::new(600).await;
            if *busy.peek() || *text.peek() != value || *active_id.peek() != id || !*ready.peek() {
                return;
            }
            busy.set(true);
            status.set("Saving…".into());
            let title = note_title(&plain_text());
            match api::save_note(&id, &value, &title).await {
                Ok(()) => {
                    saved.set(value);
                    status.set("Saved".into());
                }
                Err(e) => status.set(format!("Couldn't save: {e}")),
            }
            busy.set(false);
        });
    });

    rsx! {
        link { rel: "stylesheet", href: THEME }
        link { rel: "stylesheet", href: CSS }
        if !expanded() {
            div { class: "bubble-wrap",
                button { class: "bubble", title: "Click to open, drag to move", aria_label: "Open quick note",
                    onclick: move |_| { spawn(async move {
                        match api::resize_notes(true).await {
                            Ok(()) => expanded.set(true),
                            Err(e) => status.set(e),
                        }
                    }); },
                    span { class: "note-icon", aria_hidden: "true", }
                }
            }
        } else {
            section { class: "pad",
                header { class: "pad-header", "data-tauri-drag-region": "",
                    div { class: "note-switcher",
                        select { aria_label: "Select note", title: "Switch notes", value: "{active_id}", disabled: !ready() || busy(), onchange: move |e| change_note.call(Some(e.value())),
                            for note in notes().iter() { option { key: "{note.id}", value: "{note.id}", "{note.title}" } }
                        }
                    }
                    button { class: "new-note", title: "New note", aria_label: "New note", disabled: !ready() || busy(), onclick: move |_| change_note.call(None), "+" }
                    button { class: "settings-button", title: "Preferences", aria_label: "Preferences", aria_expanded: "{settings}", onclick: move |_| settings.set(!settings()), "···" }
                    button { class: "close", title: "Save and collapse", aria_label: "Save and collapse", disabled: !ready() || busy(),
                        onclick: move |_| { spawn(async move {
                            busy.set(true);
                            let value = text();
                            match api::save_note(&active_id(), &value, &note_title(&plain_text())).await {
                                Ok(()) => { saved.set(value); status.set("Saved".into());
                                    match api::resize_notes(false).await { Ok(()) => expanded.set(false), Err(e) => status.set(e) }
                                }
                                Err(e) => status.set(format!("Couldn't save: {e}")),
                            }
                            busy.set(false);
                        }); }, "×" }
                }
                if settings() {
                    div { class: "preferences",
                        div { class: "preferences-title", "Preferences" }
                        label { "Appearance"
                            select { value: if preferences().dark { "dark" } else { "light" }, disabled: !preferences_ready() || preferences_busy(), onchange: move |e| preferences.write().dark = e.value() == "dark",
                                option { value: "light", "Light" } option { value: "dark", "Dark" }
                            }
                        }
                        label { "Text size"
                            select { value: "{preferences().text_size}", disabled: !preferences_ready() || preferences_busy(), onchange: move |e| { if let Ok(size) = e.value().parse() { preferences.write().text_size = size; } },
                                option { value: "13", "Small" } option { value: "15", "Medium" } option { value: "17", "Large" } option { value: "19", "Extra large" }
                            }
                        }
                        label { "Stay on top" input { r#type: "checkbox", checked: preferences().always_on_top, disabled: !preferences_ready() || preferences_busy(), onchange: move |e| preferences.write().always_on_top = e.checked() } }
                        button { class: "preferences-done", disabled: !preferences_ready() || preferences_busy(), onclick: move |_| { spawn(async move {
                            preferences_busy.set(true);
                            match api::save_notes_preferences(&preferences()).await {
                                Ok(()) => { settings.set(false); status.set("Preferences saved".into()); }
                                Err(e) => status.set(format!("Couldn't save preferences: {e}")),
                            }
                            preferences_busy.set(false);
                        }); }, "Done" }
                    }
                }
                if ready() {
                    for note_id in [active_id()] {
                    NoteEditor { key: "{note_id}", id: note_id, initial: text(), size: preferences().text_size, on_change: move |(id, change): (String, EditorChange)| {
                        if !*ready.peek() || id != *active_id.peek() { return; }
                        let title = note_title(&change.plain);
                        plain_text.set(change.plain);
                        if let Some(note) = notes.write().iter_mut().find(|n| n.id == id) { note.title = title; }
                        if text() != change.html { text.set(change.html); status.set("Editing…".into()); }
                    } }
                    }
                }
                footer { span { class: "status", "{status}" }
                    span { class: "word-count", "{plain_text().split_whitespace().count()} words" }
                }
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct EditorChange {
    html: String,
    plain: String,
}

#[component]
fn NoteEditor(
    id: String,
    initial: String,
    size: u8,
    on_change: EventHandler<(String, EditorChange)>,
) -> Element {
    rsx! {
        div { class: "format-toolbar", role: "toolbar", aria_label: "Text formatting",
            button { "data-command": "bold", title: "Bold (⌘B)", aria_label: "Bold", strong { "B" } }
            button { "data-command": "italic", title: "Italic (⌘I)", aria_label: "Italic", em { "I" } }
            button { "data-command": "underline", title: "Underline (⌘U)", aria_label: "Underline", u { "U" } }
            span { class: "format-separator", }
            button { "data-command": "formatBlock", "data-value": "h2", title: "Heading", aria_label: "Heading", "H" }
            button { "data-command": "insertUnorderedList", title: "Bullet list", aria_label: "Bullet list", "• ≡" }
            button { "data-command": "insertOrderedList", title: "Numbered list", aria_label: "Numbered list", "1. ≡" }
        }
        div { id: "note-editor", class: "note-editor", contenteditable: "true", role: "textbox", aria_label: "Quick note", aria_multiline: "true", "data-placeholder": "Write something down…", style: "font-size: {size}px",
            onmounted: move |_| {
                let initial = initial.clone();
                let id = id.clone();
                spawn(async move {
                    let mut editor = document::eval(include_str!("../assets/note-editor.js"));
                    let _ = editor.send(initial);
                    while let Ok(change) = editor.recv::<EditorChange>().await { on_change.call((id.clone(), change)); }
                });
            }
        }
    }
}

fn note_title(plain: &str) -> String {
    plain
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(60).collect())
        .unwrap_or_else(|| "Untitled".into())
}
