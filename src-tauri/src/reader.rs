// SPDX-License-Identifier: GPL-3.0-or-later

use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, Prefix};

use tauri::{State, WebviewUrl, WebviewWindowBuilder, Wry};

use crate::{index::MAX_NOTE_BYTES, nav_allowed, render_with_br};

pub fn is_document_argument(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| {
        ext.eq_ignore_ascii_case("md") || ext.eq_ignore_ascii_case("markdown")
    }) && !path.is_dir()
}

fn read_document(path: &Path, limit: u64) -> io::Result<String> {
    if matches!(
        path.components().next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::DeviceNS(_) | Prefix::Verbatim(_))
    ) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "expected a Markdown file path"));
    }
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "expected a Markdown file"));
    }
    let too_large = || io::Error::new(io::ErrorKind::InvalidData, "the Markdown file exceeds the reader's size limit");
    if metadata.len() > limit {
        return Err(too_large());
    }
    let mut text = String::new();
    file.take(limit + 1).read_to_string(&mut text)?;
    if text.len() as u64 > limit {
        return Err(too_large());
    }
    if text.starts_with('\u{feff}') {
        text.replace_range(..'\u{feff}'.len_utf8(), "");
    }
    Ok(text)
}

struct Document {
    html: Result<String, String>,
}

#[tauri::command]
fn standalone_markdown(document: State<'_, Document>) -> Result<String, String> {
    if document.html.is_ok() {
        eprintln!("[markdown] opened standalone document");
    }
    document.html.clone()
}

#[tauri::command]
fn open_reader_link(url: String) -> Result<(), String> {
    crate::open_external(url)
}

pub fn run(path: &Path, mut context: tauri::Context<Wry>) {
    let name = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
    let title = format!("{name} - Opensidian");
    let content = read_document(path, MAX_NOTE_BYTES)
        .map(|text| render_with_br(&text, &[], &[], true, true))
        .map_err(|error| format!("Could not read {name}. {error}"));

    context.config_mut().app.windows.clear();
    tauri::Builder::default()
        .manage(Document { html: content })
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("reader.html".into()))
                .title(title)
                .inner_size(900.0, 700.0)
                .min_inner_size(320.0, 240.0)
                .on_navigation(nav_allowed)
                .build()?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![standalone_markdown, open_reader_link])
        .run(context)
        .expect("run standalone Markdown reader");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::PathBuf};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "opensidian-reader-{}",
                opensidian_lib::os_random_hex().unwrap()
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
    fn opens_a_markdown_file_without_modifying_its_directory() {
        let root = Fixture::new();
        let path = root.0.join("Notas de café.MD");
        let bytes = "\u{feff}# Hola\n\n**Markdown**";
        fs::write(&path, bytes).unwrap();

        assert!(is_document_argument(&path));
        let text = read_document(&path, MAX_NOTE_BYTES).unwrap();
        assert_eq!(text, "# Hola\n\n**Markdown**");
        assert!(render_with_br(&text, &[], &[], true, true).contains("<strong>Markdown</strong>"));
        assert_eq!(fs::read(&path).unwrap(), bytes.as_bytes());
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 1);
    }

    #[test]
    fn keeps_directories_as_vault_arguments_and_reports_unreadable_documents() {
        let root = Fixture::new();
        let directory = root.0.join("a vault.md");
        fs::create_dir(&directory).unwrap();
        assert!(!is_document_argument(&directory));
        assert!(!is_document_argument(&root.0.join("note.txt")));

        let path = root.0.join("missing.markdown");
        assert!(is_document_argument(&path));
        assert_eq!(read_document(&path, 8).unwrap_err().kind(), io::ErrorKind::NotFound);
        fs::write(&path, b"ninebytes").unwrap();
        assert_eq!(read_document(&path, 8).unwrap_err().kind(), io::ErrorKind::InvalidData);
        fs::write(&path, [0xff]).unwrap();
        assert_eq!(read_document(&path, 8).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
