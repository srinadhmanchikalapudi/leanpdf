//! Text selection and comment editing on the open document.
//!
//! Annotations are created through PDFium, which writes standard PDF annotation
//! dictionaries (/Highlight, /Underline, /StrikeOut, /Text) with /QuadPoints,
//! /Contents, /T (author), /M (date) and generated appearance streams, so other
//! readers (Adobe Acrobat included) show them as ordinary comments.

use chrono::Utc;
use pdfium_render::prelude::*;
use serde::Serialize;

#[derive(Serialize)]
pub struct Selection {
    /// One rectangle per selected line: left, bottom, right, top (PDF points).
    pub rects: Vec<[f32; 4]>,
    pub text: String,
}

fn open_page<'a>(doc: &'a PdfDocument<'static>, index: u32) -> Result<PdfPage<'a>, String> {
    let mut page = doc.pages().get(index as i32).map_err(|e| e.to_string())?;
    // Never regenerate the page's content stream: we only touch annotations, and
    // rewriting the content would bloat the file and risk altering the page.
    page.set_content_regeneration_strategy(PdfPageContentRegenerationStrategy::Manual);
    Ok(page)
}

pub fn select_text(doc: &PdfDocument<'static>, index: u32, a: [f32; 2], b: [f32; 2]) -> Result<Selection, String> {
    let page = open_page(doc, index)?;
    let ph = page.height().value;
    let text = page.text().map_err(|e| e.to_string())?;
    let chars = text.chars();
    let n = chars.len();
    let empty = Selection { rects: Vec::new(), text: String::new() };
    if n == 0 {
        return Ok(empty);
    }

    let find = |p: [f32; 2]| -> Option<usize> {
        if p[1] < 0.0 {
            return Some(n - 1); // dragged below the page: everything to the end
        }
        if p[1] > ph {
            return Some(0); // dragged above the page: everything from the start
        }
        for tol_y in [4.0f32, 14.0] {
            if let Some(c) = chars.get_char_near_point(
                PdfPoints::new(p[0]),
                PdfPoints::new(400.0),
                PdfPoints::new(p[1]),
                PdfPoints::new(tol_y),
            ) {
                return Some(c.index());
            }
        }
        None
    };

    let Some(ia) = find(a) else { return Ok(empty) };
    let ib = find(b).unwrap_or(ia);
    let (lo, hi) = (ia.min(ib), ia.max(ib));

    let mut s = String::new();
    for i in lo..=hi {
        if let Ok(c) = chars.get(i) {
            if let Some(ch) = c.unicode_char() {
                s.push(ch);
            }
        }
    }
    let text_out = s.replace("\r\n", "\n").replace('\r', "\n");

    let rects = text
        .segments_subset(lo, hi - lo + 1)
        .iter()
        .map(|seg| {
            let r = seg.bounds();
            [r.left().value, r.bottom().value, r.right().value, r.top().value]
        })
        .filter(|r| r[2] - r[0] > 0.5 && r[3] - r[1] > 0.5)
        .collect();
    Ok(Selection { rects, text: text_out })
}

fn union(rects: &[[f32; 4]]) -> PdfRect {
    let l = rects.iter().map(|r| r[0]).fold(f32::MAX, f32::min);
    let b = rects.iter().map(|r| r[1]).fold(f32::MAX, f32::min);
    let r = rects.iter().map(|r| r[2]).fold(f32::MIN, f32::max);
    let t = rects.iter().map(|r| r[3]).fold(f32::MIN, f32::max);
    PdfRect::new_from_values(b, l, t, r)
}

/// Adobe writes quads as top-left, top-right, bottom-left, bottom-right.
fn quad(r: &[f32; 4]) -> PdfQuadPoints {
    PdfQuadPoints::new_from_values(r[0], r[3], r[2], r[3], r[0], r[1], r[2], r[1])
}

macro_rules! finish_markup {
    ($a:expr, $rects:expr, $rgb:expr, $note:expr, $author:expr) => {{
        let a = &mut $a;
        a.set_bounds(union($rects)).map_err(|e| e.to_string())?;
        for r in $rects {
            a.attachment_points_mut().create_attachment_point_at_end(quad(r)).map_err(|e| e.to_string())?;
        }
        a.set_stroke_color(PdfColor::new($rgb[0], $rgb[1], $rgb[2], 255)).map_err(|e| e.to_string())?;
        if let Some(n) = $note {
            a.set_contents(&adobe_newlines(n)).map_err(|e| e.to_string())?;
        }
        a.set_creator($author).map_err(|e| e.to_string())?;
        a.set_modification_date(Utc::now()).map_err(|e| e.to_string())?;
        a.set_is_printed(true).map_err(|e| e.to_string())?;
    }};
}

pub fn add_markup(
    doc: &PdfDocument<'static>,
    index: u32,
    kind: &str,
    rects: &[[f32; 4]],
    rgb: [u8; 3],
    note: Option<&str>,
    author: &str,
) -> Result<(), String> {
    if rects.is_empty() {
        return Err("nothing selected".into());
    }
    let mut page = open_page(doc, index)?;
    let annots = page.annotations_mut();
    match kind {
        "highlight" => {
            let mut a = annots.create_highlight_annotation().map_err(|e| e.to_string())?;
            finish_markup!(a, rects, rgb, note, author);
        }
        "underline" => {
            let mut a = annots.create_underline_annotation().map_err(|e| e.to_string())?;
            finish_markup!(a, rects, rgb, note, author);
        }
        "strikeout" => {
            let mut a = annots.create_strikeout_annotation().map_err(|e| e.to_string())?;
            finish_markup!(a, rects, rgb, note, author);
        }
        other => return Err(format!("unknown markup kind: {other}")),
    }
    Ok(())
}

/// A sticky-note comment with its icon's top-left corner at (x, y) in PDF points.
pub fn add_note(doc: &PdfDocument<'static>, index: u32, x: f32, y: f32, text: &str, author: &str) -> Result<(), String> {
    let mut page = open_page(doc, index)?;
    let (pw, ph) = (page.width().value, page.height().value);
    let (x, y) = (x.clamp(0.0, pw - 20.0), y.clamp(20.0, ph));
    let mut a = page.annotations_mut().create_text_annotation(&adobe_newlines(text)).map_err(|e| e.to_string())?;
    a.set_bounds(PdfRect::new_from_values(y - 20.0, x, y, x + 20.0)).map_err(|e| e.to_string())?;
    a.set_stroke_color(PdfColor::new(255, 214, 0, 255)).map_err(|e| e.to_string())?;
    a.set_creator(author).map_err(|e| e.to_string())?;
    a.set_modification_date(Utc::now()).map_err(|e| e.to_string())?;
    a.set_is_printed(true).map_err(|e| e.to_string())?;
    a.set_is_zoomable(false).map_err(|e| e.to_string())?;
    a.set_is_rotatable(false).map_err(|e| e.to_string())?;
    Ok(())
}

/// Acrobat separates the lines of a comment with a carriage return; use the same.
fn adobe_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r")
}

/// True when the rich-text copy of a comment (/RC) asks for a monospace font.
pub fn rc_is_monospace(rc: &str) -> bool {
    let l = rc.to_lowercase();
    l.split("font-family:").skip(1).any(|rest| {
        let family: String = rest.chars().take_while(|c| *c != ';' && *c != '"' && *c != '\'').collect();
        ["courier", "consolas", "mono", "menlo", "lucida console", "code", "fixed"].iter().any(|m| family.contains(m))
    })
}

/// The XHTML Acrobat keeps in /RC for a comment, in the same shape Acrobat writes it.
fn rich_text_xhtml(text: &str, mono: bool) -> String {
    let mut esc = String::new();
    for ch in text.replace("\r\n", "\n").chars() {
        match ch {
            '&' => esc.push_str("&amp;"),
            '<' => esc.push_str("&lt;"),
            '>' => esc.push_str("&gt;"),
            '\n' | '\r' => esc.push_str("&#13;"),
            '\t' => esc.push_str("&#9;"),
            c => esc.push(c),
        }
    }
    let font = if mono { ";font-family:Courier New,monospace" } else { "" };
    format!(
        "<?xml version=\"1.0\"?><body xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:xfa=\"http://www.xfa.org/schema/xfa-data/1.0/\" \
         xfa:APIVersion=\"Acrobat:11.0.0\" xfa:spec=\"2.0.2\"><p dir=\"ltr\"><span dir=\"ltr\" \
         style=\"font-size:12.0pt;text-align:left;color:#000000;font-weight:normal;font-style:normal{font}\">{esc}</span></p></body>"
    )
}

pub fn set_contents(doc: &PdfDocument<'static>, index: u32, annot: u32, text: &str) -> Result<(), String> {
    let page = open_page(doc, index)?;
    let mut a = page.annotations().get(annot as usize).map_err(|e| e.to_string())?;
    // Acrobat shows /RC (rich text) in preference to /Contents when both exist, so an edit must update both.
    let old_rc = a.raw_string_value("RC");
    a.set_contents(&adobe_newlines(text)).map_err(|e| e.to_string())?;
    if let Some(rc) = old_rc {
        a.set_raw_string_value("RC", &rich_text_xhtml(text, rc_is_monospace(&rc))).map_err(|e| e.to_string())?;
    }
    a.set_modification_date(Utc::now()).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete(doc: &PdfDocument<'static>, index: u32, annot: u32) -> Result<(), String> {
    let mut page = open_page(doc, index)?;
    let annots = page.annotations_mut();
    let a = annots.get(annot as usize).map_err(|e| e.to_string())?;
    annots.delete_annotation(a).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monospace_is_read_from_rich_text() {
        assert!(rc_is_monospace(r#"<span style="font-size:12pt;font-family:Consolas">x</span>"#));
        assert!(rc_is_monospace("<span style='font-family: Courier New'>x</span>"));
        assert!(!rc_is_monospace(r#"<span style="font-family:Helvetica,sans-serif">x</span>"#));
        assert!(!rc_is_monospace("<span>plain</span>"));
    }

    #[test]
    fn rich_text_is_escaped_and_keeps_lines_and_tabs() {
        let x = rich_text_xhtml("a<b & c\n\td", true);
        assert!(x.contains("a&lt;b &amp; c&#13;&#9;d"), "{x}");
        assert!(x.contains("Courier New"));
        assert!(rc_is_monospace(&x));
        assert!(!rc_is_monospace(&rich_text_xhtml("x", false)));
    }

    #[test]
    fn lines_use_carriage_returns() {
        assert_eq!(adobe_newlines("a\r\nb\nc"), "a\rb\rc");
    }
}
