//! Comments panel, comment loading, editing and autosave.

use crate::util::*;
use crate::*;
use eframe::egui::{self, vec2, RichText};
use leanpdf_core::pdf::EditOp;

const CHUNK: u32 = 20;
const EDITABLE_EXCLUDED: [&str; 4] = ["text box", "stamp", "attachment", "caret"];

impl App {
    pub fn comments_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.label(RichText::new("Comments").strong());
        ui.separator();
        let Some(d) = &self.doc else { return };
        if d.annots.is_empty() {
            ui.label(RichText::new("No comments or markup in this PDF.").weak());
            return;
        }
        enum Act {
            Goto(usize),
            Edit(usize),
            Delete(usize),
            Copy(usize),
        }
        let mut act: Option<Act> = None;
        let accent = ui.visuals().hyperlink_color;
        let selected_fill = ui.visuals().selection.bg_fill.gamma_multiply(0.35);
        let sel = self.comment_sel;
        let scroll_to = std::mem::take(&mut self.comment_scroll);
        let sp = self.scroll_speed();
        egui::ScrollArea::vertical().auto_shrink(false).wheel_scroll_multiplier(egui::vec2(sp, sp)).show(ui, |ui| {
            for (i, a) in d.annots.iter().enumerate() {
                let is_sel = sel == Some((a.page, a.index));
                let editable = !EDITABLE_EXCLUDED.contains(&a.kind);
                let frame = egui::Frame::new()
                    .fill(if is_sel { selected_fill } else { egui::Color32::TRANSPARENT })
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(6, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        // header: kind, page, author, date; clicking it jumps to the comment
                        let head = ui
                            .horizontal_wrapped(|ui| {
                                ui.label(RichText::new(capitalize(a.kind)).color(accent).strong());
                                ui.label(RichText::new(format!("p.{}", a.page + 1)).weak().small());
                                if let Some(au) = &a.author {
                                    ui.label(RichText::new(au).weak().small());
                                }
                                if let Some(m) = &a.modified {
                                    ui.label(RichText::new(pdf_date(m)).weak().small());
                                }
                            })
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if head.clicked() {
                            act = Some(Act::Goto(i));
                        }
                        if let Some(c) = &a.contents {
                            let r = comment_text(ui, i, c, a.mono);
                            if r.double_clicked() && editable {
                                act = Some(Act::Edit(i));
                            } else if r.clicked() {
                                act = Some(Act::Goto(i));
                            }
                        }
                        if let Some(t) = &a.text {
                            ui.label(RichText::new(t).weak().small().italics());
                        }
                        // always visible, so the actions are easy to find
                        ui.horizontal(|ui| {
                            if editable && ui.small_button(if a.contents.is_some() { "Edit" } else { "Add note" }).clicked() {
                                act = Some(Act::Edit(i));
                            }
                            if a.contents.is_some() && ui.small_button("Copy").clicked() {
                                act = Some(Act::Copy(i));
                            }
                            if ui.small_button("Delete").clicked() {
                                act = Some(Act::Delete(i));
                            }
                        });
                    });
                if is_sel && scroll_to {
                    frame.response.scroll_to_me(Some(egui::Align::Center));
                }
                ui.separator();
            }
        });
        match act {
            Some(Act::Goto(i)) => {
                let (page, index, rect) = {
                    let a = &self.doc.as_ref().unwrap().annots[i];
                    (a.page, a.index, a.rect)
                };
                self.comment_sel = Some((page, index));
                self.goto_page(page as usize, Some(rect));
                if let Some(d) = &mut self.doc {
                    d.flash = Some((page as usize, rect, Instant::now()));
                }
            }
            Some(Act::Edit(i)) => {
                let (page, index) = {
                    let a = &self.doc.as_ref().unwrap().annots[i];
                    (a.page, a.index)
                };
                let pos = self.ctx.pointer_hover_pos().unwrap_or_default() + vec2(-320.0, -40.0);
                self.begin_edit(page, index, pos);
            }
            Some(Act::Copy(i)) => {
                if let Some(c) = self.doc.as_ref().and_then(|d| d.annots[i].contents.clone()) {
                    self.ctx.copy_text(c);
                    self.toast("Copied");
                }
            }
            Some(Act::Delete(i)) => {
                let (page, index) = {
                    let a = &self.doc.as_ref().unwrap().annots[i];
                    (a.page, a.index)
                };
                let yes = rfd::MessageDialog::new()
                    .set_title("Delete comment")
                    .set_description("Delete this comment from the PDF?")
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .show()
                    == rfd::MessageDialogResult::Yes;
                if yes {
                    let id = self.doc.as_ref().unwrap().id;
                    self.comment_sel = None;
                    let (tx, rx) = channel();
                    let _ = self.viewer.send(Cmd::Edit { doc: id, op: EditOp::Delete { page, annot: index }, reply: tx });
                    self.jobs.push(Job::Edit(rx, id, page as usize));
                }
            }
            None => {}
        }
    }

    /// Opens the comment editor for an existing comment.
    pub fn begin_edit(&mut self, page: u32, index: u32, pos: egui::Pos2) {
        let Some(a) = self.doc.as_ref().and_then(|d| d.annots.iter().find(|a| a.page == page && a.index == index)) else { return };
        if EDITABLE_EXCLUDED.contains(&a.kind) {
            return;
        }
        let text = a.contents.clone().unwrap_or_default();
        let mono = wants_mono(&text, a.mono);
        let text = if mono { expand_tabs(&text) } else { text };
        self.editor = Some(NoteEditor { ctx: NoteCtx::Edit { page: page as usize, annot: index }, text, pos, focus: true, mono });
    }

    // ---------- loading ----------

    /// Reads the comments page range by page range so a big PDF never stalls rendering.
    pub fn maybe_load_annots(&mut self) {
        let in_flight = self.jobs.iter().any(|j| matches!(j, Job::Annots(_, _, None)));
        let Some(d) = &mut self.doc else { return };
        let Some(at) = d.annots_at else { return };
        if Instant::now() < at || in_flight {
            return;
        }
        if d.annots_next as usize >= d.pages.len() {
            d.annots_at = None;
            return;
        }
        let from = d.annots_next;
        d.annots_next += CHUNK;
        let id = d.id;
        let (tx, rx) = channel();
        let _ = self.viewer.send(Cmd::Annots { doc: id, from, to: from + CHUNK, reply: tx });
        self.jobs.push(Job::Annots(rx, id, None));
    }

    pub fn finish_annots(&mut self, res: Result<Vec<Annot>, String>, doc: u32, page: Option<usize>) {
        let Some(d) = self.doc_mut(doc) else { return };
        let Ok(batch) = res else {
            d.annots_at = None;
            return;
        };
        match page {
            Some(p) => d.annots.retain(|a| a.page as usize != p),
            None => {}
        }
        d.annots.extend(batch);
        d.annots.sort_by(|a, b| a.page.cmp(&b.page).then(b.rect[3].partial_cmp(&a.rect[3]).unwrap_or(std::cmp::Ordering::Equal)));
    }

    // ---------- editing ----------

    pub fn save_editor(&mut self) {
        let Some(ed) = self.editor.take() else { return };
        let Some(d) = &self.doc else { return };
        let id = d.id;
        let text = ed.text.trim().to_string();
        match ed.ctx {
            NoteCtx::Sticky { page, x, y } => {
                self.note_tool = false;
                if text.is_empty() {
                    return;
                }
                let (tx, rx) = channel();
                let _ = self.viewer.send(Cmd::Edit { doc: id, op: EditOp::Note { page: page as u32, x, y, text, author: self.author() }, reply: tx });
                self.jobs.push(Job::Edit(rx, id, page));
            }
            NoteCtx::Highlight { page, rects } => {
                let c = self.colors[0];
                self.add_markup(page, "highlight", [c.r(), c.g(), c.b()], Some(text), rects);
            }
            NoteCtx::Edit { page, annot } => {
                let (tx, rx) = channel();
                let _ = self.viewer.send(Cmd::Edit { doc: id, op: EditOp::SetContents { page: page as u32, annot, text }, reply: tx });
                self.jobs.push(Job::Edit(rx, id, page));
            }
        }
    }

    pub fn finish_edit(&mut self, res: Result<(), String>, doc: u32, page: usize) {
        if let Err(e) = res {
            if !e.contains("stale") {
                self.toast(e);
            }
            return;
        }
        let Some(d) = self.doc_mut(doc) else { return };
        // redraw that page and re-read its comments
        *d.page_ver.entry(page).or_insert(0) += 1;
        let (tx, rx) = channel();
        let _ = self.viewer.send(Cmd::Annots { doc, from: page as u32, to: page as u32 + 1, reply: tx });
        self.jobs.push(Job::Annots(rx, doc, Some(page)));
        if self.doc.as_ref().is_some_and(|d| d.id == doc) {
            self.dirty = true;
            self.save_state = "Unsaved".into();
            self.save_at = Some(Instant::now() + Duration::from_millis(900));
        } else {
            // the tab was switched away while this finished: write it straight away
            let (tx, rx) = channel();
            let _ = self.viewer.send(Cmd::Save { doc, reply: tx });
            self.jobs.push(Job::Save(rx, doc));
        }
    }

    /// Writes pending edits into the PDF (as an incremental update Adobe can read).
    pub fn start_save(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(d) = &self.doc else { return };
        if self.jobs.iter().any(|j| matches!(j, Job::Save(..))) {
            self.save_at = Some(Instant::now() + Duration::from_millis(400));
            return;
        }
        self.dirty = false;
        self.save_state = "Saving…".into();
        let (tx, rx) = channel();
        let id = d.id;
        let _ = self.viewer.send(Cmd::Save { doc: id, reply: tx });
        self.jobs.push(Job::Save(rx, id));
    }

    pub fn finish_save(&mut self, res: Result<bool, String>, doc: u32) {
        let active = self.doc.as_ref().is_some_and(|d| d.id == doc);
        match res {
            Ok(wrote) => {
                if active {
                    self.save_state = if wrote && !self.dirty { "Saved".into() } else if self.dirty { "Unsaved".into() } else { String::new() };
                }
            }
            Err(e) => {
                if e.contains("stale") {
                    return;
                }
                if active {
                    self.dirty = true;
                    self.save_state = "Not saved".into();
                }
                self.toast(e);
            }
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Comments pasted from code or a table need a monospace font and their line breaks.
pub fn wants_mono(text: &str, flagged: bool) -> bool {
    flagged || text.contains('\t') || text.lines().any(|l| l.starts_with("  ") || l.trim().contains("  "))
}

/// Tabs become spaces up to the next multiple of 8, so columns line up in a monospace font.
pub fn expand_tabs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split('\n') {
        let mut col = 0usize;
        for ch in line.chars() {
            if ch == '\t' {
                let n = 8 - (col % 8);
                out.extend(std::iter::repeat(' ').take(n));
                col += n;
            } else {
                out.push(ch);
                col += 1;
            }
        }
        out.push('\n');
    }
    out.pop();
    out
}

/// The text of a comment, keeping its line breaks; code and tables in a monospace box.
fn comment_text(ui: &mut egui::Ui, i: usize, text: &str, flagged_mono: bool) -> egui::Response {
    if wants_mono(text, flagged_mono) {
        let shown = expand_tabs(text);
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(4.0)
            .inner_margin(egui::Margin::symmetric(6, 4))
            .show(ui, |ui| {
                egui::ScrollArea::horizontal()
                    .id_salt(("comment-mono", i))
                    .auto_shrink([false, true])
                    .show(ui, |ui| ui.add(egui::Label::new(RichText::new(shown).monospace().size(12.5)).extend().sense(egui::Sense::click())))
                    .inner
            })
            .inner
    } else {
        ui.add(egui::Label::new(text).wrap().sense(egui::Sense::click()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_line_up_columns() {
        assert_eq!(expand_tabs("Item\tQty\nApple\t3"), "Item    Qty\nApple   3");
    }

    #[test]
    fn tables_and_code_are_detected() {
        assert!(wants_mono("a\tb", false));
        assert!(wants_mono("if x {\n    y();\n}", false));
        assert!(wants_mono("Name  Qty", false));
        assert!(wants_mono("plain text", true));
        assert!(!wants_mono("Verify this number with finance", false));
    }
}
