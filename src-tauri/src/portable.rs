// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

use crate::{index::MAX_NOTE_BYTES, safe_rel, vaultfs::Vault};

const MANIFEST: &str = "opensidian-package.json";
const MAX_PACKAGE: u64 = 256 * 1024 * 1024;
const MAX_FILES: usize = 2048;

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    entry: String,
}

#[derive(Serialize)]
pub struct Preview {
    pub files: Vec<String>,
    pub linked: Vec<String>,
    pub omitted: Vec<String>,
    pub token: String,
}

pub struct Package {
    pub preview: Preview,
    entry: String,
    source: PathBuf,
    notes: BTreeMap<PathBuf, Note>,
    files: BTreeMap<PathBuf, String>,
}

struct Note {
    text: String,
    refs: Vec<Reference>,
}

struct Reference {
    range: Range<usize>,
    label: Range<usize>,
    wiki_label: Option<String>,
    target: Option<PathBuf>,
    fragment: String,
    image: bool,
    original: String,
}

fn fail(error: impl std::fmt::Display) -> String { error.to_string() }

fn relative(source: &Path, target: &str) -> Option<PathBuf> {
    if target.starts_with('/') || target.contains(['\\', ':', '\0']) { return None; }
    let mut path = source.parent()?.to_path_buf();
    for part in target.split('/') {
        match part {
            "." | "" => {},
            ".." => { if !path.pop() { return None; } },
            _ => path.push(part),
        }
    }
    safe_rel(&opensidian_lib::vault_path_string(&path))
}

fn resolve(fs: &Vault, catalog: &[String], source: &Path, name: &str, wiki: bool) -> Option<PathBuf> {
    let decoded = crate::pct_decode(name);
    for name in if wiki { [Some(name), decoded.as_deref()] } else { [decoded.as_deref(), None] }.into_iter().flatten() {
        let direct = relative(source, name);
        for path in direct.into_iter().chain(safe_rel(name)) {
            if fs.is_file(&path) { return Some(path); }
            if wiki && !note_file(&path) {
                for extension in ["md", "markdown"] {
                    let note = path.with_file_name(format!("{}.{extension}", path.file_name()?.to_string_lossy()));
                    if fs.is_file(&note) { return Some(note); }
                }
            }
        }
        if name.contains(['\\', ':']) || name.split('/').any(|s| s.starts_with('.')) { continue; }
        let suffix = format!("/{name}");
        let md = format!("{name}.md");
        let md_suffix = format!("/{md}");
        let mut hits = catalog.iter().filter(|item| {
            *item == name || item.ends_with(&suffix) || (wiki && (*item == &md || item.ends_with(&md_suffix)))
        });
        if let Some(path) = hits.next().filter(|_| hits.next().is_none()) {
            if let Some(path) = safe_rel(path).filter(|path| fs.is_file(path)) { return Some(path); }
        }
    }
    None
}

fn standard_label(raw: &str, image: bool) -> Option<Range<usize>> {
    if !raw.starts_with(if image { "![" } else { "[" }) { return None; }
    let start = if image { 2 } else { 1 };
    let mut depth = 1;
    let mut escaped = false;
    for (i, c) in raw.get(start..)?.char_indices() {
        if escaped { escaped = false; continue; }
        match c {
            '\\' => escaped = true,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 { return Some(start..i + start); }
            },
            _ => {},
        }
    }
    None
}

fn references(fs: &Vault, catalog: &[String], source: &Path, text: &str) -> Vec<Reference> {
    let opts = Options::ENABLE_WIKILINKS | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let mut refs = Vec::new();
    let mut pending = Vec::new();
    for (event, range) in Parser::new_ext(text, opts).into_offset_iter() {
        match event {
            Event::Start(Tag::Image { dest_url, .. } | Tag::Link { dest_url, .. }) => {
                pending.push((dest_url, range.start));
            }
            Event::End(tag @ (TagEnd::Image | TagEnd::Link)) => {
                let Some((dest, start)) = pending.pop() else { continue };
                let range = start..range.end;
                let Some(raw) = text.get(range.clone()) else { continue };
                let image = tag == TagEnd::Image;
                let wiki = raw.strip_prefix("![[").or_else(|| raw.strip_prefix("[[")).and_then(|s| s.strip_suffix("]]"));
                let (name, fragment, label, wiki_label) = if let Some(wiki) = wiki {
                    let (name, fragment, alias) = crate::index::link_parts(wiki);
                    let label = if alias.is_empty() { if name.is_empty() { fragment.trim_start_matches('#') } else { name } } else { alias };
                    (name, fragment.to_owned(), 0..0, Some(label.to_owned()))
                } else {
                    let Some(label) = standard_label(raw, image) else { continue };
                    let (name, frag) = dest.split_once('#').map_or((dest.as_ref(), String::new()), |(name, frag)| (name, format!("#{frag}")));
                    (name, frag, label, None)
                };
                let remote = name.starts_with("//") || crate::url_scheme(name).is_some();
                if remote && !image { continue; }
                let path = if remote {
                    None
                } else if name.is_empty() {
                    if wiki.is_none() || fragment.is_empty() { continue; }
                    Some(source.to_owned())
                } else { resolve(fs, catalog, source, name, wiki.is_some()) };
                refs.push(Reference {
                    range, label: (start + label.start)..(start + label.end), wiki_label,
                    target: path, fragment, image, original: name.to_owned(),
                });
            }
            _ => {},
        }
    }
    refs
}

fn note_file(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown"))
}

fn reserve_name(path: &Path, used: &mut BTreeSet<String>) -> String {
    let stem = path.file_stem().expect("file stem").to_string_lossy();
    let (stem, extension, folder) = if note_file(path) {
        let stem = stem.trim_start_matches('.');
        let valid = !stem.is_empty() && crate::vaultfs::valid_component(std::ffi::OsStr::new(&format!("{stem}.md")));
        (if valid { stem } else { "Note" }, ".md".to_owned(), "")
    } else {
        let extension = path.extension().map(|s| format!(".{}", s.to_string_lossy())).unwrap_or_default();
        (stem.as_ref(), extension, "attachments/")
    };
    for counter in 1.. {
        let name = if counter == 1 { format!("{folder}{stem}{extension}") } else { format!("{folder}{stem} ({counter}){extension}") };
        if used.insert(name.to_lowercase()) { return name; }
    }
    unreachable!()
}

pub fn plan(fs: &Vault, mut catalog: Vec<String>, entry: &str, selected: &[String]) -> Result<Package, String> {
    let entry = crate::note_rel(entry).ok_or("Invalid note name")?;
    catalog.sort_unstable();
    catalog.dedup();
    plan_entries(fs, Some(&catalog), entry, selected, |path| {
        String::from_utf8(fs.p_read(path, MAX_NOTE_BYTES).map_err(fail)?).map_err(fail)
    })
}

#[cfg(windows)]
pub fn plan_document(
    fs: &Vault, entry: &Path, selected: &[String], read: impl Fn(&Path) -> Result<String, String>,
) -> Result<Package, String> {
    if !note_file(entry) || entry.components().count() != 1 || !entry.file_name().is_some_and(crate::vaultfs::valid_component) {
        return Err("Invalid Markdown document".to_owned());
    }
    // A standalone document has no vault-wide index. Resolve explicit paths only.
    plan_entries(fs, None, entry.to_owned(), selected, read)
}

fn plan_entries(
    fs: &Vault, catalog: Option<&[String]>, entry: PathBuf, selected: &[String],
    read: impl Fn(&Path) -> Result<String, String>,
) -> Result<Package, String> {
    if selected.len() >= MAX_FILES { return Err("Too many selected notes".to_owned()); }
    let mut notes = BTreeMap::new();
    let mut chosen = BTreeSet::from([entry.clone()]);
    for name in selected {
        let path = safe_rel(name).filter(|path| note_file(path)).ok_or("Invalid linked note")?;
        let known = catalog.map_or_else(|| fs.is_file(&path), |catalog| {
            catalog.binary_search(&opensidian_lib::vault_path_string(&path)).is_ok()
        });
        if !known { return Err("Linked note no longer exists".to_owned()); }
        chosen.insert(path);
    }

    let mut files = chosen.clone();
    let mut linked = BTreeSet::new();
    let mut omitted = BTreeSet::new();
    let mut fingerprint = Sha256::new();
    let mut size = 0;
    for path in &chosen {
        let text = read(path)?;
        if text.len() as u64 > MAX_NOTE_BYTES { return Err("A Markdown document exceeds the size limit".to_owned()); }
        size += text.len() as u64;
        if size > MAX_PACKAGE { return Err("Package exceeds 256 MiB".to_owned()); }
        let refs = references(fs, catalog.unwrap_or_default(), path, &text);
        for reference in &refs {
            match reference.target.as_deref() {
                Some(target) if note_file(target) => {
                    if target != entry { linked.insert(opensidian_lib::vault_path_string(target)); }
                    if !chosen.contains(target) { omitted.insert(opensidian_lib::vault_path_string(target)); }
                }
                Some(target) => { files.insert(target.to_owned()); }
                None => { omitted.insert(reference.original.clone()); }
            }
        }
        fingerprint.update(Sha256::digest(text.as_bytes()));
        notes.insert(path.clone(), Note { text, refs });
    }
    if files.len() >= MAX_FILES { return Err("Too many files in the package".to_owned()); }
    let mut used = BTreeSet::new();
    let files = files.into_iter().map(|path| {
        let name = reserve_name(&path, &mut used);
        fingerprint.update(opensidian_lib::vault_path_string(&path));
        fingerprint.update(b"\0");
        if !note_file(&path) {
            let stat = fs.p_stat(&path).map_err(fail)?;
            size += stat.size;
            fingerprint.update(stat.size.to_le_bytes());
            fingerprint.update(stat.mtime_ms.to_le_bytes());
        }
        Ok((path, name))
    }).collect::<Result<BTreeMap<_, _>, String>>()?;
    if size > MAX_PACKAGE { return Err("Package exceeds 256 MiB".to_owned()); }
    let preview = Preview {
        files: files.keys().map(|path| opensidian_lib::vault_path_string(path)).collect(),
        linked: linked.into_iter().collect(), omitted: omitted.into_iter().collect(),
        token: format!("{:x}", fingerprint.finalize()),
    };
    Ok(Package { entry: files[&entry].clone(), source: entry, preview, notes, files })
}

fn splice(text: &str, range: Range<usize>, edits: &[(Range<usize>, String)]) -> String {
    let mut out = String::new();
    let mut at = range.start;
    for (edit, replacement) in edits {
        if edit.start < at || edit.end > range.end { continue; }
        out.push_str(&text[at..edit.start]);
        out.push_str(replacement);
        at = edit.end;
    }
    out.push_str(&text[at..range.end]);
    out
}

fn rewrite(note: &Note, files: &BTreeMap<PathBuf, String>) -> String {
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for reference in &note.refs {
        let Some(target) = reference.target.as_ref().and_then(|path| files.get(path)) else { continue };
        let label = reference.wiki_label.as_ref().map_or_else(
            || splice(&note.text, reference.label.clone(), &edits),
            |text| text.replace('\\', "\\\\").replace('[', "\\[").replace(']', "\\]"),
        );
        let bang = if reference.image && crate::index::IMG_EXTS.iter().any(|ext| target.to_ascii_lowercase().ends_with(&format!(".{ext}"))) { "!" } else { "" };
        let fragment = reference.fragment.strip_prefix('#').map_or(String::new(), |frag| {
            let decoded = crate::pct_decode(frag).unwrap_or_else(|| frag.to_owned());
            let encoded = crate::pct_encode(&decoded);
            let encoded = if target.to_ascii_lowercase().ends_with(".pdf") { encoded.replace("%3D", "=").replace("%26", "&") } else { encoded };
            format!("#{encoded}")
        });
        let replacement = format!("{bang}[{label}]({}{fragment})", crate::pct_encode(target));
        edits.retain(|(range, _)| !(range.start >= reference.range.start && range.end <= reference.range.end));
        edits.push((reference.range.clone(), replacement));
    }
    edits.sort_by_key(|(range, _)| range.start);
    splice(&note.text, 0..note.text.len(), &edits)
}

pub fn export(fs: &Vault, package: Package, expected: &str) -> Result<String, String> {
    if expected != package.preview.token { return Err("The package contents changed. Review the files again before exporting.".to_owned()); }
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut size = 0;
    zip.start_file(MANIFEST, options).map_err(fail)?;
    let manifest = Manifest { version: 1, entry: package.entry };
    serde_json::to_writer(&mut zip, &manifest).map_err(fail)?;
    for (path, name) in &package.files {
        let data = if let Some(note) = package.notes.get(path) { rewrite(note, &package.files).into_bytes() }
            else { fs.p_read(path, MAX_PACKAGE - size).map_err(fail)? };
        if note_file(path) && data.len() as u64 > MAX_NOTE_BYTES { return Err("A rewritten note exceeds the size limit".to_owned()); }
        size += data.len() as u64;
        if size > MAX_PACKAGE { return Err("Package exceeds 256 MiB".to_owned()); }
        zip.start_file(name, options).map_err(fail)?;
        zip.write_all(&data).map_err(fail)?;
    }
    let data = zip.finish().map_err(fail)?.into_inner();
    if data.len() as u64 > MAX_PACKAGE { return Err("ZIP file exceeds 256 MiB".to_owned()); }
    let note = package.source;
    let stem = note.file_stem().ok_or("Invalid note name")?.to_string_lossy();
    for counter in 1..=10_000 {
        let suffix = if counter == 1 { String::new() } else { format!(" ({counter})") };
        let destination = note.with_file_name(format!("{stem}{suffix}.opensidian.zip"));
        #[cfg(windows)]
        let written = fs.write_new_atomic(&destination, &data);
        #[cfg(not(windows))]
        let written = fs.create_new(&destination, false).and_then(|mut file| file.write_all(&data));
        match written {
            Ok(()) => return Ok(opensidian_lib::vault_path_string(&destination)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
            Err(error) => return Err(fail(error)),
        }
    }
    Err("Could not find an available filename".to_owned())
}

pub struct Session {
    pub directory: PathBuf,
    pub entry: String,
    pub title: String,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl Session {
    pub fn cleanup(&self) {
        if self.directory.canonicalize().as_ref().is_ok_and(|path| path == &self.directory) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

pub fn is_package(path: &Path) -> bool {
    path.file_name().and_then(|s| s.to_str()).is_some_and(|s| s.to_ascii_lowercase().ends_with(".opensidian.zip"))
}

pub fn launch(path: &Path) -> Result<(), String> {
    std::process::Command::new(std::env::current_exe().map_err(fail)?)
        .arg(path).env_remove("VAULT_DIR").spawn().map(drop).map_err(fail)
}

pub fn unpack(path: &Path) -> Result<Session, String> {
    let file = std::fs::File::open(path).map_err(fail)?;
    if file.metadata().map_err(fail)?.len() > MAX_PACKAGE { return Err("Package exceeds 256 MiB".to_owned()); }
    let mut zip = ZipArchive::new(file).map_err(fail)?;
    if zip.len() > MAX_FILES { return Err("Too many files in the package".to_owned()); }
    let mut data = Vec::new();
    zip.by_name(MANIFEST).map_err(|_| "This ZIP is not an Opensidian package")?.take(65537).read_to_end(&mut data).map_err(fail)?;
    if data.len() > 65536 { return Err("Package manifest is too large".to_owned()); }
    let manifest: Manifest = serde_json::from_slice(&data).map_err(fail)?;
    let archive_note = |path: &Path| path.components().count() == 1 && path.extension().is_some_and(|ext| ext == "md");
    let entry = safe_rel(&manifest.entry).filter(|path| archive_note(path))
        .ok_or("Invalid package entry note")?;
    if manifest.version != 1 { return Err("Unsupported package version".to_owned()); }
    let title = path.file_name().ok_or("Invalid package filename")?.to_string_lossy().into_owned();
    let directory = std::env::temp_dir().canonicalize().map_err(fail)?.join(format!("opensidian-package-{}", opensidian_lib::os_random_hex().map_err(fail)?));
    std::fs::create_dir(&directory).map_err(fail)?;
    let session = Session { directory, entry: entry.file_stem().unwrap().to_string_lossy().into_owned(), title };
    let fs = Vault::open(&session.directory).map_err(fail)?;
    let mut used = BTreeSet::new();
    let mut total = 0;
    for index in 0..zip.len() {
        let mut file = zip.by_index(index).map_err(fail)?;
        if file.name() == MANIFEST { continue; }
        let raw = file.name();
        let safe = safe_rel(raw).filter(|path| opensidian_lib::vault_path_string(path) == raw)
            .filter(|path| archive_note(path)
                || (path.components().count() == 2 && path.starts_with("attachments")))
            .ok_or("Unexpected path in the package")?;
        if !used.insert(raw.to_lowercase()) || file.is_dir() || file.is_symlink() { return Err("Unexpected file in the package".to_owned()); }
        let cap = if note_file(&safe) { MAX_NOTE_BYTES } else { MAX_PACKAGE };
        let limit = cap.min(MAX_PACKAGE - total);
        if file.size() > limit { return Err("Package exceeds the extraction size limit".to_owned()); }
        let mut bytes = Vec::new();
        (&mut file).take(limit + 1).read_to_end(&mut bytes).map_err(fail)?;
        if bytes.len() as u64 > limit { return Err("Package exceeds the extraction size limit".to_owned()); }
        total += bytes.len() as u64;
        fs.write_atomic(&safe, &bytes, false).map_err(fail)?;
    }
    if !fs.is_file(&entry) { return Err("The main note is missing from the package".to_owned()); }
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Session {
        let directory = std::env::temp_dir().canonicalize().unwrap().join(format!("opensidian-test-package-{}", opensidian_lib::os_random_hex().unwrap()));
        std::fs::create_dir(&directory).unwrap();
        Session { directory, entry: String::new(), title: String::new() }
    }

    #[test]
    fn includes_only_reviewed_files_and_rewrites_relative_links() {
        let original = fixture();
        let fs = Vault::open(&original.directory).unwrap();
        let input = "# Main\n\n[![Image](../media/image%20one.png)](../Other.md#Detail)\n\n![[document.pdf#page=2]]\n\n[[Private]]\n\n`![[private.png]]`\n";
        for (path, bytes) in [
            ("folder/Main.md", input.as_bytes()), ("Other.md", b"# Detail\n\n[Back](folder/Main.md)".as_slice()),
            ("Private.md", b"private"), ("media/image one.png", b"image"), ("media/private.png", b"private image"),
            ("media/document.pdf", b"%PDF-1.4"), (".obsidian/plugin.json", b"private config"),
        ] { fs.write_atomic(Path::new(path), bytes, false).unwrap(); }
        let catalog = ["folder/Main.md", "Other.md", "Private.md", "media/image one.png", "media/document.pdf", "media/private.png"];
        let catalog: Vec<String> = catalog.into_iter().map(str::to_owned).collect();
        let selected = vec!["Other.md".to_owned()];
        let package = plan(&fs, catalog.clone(), "folder/Main", &selected).unwrap();
        assert_eq!(package.preview.omitted, ["Private.md"]);
        assert_eq!(package.preview.files, ["Other.md", "folder/Main.md", "media/document.pdf", "media/image one.png"]);
        let token = package.preview.token.clone();
        assert!(export(&fs, package, "stale").is_err());
        let package = plan(&fs, catalog, "folder/Main", &selected).unwrap();
        let archive = export(&fs, package, &token).unwrap();
        let opened = unpack(&original.directory.join(archive)).unwrap();
        assert_eq!(opened.entry, "Main");
        let text = std::fs::read_to_string(opened.directory.join("Main.md")).unwrap();
        assert!(text.contains("[![Image](attachments/image%20one.png)](Other.md#Detail)"), "{text:?}");
        assert!(text.contains("[document.pdf](attachments/document.pdf#page=2)"), "{text:?}");
        assert!(text.contains("`![[private.png]]`"));
        assert!(!opened.directory.join("Private.md").exists());
        assert!(!opened.directory.join(".obsidian").exists());
        assert_eq!(std::fs::read(opened.directory.join("attachments/image one.png")).unwrap(), b"image");
        let ix = crate::Index::build(&opened.directory);
        assert_eq!(ix.backlinks("Main"), ["Other"]);
        assert_eq!(ix.backlinks("Other"), ["Main"]);
        assert_eq!(std::fs::read_to_string(original.directory.join("folder/Main.md")).unwrap(), input);
    }

    #[test]
    fn rejects_archive_paths_that_leave_the_temporary_vault() {
        let root = fixture();
        let path = root.directory.join("invalid.opensidian.zip");
        let mut zip = ZipWriter::new(std::fs::File::create(&path).unwrap());
        zip.start_file(MANIFEST, SimpleFileOptions::default()).unwrap();
        zip.write_all(br#"{"version":1,"entry":"Main.md"}"#).unwrap();
        zip.start_file("../escape.md", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"no").unwrap();
        drop(zip.finish().unwrap());
        assert!(unpack(&path).err().unwrap().contains("Unexpected path"));
    }
}
