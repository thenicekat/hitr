//! Keep the icon's desktop position separate from the expanded editor's frame.

use std::sync::Mutex;
use tauri::{LogicalSize, Manager, PhysicalPosition, PhysicalSize};

const ICON_SIZE: LogicalSize<f64> = LogicalSize::new(76.0, 76.0);
const EDITOR_SIZE: LogicalSize<f64> = LogicalSize::new(380.0, 520.0);

#[derive(Clone, Copy, Debug, PartialEq)]
struct Display {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    scale: f64,
}

impl From<tauri::Monitor> for Display {
    fn from(monitor: tauri::Monitor) -> Self {
        Self {
            position: monitor.work_area().position,
            size: monitor.work_area().size,
            scale: monitor.scale_factor(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Anchor {
    position: PhysicalPosition<i32>,
    display: Option<Display>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Frame {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    scale: f64,
}

#[derive(Default)]
struct Session {
    icon: Option<Anchor>,
}

impl Session {
    fn resize(
        &mut self,
        expanded: bool,
        current: Anchor,
        displays: &[Display],
        scale: f64,
        apply: impl FnOnce(Frame) -> Result<(), String>,
    ) -> Result<(), String> {
        let anchor = match (expanded, self.icon) {
            (true, None) => current,
            (false, Some(icon)) => icon,
            // Repeated opens must not replace the icon anchor with the editor's
            // position, including after the user drags the expanded editor.
            _ => return Ok(()),
        };
        apply(frame(anchor, expanded, displays, scale))?;
        // Retain the old state if the native operation fails, so it can be retried.
        self.icon = expanded.then_some(anchor);
        Ok(())
    }
}

#[derive(Default)]
pub struct NotesPlacement(Mutex<Session>);

pub fn reset(app: &tauri::AppHandle) {
    *app.state::<NotesPlacement>().0.lock().unwrap() = Session::default();
}

fn clamp(
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    display: Display,
) -> PhysicalPosition<i32> {
    let max_x = (i64::from(display.position.x) + i64::from(display.size.width)
        - i64::from(size.width))
    .max(i64::from(display.position.x));
    let max_y = (i64::from(display.position.y) + i64::from(display.size.height)
        - i64::from(size.height))
    .max(i64::from(display.position.y));
    PhysicalPosition::new(
        i64::from(position.x).clamp(i64::from(display.position.x), max_x) as i32,
        i64::from(position.y).clamp(i64::from(display.position.y), max_y) as i32,
    )
}

fn distance(position: PhysicalPosition<i32>, display: Display) -> i128 {
    let x = i64::from(position.x);
    let y = i64::from(position.y);
    let dx = x - x.clamp(
        i64::from(display.position.x),
        i64::from(display.position.x) + i64::from(display.size.width),
    );
    let dy = y - y.clamp(
        i64::from(display.position.y),
        i64::from(display.position.y) + i64::from(display.size.height),
    );
    i128::from(dx).pow(2) + i128::from(dy).pow(2)
}

fn frame(anchor: Anchor, expanded: bool, displays: &[Display], fallback_scale: f64) -> Frame {
    let display = anchor
        .display
        .filter(|saved| displays.contains(saved))
        .or_else(|| {
            displays
                .iter()
                .copied()
                .min_by_key(|display| distance(anchor.position, *display))
        });
    let scale = display.map_or(fallback_scale, |display| display.scale);
    let mut size = if expanded { EDITOR_SIZE } else { ICON_SIZE }.to_physical::<u32>(scale);
    let mut position = anchor.position;
    if let Some(display) = display {
        if expanded {
            // Small displays still need access to the editor's collapse control.
            size.width = size.width.min(display.size.width);
            size.height = size.height.min(display.size.height);
        }
        if expanded || anchor.display != Some(display) {
            // Restore exactly where the user left the icon on an unchanged
            // display. Only recover its position if that display changed or left.
            position = clamp(position, size, display);
        }
    }
    Frame {
        position,
        size,
        scale,
    }
}

fn on_main_thread(
    window: &tauri::WebviewWindow,
    action: impl FnOnce(&tauri::WebviewWindow) -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    let notes = window.clone();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || {
            let _ = sender.send(action(&notes));
        })
        .map_err(|e| e.to_string())?;
    receiver.recv().map_err(|e| e.to_string())?
}

pub fn resize(window: &tauri::WebviewWindow, expanded: bool) -> Result<(), String> {
    on_main_thread(window, move |notes| {
        let current = Anchor {
            position: notes.outer_position().map_err(|e| e.to_string())?,
            display: notes
                .current_monitor()
                .map_err(|e| e.to_string())?
                .map(Display::from),
        };
        let scale = notes.scale_factor().map_err(|e| e.to_string())?;
        let displays: Vec<_> = notes
            .available_monitors()
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(Display::from)
            .collect();
        notes.state::<NotesPlacement>().0.lock().unwrap().resize(
            expanded,
            current,
            &displays,
            scale,
            |frame| apply_frame(notes, frame),
        )
    })
}

pub fn move_icon(window: &tauri::WebviewWindow, x: i32, y: i32) -> Result<(), String> {
    on_main_thread(window, move |notes| {
        // A final drag IPC can arrive after an open. It must not move the editor
        // or replace the icon anchor captured when that open completed.
        if notes
            .state::<NotesPlacement>()
            .0
            .lock()
            .unwrap()
            .icon
            .is_some()
        {
            return Ok(());
        }
        apply_frame(
            notes,
            Frame {
                position: PhysicalPosition::new(x, y),
                size: notes.outer_size().map_err(|e| e.to_string())?,
                scale: notes.scale_factor().map_err(|e| e.to_string())?,
            },
        )
    })
}

#[cfg(target_os = "macos")]
fn apply_frame(window: &tauri::WebviewWindow, target: Frame) -> Result<(), String> {
    use tauri_nspanel::{NSPoint, NSRect, NSSize, NSWindow};

    let handle = window.ns_window().map_err(|e| e.to_string())?;
    // Tauri owns this window; all access is on AppKit's main thread.
    let native = unsafe { &*(handle as *const NSWindow) };
    let current = native.frame();
    let position = window.outer_position().map_err(|e| e.to_string())?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let screen_top = current.origin.y + current.size.height + f64::from(position.y) / scale;
    let position = target.position.to_logical::<f64>(target.scale);
    let size = target.size.to_logical::<f64>(target.scale);
    // Resize and move together. Separate Tauri setters enqueue AppKit work,
    // allowing resize anchoring/screen constraints to undo a restored position.
    native.setFrame_display(
        NSRect::new(
            NSPoint::new(position.x, screen_top - position.y - size.height),
            NSSize::new(size.width, size.height),
        ),
        true,
    );
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn apply_frame(window: &tauri::WebviewWindow, target: Frame) -> Result<(), String> {
    window
        .set_size(target.size.to_logical::<f64>(target.scale))
        .map_err(|e| e.to_string())?;
    window
        .set_position(target.position)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(x: i32, y: i32, width: u32, height: u32, scale: f64) -> Display {
        Display {
            position: PhysicalPosition::new(x, y),
            size: PhysicalSize::new(width, height),
            scale,
        }
    }

    fn anchor(x: i32, y: i32, display: Display) -> Anchor {
        Anchor {
            position: PhysicalPosition::new(x, y),
            display: Some(display),
        }
    }

    fn fits(frame: Frame, display: Display) {
        assert!(frame.position.x >= display.position.x);
        assert!(frame.position.y >= display.position.y);
        assert!(
            i64::from(frame.position.x) + i64::from(frame.size.width)
                <= i64::from(display.position.x) + i64::from(display.size.width)
        );
        assert!(
            i64::from(frame.position.y) + i64::from(frame.size.height)
                <= i64::from(display.position.y) + i64::from(display.size.height)
        );
    }

    #[test]
    fn every_edge_and_corner_restores_the_icon_without_drift() {
        for scale in [1.0, 1.25, 1.5, 2.0] {
            for (x, y) in [(0, 48), (-2560, 100), (300, -1800)] {
                let display = display(x, y, 2400, 1500, scale);
                let icon_size = ICON_SIZE.to_physical::<u32>(scale);
                for icon_x in [x, x + 1200, x + 2400 - icon_size.width as i32] {
                    for icon_y in [y, y + 750, y + 1500 - icon_size.height as i32] {
                        let icon = anchor(icon_x, icon_y, display);
                        let mut session = Session::default();
                        for _ in 0..20 {
                            let mut editor = None;
                            session
                                .resize(true, icon, &[display], scale, |frame| {
                                    editor = Some(frame);
                                    Ok(())
                                })
                                .unwrap();
                            fits(editor.unwrap(), display);
                            let mut restored = None;
                            let moved_editor = anchor(x + 50, y + 70, display);
                            session
                                .resize(false, moved_editor, &[display], scale, |frame| {
                                    restored = Some(frame);
                                    Ok(())
                                })
                                .unwrap();
                            let restored = restored.unwrap();
                            assert_eq!(restored.position, icon.position);
                            assert_eq!(restored.size, icon_size);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn repeated_opens_and_collapses_do_not_replace_the_anchor() {
        let display = display(0, 50, 1920, 1000, 1.0);
        let icon = anchor(1800, 900, display);
        let mut session = Session::default();
        session
            .resize(false, icon, &[display], 1.0, |_| {
                panic!("already collapsed")
            })
            .unwrap();
        session
            .resize(true, icon, &[display], 1.0, |_| Ok(()))
            .unwrap();
        let moved = anchor(100, 100, display);
        session
            .resize(true, moved, &[display], 1.0, |_| panic!("already expanded"))
            .unwrap();
        session
            .resize(false, moved, &[display], 1.0, |frame| {
                assert_eq!(frame.position, icon.position);
                Ok(())
            })
            .unwrap();
        session
            .resize(false, moved, &[display], 1.0, |_| {
                panic!("already collapsed")
            })
            .unwrap();
        // The next user placement replaces the old anchor on the next open.
        session
            .resize(true, moved, &[display], 1.0, |_| Ok(()))
            .unwrap();
        session
            .resize(false, icon, &[display], 1.0, |frame| {
                assert_eq!(frame.position, moved.position);
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn partially_offscreen_user_placement_is_preserved_on_the_same_display() {
        let display = display(0, 50, 1920, 1000, 1.0);
        let icon = anchor(-20, 1020, display);
        fits(frame(icon, true, &[display], 1.0), display);
        assert_eq!(frame(icon, false, &[display], 1.0).position, icon.position);
    }

    #[test]
    fn dragging_the_editor_to_a_different_display_keeps_the_icon_on_its_original_display() {
        let left = display(-2560, -300, 2560, 1440, 1.0);
        let main = display(0, 66, 3024, 1718, 2.0);
        for (icon, editor) in [
            (anchor(2850, 1600, main), anchor(-2400, -200, left)),
            (anchor(-100, 900, left), anchor(1000, 1000, main)),
        ] {
            let mut session = Session::default();
            session
                .resize(
                    true,
                    icon,
                    &[left, main],
                    icon.display.unwrap().scale,
                    |_| Ok(()),
                )
                .unwrap();
            session
                .resize(
                    false,
                    editor,
                    &[left, main],
                    editor.display.unwrap().scale,
                    |frame| {
                        assert_eq!(frame.position, icon.position);
                        assert_eq!(
                            frame.size,
                            ICON_SIZE.to_physical::<u32>(icon.display.unwrap().scale)
                        );
                        Ok(())
                    },
                )
                .unwrap();
        }
    }

    #[test]
    fn disconnected_display_restores_to_the_nearest_remaining_display() {
        let left = display(-1920, 0, 1920, 1080, 1.0);
        let main = display(0, 48, 3024, 1800, 2.0);
        let right = display(3024, 0, 1920, 1080, 1.0);
        let icon = anchor(-1500, 900, left);
        let recovered = frame(icon, false, &[main, right], 1.0);
        fits(recovered, main);
        assert_eq!(recovered.position, PhysicalPosition::new(0, 900));
        assert_eq!(recovered.size, PhysicalSize::new(152, 152));
    }

    #[test]
    fn changed_resolution_scale_or_dock_recovers_an_accessible_icon() {
        let old = display(0, 0, 3840, 2160, 2.0);
        let icon = anchor(3600, 1950, old);
        for new in [
            display(0, 40, 1920, 1000, 1.0),
            display(0, 50, 3840, 1800, 2.0),
        ] {
            fits(frame(icon, false, &[new], old.scale), new);
        }
    }

    #[test]
    fn editor_fits_displays_smaller_than_its_default_size() {
        for scale in [1.0, 1.25, 2.0] {
            let display = display(-200, 30, 300, 400, scale);
            let editor = frame(anchor(90, 390, display), true, &[display], scale);
            fits(editor, display);
            assert_eq!(editor.size, PhysicalSize::new(300, 400));
        }
    }

    #[test]
    fn unavailable_monitor_information_still_preserves_the_anchor() {
        let icon = Anchor {
            position: PhysicalPosition::new(-500, 900),
            display: None,
        };
        assert_eq!(frame(icon, true, &[], 2.0).position, icon.position);
        let collapsed = frame(icon, false, &[], 2.0);
        assert_eq!(collapsed.position, icon.position);
        assert_eq!(collapsed.size, PhysicalSize::new(152, 152));
    }

    #[test]
    fn failed_native_operations_can_be_retried_without_losing_the_anchor() {
        let display = display(0, 50, 1920, 1000, 1.0);
        let icon = anchor(1800, 900, display);
        let moved = anchor(200, 200, display);
        let mut session = Session::default();
        assert!(session
            .resize(true, icon, &[display], 1.0, |_| Err("failed".into()))
            .is_err());
        assert!(session.icon.is_none());
        session
            .resize(true, icon, &[display], 1.0, |_| Ok(()))
            .unwrap();
        assert!(session
            .resize(false, moved, &[display], 1.0, |_| Err("failed".into()))
            .is_err());
        session
            .resize(false, moved, &[display], 1.0, |frame| {
                assert_eq!(frame.position, icon.position);
                Ok(())
            })
            .unwrap();
    }
}
