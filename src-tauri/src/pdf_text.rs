// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};

use hayro::hayro_interpret::font::Glyph;
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::{
    interpret_page, BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, InterpreterCache,
    InterpreterSettings, Paint, PathDrawMode, SoftMask, TransformExt,
};
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::kurbo::{Affine, BezPath, Rect};
use serde::Serialize;

use crate::{index::MAX_NOTE_BYTES, ocr, pdf};

const TOO_LARGE: &str = "The extracted Markdown exceeds the document size limit. Select fewer pages.";

#[derive(Debug, Serialize)]
pub struct Conversion {
    pub text: String,
    pub omitted: Vec<usize>,
    pub recognized: Vec<usize>,
}

pub fn convert(pdf: &Pdf, selection: &str, ocr_language: Option<ocr::Language>) -> Result<Conversion, String> {
    let pages = pdf.pages();
    let selected = selected_pages(selection, pages.len())?;
    let cache = InterpreterCache::new();
    let mut result = Conversion { text: String::new(), omitted: Vec::new(), recognized: Vec::new() };
    let mut engine = None;

    for (index, page) in pages.iter().enumerate().filter(|(index, _)| selected[*index]) {
        let number = index + 1;
        let device = catch_unwind(AssertUnwindSafe(|| {
            let (w, h) = page.render_dimensions();
            let mut context = Context::new(
                page.initial_transform(true).to_kurbo(),
                Rect::new(0.0, 0.0, w.into(), h.into()),
                &cache,
                page.xref(),
                InterpreterSettings::default(),
            );
            let mut device = TextDevice::default();
            interpret_page(page, &mut context, &mut device);
            device
        })).map_err(|_| format!("Could not extract text from page {number}."))?;
        if device.too_large {
            return Err(TOO_LARGE.to_owned());
        }
        let text = if device.has_text {
            device.text
        } else if let Some(language) = ocr_language {
            if engine.is_none() {
                engine = Some(ocr::Tesseract::open(language)?);
            }
            let engine = engine.as_ref().expect("OCR engine initialized");
            let png = pdf::page_png(pdf, number, pdf::MAX_W)
                .ok_or_else(|| format!("Could not render page {number} for OCR."))?;
            let text = engine.recognize(&png).map_err(|error| format!("Page {number}. {error}"))?;
            if !text.trim().is_empty() {
                result.recognized.push(number);
            }
            text
        } else {
            String::new()
        };
        if text.trim().is_empty() {
            result.omitted.push(number);
            continue;
        }
        if !result.text.is_empty() {
            result.text.push('\n');
        }
        let _ = writeln!(result.text, "## Page {number}\n");
        append_markdown(&mut result.text, text.trim())?;
        result.text.push('\n');
    }

    if result.text.is_empty() {
        if ocr_language.is_some() {
            return Err("OCR did not find text on any of the selected pages.".to_owned());
        }
        return Err("No selectable text was found in the selected pages. Scanned pages require OCR.".to_owned());
    }
    if result.text.len() as u64 > MAX_NOTE_BYTES {
        return Err(TOO_LARGE.to_owned());
    }
    Ok(result)
}

fn selected_pages(selection: &str, count: usize) -> Result<Vec<bool>, String> {
    let invalid = || format!("Enter pages from 1 to {count}, for example 1-3, 6.");
    let selection = selection.trim();
    if selection.len() > 4096 || count == 0 {
        return Err(invalid());
    }
    if selection.is_empty() {
        return Ok(vec![true; count]);
    }

    let parse_page = |value: &str| {
        let value = value.trim();
        if !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        value.parse::<usize>().ok().filter(|page| (1..=count).contains(page)).ok_or_else(invalid)
    };
    let mut selected = vec![false; count];
    for part in selection.split(',') {
        let (first, last) = part.split_once('-').unwrap_or((part, part));
        let (first, last) = (parse_page(first)?, parse_page(last)?);
        if first > last {
            return Err(invalid());
        }
        selected[first - 1..last].fill(true);
    }
    Ok(selected)
}

fn append_markdown(out: &mut String, text: &str) -> Result<(), String> {
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let line = line.trim();
        let list_marker = line.bytes().take_while(u8::is_ascii_digit).count();
        for (offset, ch) in line.char_indices() {
            // Keep literal PDF text from becoming Markdown syntax or HTML.
            if matches!(ch, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '&' | '~' | '|')
                || (offset == 0 && matches!(ch, '#' | '-' | '+' | '='))
                || (offset == list_marker && list_marker > 0 && matches!(ch, '.' | ')'))
            {
                out.push('\\');
            }
            out.push(ch);
            if out.len() as u64 > MAX_NOTE_BYTES {
                return Err(TOO_LARGE.to_owned());
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct TextDevice {
    text: String,
    last: Option<Position>,
    has_text: bool,
    too_large: bool,
}

struct Position {
    x: f64,
    y: f64,
    advance: f64,
}

impl<'a> Device<'a> for TextDevice {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}
    fn draw_path(&mut self, _: &BezPath, _: Affine, _: &Paint<'a>, _: &PathDrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph(
        &mut self,
        glyph: &Glyph<'a>,
        transform: Affine,
        glyph_transform: Affine,
        _: &Paint<'a>,
        _: &GlyphDrawMode,
    ) {
        if self.too_large {
            return;
        }
        let [a, b, c, d, x, y] = (transform * glyph_transform).as_coeffs();
        let unit = a.hypot(b);
        if !unit.is_finite() || unit <= 0.0 || !x.is_finite() || !y.is_finite() {
            return;
        }
        let em = 1000.0 * c.hypot(d).max(unit);
        let mapped = glyph.as_unicode();
        let mut char_buf = [0; 4];
        let content: &str = match &mapped {
            Some(BfString::Char(ch)) => ch.encode_utf8(&mut char_buf),
            Some(BfString::String(text)) => text,
            None => "�",
        };
        if let Some(previous) = &self.last {
            let (dx, dy) = (x - previous.x, y - previous.y);
            if dx.hypot(dy) < 0.01 && self.text.ends_with(content) {
                return;
            }
            let along = (dx * a + dy * b) / unit;
            let across = (dx * b - dy * a).abs() / unit;
            if across > em * 0.5 || along < -em * 0.85 {
                self.text.push('\n');
                if across > em * 1.8 {
                    self.text.push('\n');
                }
            } else if along - previous.advance > em * 0.2 && !self.text.ends_with(char::is_whitespace) {
                self.text.push(' ');
            }
        }

        for ch in content.chars() {
            if ch.is_whitespace() {
                if !self.text.ends_with(char::is_whitespace) {
                    self.text.push(' ');
                }
            } else if !ch.is_control() {
                self.text.push(ch);
                self.has_text |= mapped.is_some() && ch != '�';
            }
        }
        let width = match glyph {
            Glyph::Outline(outline) => outline.advance_width().filter(|w| *w > 0.0 && w.is_finite()),
            Glyph::Type3(_) => None,
        }.unwrap_or(500.0);
        self.last = Some(Position { x, y, advance: f64::from(width) * unit });
        self.too_large = self.text.len() as u64 > MAX_NOTE_BYTES;
    }
}
