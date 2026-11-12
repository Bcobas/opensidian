// SPDX-License-Identifier: GPL-3.0-or-later

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::{index::MAX_NOTE_BYTES, vaultfs::Vault};

const MAX_VERSIONS: usize = 30;
const MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Serialize)]
pub struct Revision {
    pub id: String,
    pub timestamp: u64,
    pub bytes: u64,
}

fn folder(note: &Path) -> PathBuf {
    Path::new(".obsidian/opensidian-history").join(note)
}

fn timestamp(id: &str) -> Option<u64> {
    let (time, suffix) = id.strip_suffix(".rev")?.split_once('-')?;
    if time.len() != 16 || suffix.len() != 16 || !suffix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    time.parse().ok()
}

pub fn list(vault: &Vault, note: &Path) -> io::Result<Vec<Revision>> {
    let dir = folder(note);
    let entries = match vault.p_list(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut revisions = Vec::new();
    for (id, kind) in entries {
        if kind != "file" { continue; }
        let Some(timestamp) = timestamp(&id) else { continue };
        let stat = vault.p_stat(&dir.join(&id))?;
        if stat.kind == "file" && stat.size <= MAX_NOTE_BYTES {
            revisions.push(Revision { id, timestamp, bytes: stat.size });
        }
    }
    revisions.sort_unstable_by(|a, b| b.id.cmp(&a.id));
    Ok(revisions)
}

pub fn read(vault: &Vault, note: &Path, id: &str) -> io::Result<Vec<u8>> {
    timestamp(id).ok_or_else(|| io::Error::other("Invalid revision"))?;
    vault.p_read(&folder(note).join(id), MAX_NOTE_BYTES)
}

pub fn snapshot(vault: &Vault, note: &Path, previous: &[u8]) -> io::Result<()> {
    let revisions = list(vault, note)?;
    if let Some(last) = revisions.first() {
        if last.bytes == previous.len() as u64 && read(vault, note, &last.id)? == previous {
            return Ok(());
        }
    }
    let time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    let time = revisions.first().map_or(time, |last| time.max(last.timestamp.saturating_add(1)));
    let id = format!("{time:016}-{}.rev", opensidian_lib::os_random_hex()?);
    let dir = folder(note);
    vault.write_atomic(&dir.join(id), previous, true)?;

    let mut retained = previous.len() as u64;
    for (index, revision) in revisions.iter().enumerate() {
        if index >= MAX_VERSIONS - 1 || retained.saturating_add(revision.bytes) > MAX_BYTES {
            if let Err(error) = vault.unlink(&dir.join(&revision.id)) {
                eprintln!("[history] could not prune a revision: {error}");
            }
        } else {
            retained += revision.bytes;
        }
    }
    Ok(())
}

pub fn before_save(vault: &Vault, note: &Path, next: &[u8]) -> io::Result<()> {
    let stat = match vault.p_stat(note) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if stat.kind == "file" && stat.size <= MAX_NOTE_BYTES {
        let previous = vault.p_read(note, MAX_NOTE_BYTES)?;
        if previous != next {
            snapshot(vault, note, &previous)?;
        }
    }
    Ok(())
}

pub fn move_to(vault: &Vault, from: &Path, to: &Path) -> io::Result<()> {
    let revisions = list(vault, from)?;
    if revisions.is_empty() { return Ok(()); }
    let source = folder(from);
    let destination = folder(to);
    vault.mkdir_p(&destination)?;
    for revision in revisions {
        let target = destination.join(&revision.id);
        if !vault.is_file(&target) {
            vault.move_file(&source.join(revision.id), &target)?;
        }
    }
    let _ = vault.p_remove(&source, true);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_previous_versions_after_save_rename_and_restore() {
        let root = std::env::temp_dir().join(format!("opensidian-history-{}", opensidian_lib::os_random_hex().unwrap()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
        }
        let _cleanup = Cleanup(root.clone());
        let fs = Vault::open(&root).unwrap();
        let first = Path::new("Note.md");
        let renamed = Path::new("Renamed.md");
        fs.write_atomic(first, b"old", false).unwrap();
        before_save(&fs, first, b"old").unwrap();
        assert!(list(&fs, first).unwrap().is_empty());
        before_save(&fs, first, b"new").unwrap();
        before_save(&fs, first, b"new").unwrap();
        fs.write_atomic(first, b"new", false).unwrap();
        fs.move_file(first, renamed).unwrap();
        move_to(&fs, first, renamed).unwrap();
        let revisions = list(&fs, renamed).unwrap();
        assert_eq!(revisions.len(), 1);
        assert_eq!(read(&fs, renamed, &revisions[0].id).unwrap(), b"old");
        let mut ix = crate::Index::build(&root);
        assert!(crate::restore_revision_in(&root, &mut ix, "Renamed", &revisions[0].id, b"wrong").is_err());
        assert_eq!(crate::restore_revision_in(&root, &mut ix, "Renamed", &revisions[0].id, b"new").unwrap(), "old");
        assert_eq!(ix.content("Renamed"), Some("old"));
        let after = list(&fs, renamed).unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(read(&fs, renamed, &after[0].id).unwrap(), b"new");
    }
}
