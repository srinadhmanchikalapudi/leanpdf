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
        }
        let mut act: Option<Act> = None;
        let accent = ui.visuals().hyperlink_color;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for (i, a) in d.annots.iter().enumerate() {
                let r = egui::Frame::new().inner_margin(egui::Margin::symmetric(4, 6)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(capitalize(a.kind)).color(accent).strong());
                        ui.label(RichText::new(format!("p.{}", a.page + 1)).weak().small());
                        if let Some(au) = &a.author {
                            ui.label(RichText::new(au).weak().small());
                        }
                        if let Some(m) = &a.modified {
                            ui.label(RichText::new(pdf_date(m)).weak().small());
                        }
                    });
                    if let Some(c) = &a.contents {
                        ui.label(c);
                    }
                    if let Some(t) = &a.text {
                        ui.label(RichText::new(t).weak().small().italics());
                    }
                });
                let resp = ui.interact(r.response.rect, ui.scope_id().with(("c", i)), egui::Sense::click());
                if resp.clicked() {
                    act = Some(Act::Goto(i));
                }
                if resp.hovered() {
                    // buttons appear on hover
                    let rect = r.response.rect;
                    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(rect.right_top() + vec2(-120.0, 2.0), vec2(116.0, 20.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
                    if bui.small_button("Delete").clicked() {
                        act = Some(Act::Delete(i));
                    }
                    if !EDITABLE_EXCLUDED.contains(&a.kind) && bui.small_button(if a.contents.is_some() { "Edit" } else { "Add note" }).clicked() {
                        act = Some(Act::Edit(i));
                    }
                }
                ui.separator();
            }
        });
        match act {
            Some(Act::Goto(i)) => {
                let (page, rect) = {
                    let a = &self.doc.as_ref().unwrap().annots[i];
                    (a.page, a.rect)
                };
                self.goto_page(page as usize, Some(rect));
                if let Some(d) = &mut self.doc {
                    d.flash = Some((page as usize, rect, Instant::now()));
                }
            }
            Some(Act::Edit(i)) => {
                let (page, index, text) = {
                    let a = &self.doc.as_ref().unwrap().annots[i];
                    (a.page, a.index, a.contents.clone().unwrap_or_default())
                };
                let pos = self.ctx.pointer_hover_pos().unwrap_or_default() + vec2(-320.0, 0.0);
                self.editor = Some(NoteEditor { ctx: NoteCtx::Edit { page: page as usize, annot: index }, text, pos, focus: true });
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
                    let (tx, rx) = channel();
                    let _ = self.viewer.send(Cmd::Edit { doc: id, op: EditOp::Delete { page, annot: index }, reply: tx });
                    self.jobs.push(Job::Edit(rx, id, page as usize));
                }
            }
            None => {}
        }
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
