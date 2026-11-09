//! Editing the text of a text-box comment (a FreeText annotation).
//!
//! PDFium cannot draw a new appearance for text boxes, and a viewer shows the stored appearance
//! (not the text) until the box is edited again, so simply changing the text would leave Acrobat
//! showing the old words. This module instead writes a complete, self-contained new appearance
//! (with its own font) and the new text, as an incremental update appended to the file.

use lopdf::{Dictionary, Document, IncrementalDocument, Object, Stream, StringFormat};

/// Width of a character in Helvetica, in thousandths of the font size (WinAnsi).
fn helvetica_width(c: char) -> f32 {
    const W: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // space .. /
        556, 556, 556, 556, 556, 556, 556, 556, 556, 556, // 0-9
        278, 278, 584, 584, 584, 556, 1015, // : ; < = > ? @
        667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, // A-Z
        278, 278, 278, 469, 556, 333, // [ \ ] ^ _ `
        556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, // a-z
        334, 260, 334, 584, // { | } ~
    ];
    let code = c as u32;
    if (32..=126).contains(&code) {
        W[(code - 32) as usize] as f32
    } else {
        556.0
    }
}

fn text_width(s: &str, size: f32) -> f32 {
    s.chars().map(helvetica_width).sum::<f32>() * size / 1000.0
}

/// Breaks text into lines no wider than `max_w`, honouring explicit line breaks.
pub fn wrap_lines(text: &str, size: f32, max_w: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.replace("\r\n", "\n").replace('\r', "\n").split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            // a word longer than the box is cut at the box edge
            let mut word = word.to_string();
            loop {
                let candidate = if line.is_empty() { word.clone() } else { format!("{line} {word}") };
                if text_width(&candidate, size) <= max_w || (line.is_empty() && word.chars().count() <= 1) {
                    line = candidate;
                    break;
                }
                if line.is_empty() {
                    let mut cut = String::new();
                    let mut rest = String::new();
                    for ch in word.chars() {
                        if rest.is_empty() && text_width(&format!("{cut}{ch}"), size) <= max_w {
                            cut.push(ch);
                        } else {
                            rest.push(ch);
                        }
                    }
                    lines.push(cut);
                    word = rest;
                } else {
                    lines.push(std::mem::take(&mut line));
                }
            }
        }
        lines.push(line);
    }
    lines
}

/// A PDF text string: plain ASCII as is, anything else as UTF-16BE with a byte order mark.
pub fn pdf_text_string(s: &str) -> Vec<u8> {
    if s.is_ascii() {
        s.as_bytes().to_vec()
    } else {
        let mut out = vec![0xFE, 0xFF];
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_be_bytes());
        }
        out
    }
}

/// Text for a content stream string: WinAnsi bytes, with PDF's escapes.
fn content_string(s: &str) -> Vec<u8> {
    let mut out = vec![b'('];
    for c in s.chars() {
        let code = match c as u32 {
            0..=255 => c as u32 as u8,
            0x2018 | 0x2019 => b'\'',
            0x201C | 0x201D => b'"',
            0x2013 | 0x2014 => b'-',
            _ => b'?',
        };
        match code {
            b'(' | b')' | b'\\' => {
                out.push(b'\\');
                out.push(code);
            }
            32..=126 => out.push(code),
            _ => out.extend_from_slice(format!("\\{code:03o}").as_bytes()),
        }
    }
    out.push(b')');
    out
}

struct Style {
    size: f32,
    /// Operator that sets the text colour, e.g. "0 g" or "0.2 0.2 0.8 rg".
    color: String,
}

fn parse_da(da: &str) -> Style {
    let toks: Vec<&str> = da.split_whitespace().collect();
    let mut size = 12.0f32;
    let mut color = "0 g".to_string();
    for (i, t) in toks.iter().enumerate() {
        match *t {
            "Tf" if i >= 1 => {
                if let Ok(s) = toks[i - 1].parse::<f32>() {
                    if s > 0.0 {
                        size = s;
                    }
                }
            }
            "g" if i >= 1 => color = format!("{} g", toks[i - 1]),
            "rg" if i >= 3 => color = format!("{} {} {} rg", toks[i - 3], toks[i - 2], toks[i - 1]),
            "k" if i >= 4 => color = format!("{} {} {} {} k", toks[i - 4], toks[i - 3], toks[i - 2], toks[i - 1]),
            _ => {}
        }
    }
    Style { size, color }
}

fn floats(doc: &Document, obj: &Object) -> Vec<f32> {
    let Ok((_, obj)) = doc.dereference(obj) else { return Vec::new() };
    let Ok(arr) = obj.as_array() else { return Vec::new() };
    arr.iter().filter_map(|o| doc.dereference(o).ok().and_then(|(_, o)| o.as_float().ok())).collect()
}

fn colour_ops(c: &[f32], stroke: bool) -> Option<String> {
    let f = |v: &f32| format!("{:.3}", v);
    match c.len() {
        1 => Some(format!("{} {}", f(&c[0]), if stroke { "G" } else { "g" })),
        3 => Some(format!("{} {} {} {}", f(&c[0]), f(&c[1]), f(&c[2]), if stroke { "RG" } else { "rg" })),
        4 => Some(format!("{} {} {} {} {}", f(&c[0]), f(&c[1]), f(&c[2]), f(&c[3]), if stroke { "K" } else { "k" })),
        _ => None,
    }
}

/// The content of a new appearance stream for a box of `w` x `h` points.
fn appearance_content(w: f32, h: f32, text: &str, style: &Style, fill: Option<String>, border: Option<(String, f32)>, align: i64) -> Vec<u8> {
    let mut c = String::new();
    c.push_str("q\n");
    if let Some(f) = fill {
        c.push_str(&format!("{f}\n0 0 {w:.2} {h:.2} re f\n"));
    }
    let bw = border.as_ref().map(|b| b.1).unwrap_or(0.0);
    if let Some((col, bw)) = &border {
        if *bw > 0.0 {
            c.push_str(&format!("{col}\n{bw:.2} w\n{:.2} {:.2} {:.2} {:.2} re S\n", bw / 2.0, bw / 2.0, w - bw, h - bw));
        }
    }
    let pad = bw + 2.0;
    c.push_str(&format!("{pad:.2} {pad:.2} {:.2} {:.2} re W n\n", (w - 2.0 * pad).max(1.0), (h - 2.0 * pad).max(1.0)));
    let lead = style.size * 1.17;
    let max_w = (w - 2.0 * pad).max(style.size);
    let lines = wrap_lines(text, style.size, max_w);
    let mut out = c.into_bytes();
    out.extend_from_slice(format!("{}\n", style.color).as_bytes());
    for (i, line) in lines.iter().enumerate() {
        let y = h - pad - style.size * 0.95 - lead * i as f32;
        if y < -lead {
            break;
        }
        let lw = text_width(line, style.size);
        let x = match align {
            1 => pad + ((max_w - lw) / 2.0).max(0.0),
            2 => pad + (max_w - lw).max(0.0),
            _ => pad,
        };
        out.extend_from_slice(format!("BT /LPFont {:.2} Tf 1 0 0 1 {x:.2} {y:.2} Tm ", style.size).as_bytes());
        out.extend_from_slice(&content_string(line));
        out.extend_from_slice(b" Tj ET\n");
    }
    out.extend_from_slice(b"Q\n");
    out
}

/// Adds a self-contained appearance stream (with its own Helvetica) to the update and returns its object id.
#[allow(clippy::too_many_arguments)]
fn add_appearance(inc: &mut IncrementalDocument, w: f32, h: f32, text: &str, style: &Style, fill: Option<String>, border: Option<(String, f32)>, align: i64) -> lopdf::ObjectId {
    let content = appearance_content(w, h, text, style, fill, border, align);
    let mut font = Dictionary::new();
    font.set("Type", Object::Name(b"Font".to_vec()));
    font.set("Subtype", Object::Name(b"Type1".to_vec()));
    font.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
    font.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
    let font_id = inc.new_document.add_object(Object::Dictionary(font));

    let mut fonts = Dictionary::new();
    fonts.set("LPFont", Object::Reference(font_id));
    let mut resources = Dictionary::new();
    resources.set("Font", Object::Dictionary(fonts));
    let mut sd = Dictionary::new();
    sd.set("Type", Object::Name(b"XObject".to_vec()));
    sd.set("Subtype", Object::Name(b"Form".to_vec()));
    sd.set("FormType", Object::Integer(1));
    sd.set("BBox", Object::Array(vec![Object::Real(0.0), Object::Real(0.0), Object::Real(w), Object::Real(h)]));
    sd.set("Resources", Object::Dictionary(resources));
    inc.new_document.add_object(Object::Stream(Stream::new(sd, content)))
}

/// Adds a new text box (a FreeText comment) of the given size to a page, as an incremental update.
/// `rect` is left, bottom, right, top in PDF points; `page` is zero-based.
pub fn add_text_box(bytes: &[u8], page: u32, rect: [f32; 4], text: &str, author: &str, now: &str) -> Result<Vec<u8>, String> {
    let doc = Document::load_mem(bytes).map_err(|e| format!("could not read the PDF to add the text box: {e}"))?;
    let page_id = *doc.get_pages().get(&(page + 1)).ok_or("page not found")?;
    let page_dict = doc.get_dictionary(page_id).map_err(|e| e.to_string())?.clone();

    let (l, r) = (rect[0].min(rect[2]), rect[0].max(rect[2]));
    let (b, t) = (rect[1].min(rect[3]), rect[1].max(rect[3]));
    let (l, b) = (l, b);
    let (r, t) = (r.max(l + 24.0), t.max(b + 18.0));
    let (w, h) = (r - l, t - b);
    let style = Style { size: 12.0, color: "0 g".into() };
    let fill = Some("1.000 0.970 0.700 rg".to_string());
    let border = Some(("0.350 0.350 0.350 RG".to_string(), 1.0));

    // where the page keeps its annotation list
    enum Slot {
        None,
        Direct(Vec<Object>),
        Indirect(lopdf::ObjectId, Vec<Object>),
    }
    let slot = match page_dict.get(b"Annots") {
        Err(_) => Slot::None,
        Ok(Object::Array(a)) => Slot::Direct(a.clone()),
        Ok(Object::Reference(id)) => {
            let arr = doc.get_object(*id).and_then(|o| o.as_array()).map_err(|_| "unreadable annotation list".to_string())?;
            Slot::Indirect(*id, arr.clone())
        }
        Ok(_) => return Err("unreadable annotation list".into()),
    };

    let mut inc = IncrementalDocument::create_from(bytes.to_vec(), doc);
    let ap_id = add_appearance(&mut inc, w, h, text, &style, fill, border, 0);

    let mut ap = Dictionary::new();
    ap.set("N", Object::Reference(ap_id));
    let mut bs = Dictionary::new();
    bs.set("W", Object::Integer(1));
    let real = |v: f32| Object::Real(v);
    let mut dict = Dictionary::new();
    dict.set("Type", Object::Name(b"Annot".to_vec()));
    dict.set("Subtype", Object::Name(b"FreeText".to_vec()));
    dict.set("Rect", Object::Array(vec![real(l), real(b), real(r), real(t)]));
    dict.set("F", Object::Integer(4));
    dict.set("T", Object::String(pdf_text_string(author), StringFormat::Literal));
    dict.set("Subj", Object::String(b"Text Box".to_vec(), StringFormat::Literal));
    dict.set("Contents", Object::String(pdf_text_string(&text.replace("\r\n", "\n").replace('\n', "\r")), StringFormat::Literal));
    dict.set("RC", Object::String(pdf_text_string(&crate::edit::rich_text_xhtml(text, false)), StringFormat::Literal));
    dict.set("DA", Object::String(b"0 g /Helv 12 Tf".to_vec(), StringFormat::Literal));
    dict.set("DS", Object::String(b"font: Helvetica 12.0pt; text-align:left; color:#000000".to_vec(), StringFormat::Literal));
    dict.set("IC", Object::Array(vec![real(1.0), real(0.97), real(0.7)]));
    dict.set("C", Object::Array(vec![real(0.35), real(0.35), real(0.35)]));
    dict.set("BS", Object::Dictionary(bs));
    dict.set("Q", Object::Integer(0));
    dict.set("CreationDate", Object::String(format!("D:{now}").into_bytes(), StringFormat::Literal));
    dict.set("M", Object::String(format!("D:{now}").into_bytes(), StringFormat::Literal));
    dict.set("NM", Object::String(format!("lp-{now}-{page}-{}", (l * 7.0 + b * 13.0) as i64).into_bytes(), StringFormat::Literal));
    dict.set("P", Object::Reference(page_id));
    dict.set("AP", Object::Dictionary(ap));
    let annot_id = inc.new_document.add_object(Object::Dictionary(dict));

    match slot {
        Slot::Indirect(id, mut arr) => {
            arr.push(Object::Reference(annot_id));
            inc.new_document.set_object(id, Object::Array(arr));
        }
        Slot::Direct(mut arr) => {
            arr.push(Object::Reference(annot_id));
            let mut pd = page_dict;
            pd.set("Annots", Object::Array(arr));
            inc.new_document.set_object(page_id, Object::Dictionary(pd));
        }
        Slot::None => {
            let mut pd = page_dict;
            pd.set("Annots", Object::Array(vec![Object::Reference(annot_id)]));
            inc.new_document.set_object(page_id, Object::Dictionary(pd));
        }
    }

    let mut out = Vec::with_capacity(bytes.len() + 4096);
    inc.save_to(&mut out).map_err(|e| format!("could not write the edited PDF: {e}"))?;
    Ok(out)
}

/// Returns the file with the text of one text-box comment replaced (appended as an incremental update).
/// `page` is zero-based; `annot` is the position in the page's annotation list.
pub fn set_text_box_text(bytes: &[u8], page: u32, annot: u32, text: &str, now: &str) -> Result<Vec<u8>, String> {
    let doc = Document::load_mem(bytes).map_err(|e| format!("could not read the PDF to edit the text box: {e}"))?;
    let page_id = *doc.get_pages().get(&(page + 1)).ok_or("page not found")?;
    let annots = doc
        .get_dictionary(page_id)
        .and_then(|d| d.get(b"Annots"))
        .and_then(|o| doc.dereference(o))
        .map_err(|_| "the page has no annotations".to_string())?
        .1
        .as_array()
        .map_err(|_| "unreadable annotation list".to_string())?;
    let annot_id = annots
        .get(annot as usize)
        .and_then(|o| o.as_reference().ok())
        .ok_or("this text box is stored in a way LeanPDF cannot edit safely")?;
    let mut dict: Dictionary = doc.get_dictionary(annot_id).map_err(|e| e.to_string())?.clone();
    let is_free_text = dict.get(b"Subtype").ok().and_then(|o| o.as_name().ok()).is_some_and(|n| n == b"FreeText");
    if !is_free_text {
        return Err("the annotation at that position is not a text box".into());
    }

    let rect = dict.get(b"Rect").map(|o| floats(&doc, o)).unwrap_or_default();
    if rect.len() != 4 {
        return Err("the text box has no size".into());
    }
    let (w, h) = ((rect[2] - rect[0]).abs(), (rect[3] - rect[1]).abs());
    let da = dict.get(b"DA").ok().and_then(|o| o.as_str().ok()).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
    let style = parse_da(&da);
    let fill = dict.get(b"IC").ok().map(|o| floats(&doc, o)).and_then(|c| colour_ops(&c, false));
    let border_w = dict
        .get(b"BS")
        .ok()
        .and_then(|o| doc.dereference(o).ok())
        .and_then(|(_, o)| o.as_dict().ok())
        .and_then(|d| d.get(b"W").ok())
        .and_then(|o| o.as_float().ok())
        .unwrap_or(0.0);
    let border = dict.get(b"C").ok().map(|o| floats(&doc, o)).and_then(|c| colour_ops(&c, true)).map(|col| (col, border_w));
    let align = dict.get(b"Q").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0);
    let mono = dict
        .get(b"RC")
        .ok()
        .and_then(|o| o.as_str().ok())
        .is_some_and(|rc| crate::edit::rc_is_monospace(&String::from_utf8_lossy(rc)));

    let mut inc = IncrementalDocument::create_from(bytes.to_vec(), doc);
    let ap_id = add_appearance(&mut inc, w, h, text, &style, fill, border, align);

    let mut ap = Dictionary::new();
    ap.set("N", Object::Reference(ap_id));
    dict.set("AP", Object::Dictionary(ap));
    dict.set("Contents", Object::String(pdf_text_string(&text.replace("\r\n", "\n").replace('\n', "\r")), StringFormat::Literal));
    // Acrobat prefers the rich text over /Contents, so keep it in step.
    if dict.has(b"RC") {
        dict.set("RC", Object::String(pdf_text_string(&crate::edit::rich_text_xhtml(text, mono)), StringFormat::Literal));
    }
    dict.set("M", Object::String(format!("D:{now}").into_bytes(), StringFormat::Literal));
    inc.new_document.set_object(annot_id, Object::Dictionary(dict));

    let mut out = Vec::with_capacity(bytes.len() + 4096);
    inc.save_to(&mut out).map_err(|e| format!("could not write the edited PDF: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_wraps_inside_the_box() {
        let lines = wrap_lines("one two three four five six seven eight nine ten", 12.0, 80.0);
        assert!(lines.len() > 2);
        assert!(lines.iter().all(|l| text_width(l, 12.0) <= 80.0), "{lines:?}");
        assert_eq!(lines.join(" "), "one two three four five six seven eight nine ten");
    }

    #[test]
    fn explicit_line_breaks_are_kept_and_giant_words_are_cut() {
        assert_eq!(wrap_lines("a\r\nb\n\nc", 12.0, 200.0), vec!["a", "b", "", "c"]);
        let cut = wrap_lines("Supercalifragilisticexpialidocious", 12.0, 60.0);
        assert!(cut.len() > 1 && cut.iter().all(|l| text_width(l, 12.0) <= 60.0), "{cut:?}");
    }

    #[test]
    fn da_gives_size_and_colour() {
        let s = parse_da("0.1 0.2 0.9 rg /Helv 14 Tf");
        assert_eq!(s.size, 14.0);
        assert_eq!(s.color, "0.1 0.2 0.9 rg");
        assert_eq!(parse_da("").size, 12.0);
    }

    #[test]
    fn text_strings_use_utf16_only_when_needed() {
        assert_eq!(pdf_text_string("abc"), b"abc");
        assert_eq!(pdf_text_string("é"), vec![0xFE, 0xFF, 0x00, 0xE9]);
    }

    #[test]
    fn content_strings_escape_specials() {
        assert_eq!(content_string("a(b)\\"), b"(a\\(b\\)\\\\)".to_vec());
        assert_eq!(content_string("é"), b"(\\351)".to_vec());
    }
}
