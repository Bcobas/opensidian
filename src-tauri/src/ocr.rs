// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde::Deserialize;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Spanish,
    English,
}

impl Language {
    fn code(self) -> &'static str {
        match self {
            Self::Spanish => "spa",
            Self::English => "eng",
        }
    }
}

pub struct Tesseract {
    exe: PathBuf,
    language: Language,
}

impl Tesseract {
    pub fn open(language: Language) -> Result<Self, String> {
        let exe = if let Some(path) = std::env::var_os("OPENSIDIAN_TESSERACT") {
            let path = PathBuf::from(path);
            if !path.is_absolute() || !path.is_file() {
                return Err("OPENSIDIAN_TESSERACT must point to an existing, absolute path to tesseract.exe.".to_owned());
            }
            path
        } else {
            ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
                .iter()
                .filter_map(std::env::var_os)
                .map(|dir| PathBuf::from(dir).join("Tesseract-OCR").join("tesseract.exe"))
                .find(|path| path.is_absolute() && path.is_file())
                .ok_or("Tesseract OCR was not found. Install Tesseract and its Spanish or English language data, then try again.")?
        };
        Ok(Self { exe, language })
    }

    pub fn recognize(&self, png: &[u8]) -> Result<String, String> {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let language = self.language.code();
        let mut child = Command::new(&self.exe)
            .current_dir(self.exe.parent().expect("absolute executable path"))
            .args(["stdin", "stdout", "-l", language, "--dpi", "250"])
            .env("OMP_THREAD_LIMIT", "2")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|error| format!("Could not start Tesseract OCR. {error}"))?;

        // Feed the raster while draining both output streams to avoid a pipe deadlock.
        let mut stdin = child.stdin.take().expect("piped OCR input");
        let (output, written) = std::thread::scope(|scope| {
            let writer = scope.spawn(move || stdin.write_all(png));
            (child.wait_with_output(), writer.join().expect("OCR input writer"))
        });
        let output = output.map_err(|error| format!("Could not read OCR output. {error}"))?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            if error.contains("Error opening data file") || error.contains("Failed loading language") {
                return Err(format!("Install the Tesseract '{language}' language data and try again."));
            }
            return Err(format!("Tesseract OCR failed with {}.", output.status));
        }
        written.map_err(|error| format!("Could not send the PDF page to OCR. {error}"))?;
        String::from_utf8(output.stdout).map_err(|_| "Tesseract returned invalid UTF-8.".to_owned())
    }
}
