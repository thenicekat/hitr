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
pub struct NotesBook {
    pub active_id: String,
    pub notes: Vec<SavedNote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

fn read(path: &Path) -> Result<NotesBook, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let html = super::read_note(&path.with_file_name("quick-note.txt"))?;
            Ok(NotesBook {
                active_id: "1".into(),
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

// Keep tasks created in the earlier Focus experiment as ordinary actionable
// lines. The original file is retained, and a stable note ID makes this one-shot.
fn import_focus_tasks(path: &Path, book: &mut NotesBook) -> Result<(), String> {
    if book.notes.iter().any(|note| note.id == "focus-import") {
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
    let path = path()?;
    let mut book = read(&path)?;
    let id = (book
        .notes
        .iter()
        .filter_map(|n| n.id.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
        + 1)
    .to_string();
    book.notes.push(SavedNote {
        id: id.clone(),
        title: "Untitled".into(),
        html: String::new(),
    });
    book.active_id = id;
    write(&path, &book)?;
    Ok(book)
}

#[cfg(test)]
mod tests {
    use super::*;
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
