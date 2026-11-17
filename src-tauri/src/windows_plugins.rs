// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use std::path::Path;
use std::sync::{atomic::{AtomicBool, Ordering}, Mutex};
use tauri::{Emitter, Manager, State, Window};
use ::windows::core::{w, PCWSTR};
use ::windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, IDNO, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO};

#[derive(Default)]
pub struct WindowsPlugins {
    pages: Mutex<HashMap<String, bool>>,
    epoch: Mutex<u64>,
    prompting: AtomicBool,
}

impl WindowsPlugins {
    pub fn set_page(&self, label: &str, trusted: bool) {
        self.pages.lock().unwrap().insert(label.to_owned(), trusted);
    }

    pub fn remove_page(&self, label: &str) {
        self.pages.lock().unwrap().remove(label);
    }

    pub fn page_state(&self, label: &str, root: &Path) -> serde_json::Value {
        let mut state = super::plugins_state_of(root);
        state["pageEval"] = self.pages.lock().unwrap().get(label).copied().unwrap_or(false).into();
        state
    }
}

async fn ask_trust(win: &Window, root: &Path) -> Result<bool, String> {
    let text = format!(
        "Allow enabled community plugins to run in this vault?\n\n{}\n\nPlugins are third-party code and can read and modify files in the vault.",
        dunce::simplified(root).display()
    );
    let text: Vec<_> = text.encode_utf16().chain([0]).collect();
    let owner = win.clone();
    let (reply, answer) = std::sync::mpsc::sync_channel(1);
    win.run_on_main_thread(move || {
        let result = owner.hwnd().map_err(|error| error.to_string()).and_then(|owner| {
            let button = unsafe {
                MessageBoxW(Some(owner), PCWSTR(text.as_ptr()), w!("Trust community plugins?"),
                    MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2)
            };
            match button {
                IDYES => Ok(true),
                IDNO => Ok(false),
                _ => Err("Could not show the Windows trust dialog".into()),
            }
        });
        // The WebView may have closed while the native dialog was open.
        let _ = reply.send(result);
    }).map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || answer.recv())
        .await.map_err(|error| error.to_string())?.map_err(|error| error.to_string())?
}

#[tauri::command]
pub async fn plugins_set(
    win: Window, v: State<'_, super::Vault>, trusted: Option<bool>, id: Option<String>, on: Option<bool>,
) -> Result<serde_json::Value, String> {
    let root = super::cur_vault(&v).ok_or("no vault open")?;
    let key = super::plugins::trust_key(&root);
    let plugins = win.state::<WindowsPlugins>();
    let epoch = *plugins.epoch.lock().unwrap();
    if trusted == Some(true) && !super::plugins::is_trusted_key(&super::cfg_value(), &key) {
        if plugins.prompting.swap(true, Ordering::AcqRel) {
            return Err("The Windows trust dialog is already open".into());
        }
        struct Prompt<'a>(&'a AtomicBool);
        impl Drop for Prompt<'_> {
            fn drop(&mut self) { self.0.store(false, Ordering::Release); }
        }
        let _prompt = Prompt(&plugins.prompting);
        if !ask_trust(&win, &root).await? {
            return Ok(plugins.page_state(win.label(), &root));
        }
    }
    {
        let mut current = plugins.epoch.lock().unwrap();
        if let Some(trust) = trusted {
            if trust && epoch != *current {
                return Err("Vault trust changed while the request was open. Try again.".into());
            }
            super::cfgstore::update_in(
                &super::vaultlock::env_lock_dir(), &super::cfg_path(),
                &[super::cfgstore::Op::SetIn(super::plugins::TRUST_KEY.into(), key, trust.into())],
            ).map_err(|error| format!("Could not save vault trust: {error}"))?;
            *current = current.wrapping_add(1);
        }
        if let (Some(id), Some(on)) = (id.as_deref(), on) {
            super::plugins::set_enabled(&root, id, on)?;
        }
    }
    // Other windows have independent WebViews and must reconcile their loaded plugins.
    let _ = win.app_handle().emit("vault-plugins-changed", win.label());
    Ok(plugins.page_state(win.label(), &root))
}

#[cfg(test)]
#[test]
fn windows_plugin_origin_is_available_only_in_trusted_pages() {
    let header = "default-src 'self'; script-src 'self'; connect-src ipc:";
    assert_eq!(super::csp_by_trust(header, false), header);
    let trusted = super::csp_by_trust(header, true);
    assert!(trusted.contains("script-src 'self' 'unsafe-eval' blob: http://opensidian-plugin.localhost;"));
}
