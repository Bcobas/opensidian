// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, EventTarget, Manager, State, Window, WindowEvent};

#[derive(Clone, serde::Serialize)]
pub struct NoteWindowSeed {
    name: String,
    mode: String,
    pinned: bool,
}

struct NoteWindow {
    seed: NoteWindowSeed,
    ready: Option<mpsc::SyncSender<()>>,
}

#[derive(Default)]
pub struct NoteWindows {
    notes: Mutex<HashMap<String, NoteWindow>>,
    armed: Mutex<HashSet<String>>,
}

#[tauri::command]
pub async fn open_note_window(
    app: AppHandle, vault: State<'_, super::Vault>, name: String, mode: String, pinned: bool,
) -> Result<(), String> {
    let root = super::cur_vault(&vault).ok_or("No vault open")?;
    let path = super::note_rel(&name).ok_or("Invalid note name")?;
    let fs = super::vaultfs::Vault::open(&root).map_err(|error| error.to_string())?;
    super::read_property_note(&fs, &path)?;
    if !matches!(mode.as_str(), "livepreview" | "source" | "reading") {
        return Err("Invalid note view".into());
    }
    let mut config = app.config().app.windows.first().cloned().ok_or("App window is not configured")?;
    let label = format!("vault-note-{}", opensidian_lib::os_random_hex().map_err(|error| error.to_string())?);
    config.label = label.clone();
    config.title = format!("{} — Opensidian", name.rsplit('/').next().unwrap_or(&name));
    config.visible = false;
    let (ready, receive) = mpsc::sync_channel(1);
    let windows = app.state::<NoteWindows>();
    windows.notes.lock().unwrap().insert(label.clone(), NoteWindow {
        seed: NoteWindowSeed { name, mode, pinned },
        ready: Some(ready),
    });
    let result = async {
        super::page_window(&app, &config).map_err(|error| error.to_string())?;
        tauri::async_runtime::spawn_blocking(move || receive.recv_timeout(Duration::from_secs(20)))
            .await.map_err(|error| error.to_string())?
            .map_err(|_| "The new note window did not finish opening".to_owned())
    }.await;
    if result.is_err() {
        windows.notes.lock().unwrap().remove(&label);
        if let Some(window) = app.get_webview_window(&label) {
            // A window which failed to start has no edits to flush.
            let _ = window.destroy();
        }
    }
    result
}

#[tauri::command]
pub fn note_window_seed(win: Window, windows: State<NoteWindows>) -> Option<NoteWindowSeed> {
    windows.notes.lock().unwrap().get(win.label()).map(|window| window.seed.clone())
}

#[tauri::command]
pub fn note_window_ready(win: Window, windows: State<NoteWindows>) -> Result<(), String> {
    let ready = windows.notes.lock().unwrap().get_mut(win.label())
        .ok_or("This is not a secondary note window")?.ready.take();
    win.show().map_err(|error| error.to_string())?;
    // The originating window may have already closed.
    if let Some(ready) = ready { let _ = ready.send(()); }
    Ok(())
}

#[tauri::command]
pub fn arm_window_close(win: Window, windows: State<NoteWindows>) {
    windows.armed.lock().unwrap().insert(win.label().to_owned());
}

#[tauri::command]
pub fn close_window_ready(win: Window) -> Result<(), String> {
    win.destroy().map_err(|error| error.to_string())
}

pub fn on_event(win: &Window, event: &WindowEvent) {
    let windows = win.state::<NoteWindows>();
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            if windows.armed.lock().unwrap().contains(win.label())
                && win.emit_to(EventTarget::labeled(win.label()), "vault-window-close", ()).is_ok()
            {
                api.prevent_close();
            }
        }
        WindowEvent::Destroyed => {
            windows.armed.lock().unwrap().remove(win.label());
            windows.notes.lock().unwrap().remove(win.label());
        }
        _ => {}
    }
}

#[tauri::command]
pub fn notify_note_windows(win: Window, change: super::watcher::Change) {
    let app = win.app_handle();
    for label in app.webview_windows().keys().filter(|label| *label != win.label()) {
        // A peer can close between listing and emitting.
        let _ = app.emit_to(EventTarget::labeled(label), "vault-peer-change", &change);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{fs, history, note_rel, vaultfs, write_note_if_current, Index};

    #[test]
    fn second_window_cannot_overwrite_a_newer_revision() {
        let root = std::env::temp_dir().join(format!("opensidian-window-{}", opensidian_lib::os_random_hex().unwrap()));
        fs::create_dir(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        let original = "# Beginning\r\n";
        let path = note_rel("Plan").unwrap();
        fs::write(root.join(&path), original).unwrap();
        let mut index = Index::build(&root);
        let committed = "# First window\r\n";
        write_note_if_current(&root, &mut index, "Plan", committed, original).unwrap();
        assert!(write_note_if_current(&root, &mut index, "Plan", "# Second window\n", original).is_err());
        assert_eq!(fs::read_to_string(root.join(&path)).unwrap(), committed);
        assert_eq!(index.content("Plan"), Some(committed));
        let vault = vaultfs::Vault::open(&root).unwrap();
        let revision = &history::list(&vault, &path).unwrap()[0].id;
        assert_eq!(history::read(&vault, &path, revision).unwrap(), original.as_bytes());
        fs::remove_file(root.join(&path)).unwrap();
        assert!(write_note_if_current(&root, &mut index, "Plan", "# Removed\n", committed).is_err());
        assert!(!root.join(&path).exists());
    }
}
