use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Mutex};

static LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedNote {
    pub id: String,
    pub title: String,
    pub html: String,
}
#[derive(Serialize, Deserialize)]
pub struct DeletedNote {
    pub note: SavedNote,
    pub index: usize,
    pub replacement_id: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct NotesBook {
    pub active_id: String,
    pub notes: Vec<SavedNote>,
    #[serde(default = "already_onboarded")]
    pub onboarding_complete: bool,
    #[serde(default)]
    pub focus_imported: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_note: Option<DeletedNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

// Books saved before onboarding belong to returning users.
fn already_onboarded() -> bool {
    true
}

fn has_existing_data(path: &Path) -> Result<bool, String> {
    for name in [
        "quick-note.html",
        "quick-note.txt",
        "focus.json",
        "notes-preferences.json",
        "vault.age",
        "config.json",
    ] {
        if path
            .with_file_name(name)
            .try_exists()
            .map_err(|e| e.to_string())?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read(path: &Path) -> Result<NotesBook, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let html = super::read_note(&path.with_file_name("quick-note.txt"))?;
            Ok(NotesBook {
                active_id: "1".into(),
                onboarding_complete: has_existing_data(path)?,
                focus_imported: false,
                deleted_note: None,
                warning: None,
                notes: vec![SavedNote {
                    id: "1".into(),
                    title: "Scratchpad".into(),
                    html,
                }],
            })
        }
        Err(e) => Err(e.to_string()),
    }
}
fn write(path: &Path, book: &NotesBook) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("Invalid notes path")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(&serde_json::to_vec(book).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
fn path() -> Result<std::path::PathBuf, String> {
    Ok(super::note_path()?.with_file_name("notes.json"))
}

#[tauri::command]
pub fn load_notes() -> Result<NotesBook, String> {
    let _lock = LOCK.lock().map_err(|e| e.to_string())?;
    let path = path()?;
    let mut book = read(&path)?;
    let warning = import_focus_tasks(&path.with_file_name("focus.json"), &mut book).err();
    write(&path, &book)?;
    book.warning = warning;
    Ok(book)
}

#[tauri::command]
pub fn finish_onboarding() -> Result<(), String> {
    let _lock = LOCK.lock().map_err(|e| e.to_string())?;
    finish_onboarding_at(&path()?)
}

fn finish_onboarding_at(path: &Path) -> Result<(), String> {
    let mut book = read(path)?;
    book.onboarding_complete = true;
    write(path, &book)
}

// Keep tasks created in the earlier Focus experiment as ordinary actionable
// lines. Remember the import separately so deleting the note cannot reimport it.
fn import_focus_tasks(path: &Path, book: &mut NotesBook) -> Result<(), String> {
    if book.focus_imported {
        return Ok(());
    }
    if book.notes.iter().any(|note| note.id == "focus-import")
        || book
            .deleted_note
            .as_ref()
            .is_some_and(|deleted| deleted.note.id == "focus-import")
    {
        book.focus_imported = true;
        return Ok(());
    }
    #[derive(Deserialize)]
    struct LegacyTask {
        id: u64,
        title: String,
        completed_at: Option<u64>,
    }
    #[derive(Deserialize)]
    struct LegacyBoard {
        tasks: Vec<LegacyTask>,
    }
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Could not import old Focus tasks: {error}")),
    };
    let board: LegacyBoard = serde_json::from_slice(&bytes).map_err(|error| {
        format!("Could not import old Focus tasks; focus.json was left unchanged: {error}")
    })?;
    if board.tasks.is_empty() {
        return Ok(());
    }
    let mut html = String::from("<h2>Things to do</h2>");
    for task in board.tasks {
        let text = task
            .title
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        let done = if task.completed_at.is_some() {
            " data-done=\"true\""
        } else {
            ""
        };
        html.push_str(&format!(
            "<div data-task-id=\"imported-{}\"{done}>{text}</div>",
            task.id
        ));
    }
    book.notes.push(SavedNote {
        id: "focus-import".into(),
        title: "Things to do".into(),
        html,
    });
    book.focus_imported = true;
    Ok(())
}
#[tauri::command]
pub fn save_note(id: String, text: String, title: String) -> Result<(), String> {
    let _lock = LOCK.lock().map_err(|e| e.to_string())?;
    let path = path()?;
    let mut book = read(&path)?;
    update(&mut book, &id, text, title)?;
    write(&path, &book)
}
fn update(book: &mut NotesBook, id: &str, html: String, title: String) -> Result<(), String> {
    let note = book
        .notes
        .iter_mut()
        .find(|n| n.id == id)
        .ok_or("Note not found")?;
    note.html = html;
    note.title = if title.trim().is_empty() {
        "Untitled".into()
    } else {
        title.trim().chars().take(60).collect()
    };
    Ok(())
}
#[tauri::command]
pub fn select_note(id: String) -> Result<NotesBook, String> {
    let _lock = LOCK.lock().map_err(|e| e.to_string())?;
    let path = path()?;
    let mut book = read(&path)?;
    if !book.notes.iter().any(|n| n.id == id) {
        return Err("Note not found".into());
    }
    book.active_id = id;
    write(&path, &book)?;
    Ok(book)
}
#[tauri::command]
pub fn create_note() -> Result<NotesBook, String> {
    let _lock = LOCK.lock().map_err(|e| e.to_string())?;
    create_note_at(&path()?)
}

fn blank_note(book: &NotesBook) -> Result<SavedNote, String> {
    // Reserve the deleted note's ID until Undo is consumed or dismissed.
    let id = book
        .notes
        .iter()
        .chain(book.deleted_note.iter().map(|deleted| &deleted.note))
        .filter_map(|n| n.id.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or("Could not allocate a note ID")?
        .to_string();
    Ok(SavedNote {
        id,
        title: "Untitled".into(),
        html: String::new(),
    })
}

fn create_note_at(path: &Path) -> Result<NotesBook, String> {
    let mut book = read(path)?;
    let note = blank_note(&book)?;
    book.active_id = note.id.clone();
    book.notes.push(note);
    write(path, &book)?;
    Ok(book)
}

#[tauri::command]
pub fn delete_note(id: String, text: String, title: String) -> Result<NotesBook, String> {
    let _lock = LOCK.lock().map_err(|e| e.to_string())?;
    delete_note_at(&path()?, &id, text, title)
}

fn delete_note_at(path: &Path, id: &str, html: String, title: String) -> Result<NotesBook, String> {
    let mut book = read(path)?;
    // Capture the latest draft and delete it in one write, including reminders.
    update(&mut book, id, html, title)?;
    let index = book
        .notes
        .iter()
        .position(|note| note.id == id)
        .ok_or("Note not found")?;
    let note = book.notes.remove(index);
    book.focus_imported |= note.id == "focus-import";
    book.deleted_note = Some(DeletedNote {
        note,
        index,
        replacement_id: None,
    });
    if book.notes.is_empty() {
        let replacement = blank_note(&book)?;
        if let Some(deleted) = book.deleted_note.as_mut() {
            deleted.replacement_id = Some(replacement.id.clone());
        }
        book.active_id = replacement.id.clone();
        book.notes.push(replacement);
    } else if book.active_id == id {
        book.active_id = book.notes[index.min(book.notes.len() - 1)].id.clone();
    }
    write(path, &book)?;
    Ok(book)
}

#[tauri::command]
pub fn restore_deleted_note() -> Result<NotesBook, String> {
    let _lock = LOCK.lock().map_err(|e| e.to_string())?;
    restore_deleted_note_at(&path()?)
}

fn restore_deleted_note_at(path: &Path) -> Result<NotesBook, String> {
    let mut book = read(path)?;
    let deleted = book
        .deleted_note
        .take()
        .ok_or("No deleted note to restore")?;
    if book.notes.iter().any(|note| note.id == deleted.note.id) {
        return Err("A note with that ID already exists".into());
    }
    // Remove only an untouched placeholder created by deleting the last note.
    book.notes.retain(|note| {
        Some(&note.id) != deleted.replacement_id.as_ref()
            || !note.html.is_empty()
            || note.title != "Untitled"
    });
    book.active_id = deleted.note.id.clone();
    book.notes
        .insert(deleted.index.min(book.notes.len()), deleted.note);
    write(path, &book)?;
    Ok(book)
}

#[tauri::command]
pub fn dismiss_deleted_note() -> Result<NotesBook, String> {
    let _lock = LOCK.lock().map_err(|e| e.to_string())?;
    dismiss_deleted_note_at(&path()?)
}

fn dismiss_deleted_note_at(path: &Path) -> Result<NotesBook, String> {
    let mut book = read(path)?;
    book.deleted_note = None;
    write(path, &book)?;
    Ok(book)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_book(path: &Path, ids: &[&str], active: &str) {
        let mut book = read(path).unwrap();
        book.onboarding_complete = true;
        book.active_id = active.into();
        book.notes = ids
            .iter()
            .map(|id| SavedNote {
                id: (*id).into(),
                title: format!("Note {id}"),
                html: format!("<p>Note {id}</p>"),
            })
            .collect();
        write(path, &book).unwrap();
    }

    #[test]
    fn delete_and_undo_preserve_latest_drafts_reminders_and_note_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        seed_book(&path, &["1", "2", "3"], "2");
        let draft = "<h2>Latest draft</h2><div data-task-id=\"task\" data-remind-at=\"1900000000000\">Send it</div>";
        let deleted = delete_note_at(&path, "2", draft.into(), "Latest draft".into()).unwrap();
        assert_eq!(deleted.active_id, "3");
        assert_eq!(deleted.notes.len(), 2);
        assert!(deleted.notes.iter().all(|note| note.id != "2"));
        assert!(deleted.onboarding_complete);

        // Read from disk to simulate relaunch, then edit the neighbor before Undo.
        let mut reloaded = read(&path).unwrap();
        assert!(update(&mut reloaded, "2", "stale save".into(), "Stale".into()).is_err());
        update(
            &mut reloaded,
            "3",
            "<p>New work</p>".into(),
            "New work".into(),
        )
        .unwrap();
        write(&path, &reloaded).unwrap();
        let restored = restore_deleted_note_at(&path).unwrap();
        assert_eq!(restored.active_id, "2");
        assert_eq!(
            restored
                .notes
                .iter()
                .map(|note| note.id.as_str())
                .collect::<Vec<_>>(),
            ["1", "2", "3"]
        );
        assert_eq!(restored.notes[1].title, "Latest draft");
        assert_eq!(restored.notes[1].html, draft);
        assert_eq!(restored.notes[2].html, "<p>New work</p>");
        assert!(read(&path).unwrap().deleted_note.is_none());
    }

    #[test]
    fn delete_last_selects_previous_and_new_notes_do_not_reuse_undo_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        seed_book(&path, &["1", "2"], "2");
        let deleted = delete_note_at(&path, "2", "Keep me".into(), "Second".into()).unwrap();
        assert_eq!(deleted.active_id, "1");
        let created = create_note_at(&path).unwrap();
        assert_eq!(created.active_id, "3");
        let restored = restore_deleted_note_at(&path).unwrap();
        assert_eq!(restored.notes.len(), 3);
        assert_eq!(restored.notes[1].id, "2");
        assert_eq!(restored.notes[1].html, "Keep me");
        assert_eq!(restored.notes[2].id, "3");
    }

    #[test]
    fn deleting_only_note_creates_blank_and_undo_removes_only_that_blank() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        seed_book(&path, &["1"], "1");
        let deleted = delete_note_at(&path, "1", "Original".into(), "Original".into()).unwrap();
        assert_eq!(deleted.notes.len(), 1);
        assert_ne!(deleted.active_id, "1");
        assert!(deleted.notes[0].html.is_empty());
        let restored = restore_deleted_note_at(&path).unwrap();
        assert_eq!(restored.notes.len(), 1);
        assert_eq!(restored.active_id, "1");
        assert_eq!(restored.notes[0].html, "Original");

        delete_note_at(&path, "1", "Original".into(), "Original".into()).unwrap();
        let created = create_note_at(&path).unwrap();
        let intentional_blank = created.active_id;
        let restored = restore_deleted_note_at(&path).unwrap();
        assert_eq!(restored.notes.len(), 2);
        assert!(restored
            .notes
            .iter()
            .any(|note| note.id == intentional_blank));
    }

    #[test]
    fn undo_keeps_work_typed_into_the_replacement_note() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        seed_book(&path, &["1"], "1");
        let mut deleted = delete_note_at(&path, "1", "Original".into(), "Original".into()).unwrap();
        let replacement = deleted.active_id.clone();
        update(
            &mut deleted,
            &replacement,
            "New thought".into(),
            "New thought".into(),
        )
        .unwrap();
        write(&path, &deleted).unwrap();
        let restored = restore_deleted_note_at(&path).unwrap();
        assert_eq!(restored.active_id, "1");
        assert_eq!(restored.notes.len(), 2);
        assert_eq!(restored.notes[0].html, "Original");
        assert_eq!(restored.notes[1].html, "New thought");
    }

    #[test]
    fn undo_restores_only_latest_deletion_and_dismissal_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        seed_book(&path, &["1", "2", "3"], "2");
        delete_note_at(&path, "2", "Second".into(), "Second".into()).unwrap();
        delete_note_at(&path, "3", "Third".into(), "Third".into()).unwrap();
        let restored = restore_deleted_note_at(&path).unwrap();
        assert_eq!(restored.active_id, "3");
        assert!(restored.notes.iter().all(|note| note.id != "2"));
        delete_note_at(&path, "3", "Third".into(), "Third".into()).unwrap();
        dismiss_deleted_note_at(&path).unwrap();
        assert!(read(&path).unwrap().deleted_note.is_none());
        let before = std::fs::read(&path).unwrap();
        assert!(restore_deleted_note_at(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn invalid_deletes_and_undo_conflicts_do_not_overwrite_notes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        seed_book(&path, &["1", "2"], "1");
        let before = std::fs::read(&path).unwrap();
        assert!(delete_note_at(&path, "missing", "Draft".into(), "Draft".into()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut deleted = delete_note_at(&path, "2", "Second".into(), "Second".into()).unwrap();
        assert_eq!(deleted.active_id, "1");
        deleted.notes.push(SavedNote {
            id: "2".into(),
            title: "Conflicting".into(),
            html: "Keep this".into(),
        });
        write(&path, &deleted).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(restore_deleted_note_at(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);

        std::fs::write(&path, "broken json").unwrap();
        assert!(delete_note_at(&path, "1", "Draft".into(), "Draft".into()).is_err());
        assert!(restore_deleted_note_at(&path).is_err());
        assert!(dismiss_deleted_note_at(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken json");
    }

    #[test]
    fn deleted_focus_import_stays_deleted_after_undo_is_dismissed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        let legacy = dir.path().join("focus.json");
        let source = r#"{"tasks":[{"id":1,"title":"Legacy task","completed_at":null}]}"#;
        std::fs::write(&legacy, source).unwrap();
        let mut book = read(&path).unwrap();
        import_focus_tasks(&legacy, &mut book).unwrap();
        write(&path, &book).unwrap();
        delete_note_at(&path, "focus-import", "Legacy task".into(), "Tasks".into()).unwrap();
        dismiss_deleted_note_at(&path).unwrap();
        let mut loaded = read(&path).unwrap();
        import_focus_tasks(&legacy, &mut loaded).unwrap();
        assert_eq!(loaded.notes.len(), 1);
        assert!(loaded.focus_imported);
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), source);

        // Books from before the marker existed also remember an existing import.
        let old = r#"{"active_id":"focus-import","notes":[{"id":"focus-import","title":"Tasks","html":"Task"}]}"#;
        std::fs::write(&path, old).unwrap();
        let mut loaded = read(&path).unwrap();
        import_focus_tasks(&legacy, &mut loaded).unwrap();
        assert!(loaded.focus_imported);
        assert_eq!(loaded.notes.len(), 1);
    }

    #[test]
    fn new_install_resumes_onboarding_until_finished_without_changing_notes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        let mut book = read(&path).unwrap();
        assert!(!book.onboarding_complete);
        assert!(book.notes[0].html.is_empty());
        update(
            &mut book,
            "1",
            "<p>Keep my draft</p>".into(),
            "Draft".into(),
        )
        .unwrap();
        write(&path, &book).unwrap();
        assert!(!read(&path).unwrap().onboarding_complete);
        finish_onboarding_at(&path).unwrap();
        finish_onboarding_at(&path).unwrap();
        let loaded = read(&path).unwrap();
        assert!(loaded.onboarding_complete);
        assert_eq!(loaded.active_id, "1");
        assert_eq!(loaded.notes.len(), 1);
        assert_eq!(loaded.notes[0].html, "<p>Keep my draft</p>");
        assert_eq!(loaded.notes[0].title, "Draft");
    }

    #[test]
    fn existing_books_and_legacy_installs_skip_automatic_onboarding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        let original = r#"{"active_id":"old","notes":[{"id":"old","title":"Existing","html":"<p>Keep this</p>"}]}"#;
        std::fs::write(&path, original).unwrap();
        let book = read(&path).unwrap();
        assert!(book.onboarding_complete);
        assert_eq!(book.active_id, "old");
        assert_eq!(book.notes[0].html, "<p>Keep this</p>");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

        for name in [
            "quick-note.html",
            "quick-note.txt",
            "focus.json",
            "notes-preferences.json",
            "vault.age",
            "config.json",
        ] {
            let legacy = tempfile::tempdir().unwrap();
            std::fs::write(legacy.path().join(name), "").unwrap();
            assert!(
                read(&legacy.path().join("notes.json"))
                    .unwrap()
                    .onboarding_complete,
                "{name}"
            );
        }
    }

    #[test]
    fn onboarding_does_not_replace_an_unreadable_book() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        std::fs::write(&path, "broken json").unwrap();
        assert!(finish_onboarding_at(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken json");
    }

    #[test]
    fn preserves_legacy_note_and_isolates_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        std::fs::write(dir.path().join("quick-note.html"), "<b>Existing</b>").unwrap();
        let mut book = read(&path).unwrap();
        assert_eq!(book.notes[0].html, "<b>Existing</b>");
        book.notes.push(SavedNote {
            id: "2".into(),
            title: "Untitled".into(),
            html: String::new(),
        });
        update(&mut book, "2", "<h2>Second</h2>".into(), "Second".into()).unwrap();
        book.active_id = "2".into();
        write(&path, &book).unwrap();
        let loaded = read(&path).unwrap();
        assert_eq!(loaded.active_id, "2");
        assert_eq!(loaded.notes[0].html, "<b>Existing</b>");
        assert_eq!(loaded.notes[1].html, "<h2>Second</h2>");
        assert!(update(&mut book, "missing", "oops".into(), "Oops".into()).is_err());
    }

    #[test]
    fn imports_focus_tasks_once_without_changing_notes_or_source() {
        let dir = tempfile::tempdir().unwrap();
        let notes_path = dir.path().join("notes.json");
        let focus_path = dir.path().join("focus.json");
        let source = r#"{"tasks":[{"id":1,"title":"Check <draft> & send","completed_at":null},{"id":2,"title":"Finished","completed_at":100}]}"#;
        std::fs::write(&focus_path, source).unwrap();
        let mut book = read(&notes_path).unwrap();
        update(
            &mut book,
            "1",
            "<b>My existing note</b>".into(),
            "My note".into(),
        )
        .unwrap();
        import_focus_tasks(&focus_path, &mut book).unwrap();
        write(&notes_path, &book).unwrap();
        let mut loaded = read(&notes_path).unwrap();
        import_focus_tasks(&focus_path, &mut loaded).unwrap();
        assert_eq!(loaded.notes.len(), 2);
        assert_eq!(loaded.notes[0].html, "<b>My existing note</b>");
        assert_eq!(loaded.active_id, "1");
        assert!(loaded.notes[1]
            .html
            .contains("Check &lt;draft&gt; &amp; send"));
        assert!(loaded.notes[1]
            .html
            .contains("data-task-id=\"imported-2\" data-done=\"true\""));
        assert_eq!(std::fs::read_to_string(focus_path).unwrap(), source);
    }

    #[test]
    fn reminder_attributes_round_trip_and_bad_imports_leave_notes_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.json");
        let mut book = read(&path).unwrap();
        let html =
            "<div data-task-id=\"line-123\" data-remind-at=\"1900000000000\">Send update</div>";
        update(&mut book, "1", html.into(), "Send update".into()).unwrap();
        write(&path, &book).unwrap();
        let loaded = read(&path).unwrap();
        assert_eq!(loaded.notes[0].html, html);
        let legacy = dir.path().join("focus.json");
        std::fs::write(&legacy, "broken json").unwrap();
        assert!(import_focus_tasks(&legacy, &mut book).is_err());
        assert_eq!(book.notes.len(), 1);
        assert_eq!(book.notes[0].html, html);
        assert_eq!(std::fs::read_to_string(legacy).unwrap(), "broken json");
    }
}
