//! A nonactivating notes panel can join another app's fullscreen Space
//! without activating Hitr or switching back to its API workspace.

use tauri::Manager;
use tauri_nspanel::{tauri_panel, ManagerExt, WebviewWindowExt};

// Keep macro-generated AppKit imports scoped to this module.
tauri_panel! {
    panel!(NotesPanel {
        config: {
            can_become_key_window: true,
            can_become_main_window: false,
            is_floating_panel: true,
            hides_on_deactivate: false
        }
    })
}

pub fn configure(notes: &tauri::WebviewWindow) -> Result<(), String> {
    let window = notes.clone();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    notes
        .run_on_main_thread(move || {
            let _ = sender.send(configure_on_main_thread(&window));
        })
        .map_err(|error| error.to_string())?;
    receiver.recv().map_err(|error| error.to_string())?
}

fn configure_on_main_thread(window: &tauri::WebviewWindow) -> Result<(), String> {
    use tauri_nspanel::objc2_app_kit::{NSWindowCollectionBehavior, NSWindowStyleMask};

    // Reopening notes reuses the panel; only convert a native window once.
    let panel = match window.app_handle().get_webview_panel(window.label()) {
        Ok(panel) => panel,
        Err(_) => {
            let panel = window
                .to_panel::<NotesPanel>()
                .map_err(|error| error.to_string())?;
            let app = window.app_handle().clone();
            let label = window.label().to_string();
            window.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::CloseRequested { .. }) {
                    // Restore Tauri's native class before it closes the window.
                    // A later open_notes_workspace can create a fresh panel.
                    if let Ok(panel) = app.get_webview_panel(&label) {
                        panel.to_window();
                        app.remove_webview_panel(&label);
                    }
                }
            });
            panel
        }
    };
    panel
        .add_style_mask(NSWindowStyleMask::NonactivatingPanel)
        .map_err(|error| error.to_string())?;
    panel.set_released_when_closed(false);

    let mut behavior = panel.as_panel().collectionBehavior();
    behavior.remove(
        NSWindowCollectionBehavior::Managed
            | NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::MoveToActiveSpace
            | NSWindowCollectionBehavior::FullScreenPrimary
            | NSWindowCollectionBehavior::FullScreenNone,
    );
    behavior.insert(
        NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    if tauri_nspanel::objc2::available!(macos = 13.0) {
        // These Stage Manager/fullscreen roles are mutually exclusive.
        behavior
            .remove(NSWindowCollectionBehavior::Primary | NSWindowCollectionBehavior::Auxiliary);
        behavior.insert(NSWindowCollectionBehavior::CanJoinAllApplications);
    }
    panel.set_collection_behavior(behavior);
    // Order the configured panel into the active Space without activating Hitr.
    panel.order_front_regardless();
    Ok(())
}
