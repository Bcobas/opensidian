// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

use serde::Serialize;
use tauri::{Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder, WindowEvent, Wry};

use crate::{index::MAX_NOTE_BYTES, nav_allowed, vaultfs};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Markdown,
    Text,
}

fn kind(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "md" | "markdown" => Some(Kind::Markdown),
        "txt" => Some(Kind::Text),
        _ => None,
    }
}

pub fn is_document_argument(path: &Path) -> bool {
    kind(path).is_some() && !path.is_dir()
}

fn read_bytes(file: File, limit: u64) -> io::Result<Vec<u8>> {
    let too_large = || io::Error::new(io::ErrorKind::InvalidData, "the document exceeds the size limit");
    if file.metadata()?.len() > limit {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(too_large());
    }
    Ok(bytes)
}

#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

struct Snapshot {
    raw: Vec<u8>,
    text: String,
    encoding: Encoding,
    line_ending: &'static str,
}

impl Snapshot {
    fn read(file: File) -> io::Result<Self> {
        let raw = read_bytes(file, MAX_NOTE_BYTES)?;
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "expected UTF-8 or UTF-16 text");
        let (encoding, decoded) = match raw.as_slice() {
            [0xff, 0xfe, rest @ ..] | [0xfe, 0xff, rest @ ..] => {
                let little = raw[0] == 0xff;
                if rest.len() % 2 != 0 {
                    return Err(invalid());
                }
                let units = rest.chunks_exact(2).map(|p| {
                    if little { u16::from_le_bytes([p[0], p[1]]) } else { u16::from_be_bytes([p[0], p[1]]) }
                }).collect::<Vec<_>>();
                let encoding = if little { Encoding::Utf16Le } else { Encoding::Utf16Be };
                (encoding, String::from_utf16(&units).map_err(|_| invalid())?)
            }
            [0xef, 0xbb, 0xbf, rest @ ..] => {
                (Encoding::Utf8Bom, std::str::from_utf8(rest).map_err(|_| invalid())?.to_owned())
            }
            _ => (Encoding::Utf8, std::str::from_utf8(&raw).map_err(|_| invalid())?.to_owned()),
        };
        let line_ending = if decoded.contains("\r\n") {
            "\r\n"
        } else if decoded.contains('\r') && !decoded.contains('\n') {
            "\r"
        } else {
            "\n"
        };
        let text = decoded.replace("\r\n", "\n").replace('\r', "\n");
        Ok(Self { raw, text, encoding, line_ending })
    }

    fn encode(&self, text: &str) -> Vec<u8> {
        let formatted = text.replace('\n', self.line_ending);
        match self.encoding {
            Encoding::Utf8 => formatted.into_bytes(),
            Encoding::Utf8Bom => [&b"\xef\xbb\xbf"[..], formatted.as_bytes()].concat(),
            Encoding::Utf16Le | Encoding::Utf16Be => {
                let little = matches!(self.encoding, Encoding::Utf16Le);
                let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
                for unit in formatted.encode_utf16() {
                    let encoded = if little { unit.to_le_bytes() } else { unit.to_be_bytes() };
                    bytes.extend_from_slice(&encoded);
                }
                bytes
            }
        }
    }
}

struct OpenedDocument {
    directory: vaultfs::Vault,
    parent: PathBuf,
    name: PathBuf,
    kind: Kind,
    snapshot: Snapshot,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum SaveResult {
    Saved,
    Changed,
}

impl OpenedDocument {
    fn open(path: &Path) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "expected a Markdown or text file path");
        let kind = kind(path).ok_or_else(invalid)?;
        if matches!(
            path.components().next(),
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::DeviceNS(_) | Prefix::Verbatim(_))
        ) {
            return Err(invalid());
        }
        let path = path.canonicalize()?;
        if self::kind(&path) != Some(kind) {
            return Err(invalid());
        }
        let parent = path.parent().ok_or_else(invalid)?.to_owned();
        let name = PathBuf::from(path.file_name().ok_or_else(invalid)?);
        let directory = vaultfs::Vault::open(&parent)?;
        let snapshot = Snapshot::read(directory.open_read(&name)?)?;
        Ok(Self { directory, parent, name, kind, snapshot })
    }

    fn save(&mut self, text: String) -> io::Result<SaveResult> {
        if self.kind != Kind::Markdown {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "use Save as Markdown for text files"));
        }
        if self.snapshot.text == text {
            return Ok(SaveResult::Saved);
        }
        let bytes = self.snapshot.encode(&text);
        check_size(bytes.len())?;
        if !self.directory.replace_if_unchanged(&self.name, &self.snapshot.raw, &bytes)? {
            return Ok(SaveResult::Changed);
        }
        self.snapshot.raw = bytes;
        self.snapshot.text = text;
        Ok(SaveResult::Saved)
    }

    fn export(&self, text: &str) -> io::Result<String> {
        check_size(text.len())?;
        let stem = self.name.file_stem().unwrap_or(self.name.as_os_str()).to_string_lossy();
        let base = if self.kind == Kind::Markdown { format!("{stem} - copy") } else { stem.into_owned() };
        for index in 1..=10_000 {
            let name = if index == 1 { format!("{base}.md") } else { format!("{base} ({index}).md") };
            match self.directory.write_new_atomic(Path::new(&name), text.as_bytes()) {
                Ok(()) => return Ok(name),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "no available name for the Markdown copy"))
    }

    fn image(&self, target: &str) -> Option<(&'static str, Vec<u8>)> {
        let relative = relative_path(target)?;
        let mime = image_mime(&relative)?;
        let file = self.directory.open_read(&relative).ok()?;
        Some((mime, read_bytes(file, crate::MAX_IMG_BYTES).ok()?))
    }

    fn linked_document(&self, target: &str) -> Option<PathBuf> {
        // The renderer already decoded standard links. Decode wiki targets only
        // if the literal name does not exist, to preserve filenames with '%'.
        let decoded = crate::pct_decode(target);
        for candidate in std::iter::once(target).chain(decoded.as_deref()) {
            let Some(relative) = plain_relative_path(candidate) else { continue };
            if kind(&relative).is_some() && self.directory.is_file(&relative) {
                return Some(self.parent.join(&relative));
            }
            for suffix in [".md", ".markdown"] {
                let mut name = relative.as_os_str().to_os_string();
                name.push(suffix);
                if self.directory.is_file(Path::new(&name)) {
                    return Some(self.parent.join(name));
                }
            }
        }
        None
    }
}

fn check_size(size: usize) -> io::Result<()> {
    if size as u64 > MAX_NOTE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "the document exceeds the size limit"));
    }
    Ok(())
}

fn plain_relative_path(target: &str) -> Option<PathBuf> {
    crate::safe_rel(target.trim_start_matches("./"))
}

fn relative_path(target: &str) -> Option<PathBuf> {
    let decoded = crate::pct_decode(target)?;
    plain_relative_path(&decoded)
}

fn image_mime(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    crate::IMG_TYPES.iter().find_map(|&(key, mime)| (key == ext).then_some(mime))
}

fn preview(text: &str) -> String {
    crate::render_with_images(text, &[], true, true, &|target, alt| {
        let Some(path) = relative_path(target).filter(|p| image_mime(p).is_some()) else {
            return crate::image_html(&[], target, alt);
        };
        let path = crate::pct_encode(&opensidian_lib::vault_path_string(&path));
        format!("<img class=\"vimg\" src=\"{}/{path}\" alt=\"{}\">", crate::IMG_ORIGIN, crate::esc(alt))
    })
}

struct Reader {
    file: Mutex<Result<OpenedDocument, String>>,
    ready: AtomicBool,
}

impl Reader {
    fn with_file<T>(&self, f: impl FnOnce(&mut OpenedDocument) -> Result<T, String>) -> Result<T, String> {
        let mut document = self.file.lock().map_err(|e| e.to_string())?;
        f(document.as_mut().map_err(|e| e.clone())?)
    }
}

#[derive(Serialize)]
struct DocumentView {
    kind: Kind,
    text: String,
}

#[tauri::command]
fn standalone_document(reader: State<'_, Reader>) -> Result<DocumentView, String> {
    reader.ready.store(true, Ordering::Release);
    reader.with_file(|doc| {
        eprintln!("[reader] opened standalone document");
        Ok(DocumentView { kind: doc.kind, text: doc.snapshot.text.clone() })
    })
}

#[tauri::command]
fn preview_reader_markdown(text: String) -> Result<String, String> {
    check_size(text.len()).map_err(|e| e.to_string())?;
    Ok(preview(&text))
}

#[tauri::command]
fn save_reader_markdown(reader: State<'_, Reader>, text: String) -> Result<SaveResult, String> {
    check_size(text.len()).map_err(|e| e.to_string())?;
    reader.with_file(|doc| doc.save(text).map_err(|e| e.to_string()))
}

#[tauri::command]
fn export_reader_markdown(reader: State<'_, Reader>, text: String) -> Result<String, String> {
    reader.with_file(|doc| doc.export(&text).map_err(|e| e.to_string()))
}

#[tauri::command]
fn reload_reader_document(reader: State<'_, Reader>) -> Result<String, String> {
    reader.with_file(|doc| {
        doc.snapshot = Snapshot::read(doc.directory.open_read(&doc.name).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        Ok(doc.snapshot.text.clone())
    })
}

#[tauri::command]
fn open_reader_link(url: String) -> Result<(), String> {
    crate::open_external(url)
}

#[tauri::command]
fn open_reader_document(reader: State<'_, Reader>, target: String) -> Result<(), String> {
    let path = reader.with_file(|doc| doc.linked_document(&target).ok_or("Linked document was not found".to_owned()))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe).arg(path).env_remove("VAULT_DIR").spawn()
        .map(drop).map_err(|e| e.to_string())
}

#[tauri::command]
fn close_reader(window: tauri::WebviewWindow) -> Result<(), String> {
    window.destroy().map_err(|e| e.to_string())
}

pub fn run(path: &Path, mut context: tauri::Context<Wry>) {
    let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
    let title = format!("{name} - Opensidian");
    let file = OpenedDocument::open(path).map_err(|error| format!("Could not read {name}. {error}"));

    context.config_mut().app.windows.clear();
    tauri::Builder::default()
        .manage(Reader { file: Mutex::new(file), ready: AtomicBool::new(false) })
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("reader.html".into()))
                .title(title)
                .inner_size(900.0, 700.0)
                .min_inner_size(320.0, 240.0)
                .on_navigation(nav_allowed)
                .build()?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.state::<Reader>().ready.load(Ordering::Acquire)
                    && window.emit("reader-close-requested", ()).is_ok()
                {
                    api.prevent_close();
                }
            }
        })
        .register_uri_scheme_protocol(crate::IMG_SCHEME, |context, request| {
            let path = request.uri().path().trim_start_matches('/');
            let result = context.app_handle().state::<Reader>()
                .with_file(|doc| Ok(doc.image(path))).ok().flatten();
            let response = tauri::http::Response::builder()
                .header("X-Content-Type-Options", "nosniff")
                .header("Cache-Control", "no-store");
            match result {
                Some((mime, bytes)) => response.status(200).header("Content-Type", mime).body(bytes),
                None => response.status(404).body(Vec::new()),
            }.expect("reader image response")
        })
        .invoke_handler(tauri::generate_handler![
            standalone_document, preview_reader_markdown, save_reader_markdown, export_reader_markdown,
            reload_reader_document, open_reader_link, open_reader_document, close_reader
        ])
        .run(context)
        .expect("run standalone document reader");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "opensidian-reader-{}", opensidian_lib::os_random_hex().unwrap()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn edits_original_markdown_and_preserves_bom_and_crlf() {
        let root = Fixture::new();
        let path = root.0.join("Notas de café.MD");
        fs::write(&path, "\u{feff}# Hola\r\n\r\n**Markdown**").unwrap();
        assert!(is_document_argument(&path));
        let mut doc = OpenedDocument::open(&path).unwrap();
        assert_eq!(doc.snapshot.text, "# Hola\n\n**Markdown**");
        assert!(preview(&doc.snapshot.text).contains("<strong>Markdown</strong>"));
        assert_eq!(doc.save("# Adiós\n\nFin".to_owned()).unwrap(), SaveResult::Saved);
        assert_eq!(fs::read_to_string(&path).unwrap(), "\u{feff}# Adiós\r\n\r\nFin");
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[test]
    fn detects_external_changes_and_exports_the_unsaved_draft() {
        let root = Fixture::new();
        let path = root.0.join("Notes.md");
        fs::write(&path, "Initial").unwrap();
        let mut doc = OpenedDocument::open(&path).unwrap();
        fs::write(&path, "External edit").unwrap();
        assert_eq!(doc.save("My draft".to_owned()).unwrap(), SaveResult::Changed);
        assert_eq!(fs::read_to_string(&path).unwrap(), "External edit");
        let exported = doc.export("My draft").unwrap();
        assert_eq!(fs::read_to_string(root.0.join(exported)).unwrap(), "My draft");
        fs::remove_file(&path).unwrap();
        assert_eq!(doc.save("Other draft".to_owned()).unwrap(), SaveResult::Changed);
        assert!(!path.exists());
    }

    #[test]
    fn converts_utf16_text_without_overwriting_the_original_or_existing_markdown() {
        let root = Fixture::new();
        let path = root.0.join("Notas.txt");
        let original: Vec<u8> = [0xff, 0xfe].into_iter()
            .chain("café\r\n\r\n- idea".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        fs::write(&path, &original).unwrap();
        fs::write(root.0.join("Notas.md"), "Existing Markdown").unwrap();
        let mut doc = OpenedDocument::open(&path).unwrap();
        assert_eq!(doc.snapshot.text, "café\n\n- idea");
        assert!(doc.save("bad".to_owned()).is_err());
        assert_eq!(doc.export(&doc.snapshot.text).unwrap(), "Notas (2).md");
        assert_eq!(fs::read_to_string(root.0.join("Notas (2).md")).unwrap(), "café\n\n- idea");
        assert_eq!(fs::read_to_string(root.0.join("Notas.md")).unwrap(), "Existing Markdown");
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn reads_relative_attachments_and_resolves_relative_note_links() {
        let root = Fixture::new();
        let path = root.0.join("Guía.md");
        fs::write(&path, "![capture](./captura%20uno.png)\n\n![[captura uno.png]]\n\n[Anexo](docs/Anexo.md)").unwrap();
        fs::write(root.0.join("captura uno.png"), b"image bytes").unwrap();
        fs::create_dir(root.0.join("docs")).unwrap();
        fs::write(root.0.join("docs/Anexo.md"), b"appendix").unwrap();
        let doc = OpenedDocument::open(&path).unwrap();
        let html = preview(&doc.snapshot.text);
        assert_eq!(html.matches("http://opensidian-img.localhost/captura%20uno.png").count(), 2);
        assert!(html.contains("data-note=\"docs/Anexo\""));
        assert_eq!(doc.image("captura%20uno.png").unwrap(), ("image/png", b"image bytes".to_vec()));
        assert!(doc.image("%2e%2e/outside.png").is_none());
        assert_eq!(doc.linked_document("docs/Anexo").unwrap(), doc.parent.join("docs/Anexo.md"));
        assert!(doc.linked_document("../Other").is_none());
    }

    #[test]
    fn keeps_directories_as_vault_arguments_and_reports_unreadable_documents() {
        let root = Fixture::new();
        let directory = root.0.join("a vault.md");
        fs::create_dir(&directory).unwrap();
        assert!(!is_document_argument(&directory));
        assert!(!is_document_argument(&root.0.join("note.pdf")));
        let path = root.0.join("missing.markdown");
        assert!(is_document_argument(&path));
        assert!(matches!(OpenedDocument::open(&path), Err(e) if e.kind() == io::ErrorKind::NotFound));
        fs::write(&path, [0xff]).unwrap();
        assert!(matches!(OpenedDocument::open(&path), Err(e) if e.kind() == io::ErrorKind::InvalidData));
        assert!(read_bytes(File::open(&path).unwrap(), 0).is_err());
    }
}
