// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::Path;

use serde::Serialize;

use crate::{history, index::MAX_NOTE_BYTES, note_rel, safe_rel, vaultfs, Index, TRASH_DIR};

#[derive(Serialize)]
pub struct Merge {
    pub content: String,
    pub trashed: bool,
    pub warning: Option<String>,
}

fn read_note(vault: &vaultfs::Vault, path: &Path) -> Result<String, String> {
    // Use the actual spelling on disk so aliases cannot merge a note into itself.
    vault.with_shell_path(path, |absolute, is_dir| {
        if is_dir || absolute.canonicalize().ok().as_deref() != Some(absolute) {
            Err("Select a note at its current vault path".to_owned())
        } else {
            Ok(())
        }
    }).map_err(|error| error.to_string())??;
    let bytes = vault.p_read(path, MAX_NOTE_BYTES).map_err(|error| error.to_string())?;
    String::from_utf8(bytes).map_err(|_| "The note is not valid UTF-8".to_owned())
}

pub fn run(root: &Path, index: &mut Index, from: &str, to: &str, trash_source: bool) -> Result<Merge, String> {
    let from_key = safe_rel(from).ok_or("Invalid source note")?;
    let to_key = safe_rel(to).ok_or("Invalid destination note")?;
    if from_key == to_key { return Err("Choose a different destination note".into()); }
    let stem = from_key.file_name().and_then(|s| s.to_str()).ok_or("Invalid source note")?;
    let from = note_rel(from).ok_or("Invalid source note")?;
    let to = note_rel(to).ok_or("Invalid destination note")?;
    let vault = vaultfs::Vault::open(root).map_err(|error| error.to_string())?;
    let source = read_note(&vault, &from).map_err(|error| format!("Could not read the source note: {error}"))?;
    let destination = read_note(&vault, &to).map_err(|error| format!("Could not read the destination note: {error}"))?;
    let body = source.strip_prefix('\u{feff}').unwrap_or(&source);
    if body.trim().is_empty() { return Err("The source note has no content to merge".into()); }
    let mut content = destination.clone();
    if !destination.trim_start_matches('\u{feff}').is_empty() {
        let crlf = destination.as_bytes().windows(2).any(|pair| pair == b"\r\n");
        let eol = if crlf { "\r\n" } else { "\n" };
        if !content.ends_with('\n') { content.push_str(eol); }
        if !content.ends_with("\n\n") && !content.ends_with("\n\r\n") { content.push_str(eol); }
    }
    content.push_str(body);
    if content.len() as u64 > MAX_NOTE_BYTES { return Err("The combined note exceeds the 32 MiB note limit".into()); }

    // Check the trash path before committing, and never remove the source before the destination is saved.
    let trash = Path::new(TRASH_DIR);
    if trash_source { vault.mkdir_p(trash).map_err(|error| format!("Could not open the vault trash: {error}"))?; }
    history::snapshot(&vault, &to, destination.as_bytes()).map_err(|error| format!("Could not save the destination's previous version: {error}"))?;
    if !vault.replace_if_unchanged(&to, destination.as_bytes(), content.as_bytes())
        .map_err(|error| format!("Could not save the destination note: {error}"))? {
        return Err("The destination note changed during the merge. Try again.".into());
    }
    index.upsert(&opensidian_lib::vault_path_string(&to_key), &content);

    let mut merged = Merge { content, trashed: false, warning: None };
    if trash_source {
        match vault.trash_if_unchanged(&from, trash, stem, "md", source.as_bytes()) {
            Ok(_) => {
                index.remove(&opensidian_lib::vault_path_string(&from_key));
                merged.trashed = true;
            }
            Err(error) => merged.warning = Some(format!("The merge was saved. The source note was kept: {error}")),
        }
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("opensidian-merge-{}", opensidian_lib::os_random_hex().unwrap()));
            fs::create_dir_all(root.join("Project")).unwrap();
            fs::write(root.join("Source.md"), "# Café\n![[image.png]]\n").unwrap();
            fs::write(root.join("Project/Target.md"), "# Target\r\n").unwrap();
            Self(root)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn appends_content_and_saves_destination_history() {
        let root = Fixture::new();
        let mut index = Index::build(&root.0);
        let merge = run(&root.0, &mut index, "Source", "Project/Target", false).unwrap();
        assert!(!merge.trashed);
        assert_eq!(merge.content, "# Target\r\n\r\n# Café\n![[image.png]]\n");
        assert_eq!(index.content("Project/Target"), Some(merge.content.as_str()));
        assert!(root.0.join("Source.md").exists());
        let vault = vaultfs::Vault::open(&root.0).unwrap();
        let path = Path::new("Project/Target.md");
        let revisions = history::list(&vault, path).unwrap();
        assert_eq!(history::read(&vault, path, &revisions[0].id).unwrap(), b"# Target\r\n");
    }

    #[test]
    fn optionally_moves_source_to_trash_after_saving_destination() {
        let root = Fixture::new();
        let mut index = Index::build(&root.0);
        let merge = run(&root.0, &mut index, "Source", "Project/Target", true).unwrap();
        assert!(merge.trashed && merge.warning.is_none());
        assert_eq!(fs::read_to_string(root.0.join(".trash/Source.md")).unwrap(), "# Café\n![[image.png]]\n");
        assert!(!root.0.join("Source.md").exists());
        assert!(index.content("Source").is_none());
    }

    #[test]
    fn refuses_self_merge_and_preserves_source_when_destination_is_locked() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = Fixture::new();
        let mut index = Index::build(&root.0);
        for to in ["Source", "source", "../Outside", "Missing"] {
            assert!(run(&root.0, &mut index, "Source", to, true).is_err(), "{to}");
        }
        let target = root.0.join("Project/Target.md");
        let _handle = fs::OpenOptions::new().read(true).share_mode(1 | 2).open(&target).unwrap();
        assert!(run(&root.0, &mut index, "Source", "Project/Target", true).is_err());
        assert!(root.0.join("Source.md").exists());
        assert_eq!(fs::read(&target).unwrap(), b"# Target\r\n");
        assert_eq!(index.content("Project/Target"), Some("# Target\r\n"));
    }

    #[test]
    fn reports_saved_merge_if_the_source_cannot_be_moved() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = Fixture::new();
        let mut index = Index::build(&root.0);
        let source = root.0.join("Source.md");
        let _handle = fs::OpenOptions::new().read(true).share_mode(1 | 2).open(&source).unwrap();
        let merged = run(&root.0, &mut index, "Source", "Project/Target", true).unwrap();
        assert!(!merged.trashed && merged.warning.is_some());
        assert!(source.exists());
        assert_eq!(fs::read_to_string(root.0.join("Project/Target.md")).unwrap(), merged.content);
        assert_eq!(index.content("Project/Target"), Some(merged.content.as_str()));
    }
}
