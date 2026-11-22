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
}

fn read(path: &Path) -> Result<NotesBook, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let html = super::read_note(&path.with_file_name("quick-note.txt"))?;
            Ok(NotesBook {
                active_id: "1".into(),
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
    let book = read(&path)?;
    write(&path, &book)?;
    Ok(book)
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
}
