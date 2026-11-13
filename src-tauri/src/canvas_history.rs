// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::{Path, PathBuf};

use crate::{canvasfs, history, vaultfs::Vault};

fn location(root: &Path, path: &str) -> Result<(Vault, PathBuf), String> {
    let rel = canvasfs::canvas_rel("history", path)?;
    let vault = Vault::open(root).map_err(|error| error.to_string())?;
    Ok((vault, rel))
}

pub fn list(root: &Path, path: &str) -> Result<Vec<history::Revision>, String> {
    let (vault, rel) = location(root, path)?;
    history::list(&vault, &rel).map_err(|error| error.to_string())
}

pub fn compare(root: &Path, path: &str, id: &str) -> Result<(String, String), String> {
    let (vault, rel) = location(root, path)?;
    let current = vault.p_read(&rel, canvasfs::CANVAS_MAX).map_err(|error| error.to_string())?;
    let previous = history::read(&vault, &rel, id).map_err(|error| error.to_string())?;
    Ok((text(current)?, text(previous)?))
}

fn text(bytes: Vec<u8>) -> Result<String, String> {
    String::from_utf8(bytes).map_err(|_| "The canvas is not valid UTF-8".to_owned())
}

pub fn replace(vault: &Vault, path: &Path, current: &[u8], next: &[u8]) -> Result<(), String> {
    if current == next { return Ok(()); }
    if next.len() as u64 > canvasfs::CANVAS_MAX { return Err("EFBIG: canvas too large".into()); }
    history::snapshot(vault, path, current)
        .map_err(|error| format!("Could not save the previous canvas version: {error}"))?;
    if !vault.replace_if_unchanged(path, current, next).map_err(|error| error.to_string())? {
        return Err("The canvas changed on disk. Open the comparison again.".into());
    }
    Ok(())
}

pub fn write(root: &Path, path: &str, data: Option<&str>) -> Result<serde_json::Value, String> {
    let next = data.ok_or("EINVAL: missing canvas data")?.as_bytes();
    let (vault, rel) = location(root, path)?;
    let current = vault.p_read(&rel, canvasfs::CANVAS_MAX).map_err(|error| error.to_string())?;
    replace(&vault, &rel, &current, next)?;
    Ok(serde_json::Value::Null)
}

pub fn restore(root: &Path, path: &str, id: &str, expected: &[u8]) -> Result<String, String> {
    let (vault, rel) = location(root, path)?;
    let current = vault.p_read(&rel, canvasfs::CANVAS_MAX).map_err(|error| error.to_string())?;
    if current != expected { return Err("The canvas changed on disk. Select the revision again.".into()); }
    let previous = text(history::read(&vault, &rel, id).map_err(|error| error.to_string())?)?;
    replace(&vault, &rel, &current, previous.as_bytes())?;
    Ok(previous)
}

pub fn move_to(root: &Path, from: &str, to: &str) -> Result<(), String> {
    let (vault, from) = location(root, from)?;
    let to = canvasfs::canvas_rel("history", to)?;
    history::move_to(&vault, &from, &to).map_err(|error| error.to_string())
}

pub fn move_folder(vault: &Vault, root: &Path, from: &Path, to: &Path) -> usize {
    canvasfs::list(&root.join(to)).into_iter().filter(|path| {
        let rel = Path::new(path);
        history::move_to(vault, &from.join(rel), &to.join(rel)).is_err()
    }).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("opensidian-canvas-history-{}", opensidian_lib::os_random_hex().unwrap()));
            fs::create_dir_all(root.join("Folder/Sub")).unwrap();
            fs::write(root.join("Folder/Sub/Map.canvas"), "").unwrap();
            Self(root)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn saves_versions_and_restores_only_the_compared_canvas() {
        let root = Fixture::new();
        let path = "Folder/Sub/Map.canvas";
        let first = r#"{"nodes":[],"edges":[]}"#;
        write(&root.0, path, Some(first)).unwrap();
        write(&root.0, path, Some(first)).unwrap();
        let revisions = list(&root.0, path).unwrap();
        assert_eq!(revisions.len(), 1);
        let id = &revisions[0].id;
        assert_eq!(compare(&root.0, path, id).unwrap(), (first.into(), String::new()));
        assert!(restore(&root.0, path, id, b"stale").is_err());
        assert_eq!(restore(&root.0, path, id, first.as_bytes()).unwrap(), "");
        assert_eq!(fs::read(root.0.join(path)).unwrap(), b"");
        let versions = list(&root.0, path).unwrap();
        assert_eq!(compare(&root.0, path, &versions[0].id).unwrap().1, first);
        for bad in ["../Outside.canvas", ".obsidian/Internal.canvas", "Folder/Sub/Note.md"] {
            assert!(list(&root.0, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn keeps_history_for_relinked_and_moved_canvases() {
        let root = Fixture::new();
        let old = "Folder/Sub/Map.canvas";
        let renamed = "Folder/Sub/Renamed.canvas";
        let initial = r#"{"nodes":[{"id":"card","type":"file","file":"Old.md"}],"edges":[]}"#;
        fs::write(root.0.join(old), initial).unwrap();
        canvasfs::op_with_relink(&root.0, "relink", "", Some(r#"{"from":"Old.md","to":"New.md"}"#), replace).unwrap();
        let versions = list(&root.0, old).unwrap();
        let id = &versions[0].id;
        assert_eq!(compare(&root.0, old, id).unwrap().1, initial);
        canvasfs::op(&root.0, "rename", old, Some(renamed)).unwrap();
        move_to(&root.0, old, renamed).unwrap();
        assert!(list(&root.0, old).unwrap().is_empty());
        let vault = Vault::open(&root.0).unwrap();
        vault.move_dir(Path::new("Folder"), Path::new("Moved")).unwrap();
        assert_eq!(move_folder(&vault, &root.0, Path::new("Folder"), Path::new("Moved")), 0);
        assert_eq!(compare(&root.0, "Moved/Sub/Renamed.canvas", id).unwrap().1, initial);
    }
}
