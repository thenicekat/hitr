use tauri::{AppHandle, State};
use tokio::sync::Mutex;

#[derive(Default)]
pub struct Installer(Mutex<()>);

#[cfg(target_os = "macos")]
fn is_installable_bundle() -> bool {
    // Source and debug runs use a standalone executable under target/.
    !cfg!(debug_assertions)
        && std::env::current_exe()
            .ok()
            .and_then(|path| path.ancestors().nth(3).map(std::path::Path::to_path_buf))
            .is_some_and(|bundle| bundle.extension().is_some_and(|ext| ext == "app"))
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    if is_installable_bundle() {
        return Ok(find_update(&app).await?.map(|update| update.version));
    }
    let _ = app;
    Ok(None)
}

#[cfg(target_os = "macos")]
async fn find_update(app: &AppHandle) -> Result<Option<tauri_plugin_updater::Update>, String> {
    use tauri_plugin_updater::UpdaterExt;
    app.updater_builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
pub fn check_from_menu(app: AppHandle) {
    check_in_background(app, true);
}

#[cfg(target_os = "macos")]
pub fn start_automatic_checks(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            check_in_background(app.clone(), false);
            tokio::time::sleep(std::time::Duration::from_secs(24 * 60 * 60)).await;
        }
    });
}

#[cfg(target_os = "macos")]
fn check_in_background(app: AppHandle, manual: bool) {
    use tauri::Manager;
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};

    tauri::async_runtime::spawn(async move {
        if !is_installable_bundle() {
            if manual {
                app.dialog()
                    .message("Update checks are available in an installed Hitr app.")
                    .title("Check for Updates")
                    .show(|_| {});
            }
            return;
        }
        match find_update(&app).await {
            Ok(Some(update)) => {
                let version = update.version;
                let callback_app = app.clone();
                app.dialog()
                    .message(format!(
                        "Hitr {version} is available. Update and restart now?"
                    ))
                    .title("Update Available")
                    .buttons(MessageDialogButtons::OkCancelCustom(
                        "Update and Restart".into(),
                        "Later".into(),
                    ))
                    .show(move |install| {
                        if install {
                            tauri::async_runtime::spawn(async move {
                                if let Err(error) = install_update(
                                    callback_app.clone(),
                                    callback_app.state::<Installer>(),
                                    version,
                                )
                                .await
                                {
                                    callback_app
                                        .dialog()
                                        .message(format!("Update failed: {error}"))
                                        .title("Update Failed")
                                        .show(|_| {});
                                }
                            });
                        }
                    });
            }
            Ok(None) if manual => app
                .dialog()
                .message("Hitr is up to date.")
                .title("Check for Updates")
                .show(|_| {}),
            Err(error) if manual => app
                .dialog()
                .message(format!("Could not check for updates: {error}"))
                .title("Update Check Failed")
                .show(|_| {}),
            _ => {}
        }
    });
}

#[tauri::command]
pub async fn install_update(
    app: AppHandle,
    installer: State<'_, Installer>,
    version: String,
) -> Result<(), String> {
    let _guard = installer
        .0
        .try_lock()
        .map_err(|_| "An update is already running.")?;

    #[cfg(target_os = "macos")]
    if is_installable_bundle() {
        let update = find_update(&app)
            .await?
            .filter(|update| update.version == version)
            .ok_or("This update is no longer available. Restart Hitr to check again.")?;
        update
            .download_and_install(|_, _| {}, || {})
            .await
            .map_err(|error| error.to_string())?;
        app.restart();
    }

    let _ = (app, version);
    Err("Updates are available for installed macOS app bundles.".into())
}
