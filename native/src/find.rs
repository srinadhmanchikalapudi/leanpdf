//! Find in this document (Ctrl+F): a bar under the tabs, every match highlighted on the pages,
//! Enter / F3 for the next match and Shift+Enter / Shift+F3 for the previous one.

use crate::*;
use eframe::egui::{self, Key, RichText};
use leanpdf_core::pdf::FindMatch;

/// Pages searched per request, so page renders are never kept waiting for long.
const CHUNK: u32 = 40;

#[derive(Default)]
pub struct FindState {
    pub open: bool,
    pub term: String,
    pub focus: bool,
    pub matches: Vec<FindMatch>,
    pub cur: Option<usize>,
    pub next_page: u32,
    pub searching: bool,
    pub seq: u64,
    /// The text the current results are for.
    pub for_term: String,
    pub capped: bool,
}

impl App {
    pub fn open_find(&mut self) {
        let Some(d) = &mut self.doc else { return };
        d.find.open = true;
        d.find.focus = true;
        if let Some(s) = &d.sel {
            let t = s.text.trim();
            if !t.is_empty() && t.chars().count() <= 80 && !t.contains('\n') {
                d.find.term = t.to_string();
            }
        }
        let again = d.find.term.trim() != d.find.for_term || d.find.matches.is_empty();
        if !d.find.term.trim().is_empty() && again {
            self.start_find();
        }
        self.refit();
    }

    pub fn close_find(&mut self) {
        if let Some(d) = &mut self.doc {
            d.find.open = false;
            d.find.matches.clear();
            d.find.cur = None;
            d.find.seq += 1;
            d.find.searching = false;
            d.find.for_term.clear();
        }
        self.refit();
    }

    pub fn start_find(&mut self) {
        let Some(d) = &mut self.doc else { return };
        let f = &mut d.find;
        f.seq += 1;
        f.matches.clear();
        f.cur = None;
        f.next_page = 0;
        f.capped = false;
        f.for_term = f.term.trim().to_string();
        f.searching = !f.for_term.is_empty();
        if f.searching {
            self.find_chunk();
        }
    }

    fn find_chunk(&mut self) {
        let Some(d) = &self.doc else { return };
        let (tx, rx) = channel();
        let _ = self.viewer.send(Cmd::FindRange { doc: d.id, term: d.find.for_term.clone(), from: d.find.next_page, to: d.find.next_page + CHUNK, reply: tx });
        let (id, seq) = (d.id, d.find.seq);
        self.jobs.push(Job::FindRange(rx, id, seq));
    }

    pub fn finish_find_range(&mut self, res: Result<Vec<FindMatch>, String>, doc: u32, seq: u64) {
        let more;
        let mut jump = None;
        {
            let Some(d) = self.doc_mut(doc) else { return };
            if d.find.seq != seq || !d.find.open {
                return;
            }
            let total = d.pages.len() as u32;
            match res {
                Ok(mut m) => {
                    let first = d.find.matches.is_empty();
                    d.find.matches.append(&mut m);
                    if d.find.matches.len() >= 5000 {
                        d.find.capped = true;
                    }
                    d.find.next_page += CHUNK;
                    more = d.find.next_page < total && !d.find.capped;
                    if first && !d.find.matches.is_empty() {
                        // start at the first match on or after the page being read
                        let cur_page = d.cur_page as u32;
                        let idx = d.find.matches.iter().position(|x| x.page >= cur_page).unwrap_or(0);
                        d.find.cur = Some(idx);
                        jump = Some(idx);
                    } else if d.find.cur.is_none() && !d.find.matches.is_empty() && !more {
                        d.find.cur = Some(0);
                        jump = Some(0);
                    }
                }
                Err(_) => more = false,
            }
            if !more {
                d.find.searching = false;
            }
        }
        if let Some(i) = jump {
            self.show_find_match(i);
        }
        if more {
            self.find_chunk();
        }
    }

    fn show_find_match(&mut self, i: usize) {
        let Some(d) = &self.doc else { return };
        let Some(m) = d.find.matches.get(i) else { return };
        let (page, rect) = (m.page as usize, m.rects.first().copied());
        self.goto_page(page, rect);
    }

    pub fn find_step(&mut self, dir: i32) {
        let Some(d) = &mut self.doc else { return };
        let n = d.find.matches.len();
        if n == 0 {
            return;
        }
        let next = match d.find.cur {
            Some(c) => (c as i32 + dir).rem_euclid(n as i32) as usize,
            None => 0,
        };
        d.find.cur = Some(next);
        self.show_find_match(next);
    }

    pub fn find_bar(&mut self, ui: &mut egui::Ui) {
        let Some(d) = &mut self.doc else { return };
        let mut step = 0i32;
        let mut close = false;
        let mut restart = false;
        let id = egui::Id::new("find_input");
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            ui.label("Find");
            let r = ui.add(egui::TextEdit::singleline(&mut d.find.term).id(id).desired_width(260.0).hint_text("Search this document"));
            if d.find.focus {
                r.request_focus();
                d.find.focus = false;
            }
            if r.changed() {
                restart = true;
            }
            // Enter makes a single-line field give up focus, so look at lost_focus as well
            if r.has_focus() || r.lost_focus() {
                let (enter, shift, esc) = ui.input(|i| (i.key_pressed(Key::Enter), i.modifiers.shift, i.key_pressed(Key::Escape)));
                if enter {
                    if d.find.term.trim() != d.find.for_term {
                        restart = true;
                    } else {
                        step = if shift { -1 } else { 1 };
                    }
                }
                if esc {
                    close = true;
                }
            }
            let has = !d.find.matches.is_empty();
            if ui.add_enabled(has, egui::Button::new("Prev")).on_hover_text("Previous match (Shift+Enter)").clicked() {
                step = -1;
            }
            if ui.add_enabled(has, egui::Button::new("Next")).on_hover_text("Next match (Enter, F3)").clicked() {
                step = 1;
            }
            let status = if d.find.for_term.is_empty() {
                String::new()
            } else if d.find.matches.is_empty() {
                if d.find.searching { "Searching…".to_string() } else { "No matches".to_string() }
            } else {
                let n = d.find.matches.len();
                let cur = d.find.cur.map(|c| c + 1).unwrap_or(0);
                let plus = if d.find.capped { "+" } else { "" };
                if d.find.searching { format!("{cur} of {n}{plus}…") } else { format!("{cur} of {n}{plus}") }
            };
            ui.label(RichText::new(status).weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::new(RichText::new("×").weak()).frame(false)).on_hover_text("Close (Esc)").clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(3.0);
        if close {
            self.close_find();
        } else if restart {
            self.start_find();
            if let Some(d) = &mut self.doc {
                d.find.focus = true;
            }
        } else if step != 0 {
            self.find_step(step);
            if let Some(d) = &mut self.doc {
                d.find.focus = true;
            }
        }
    }
}
