// SPDX-License-Identifier: GPL-3.0-or-later

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{history, note_rel, safe_rel, vaultfs, Index, TRASH_DIR};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Operation {
    Move { to: String },
    Trash,
}

#[derive(Serialize)]
pub struct Change {
    path: String,
    warnings: Vec<String>,
}

fn check_directory(vault: &vaultfs::Vault, rel: &Path) -> io::Result<()> {
    if rel.as_os_str().is_empty() { return Ok(()); }
    vault.with_shell_path(rel, |path, is_dir| {
        if !is_dir {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "expected a folder"));
        }
        // The index stores the on-disk spelling, even on a case-insensitive volume.
        if path.canonicalize()? != path {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "use the folder's current path"));
        }
        Ok(())
    })?
}

fn trash(vault: &vaultfs::Vault, from: &Path) -> io::Result<PathBuf> {
    let trash = Path::new(TRASH_DIR);
    vault.mkdir_p(trash)?;
    let slot = trash.join(format!("folder-{}", opensidian_lib::os_random_hex()?));
    vault.p_mkdir(&slot, false)?;
    let target = slot.join(from.file_name().expect("validated folder path"));
    if let Err(error) = vault.move_dir(from, &target) {
        let _ = vault.p_remove(&slot, true);
        return Err(error);
    }
    Ok(target)
}

pub fn run(root: &Path, index: &mut Index, from: &str, operation: Operation) -> Result<Change, String> {
    let source = safe_rel(from).ok_or("Invalid source folder")?;
    let from = opensidian_lib::vault_path_string(&source);
    let vault = vaultfs::Vault::open(root).map_err(|e| e.to_string())?;
    check_directory(&vault, &source).map_err(|e| format!("Could not access the source folder: {e}"))?;
    let Operation::Move { to } = operation else {
        let target = trash(&vault, &source).map_err(|e| format!("Could not move the folder to the vault trash: {e}"))?;
        index.relocate_folder(&from, None);
        return Ok(Change { path: opensidian_lib::vault_path_string(&target), warnings: Vec::new() });
    };

    let target = safe_rel(&to).ok_or("Invalid destination folder")?;
    let to = opensidian_lib::vault_path_string(&target);
    if target == source {
        return Ok(Change { path: to, warnings: Vec::new() });
    }
    if target.starts_with(&source) {
        return Err("Choose a destination outside the folder being moved".into());
    }
    check_directory(&vault, target.parent().unwrap_or(Path::new("")))
        .map_err(|e| format!("Could not access the destination folder: {e}"))?;
    vault.move_dir(&source, &target).map_err(|e| match e.kind() {
        io::ErrorKind::AlreadyExists => "An item already exists at the destination".into(),
        _ => format!("Could not move the folder: {e}"),
    })?;

    let prefix = format!("{from}/");
    let moved: Vec<_> = index.names().iter().filter_map(|name| {
        let suffix = name.strip_prefix(&prefix)?;
        Some((note_rel(name)?, note_rel(&format!("{to}/{suffix}"))?))
    }).collect();
    index.relocate_folder(&from, Some(&to));
    let mut warnings = Vec::new();
    if let Err(error) = crate::move_folder_bookmarks(root, &prefix, &format!("{to}/")) {
        warnings.push(format!("Could not update bookmarks: {error}"));
    }
    let history_failures = moved.into_iter().filter(|(old, new)| {
        history::move_to(&vault, old, new).is_err()
    }).count();
    if history_failures > 0 {
        warnings.push(format!("Could not move version history for {history_failures} note(s)"));
    }
    Ok(Change { path: to, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("opensidian-folders-{}", opensidian_lib::os_random_hex().unwrap()));
            fs::create_dir_all(path.join("Café/sub")).unwrap();
            fs::create_dir(path.join("Archive")).unwrap();
            fs::write(path.join("Café/sub/Plan.md"), "# Plan\n![[Photo.png]]").unwrap();
            fs::write(path.join("Café/sub/Photo.png"), b"photo").unwrap();
            fs::write(path.join("Café.md"), "Outside the folder").unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn moves_nested_content_and_history_and_reindexes_paths() {
        let root = Fixture::new();
        let vault = vaultfs::Vault::open(&root.0).unwrap();
        history::snapshot(&vault, Path::new("Café/sub/Plan.md"), b"previous version").unwrap();
        let mut index = Index::build(&root.0);
        let change = run(&root.0, &mut index, "Café", Operation::Move { to: "Archive/Project".into() }).unwrap();
        assert_eq!(change.path, "Archive/Project");
        assert!(change.warnings.is_empty(), "{:?}", change.warnings);
        assert!(!root.0.join("Café").exists());
        assert_eq!(fs::read(root.0.join("Archive/Project/sub/Photo.png")).unwrap(), b"photo");
        assert_eq!(index.names(), ["Archive/Project/sub/Plan", "Café"]);
        assert_eq!(index.images(), ["Archive/Project/sub/Photo.png"]);
        assert_eq!(history::list(&vault, Path::new("Archive/Project/sub/Plan.md")).unwrap().len(), 1);
    }

    #[test]
    fn renames_a_folder_with_a_case_only_change() {
        let root = Fixture::new();
        let mut index = Index::build(&root.0);
        let change = run(&root.0, &mut index, "Archive", Operation::Move { to: "archive".into() }).unwrap();
        assert_eq!(change.path, "archive");
        let paths: Vec<_> = fs::read_dir(&root.0).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert!(paths.iter().any(|name| name == "archive"));
        assert!(!paths.iter().any(|name| name == "Archive"));
    }

    #[test]
    fn updates_bookmark_paths_without_losing_groups_or_metadata() {
        let root = Fixture::new();
        fs::create_dir(root.0.join(".obsidian")).unwrap();
        let bookmarks = root.0.join(".obsidian/bookmarks.json");
        let original = serde_json::json!({ "extra": true, "items": [{
            "type": "group", "ctime": 10, "title": "Work", "items": [
                {"type": "file", "ctime": 20, "path": "Café/sub/Plan.md", "title": "Plan", "custom": 1},
                {"type": "file", "ctime": 21, "path": "Café/sub/Guide.pdf"},
                {"type": "file", "ctime": 22, "path": "Café.md"}
            ]
        }] });
        fs::write(&bookmarks, original.to_string()).unwrap();
        let mut index = Index::build(&root.0);
        let change = run(&root.0, &mut index, "Café", Operation::Move { to: "Renamed".into() }).unwrap();
        assert!(change.warnings.is_empty(), "{:?}", change.warnings);
        let actual: serde_json::Value = serde_json::from_slice(&fs::read(&bookmarks).unwrap()).unwrap();
        let mut expected = original;
        expected["items"][0]["items"][0]["path"] = "Renamed/sub/Plan.md".into();
        expected["items"][0]["items"][1]["path"] = "Renamed/sub/Guide.pdf".into();
        assert_eq!(actual, expected);
    }

    #[test]
    fn refuses_collisions_and_descendants_then_trashes_the_whole_tree() {
        let root = Fixture::new();
        let mut index = Index::build(&root.0);
        for target in ["Archive", "../Outside", "Café/sub/Project", "C:/Outside", "Missing/Project"] {
            assert!(run(&root.0, &mut index, "Café", Operation::Move { to: target.into() }).is_err(), "{target}");
        }
        assert!(run(&root.0, &mut index, "Café.md", Operation::Trash).is_err());
        assert!(run(&root.0, &mut index, "", Operation::Trash).is_err());
        assert!(index.content("Café/sub/Plan").is_some());
        let trashed = run(&root.0, &mut index, "Café", Operation::Trash).unwrap().path;
        assert!(trashed.starts_with(".trash/"));
        assert_eq!(fs::read(root.0.join(trashed).join("sub/Photo.png")).unwrap(), b"photo");
        assert!(!root.0.join("Café").exists());
        assert_eq!(index.names(), ["Café"]);
        assert!(index.images().is_empty());
    }
}
