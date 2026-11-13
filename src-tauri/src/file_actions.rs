// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;

use opensidian_lib::vaultfs::Vault;
use serde::Deserialize;
use ::windows::Win32::Foundation::HWND;

use crate::{media_mime, safe_rel, windows, IMG_TYPES};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    CopyPath,
    OpenDefault,
    Reveal,
}

fn with_target<T>(
    root: &Path, path: &str, action: Action, use_target: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<T, String> {
    let rel = safe_rel(path).ok_or("Invalid vault path")?;
    let vault = Vault::open(root).map_err(|error| format!("Could not open the vault: {error}"))?;
    vault.with_shell_path(&rel, |target, is_dir| {
        if let Action::OpenDefault = action {
            let ext = rel.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase();
            let supported = matches!(ext.as_str(), "md" | "markdown" | "txt" | "pdf" | "canvas" | "base")
                || IMG_TYPES.iter().any(|(image, _)| *image == ext)
                || media_mime(&ext).is_some();
            if is_dir || !supported {
                return Err("Choose a note, document, canvas or media file".into());
            }
        }
        use_target(target)
    }).map_err(|error| format!("Could not access the vault item: {error}"))?
}

pub fn run(owner: HWND, root: &Path, path: &str, action: Action) -> Result<(), String> {
    with_target(root, path, action, |target| match action {
        Action::CopyPath => windows::copy_path(owner, target).map_err(|error| format!("Could not copy the path: {error}")),
        Action::OpenDefault => windows::open_file(owner, target),
        Action::Reveal => windows::reveal_path(target).map_err(|error| format!("Could not show the item in Explorer: {error}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn selects_nested_notes_attachments_and_folders() {
        let root = std::env::temp_dir().join(format!("opensidian-shell-select-{}", std::process::id()));
        fs::create_dir_all(root.join("Proyecto.md")).unwrap();
        fs::write(root.join("Proyecto.md/Notas de café.md"), "# Café").unwrap();
        fs::write(root.join("Proyecto.md/Imagen.PNG"), "").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let note = with_target(&root, "Proyecto.md/Notas de café.md", Action::OpenDefault, |path| Ok(path.to_owned()));
        assert_eq!(note.unwrap(), canonical_root.join("Proyecto.md").join("Notas de café.md"));
        assert!(with_target(&root, "Proyecto.md/Imagen.PNG", Action::OpenDefault, |_| Ok(())).is_ok());
        for action in [Action::CopyPath, Action::Reveal] {
            assert!(with_target(&root, "Proyecto.md", action, |_| Ok(())).is_ok());
        }
        assert!(with_target::<()>(&root, "Proyecto.md", Action::OpenDefault, |_| panic!("folder dispatched")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unsafe_or_missing_paths_and_blocks_executable_dispatch() {
        let root = std::env::temp_dir().join(format!("opensidian-shell-refuse-{}", std::process::id()));
        fs::create_dir_all(root.join(".obsidian")).unwrap();
        fs::write(root.join(".obsidian/app.json"), "{}").unwrap();
        fs::write(root.join("run.cmd"), "").unwrap();

        for path in ["", "../other.md", "C:/other.md", ".obsidian/app.json", "missing.md", "run.cmd:stream"] {
            let result = with_target::<()>(&root, path, Action::Reveal, |_| panic!("unsafe path dispatched"));
            assert!(result.is_err(), "{path}");
        }
        assert!(with_target::<()>(&root, "run.cmd", Action::OpenDefault, |_| panic!("executable dispatched")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
