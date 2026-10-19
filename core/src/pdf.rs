//! PDFium access. One process-wide library handle, plus a worker thread that
//! owns every open document (PDFium documents are not `Send`).

use crate::edit;
use pdfium_render::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};

static PDFIUM: OnceLock<Pdfium> = OnceLock::new();
static INIT: Mutex<()> = Mutex::new(());

fn candidate_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(d) = std::env::var("LEANPDF_PDFIUM_DIR") {
        v.push(PathBuf::from(d));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.to_path_buf());
            v.push(dir.join("pdfium"));
            v.push(dir.join("resources"));
        }
    }
    v.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("pdfium"));
    v
}

/// Loads pdfium.dll once, searching next to the exe and in the repo's `pdfium/` folder.
pub fn pdfium() -> Result<&'static Pdfium, String> {
    let _guard = INIT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = PDFIUM.get() {
        return Ok(p);
    }
    let dirs = candidate_dirs();
    for dir in &dirs {
        let lib = Pdfium::pdfium_platform_library_name_at_path(dir);
        if lib.exists() {
            let bindings = Pdfium::bind_to_library(&lib).map_err(|e| format!("{}: {e}", lib.display()))?;
            let _ = PDFIUM.set(Pdfium::new(bindings));
            return Ok(PDFIUM.get().expect("just set"));
        }
    }
    Err(format!(
        "pdfium.dll not found. Looked in: {}",
        dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join("; ")
    ))
}

/// Files up to this size are read through the OS file API and handed to PDFium
/// as bytes. That path blocks correctly while a cloud (OneDrive) placeholder
/// downloads and reports the real I/O error; bigger files are streamed by PDFium
/// so they don't sit in RAM.
const READ_WHOLE_LIMIT: u64 = 32 * 1024 * 1024;

fn describe(e: PdfiumError) -> String {
    let dbg = format!("{e:?}");
    if dbg.contains("FormatError") {
        "the file is damaged or not a valid PDF (format error)".into()
    } else if dbg.contains("PasswordError") {
        "the PDF is password-protected".into()
    } else if dbg.contains("FileError") {
        "the file could not be read (file error)".into()
    } else {
        e.to_string()
    }
}

fn try_load(pdfium: &'static Pdfium, path: &str) -> Result<PdfDocument<'static>, String> {
    let size = std::fs::metadata(path).map_err(|e| format!("cannot access file: {e}"))?.len();
    if size == 0 {
        return Err("file is empty (0 bytes), possibly still syncing".into());
    }
    if size <= READ_WHOLE_LIMIT {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read file: {e}"))?;
        pdfium.load_pdf_from_byte_vec(bytes, None).map_err(describe)
    } else {
        pdfium.load_pdf_from_file(path, None).map_err(describe)
    }
}

/// Opens a PDF, retrying a few times because files in cloud-synced folders can
/// fail transiently while they download. Password/format problems that persist
/// are reported with PDFium's own wording.
pub fn load_document(pdfium: &'static Pdfium, path: &str) -> Result<PdfDocument<'static>, String> {
    let mut last = String::new();
    for attempt in 0..3u64 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(400 * attempt));
        }
        match try_load(pdfium, path) {
            Ok(doc) => return Ok(doc),
            Err(e) => {
                let permanent = e.contains("password-protected");
                last = e;
                if permanent {
                    break;
                }
            }
        }
    }
    Err(last)
}

#[derive(Serialize, Clone)]
pub struct DocInfo {
    pub id: u32,
    pub page_count: u32,
    /// (width, height) of every page in PDF points.
    pub pages: Vec<[f32; 2]>,
}

#[derive(Serialize)]
pub struct Annot {
    pub page: u32,
    /// Position in the page's annotation list; identifies it for edit/delete.
    pub index: u32,
    pub kind: &'static str,
    pub author: Option<String>,
    pub contents: Option<String>,
    pub modified: Option<String>,
    /// Text under a highlight/underline/strikeout.
    pub text: Option<String>,
    /// left, bottom, right, top in PDF points (origin bottom-left).
    pub rect: [f32; 4],
}

type Reply<T> = Sender<Result<T, String>>;

pub enum EditOp {
    Markup { page: u32, kind: String, rects: Vec<[f32; 4]>, rgb: [u8; 3], note: Option<String>, author: String },
    Note { page: u32, x: f32, y: f32, text: String, author: String },
    SetContents { page: u32, annot: u32, text: String },
    Delete { page: u32, annot: u32 },
}

impl EditOp {
    fn page(&self) -> u32 {
        match self {
            EditOp::Markup { page, .. } | EditOp::Note { page, .. } | EditOp::SetContents { page, .. } | EditOp::Delete { page, .. } => *page,
        }
    }
}

pub enum Cmd {
    Open { path: String, reply: Reply<DocInfo> },
    Render { doc: u32, page: u32, width: u32, reply: Box<dyn FnOnce(Result<Vec<u8>, String>) + Send> },
    /// Same as `Render` but returns (width, height, RGBA bytes) for native UIs.
    RenderRgba { doc: u32, page: u32, width: u32, reply: Box<dyn FnOnce(Result<(u32, u32, Vec<u8>), String>) + Send> },
    Annots { doc: u32, from: u32, to: u32, reply: Reply<Vec<Annot>> },
    Find { doc: u32, page: u32, terms: Vec<String>, reply: Reply<Vec<[f32; 4]>> },
    Select { doc: u32, page: u32, a: [f32; 2], b: [f32; 2], reply: Reply<edit::Selection> },
    Edit { doc: u32, op: EditOp, reply: Reply<()> },
    /// Writes pending edits to disk. Replies true if anything was written.
    Save { doc: u32, reply: Reply<bool> },
    /// Saves pending edits, then closes the document and frees it.
    Close { doc: u32, reply: Reply<()> },
    /// Plain text of pages `from..to` as (page index, text); pages without text are skipped.
    Text { doc: u32, from: u32, to: u32, reply: Reply<Vec<(u32, String)>> },
}

pub fn spawn_worker() -> Sender<Cmd> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("pdf-viewer".into())
        .spawn(move || worker(rx))
        .expect("spawn viewer thread");
    tx
}

struct Open {
    id: u32,
    doc: PdfDocument<'static>,
    path: String,
    /// Edits exist that have not been written to disk yet.
    dirty: bool,
    /// Pages whose annotations changed since the last save.
    touched: std::collections::HashSet<u32>,
    /// Streamed by PDFium (large file), so the file handle must be released before replacing it.
    from_file: bool,
}

fn open_info(doc: &PdfDocument<'static>, id: u32) -> Result<DocInfo, String> {
    let pages = doc
        .pages()
        .page_sizes()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|r| [r.width().value, r.height().value])
        .collect::<Vec<_>>();
    Ok(DocInfo { id, page_count: pages.len() as u32, pages })
}

fn worker(rx: Receiver<Cmd>) {
    let lib = pdfium();
    let mut docs: HashMap<u32, Open> = HashMap::new();
    let mut next_id = 1u32;

    while let Ok(first) = rx.recv() {
        // Drain whatever queued up while we were busy. Non-render work runs first
        // and renders run newest-first, because the newest request is the page the
        // user is looking at after a fast scroll.
        let mut batch = vec![first];
        while let Ok(c) = rx.try_recv() {
            batch.push(c);
        }
        let (mut renders, others): (Vec<Cmd>, Vec<Cmd>) =
            batch.into_iter().partition(|c| matches!(c, Cmd::Render { .. } | Cmd::RenderRgba { .. }));
        renders.reverse();

        for cmd in others.into_iter().chain(renders) {
            let pdfium = match &lib {
                Ok(p) => *p,
                Err(e) => {
                    fail(cmd, e.clone());
                    continue;
                }
            };
            match cmd {
                Cmd::Open { path, reply } => {
                    let from_file = std::fs::metadata(&path).map(|m| m.len() > READ_WHOLE_LIMIT).unwrap_or(false);
                    let result = load_document(pdfium, &path)
                        .map_err(|e| format!("Could not open PDF: {e}"))
                        .and_then(|doc| {
                            let id = next_id;
                            next_id += 1;
                            let info = open_info(&doc, id)?;
                            docs.insert(id, Open { id, doc, path: path.clone(), dirty: false, touched: Default::default(), from_file });
                            Ok(info)
                        });
                    let _ = reply.send(result);
                }
                Cmd::Render { doc, page, width, reply } => {
                    reply(with_doc(&docs, doc, |o| render_png(&o.doc, page, width)));
                }
                Cmd::RenderRgba { doc, page, width, reply } => {
                    reply(with_doc(&docs, doc, |o| render_rgba(&o.doc, page, width)));
                }
                Cmd::Annots { doc, from, to, reply } => {
                    let _ = reply.send(with_doc(&docs, doc, |o| Ok(annotations(&o.doc, from, to))));
                }
                Cmd::Find { doc, page, terms, reply } => {
                    let _ = reply.send(with_doc(&docs, doc, |o| find_rects(&o.doc, page, &terms)));
                }
                Cmd::Select { doc, page, a, b, reply } => {
                    let _ = reply.send(with_doc(&docs, doc, |o| edit::select_text(&o.doc, page, a, b)));
                }
                Cmd::Edit { doc, op, reply } => {
                    let result = match docs.get_mut(&doc) {
                        Some(o) => {
                            let page = op.page();
                            let r = apply_edit(&o.doc, op);
                            if r.is_ok() {
                                o.dirty = true;
                                o.touched.insert(page);
                            }
                            r
                        }
                        None => Err("stale document".into()),
                    };
                    let _ = reply.send(result);
                }
                Cmd::Save { doc, reply } => {
                    let _ = reply.send(save_doc(pdfium, &mut docs, doc));
                }
                Cmd::Text { doc, from, to, reply } => {
                    let _ = reply.send(with_doc(&docs, doc, |o| Ok(page_texts(&o.doc, from, to))));
                }
                Cmd::Close { doc, reply } => {
                    let r = save_doc(pdfium, &mut docs, doc).map(|_| ());
                    docs.remove(&doc);
                    let _ = reply.send(r);
                }
            }
        }
    }
}

/// Saves one open document (if it has pending edits), keeping it open.
fn save_doc(pdfium: &'static Pdfium, docs: &mut HashMap<u32, Open>, id: u32) -> Result<bool, String> {
    let Some(o) = docs.remove(&id) else { return Err("stale document".into()) };
    let mut slot = Some(o);
    let result = save_current(pdfium, &mut slot);
    if let Some(o) = slot {
        docs.insert(id, o);
    }
    result
}

fn apply_edit(doc: &PdfDocument<'static>, op: EditOp) -> Result<(), String> {
    match op {
        EditOp::Markup { page, kind, rects, rgb, note, author } => {
            edit::add_markup(doc, page, &kind, &rects, rgb, note.as_deref(), &author)
        }
        EditOp::Note { page, x, y, text, author } => edit::add_note(doc, page, x, y, &text, &author),
        EditOp::SetContents { page, annot, text } => edit::set_contents(doc, page, annot, &text),
        EditOp::Delete { page, annot } => edit::delete(doc, page, annot),
    }
}

/// Appends the pending edits to the file as an incremental update (so everything
/// already in the PDF, including other programs' comments, is kept byte-for-byte),
/// then replaces the file atomically and reloads it.
fn save_current(pdfium: &'static Pdfium, current: &mut Option<Open>) -> Result<bool, String> {
    let Some(o) = current.take() else { return Ok(false) };
    if !o.dirty {
        *current = Some(o);
        return Ok(false);
    }

    // PDFium only builds a new annotation's appearance stream when it draws the page.
    // Do that now, so what we save has appearances other readers can show.
    for p in &o.touched {
        let _ = render_png(&o.doc, *p, 100);
    }

    let produced = (|| -> Result<Vec<u8>, String> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        o.doc.save_incremental_to_writer(&mut cursor).map_err(|e| format!("PDFium could not save: {e}"))?;
        let mut out = cursor.into_inner();
        ensure_xref_stream_type(&mut out);
        ensure_appended_only(&o.path, &out)?;
        let tmp = format!("{}.leanpdf-tmp", o.path);
        std::fs::write(&tmp, &out).map_err(|e| format!("could not write {tmp}: {e}"))?;
        Ok(out)
    })();
    if let Err(e) = produced {
        *current = Some(o);
        return Err(e);
    }

    let (id, path, from_file) = (o.id, o.path.clone(), o.from_file);
    let tmp = format!("{path}.leanpdf-tmp");
    let mut keep = Some(o);
    if from_file {
        keep = None; // PDFium is streaming this file; release it so it can be replaced
    }

    let mut replaced = Err(String::new());
    for attempt in 0..4u64 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(300 * attempt));
        }
        replaced = std::fs::rename(&tmp, &path).map_err(|e| e.to_string());
        if replaced.is_ok() {
            break;
        }
    }
    if let Err(e) = replaced {
        let _ = std::fs::remove_file(&tmp);
        // Put the document back so the user's edits are still on screen.
        *current = match keep {
            Some(o) => Some(o),
            None => load_document(pdfium, &path).ok().map(|doc| Open { id, doc, path, dirty: false, touched: Default::default(), from_file }),
        };
        return Err(format!("Could not replace the file (is it open in another program?): {e}"));
    }

    drop(keep);
    let doc = load_document(pdfium, &path).map_err(|e| format!("saved, but reopening failed: {e}"))?;
    *current = Some(Open { id, doc, path, dirty: false, touched: Default::default(), from_file });
    Ok(true)
}

/// When the original file uses cross-reference streams (Acrobat's do), PDFium's
/// incremental update ends with a new xref stream whose dictionary lacks the
/// required `/Type /XRef`. Lenient readers cope; strict ones reject the file.
/// Insert it. The stream sits at the end of the file and `startxref` points at its
/// first byte, so nothing else in the file moves.
fn ensure_xref_stream_type(out: &mut Vec<u8>) {
    let find_last = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).rposition(|w| w == needle);
    let Some(sx) = find_last(out, b"startxref") else { return };
    let digits: Vec<u8> = out[sx + 9..].iter().copied().skip_while(|b| b.is_ascii_whitespace()).take_while(|b| b.is_ascii_digit()).collect();
    let Ok(off) = String::from_utf8_lossy(&digits).parse::<usize>() else { return };
    if off >= out.len() {
        return;
    }
    let region_end = (off + 600).min(out.len());
    let region = &out[off..region_end];
    let Some(obj) = region.windows(3).position(|w| w == b"obj") else { return };
    let Some(dict) = region.windows(2).position(|w| w == b"<<") else { return };
    let Some(stream) = region.windows(6).position(|w| w == b"stream") else { return };
    if dict < obj || stream < dict {
        return; // not "N G obj << ... >> stream": a classic xref table or something else
    }
    let dict_bytes = &region[dict..stream];
    let is_xref_stream = dict_bytes.windows(2).any(|w| w == b"/W") && dict_bytes.windows(5).any(|w| w == b"/Size");
    if !is_xref_stream || dict_bytes.windows(5).any(|w| w == b"/Type") {
        return;
    }
    let at = off + dict + 2;
    out.splice(at..at, b"/Type/XRef".iter().copied());
}

/// An incremental save must begin with the exact bytes of the existing file.
/// If PDFium rewrote the file instead, refuse rather than risk changing other
/// programs' data.
fn ensure_appended_only(path: &str, out: &[u8]) -> Result<(), String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("cannot re-read original: {e}"))?;
    let len = f.metadata().map_err(|e| e.to_string())?.len() as usize;
    if out.len() <= len {
        return Err("save produced no new data".into());
    }
    let mut buf = vec![0u8; 1 << 20];
    let mut pos = 0usize;
    while pos < len {
        let n = f.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if out[pos..pos + n] != buf[..n] {
            return Err("PDFium rewrote the file instead of appending; not saving to protect existing data".into());
        }
        pos += n;
    }
    Ok(())
}

fn fail(cmd: Cmd, err: String) {
    match cmd {
        Cmd::Open { reply, .. } => drop(reply.send(Err(err))),
        Cmd::Render { reply, .. } => reply(Err(err)),
        Cmd::RenderRgba { reply, .. } => reply(Err(err)),
        Cmd::Annots { reply, .. } => drop(reply.send(Err(err))),
        Cmd::Find { reply, .. } => drop(reply.send(Err(err))),
        Cmd::Select { reply, .. } => drop(reply.send(Err(err))),
        Cmd::Edit { reply, .. } => drop(reply.send(Err(err))),
        Cmd::Save { reply, .. } => drop(reply.send(Err(err))),
        Cmd::Close { reply, .. } => drop(reply.send(Err(err))),
        Cmd::Text { reply, .. } => drop(reply.send(Err(err))),
    }
}

fn with_doc<T>(docs: &HashMap<u32, Open>, id: u32, f: impl FnOnce(&Open) -> Result<T, String>) -> Result<T, String> {
    match docs.get(&id) {
        Some(o) => f(o),
        None => Err("stale document".into()),
    }
}

fn render_rgba(doc: &PdfDocument<'static>, index: u32, width: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let page = doc.pages().get(index as i32).map_err(|e| e.to_string())?;
    let width = width.clamp(100, 4000);
    let cfg = PdfRenderConfig::new()
        .set_target_width(width as i32)
        .render_form_data(true)
        .render_annotations(true);
    let bitmap = page.render_with_config(&cfg).map_err(|e| e.to_string())?;
    Ok((bitmap.width() as u32, bitmap.height() as u32, bitmap.as_rgba_bytes()))
}

fn render_png(doc: &PdfDocument<'static>, index: u32, width: u32) -> Result<Vec<u8>, String> {
    let page = doc.pages().get(index as i32).map_err(|e| e.to_string())?;
    let width = width.clamp(100, 4000);
    let cfg = PdfRenderConfig::new()
        .set_target_width(width as i32)
        .render_form_data(true)
        .render_annotations(true);
    let bitmap = page.render_with_config(&cfg).map_err(|e| e.to_string())?;
    let (w, h) = (bitmap.width() as u32, bitmap.height() as u32);
    let rgba = bitmap.as_rgba_bytes();
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    for px in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    let mut out = Vec::with_capacity(rgb.len() / 4);
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        enc.set_filter(png::Filter::Sub);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&rgb).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

fn kind_name(t: PdfPageAnnotationType) -> Option<&'static str> {
    use PdfPageAnnotationType::*;
    Some(match t {
        Text => "note",
        Highlight => "highlight",
        Underline => "underline",
        Strikeout => "strikeout",
        Squiggly => "squiggly",
        FreeText => "text box",
        Ink => "drawing",
        Square | Circle | Line | Polygon | Polyline => "shape",
        Stamp => "stamp",
        Caret => "caret",
        FileAttachment => "attachment",
        _ => return None, // links, popups, form widgets, etc. are not comments
    })
}

fn annotations(doc: &PdfDocument<'static>, from: u32, to: u32) -> Vec<Annot> {
    let mut out = Vec::new();
    let count = doc.pages().len() as u32;
    for index in from..to.min(count) {
        let Ok(page) = doc.pages().get(index as i32) else { continue };
        let text = page.text().ok();
        for (ai, annot) in page.annotations().iter().enumerate() {
            let Some(kind) = kind_name(annot.annotation_type()) else { continue };
            let Ok(b) = annot.bounds() else { continue };

            let mut under = None;
            if matches!(kind, "highlight" | "underline" | "strikeout" | "squiggly") {
                if let Some(text) = &text {
                    let mut s = String::new();
                    for q in annot.attachment_points().iter() {
                        if let Ok(chars) = text.chars_inside_rect(q.to_rect()) {
                            for c in chars.iter() {
                                if let Some(ch) = c.unicode_char() {
                                    s.push(ch);
                                }
                            }
                        }
                        s.push(' ');
                    }
                    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !s.is_empty() {
                        under = Some(s.chars().take(400).collect());
                    }
                }
            }

            out.push(Annot {
                page: index,
                index: ai as u32,
                kind,
                author: annot.creator().filter(|s| !s.is_empty()),
                contents: annot.contents().filter(|s| !s.trim().is_empty()),
                modified: annot.modification_date(),
                text: under,
                rect: [b.left().value, b.bottom().value, b.right().value, b.top().value],
            });
        }
    }
    out
}

fn find_rects(doc: &PdfDocument<'static>, index: u32, terms: &[String]) -> Result<Vec<[f32; 4]>, String> {
    let page = doc.pages().get(index as i32).map_err(|e| e.to_string())?;
    let text = page.text().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for term in terms.iter().filter(|t| !t.is_empty()) {
        let Ok(search) = text.search(term, &PdfSearchOptions::new()) else { continue };
        for group in search.iter(PdfSearchDirection::SearchForward) {
            for seg in group.iter() {
                let b = seg.bounds();
                out.push([b.left().value, b.bottom().value, b.right().value, b.top().value]);
            }
            if out.len() > 300 {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

fn page_texts(doc: &PdfDocument<'static>, from: u32, to: u32) -> Vec<(u32, String)> {
    let count = doc.pages().len() as u32;
    let mut out = Vec::new();
    for i in from..to.min(count) {
        let Some(raw) = page_text(doc, i as i32) else { continue };
        // keep line breaks (they help readers and models), drop the carriage returns and runs of blank lines
        let mut text = String::new();
        let mut blank = 0;
        for line in raw.replace("\r\n", "\n").replace('\r', "\n").lines() {
            let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
            if l.is_empty() {
                blank += 1;
                if blank == 1 && !text.is_empty() {
                    text.push('\n');
                }
            } else {
                blank = 0;
                text.push_str(&l);
                text.push('\n');
            }
        }
        if !text.trim().is_empty() {
            out.push((i, text));
        }
    }
    out
}

/// Plain text of one page (used by the indexer).
pub fn page_text(doc: &PdfDocument<'_>, index: i32) -> Option<String> {
    let page = doc.pages().get(index).ok()?;
    let text = page.text().ok()?;
    Some(text.all())
}
