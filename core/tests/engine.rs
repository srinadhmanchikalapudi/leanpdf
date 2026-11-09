//! End-to-end tests of the PDFium worker on a small fixture PDF. They need pdfium.dll
//! (`pdfium/` in the repository; tools/publish.ps1 and the CI workflow download it).

use leanpdf_core::pdf::{self, Cmd, DocInfo, EditOp};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("sample.pdf")
}

/// A private copy of the fixture, so tests that save do not touch the original.
fn scratch_copy(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("leanpdf-test-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    let to = dir.join("sample.pdf");
    std::fs::copy(fixture(), &to).unwrap();
    to
}

fn ask<T: Send + 'static>(tx: &Sender<Cmd>, make: impl FnOnce(Sender<Result<T, String>>) -> Cmd) -> Result<T, String> {
    let (rtx, rrx) = channel();
    tx.send(make(rtx)).unwrap();
    rrx.recv_timeout(std::time::Duration::from_secs(30)).expect("worker did not answer")
}

fn open(tx: &Sender<Cmd>, path: &PathBuf) -> DocInfo {
    ask(tx, |reply| Cmd::Open { path: path.to_string_lossy().into_owned(), reply }).expect("open")
}

fn render(tx: &Sender<Cmd>, doc: u32, page: u32, width: u32, rot: u8) -> (u32, u32, Vec<u8>) {
    let (rtx, rrx) = channel();
    tx.send(Cmd::RenderRgba { doc, page, width, rot, reply: Box::new(move |r| drop(rtx.send(r))) }).unwrap();
    rrx.recv_timeout(std::time::Duration::from_secs(30)).unwrap().expect("render")
}

#[test]
fn opens_and_reports_page_sizes() {
    let tx = pdf::spawn_worker();
    let info = open(&tx, &fixture());
    assert_eq!(info.page_count, 3);
    assert_eq!(info.pages[0], [612.0, 792.0]);
    assert_eq!(info.pages[2], [792.0, 612.0], "page 3 is landscape");
}

#[test]
fn a_missing_file_is_an_error_not_a_crash() {
    let tx = pdf::spawn_worker();
    let r = ask(&tx, |reply| Cmd::Open { path: "does-not-exist.pdf".into(), reply });
    assert!(r.is_err());
}

#[test]
fn reads_the_outline_with_levels_and_pages() {
    let tx = pdf::spawn_worker();
    let info = open(&tx, &fixture());
    let items = ask(&tx, |reply| Cmd::Outline { doc: info.id, reply }).unwrap();
    let titles: Vec<_> = items.iter().map(|i| (i.title.as_str(), i.depth, i.page)).collect();
    assert_eq!(
        titles,
        vec![("Chapter One", 0, Some(0)), ("Chapter Two", 0, Some(1)), ("Section 2.1", 1, Some(1)), ("Chapter Three", 0, Some(2))]
    );
}

#[test]
fn finds_every_match_across_pages() {
    let tx = pdf::spawn_worker();
    let info = open(&tx, &fixture());
    let hits = ask(&tx, |reply| Cmd::FindRange { doc: info.id, term: "renewal".into(), from: 0, to: info.page_count, reply }).unwrap();
    let pages: Vec<u32> = hits.iter().map(|h| h.page).collect();
    assert_eq!(pages, vec![0, 1], "matching ignores case and reports each page");
    assert!(hits.iter().all(|h| !h.rects.is_empty()));
    let none = ask(&tx, |reply| Cmd::FindRange { doc: info.id, term: "zebra".into(), from: 0, to: info.page_count, reply }).unwrap();
    assert!(none.is_empty());
    let empty = ask(&tx, |reply| Cmd::FindRange { doc: info.id, term: "  ".into(), from: 0, to: info.page_count, reply }).unwrap();
    assert!(empty.is_empty(), "a blank search finds nothing");
}

#[test]
fn rotated_pages_are_rendered_turned() {
    let tx = pdf::spawn_worker();
    let info = open(&tx, &fixture());
    let (w0, h0, _) = render(&tx, info.id, 0, 306, 0);
    assert!(h0 > w0, "portrait page stays portrait: {w0}x{h0}");
    let (w1, h1, _) = render(&tx, info.id, 0, 400, 1);
    assert!(w1 > h1, "a quarter turn makes the portrait page landscape: {w1}x{h1}");
    assert!((w1 as i32 - 400).abs() <= 2, "the requested width is the width as shown: {w1}");
    let (w2, h2, _) = render(&tx, info.id, 0, 306, 2);
    assert_eq!((w2, h2), (w0, h0), "a half turn keeps the shape");
}

#[test]
fn adding_editing_and_deleting_a_comment_round_trips_through_the_file() {
    let path = scratch_copy("roundtrip");
    let tx = pdf::spawn_worker();
    let info = open(&tx, &path);

    ask(&tx, |reply| Cmd::Edit { doc: info.id, op: EditOp::Note { page: 0, x: 100.0, y: 600.0, text: "first".into(), author: "Tester".into() }, reply }).unwrap();
    assert!(ask(&tx, |reply| Cmd::Save { doc: info.id, reply }).unwrap(), "something was written");

    let list = ask(&tx, |reply| Cmd::Annots { doc: info.id, from: 0, to: 3, reply }).unwrap();
    let note = list.iter().find(|a| a.kind == "note").expect("the note is listed after saving");
    assert_eq!(note.contents.as_deref(), Some("first"));
    assert_eq!(note.author.as_deref(), Some("Tester"));

    ask(&tx, |reply| Cmd::Edit { doc: info.id, op: EditOp::SetContents { page: 0, annot: note.index, text: "line one\nline two".into() }, reply }).unwrap();
    ask(&tx, |reply| Cmd::Save { doc: info.id, reply }).unwrap();
    let list = ask(&tx, |reply| Cmd::Annots { doc: info.id, from: 0, to: 3, reply }).unwrap();
    let note = list.iter().find(|a| a.kind == "note").unwrap();
    assert_eq!(note.contents.as_deref(), Some("line one\nline two"), "line breaks survive the round trip");

    ask(&tx, |reply| Cmd::Edit { doc: info.id, op: EditOp::Delete { page: 0, annot: note.index }, reply }).unwrap();
    ask(&tx, |reply| Cmd::Save { doc: info.id, reply }).unwrap();
    let list = ask(&tx, |reply| Cmd::Annots { doc: info.id, from: 0, to: 3, reply }).unwrap();
    assert!(list.iter().all(|a| a.kind != "note"));
}

#[test]
fn saving_only_appends_so_the_original_bytes_are_kept() {
    let path = scratch_copy("append");
    let before = std::fs::read(&path).unwrap();
    let tx = pdf::spawn_worker();
    let info = open(&tx, &path);
    ask(&tx, |reply| Cmd::Edit { doc: info.id, op: EditOp::Note { page: 1, x: 80.0, y: 500.0, text: "kept".into(), author: "Tester".into() }, reply }).unwrap();
    ask(&tx, |reply| Cmd::Save { doc: info.id, reply }).unwrap();
    let after = std::fs::read(&path).unwrap();
    assert!(after.len() > before.len());
    assert_eq!(&after[..before.len()], &before[..], "an incremental update never rewrites existing bytes");
}

#[test]
fn page_text_is_extracted() {
    let tx = pdf::spawn_worker();
    let info = open(&tx, &fixture());
    let texts = ask(&tx, |reply| Cmd::Text { doc: info.id, from: 0, to: 3, reply }).unwrap();
    assert_eq!(texts.len(), 3);
    assert!(texts[0].1.contains("quick brown fox"));
}

fn textbox_fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("leanpdf-test-{}-textbox-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    let to = dir.join("textbox.pdf");
    std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("textbox.pdf"), &to).unwrap();
    to
}

#[test]
fn a_text_box_comment_can_be_edited_and_its_appearance_changes() {
    let path = textbox_fixture("edit");
    let before = std::fs::read(&path).unwrap();
    let tx = pdf::spawn_worker();
    let info = open(&tx, &path);
    let list = ask(&tx, |reply| Cmd::Annots { doc: info.id, from: 0, to: 1, reply }).unwrap();
    let boxed = list.iter().find(|a| a.kind == "text box").expect("the text box is listed");
    assert_eq!(boxed.contents.as_deref(), Some("Original box text"));
    let (_, _, pixels_before) = render(&tx, info.id, 0, 612, 0);

    ask(&tx, |reply| Cmd::EditTextBox { doc: info.id, page: 0, annot: boxed.index, text: "Changed text\nsecond line".into(), reply }).unwrap();

    let list = ask(&tx, |reply| Cmd::Annots { doc: info.id, from: 0, to: 1, reply }).unwrap();
    let boxed = list.iter().find(|a| a.kind == "text box").unwrap();
    assert_eq!(boxed.contents.as_deref(), Some("Changed text\nsecond line"));
    let (_, _, pixels_after) = render(&tx, info.id, 0, 612, 0);
    assert_ne!(pixels_before, pixels_after, "the drawn box changed with its text, not just the stored text");

    let after = std::fs::read(&path).unwrap();
    assert_eq!(&after[..before.len()], &before[..], "appended, original bytes untouched");
    if let Ok(keep) = std::env::var("LEANPDF_TEST_OUT") {
        let _ = std::fs::write(keep, &after); // lets a person open the result in other programs
    }
    // the new appearance must carry its own font, or a strict viewer could draw nothing
    let text = String::from_utf8_lossy(&after[before.len()..]).into_owned();
    assert!(text.contains("/LPFont") && text.contains("/Helvetica"), "{text}");
}

#[test]
fn editing_something_that_is_not_a_text_box_is_refused() {
    let path = scratch_copy("notbox");
    let tx = pdf::spawn_worker();
    let info = open(&tx, &path);
    let r = ask(&tx, |reply| Cmd::EditTextBox { doc: info.id, page: 0, annot: 0, text: "x".into(), reply });
    assert!(r.is_err());
}

#[test]
fn a_new_text_box_can_be_added_by_size_and_then_edited() {
    let path = scratch_copy("addbox");
    let before = std::fs::read(&path).unwrap();
    let tx = pdf::spawn_worker();
    let info = open(&tx, &path);
    let (_, _, pixels_before) = render(&tx, info.id, 0, 612, 0);

    ask(&tx, |reply| Cmd::AddTextBox { doc: info.id, page: 0, rect: [100.0, 400.0, 340.0, 480.0], text: "A new box\nwith two lines".into(), author: "Tester".into(), reply }).unwrap();

    let list = ask(&tx, |reply| Cmd::Annots { doc: info.id, from: 0, to: 1, reply }).unwrap();
    let boxed = list.iter().find(|a| a.kind == "text box").expect("the new text box is listed");
    assert_eq!(boxed.contents.as_deref(), Some("A new box\nwith two lines"));
    assert_eq!(boxed.author.as_deref(), Some("Tester"));
    assert!((boxed.rect[2] - boxed.rect[0] - 240.0).abs() < 0.5 && (boxed.rect[3] - boxed.rect[1] - 80.0).abs() < 0.5, "{:?}", boxed.rect);
    let (_, _, pixels_after) = render(&tx, info.id, 0, 612, 0);
    assert_ne!(pixels_before, pixels_after, "the box is drawn");

    // and it can be edited like any text box
    ask(&tx, |reply| Cmd::EditTextBox { doc: info.id, page: 0, annot: boxed.index, text: "edited".into(), reply }).unwrap();
    let list = ask(&tx, |reply| Cmd::Annots { doc: info.id, from: 0, to: 1, reply }).unwrap();
    assert_eq!(list.iter().find(|a| a.kind == "text box").unwrap().contents.as_deref(), Some("edited"));

    let after = std::fs::read(&path).unwrap();
    assert_eq!(&after[..before.len()], &before[..], "appended, original bytes untouched");
    if let Ok(keep) = std::env::var("LEANPDF_TEST_OUT_ADD") {
        let _ = std::fs::write(keep, &after);
    }
}

#[test]
fn a_text_box_can_be_added_to_a_page_that_already_has_annotations() {
    let path = textbox_fixture("add");
    let tx = pdf::spawn_worker();
    let info = open(&tx, &path);
    ask(&tx, |reply| Cmd::AddTextBox { doc: info.id, page: 0, rect: [320.0, 300.0, 400.0, 340.0], text: "second".into(), author: "Tester".into(), reply }).unwrap();
    let list = ask(&tx, |reply| Cmd::Annots { doc: info.id, from: 0, to: 1, reply }).unwrap();
    assert_eq!(list.iter().filter(|a| a.kind == "text box").count(), 2, "the old text box is still there");
}
