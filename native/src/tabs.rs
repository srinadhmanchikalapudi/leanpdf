//! Open documents as tabs. The active document lives in `App::doc`; the others wait in
//! `App::background` (their page textures are freed, their scroll position and comments kept).

use crate::util::*;
use crate::*;
use eframe::egui::{self, RichText};

impl App {
    pub fn doc_mut(&mut self, id: u32) -> Option<&mut Doc> {
        if self.doc.as_ref().is_some_and(|d| d.id == id) {
            self.doc.as_mut()
        } else {
            self.background.get_mut(&id)
        }
    }

    pub fn tab_for_path(&self, path: &str) -> Option<u32> {
        if let Some(d) = &self.doc {
            if d.path == path {
                return Some(d.id);
            }
        }
        self.background.values().find(|d| d.path == path).map(|d| d.id)
    }

    fn tab_name(&self, id: u32) -> String {
        match &self.doc {
            Some(d) if d.id == id => d.name.clone(),
            _ => self.background.get(&id).map(|d| d.name.clone()).unwrap_or_default(),
        }
    }

    fn tab_path(&self, id: u32) -> String {
        match &self.doc {
            Some(d) if d.id == id => d.path.clone(),
            _ => self.background.get(&id).map(|d| d.path.clone()).unwrap_or_default(),
        }
    }

    /// Moves the active document to the background: frees its textures and saves pending edits.
    pub fn park_active(&mut self) {
        let Some(mut d) = self.doc.take() else { return };
        d.tex.clear();
        d.inflight.clear();
        d.sel = None;
        d.drag = None;
        if self.dirty {
            let (tx, rx) = channel();
            let _ = self.viewer.send(Cmd::Save { doc: d.id, reply: tx });
            self.jobs.push(Job::Save(rx, d.id));
            self.dirty = false;
        }
        self.save_at = None;
        self.seltool = None;
        self.editor = None;
        self.note_tool = false;
        self.save_state.clear();
        self.background.insert(d.id, d);
    }

    /// Makes an already-open tab the active one.
    pub fn activate(&mut self, id: u32) {
        if self.doc.as_ref().is_some_and(|d| d.id == id) {
            return;
        }
        let Some(mut d) = self.background.remove(&id) else { return };
        self.park_active();
        d.scroll_to = Some(d.scroll_y);
        d.viewport_w = 0.0; // re-fit if this tab is in fit-to-width mode
        d.zoom_changed = Instant::now() - Duration::from_secs(1);
        self.doc = Some(d);
        self.reveal = true;
        self.reveal_current();
        self.tab_scroll = true;
        self.remember();
        self.save_ui_state();
    }

    /// Adds a freshly opened document as a tab (after `replace`'s slot, or at the end).
    pub fn add_tab(&mut self, d: Doc, replace: Option<u32>, activate: bool) {
        let id = d.id;
        match replace.and_then(|old| self.tab_order.iter().position(|&t| t == old)) {
            Some(pos) => self.tab_order.insert(pos + 1, id),
            None => self.tab_order.push(id),
        }
        if let Some(old) = replace {
            self.close_tab(old);
        }
        if activate || self.doc.is_none() {
            self.park_active();
            self.doc = Some(d);
            self.tab_scroll = true;
            self.reveal = true;
            self.reveal_current();
        } else {
            self.background.insert(id, d);
        }
        self.save_ui_state();
    }

    pub fn close_tab(&mut self, id: u32) {
        let Some(pos) = self.tab_order.iter().position(|&t| t == id) else { return };
        // The engine saves pending edits before it closes the document.
        let (tx, _rx) = channel();
        let _ = self.viewer.send(Cmd::Close { doc: id, reply: tx });
        if let Some(c) = self.chats.remove(&id) {
            if let Some(s) = c.stream {
                s.cancel_now();
            }
        }
        self.tab_order.remove(pos);
        let was_active = self.doc.as_ref().is_some_and(|d| d.id == id);
        if was_active {
            self.doc = None;
            self.dirty = false;
            self.save_at = None;
            self.seltool = None;
            self.editor = None;
            self.note_tool = false;
            self.save_state.clear();
            let next = self.tab_order.get(pos).or_else(|| self.tab_order.get(pos.wrapping_sub(1))).copied();
            match next {
                Some(n) => self.activate(n),
                None => self.message = "Choose a PDF from the sidebar, or press Ctrl+O.".into(),
            }
        } else {
            self.background.remove(&id);
        }
        self.remember();
        self.save_ui_state();
    }

    pub fn close_active_tab(&mut self) {
        if let Some(id) = self.doc.as_ref().map(|d| d.id) {
            self.close_tab(id);
        }
    }

    pub fn cycle_tab(&mut self, dir: i32) {
        let Some(cur) = self.doc.as_ref().map(|d| d.id) else { return };
        let Some(pos) = self.tab_order.iter().position(|&t| t == cur) else { return };
        let n = self.tab_order.len() as i32;
        if n < 2 {
            return;
        }
        let next = self.tab_order[((pos as i32 + dir).rem_euclid(n)) as usize];
        self.activate(next);
    }

    pub fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let tabs: Vec<(u32, String, String)> = self.tab_order.iter().map(|&id| (id, self.tab_name(id), self.tab_path(id))).collect();
        let active = self.doc.as_ref().map(|d| d.id);
        let scroll_to_active = std::mem::take(&mut self.tab_scroll);
        let mut activate = None;
        let mut close = None;
        ui.add_space(3.0);
        egui::ScrollArea::horizontal().id_salt("tabs").auto_shrink([false, true]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
            ui.horizontal(|ui| {
                for (id, name, path) in &tabs {
                    let is_active = Some(*id) == active;
                    let mut label = name.trim_end_matches(".pdf").to_string();
                    if label.chars().count() > 24 {
                        label = label.chars().take(23).collect::<String>() + "…";
                    }
                    let mut text = RichText::new(label);
                    if is_active {
                        text = text.strong();
                    }
                    let r = ui.add(egui::Button::selectable(is_active, text)).on_hover_text(path);
                    if r.clicked() {
                        activate = Some(*id);
                    }
                    if r.middle_clicked() {
                        close = Some(*id);
                    }
                    if is_active && scroll_to_active {
                        r.scroll_to_me(None);
                    }
                    let x = ui.add(egui::Button::new(RichText::new("×").weak()).frame(false).small()).on_hover_text("Close tab (Ctrl+W)");
                    if x.clicked() {
                        close = Some(*id);
                    }
                    ui.add_space(6.0);
                }
            });
        });
        ui.add_space(3.0);
        if let Some(id) = close {
            self.close_tab(id);
        } else if let Some(id) = activate {
            self.activate(id);
        }
    }

    // ---------- remembering the tabs between runs ----------

    pub fn tab_state(&self) -> Vec<(String, usize)> {
        self.tab_order
            .iter()
            .filter_map(|&id| {
                let d = match &self.doc {
                    Some(d) if d.id == id => Some(d),
                    _ => self.background.get(&id),
                };
                d.map(|d| (d.path.clone(), d.cur_page))
            })
            .collect()
    }

    /// Opens the tabs from the previous run (each as a tab; the remembered one becomes active).
    pub fn restore_tabs(&mut self) {
        let list = std::mem::take(&mut self.restore_pending);
        let active = self.restore_active.take();
        for (path, page) in list {
            if !std::path::Path::new(&path).is_file() {
                continue;
            }
            let is_active = active.as_deref() == Some(path.as_str());
            self.open_file_ex(path, page, Vec::new(), Vec::new(), true, false, is_active);
        }
    }

    pub fn tab_title_for_window(&self) -> String {
        match &self.doc {
            Some(d) => format!("{} – LeanPDF", base_name(&d.path.replace('\\', "/"))),
            None => "LeanPDF".to_string(),
        }
    }
}
