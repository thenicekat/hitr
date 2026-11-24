use crate::api;
use crate::onboarding::Onboarding;
use dioxus::prelude::*;

static THEME: Asset = asset!("/assets/theme.css");
static CSS: Asset = asset!("/assets/notes.css");

#[derive(Clone, serde::Deserialize)]
struct ActionLine {
    id: String,
    text: String,
    done: bool,
    remind_at: Option<u64>,
}

#[derive(Clone, serde::Deserialize)]
struct NoteOutline {
    note_id: String,
    lines: Vec<ActionLine>,
}

async fn inspect_notes(notes: &[crate::types::SavedNote]) -> Result<Vec<NoteOutline>, String> {
    let mut script = document::eval(concat!(
        include_str!("../assets/note-content.js"),
        "\nconst notes = await dioxus.recv(); dioxus.send(notes.map(note => ({note_id: note.id, lines: noteContent.outline(note.html)})));"
    ));
    script.send(notes).map_err(|error| error.to_string())?;
    script.recv().await.map_err(|error| error.to_string())
}

#[component]
pub fn Notes() -> Element {
    let mut outlines = use_signal(Vec::<NoteOutline>::new);
    let mut reveal_line = use_signal(|| None::<String>);
    let mut now = use_signal(|| js_sys::Date::now() as u64);
    let mut window_error = use_signal(|| None::<String>);
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
    let mut onboarding_open = use_signal(|| false);
    let mut onboarding_pending = use_signal(|| false);
    let mut onboarding_step = use_signal(|| 0usize);

    let apply_book = use_callback(move |book: crate::types::NotesBook| {
        spawn(async move {
            onboarding_pending.set(!book.onboarding_complete);
            if !book.onboarding_complete {
                onboarding_open.set(true);
            }
            if let Some(warning) = book.warning {
                window_error.set(Some(warning));
            }
            match inspect_notes(&book.notes).await {
                Ok(index) => outlines.set(index),
                Err(error) => window_error.set(Some(format!("Couldn't read reminders: {error}"))),
            }
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
        let dirty = html != saved();
        spawn(async move {
            let result = async {
                if dirty {
                    api::save_note(&id, &html, &title).await?;
                }
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

    let leave_note = use_callback(move |open_apis: bool| {
        if *busy.peek() {
            return;
        }
        busy.set(true);
        spawn(async move {
            let result = async {
                while ready() && text() != saved() {
                    let value = text();
                    api::save_note(&active_id(), &value, &note_title(&plain_text()))
                        .await
                        .map_err(|e| format!("Couldn't save note: {e}"))?;
                    saved.set(value);
                    status.set("Saved".into());
                }
                if open_apis {
                    api::open_api_workspace()
                        .await
                        .map_err(|e| format!("Couldn't open APIs: {e}"))?;
                }
                api::resize_notes(false).await?;
                expanded.set(false);
                settings.set(false);
                Ok::<(), String>(())
            }
            .await;
            if let Err(error) = result {
                window_error.set(Some(error));
            }
            busy.set(false);
        });
    });

    let finish_tour = use_callback(move |()| {
        if *busy.peek() {
            return;
        }
        busy.set(true);
        spawn(async move {
            let result = if onboarding_pending() {
                api::finish_onboarding().await
            } else {
                Ok(())
            };
            match result {
                Ok(()) => {
                    onboarding_pending.set(false);
                    onboarding_open.set(false);
                    window_error.set(None);
                }
                Err(error) => {
                    window_error.set(Some(format!("Couldn't save your progress: {error}")))
                }
            }
            busy.set(false);
        });
    });

    let replay_tour = use_callback(move |()| {
        if *busy.peek() || !*ready.peek() || *preferences_busy.peek() {
            return;
        }
        busy.set(true);
        preferences_busy.set(true);
        spawn(async move {
            let result = async {
                while text() != saved() {
                    let value = text();
                    api::save_note(&active_id(), &value, &note_title(&plain_text()))
                        .await
                        .map_err(|e| format!("Couldn't save note: {e}"))?;
                    saved.set(value);
                    status.set("Saved".into());
                }
                api::save_notes_preferences(&preferences())
                    .await
                    .map_err(|e| format!("Couldn't save preferences: {e}"))?;
                Ok::<(), String>(())
            }
            .await;
            match result {
                Ok(()) => {
                    onboarding_step.set(0);
                    settings.set(false);
                    window_error.set(None);
                    onboarding_open.set(true);
                }
                Err(error) => window_error.set(Some(error)),
            }
            preferences_busy.set(false);
            busy.set(false);
        });
    });

    // Native navigation can expand this window, including before the editor
    // mounts. Read its real size so there is no one-shot event to miss.
    use_hook(move || {
        spawn(async move {
            let mut sizes = document::eval(
                "const report = () => dioxus.send(window.innerWidth > 100); window.addEventListener('resize', report); report(); await new Promise(() => {});",
            );
            while let Ok(value) = sizes.recv::<bool>().await {
                expanded.set(value);
            }
        });
    });

    use_hook(move || {
        document::eval(include_str!("../assets/notes-drag.js"));
        spawn(async move {
            loop {
                gloo_timers::future::TimeoutFuture::new(1000).await;
                now.set(js_sys::Date::now() as u64);
            }
        });
        spawn(async move {
            match api::load_notes_preferences().await {
                Ok(value) => {
                    preferences.set(value);
                    preferences_ready.set(true);
                }
                Err(e) => window_error.set(Some(format!("Couldn't load preferences: {e}"))),
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

    let mut due: Vec<_> = outlines()
        .into_iter()
        .flat_map(|note| {
            note.lines
                .into_iter()
                .filter(|line| !line.done && line.remind_at.is_some_and(|time| time <= now()))
                .map(move |line| (note.note_id.clone(), line))
        })
        .collect();
    due.sort_by_key(|(_, line)| line.remind_at);
    let due_count = due.len();
    let first_due = due.first().cloned();
    let footer_due = first_due.clone();

    rsx! {
        link { rel: "stylesheet", href: THEME }
        link { rel: "stylesheet", href: CSS }
        if !expanded() {
            div { class: "bubble-wrap",
                button {
                    class: if due_count > 0 { "bubble is-due" } else { "bubble" },
                    title: if due_count > 0 { format!("{due_count} reminder(s) due — click to return to the note") } else { "Click to open notes, drag to move".into() },
                    aria_label: if due_count > 0 { format!("Open notes, {due_count} reminders due") } else { "Open notes".into() },
                    onclick: move |_| {
                        if let Some((note_id, line)) = first_due.clone() {
                            reveal_line.set(Some(line.id));
                            change_note.call(Some(note_id));
                        }
                        spawn(async move {
                            match api::resize_notes(true).await {
                                Ok(()) => expanded.set(true),
                                Err(e) => window_error.set(Some(e)),
                            }
                        });
                    },
                    span { class: "note-icon", aria_hidden: "true" }
                }
            }
        } else {
            section { class: "pad",
                if onboarding_open() && ready() {
                    Onboarding {
                        step: onboarding_step,
                        busy: busy(),
                        error: window_error(),
                        on_finish: move |_| finish_tour.call(()),
                        on_collapse: move |_| leave_note.call(false),
                    }
                } else {
                    header { class: "pad-header", "data-tauri-drag-region": "",
                        div { class: "note-switcher",
                            select {
                                aria_label: "Select note",
                                title: "Switch notes",
                                value: "{active_id}",
                                disabled: !ready() || busy(),
                                onchange: move |e| {
                                    reveal_line.set(None);
                                    change_note.call(Some(e.value()));
                                },
                                for note in notes().iter() {
                                    option { key: "{note.id}", value: "{note.id}", "{note.title}" }
                                }
                            }
                        }
                        button {
                            class: "new-note",
                            title: "New note",
                            aria_label: "New note",
                            disabled: !ready() || busy(),
                            onclick: move |_| {
                                reveal_line.set(None);
                                change_note.call(None);
                            },
                            "+"
                        }
                        button {
                            class: "open-apis",
                            title: "Open APIs",
                            aria_label: "Open APIs",
                            disabled: busy(),
                            onclick: move |_| leave_note.call(true),
                            svg {
                                width: "15",
                                height: "15",
                                view_box: "0 0 24 24",
                                fill: "none",
                                stroke: "currentColor",
                                stroke_width: "1.6",
                                stroke_linecap: "round",
                                stroke_linejoin: "round",
                                "aria-hidden": "true",
                                path { d: "m8 7-5 5 5 5m8-10 5 5-5 5m-3-14-2 18" }
                            }
                        }
                        button {
                            class: "settings-button",
                            title: "Preferences",
                            aria_label: "Preferences",
                            aria_expanded: "{settings}",
                            onclick: move |_| settings.set(!settings()),
                            "···"
                        }
                        button {
                            class: "close",
                            title: "Save and collapse",
                            aria_label: "Save and collapse",
                            disabled: busy(),
                            onclick: move |_| leave_note.call(false),
                            "×"
                        }
                    }
                    if let Some(message) = window_error() {
                        div { class: "workspace-error", role: "alert",
                            "{message}"
                            button {
                                r#type: "button",
                                aria_label: "Dismiss message",
                                onclick: move |_| window_error.set(None),
                                "×"
                            }
                        }
                    }
                    if settings() {
                        div { class: "preferences",
                            div { class: "preferences-title", "Preferences" }
                            label {
                                "Appearance"
                                select {
                                    value: if preferences().dark { "dark" } else { "light" },
                                    disabled: !preferences_ready() || preferences_busy(),
                                    onchange: move |e| preferences.write().dark = e.value() == "dark",
                                    option { value: "light", "Light" }
                                    option { value: "dark", "Dark" }
                                }
                            }
                            label {
                                "Text size"
                                select {
                                    value: "{preferences().text_size}",
                                    disabled: !preferences_ready() || preferences_busy(),
                                    onchange: move |e| {
                                        if let Ok(size) = e.value().parse() {
                                            preferences.write().text_size = size;
                                        }
                                    },
                                    option { value: "13", "Small" }
                                    option { value: "15", "Medium" }
                                    option { value: "17", "Large" }
                                    option { value: "19", "Extra large" }
                                }
                            }
                            label {
                                "Stay on top"
                                input {
                                    r#type: "checkbox",
                                    checked: preferences().always_on_top,
                                    disabled: !preferences_ready() || preferences_busy(),
                                    onchange: move |e| preferences.write().always_on_top = e.checked(),
                                }
                            }
                            div { class: "preferences-actions",
                                button {
                                    class: "preferences-tour",
                                    disabled: !ready() || !preferences_ready() || preferences_busy() || busy(),
                                    onclick: move |_| replay_tour.call(()),
                                    "Quick tour"
                                }
                                button {
                                    class: "preferences-done",
                                    disabled: !preferences_ready() || preferences_busy(),
                                    onclick: move |_| {
                                        spawn(async move {
                                            preferences_busy.set(true);
                                            match api::save_notes_preferences(&preferences()).await {
                                                Ok(()) => {
                                                    settings.set(false);
                                                    status.set("Preferences saved".into());
                                                }
                                                Err(e) => {
                                                    window_error.set(Some(format!("Couldn't save preferences: {e}")))
                                                }
                                            }
                                            preferences_busy.set(false);
                                        });
                                    },
                                    "Done"
                                }
                            }
                        }
                    }
                    if ready() {
                        for note_id in [active_id()] {
                            NoteEditor {
                                key: "{note_id}",
                                id: note_id,
                                initial: text(),
                                size: preferences().text_size,
                                reveal: reveal_line(),
                                on_change: move |(id, change): (String, EditorChange)| {
                                    if !*ready.peek() || id != *active_id.peek() {
                                        return;
                                    }
                                    let title = note_title(&change.plain);
                                    plain_text.set(change.plain);
                                    if let Some(note) = notes.write().iter_mut().find(|n| n.id == id) {
                                        note.title = title;
                                        note.html = change.html.clone();
                                    }
                                    let mut index = outlines.write();
                                    if let Some(note) = index.iter_mut().find(|note| note.note_id == id) {
                                        note.lines = change.lines;
                                    } else {
                                        index
                                            .push(NoteOutline {
                                                note_id: id,
                                                lines: change.lines,
                                            });
                                    }
                                    if text() != change.html {
                                        text.set(change.html);
                                        status.set("Editing…".into());
                                    }
                                },
                            }
                        }
                    }
                    footer {
                        span { class: "status", "{status}" }
                        if let Some((note_id, line)) = footer_due {
                            button {
                                class: "reminders-due",
                                title: "{line.text}",
                                disabled: !ready() || busy(),
                                onclick: move |_| {
                                    reveal_line.set(Some(line.id.clone()));
                                    if note_id == active_id() {
                                        document::eval(
                                            "setTimeout(() => document.getElementById('note-editor')?.dispatchEvent(new Event('hitr-reveal-line')), 0);",
                                        );
                                    } else {
                                        change_note.call(Some(note_id.clone()));
                                    }
                                },
                                "{due_count} due"
                            }
                        } else {
                            span { class: "word-count", "{plain_text().split_whitespace().count()} words" }
                        }
                    }
                }
            }
        }
    }
}

#[derive(serde::Deserialize)]
struct EditorChange {
    html: String,
    plain: String,
    lines: Vec<ActionLine>,
}

#[component]
fn NoteEditor(
    id: String,
    initial: String,
    size: u8,
    reveal: Option<String>,
    on_change: EventHandler<(String, EditorChange)>,
) -> Element {
    rsx! {
        div {
            id: "note-format-toolbar",
            class: "format-toolbar",
            role: "toolbar",
            aria_label: "Text formatting and reminders",
            button {
                "data-command": "bold",
                title: "Bold (⌘B)",
                aria_label: "Bold",
                strong { "B" }
            }
            button {
                "data-command": "italic",
                title: "Italic (⌘I)",
                aria_label: "Italic",
                em { "I" }
            }
            button {
                "data-command": "underline",
                title: "Underline (⌘U)",
                aria_label: "Underline",
                u { "U" }
            }
            span { class: "format-separator" }
            button {
                "data-command": "formatBlock",
                "data-value": "h2",
                title: "Heading",
                aria_label: "Heading",
                "H"
            }
            button {
                "data-command": "insertUnorderedList",
                title: "Bullet list",
                aria_label: "Bullet list",
                "• ≡"
            }
            button {
                "data-command": "insertOrderedList",
                title: "Numbered list",
                aria_label: "Numbered list",
                "1. ≡"
            }
            span { class: "format-separator" }
            button {
                "data-action": "task",
                title: "Turn this line into a checkbox (⌘⇧L)",
                aria_label: "Toggle task on this line",
                "☐"
            }
            button {
                "data-action": "remind",
                title: "Remind me about this line",
                aria_label: "Remind me about this line",
                svg {
                    width: "15",
                    height: "15",
                    view_box: "0 0 24 24",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "1.6",
                    stroke_linecap: "round",
                    stroke_linejoin: "round",
                    "aria-hidden": "true",
                    path { d: "M18 8a6 6 0 0 0-12 0c0 7-3 7-3 9h18c0-2-3-2-3-9M10 21h4" }
                }
            }
        }
        div {
            id: "note-editor",
            class: "note-editor",
            contenteditable: "true",
            role: "textbox",
            aria_label: "Quick note",
            aria_multiline: "true",
            "data-placeholder": "Write a thought, a plan, or something to do…",
            "data-reveal-line": reveal.unwrap_or_default(),
            style: "font-size: {size}px",
            onmounted: move |_| {
                let initial = initial.clone();
                let id = id.clone();
                spawn(async move {
                    let mut editor = document::eval(
                        concat!(
                            include_str!("../assets/note-content.js"),
                            "\n",
                            include_str!("../assets/note-editor.js"),
                        ),
                    );
                    let _ = editor.send(initial);
                    while let Ok(change) = editor.recv::<EditorChange>().await {
                        on_change.call((id.clone(), change));
                    }
                });
            },
        }
        div { id: "note-reminder-popup" }
    }
}

fn note_title(plain: &str) -> String {
    plain
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().chars().take(60).collect())
        .unwrap_or_else(|| "Untitled".into())
}
