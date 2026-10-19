//! Toolbar, the scrolling page view, text selection, floating tools, shortcuts.

use crate::*;
use eframe::egui::{self, pos2, vec2, Color32, Pos2, Rect, RichText, Sense, Stroke};
use leanpdf_core::pdf::EditOp;

const PT: f32 = 96.0 / 72.0; // logical pixels per PDF point at 100%
const GAP: f32 = 14.0;
const MIN_ZOOM: f32 = 0.25;
const MAX_ZOOM: f32 = 5.0;
const MAX_INFLIGHT: usize = 3;

impl Doc {
    pub fn layout(&mut self) {
        self.tops.clear();
        let mut y = GAP;
        for p in &self.pages {
            self.tops.push(y);
            y += p[1] * PT * self.zoom + GAP;
        }
        self.total_h = y + 30.0;
    }

    pub fn page_size(&self, i: usize) -> egui::Vec2 {
        vec2(self.pages[i][0] * PT * self.zoom, self.pages[i][1] * PT * self.zoom)
    }

    fn wanted_width(&self, i: usize, ppp: f32) -> u32 {
        ((self.pages[i][0] * PT * self.zoom * ppp).round() as u32).clamp(100, 4000)
    }

    fn fit_zoom(&self, avail_w: f32) -> f32 {
        let i = self.cur_page.min(self.pages.len().saturating_sub(1));
        ((avail_w - 48.0) / (self.pages[i][0] * PT)).clamp(MIN_ZOOM, MAX_ZOOM)
    }
}

enum ViewAct {
    DragStart { page: usize, pos: Pos2, rect: Rect },
    DragMove { pos: Pos2, rect: Rect },
    DragEnd { pos: Pos2, rect: Rect },
    Sticky { page: usize, pos: Pos2, rect: Rect },
}

fn to_pdf(rect: Rect, size: [f32; 2], pos: Pos2) -> [f32; 2] {
    [(pos.x - rect.left()) / rect.width() * size[0], size[1] - (pos.y - rect.top()) / rect.height() * size[1]]
}

fn from_pdf(rect: Rect, size: [f32; 2], r: Rect4) -> Rect {
    let (sx, sy) = (rect.width() / size[0], rect.height() / size[1]);
    Rect::from_min_max(
        pos2(rect.left() + r[0] * sx, rect.top() + (size[1] - r[3]) * sy),
        pos2(rect.left() + r[2] * sx, rect.top() + (size[1] - r[1]) * sy),
    )
}

impl App {
    // ---------- toolbar ----------

    pub fn toolbar(&mut self, ui: &mut egui::Ui) {
        let (can_prev, can_next) = self.nav_state();
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            if ui.selectable_label(self.side_open, "Sidebar").on_hover_text("Show / hide sidebar (Ctrl+B)").clicked() {
                self.side_open = !self.side_open;
                self.refit();
            }
            if ui.button("Open…").on_hover_text("Open a PDF (Ctrl+O)").clicked() {
                self.open_dialog();
            }
            ui.separator();
            if ui.add_enabled(can_prev, egui::Button::new("‹ Prev")).on_hover_text("Previous PDF in folder (Alt+←)").clicked() {
                self.step_file(-1);
            }
            if ui.add_enabled(can_next, egui::Button::new("Next ›")).on_hover_text("Next PDF in folder (Alt+→)").clicked() {
                self.step_file(1);
            }
            if let Some(d) = &self.doc {
                ui.label(RichText::new(&d.name).strong()).on_hover_text(&d.path);
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let n = self.annot_count();
                let label = format!("Comments  {n}");
                if ui.selectable_label(self.comments_open, label).on_hover_text("Comments in this PDF").clicked() {
                    self.comments_open = !self.comments_open;
                    self.refit();
                }
                ui.separator();
                if self.doc.is_some() {
                    let c = if self.save_state == "Not saved" { Color32::from_rgb(220, 90, 80) } else { ui.visuals().weak_text_color() };
                    ui.label(RichText::new(&self.save_state).small().color(c));
                    if ui.selectable_label(self.note_tool, "+ Note").on_hover_text("Place a sticky note (N)").clicked() {
                        self.set_note_tool(!self.note_tool);
                    }
                    ui.separator();
                    if ui.button("Fit").on_hover_text("Fit to width (Ctrl+0)").clicked() {
                        self.fit_now();
                    }
                    if ui.button("+").on_hover_text("Zoom in (Ctrl++)").clicked() {
                        self.zoom_by(1.15);
                    }
                    let z = self.doc.as_ref().map(|d| d.zoom).unwrap_or(1.0);
                    ui.label(RichText::new(format!("{:>3}%", (z * 100.0).round() as i32)).weak());
                    if ui.button("−").on_hover_text("Zoom out (Ctrl+−)").clicked() {
                        self.zoom_by(1.0 / 1.15);
                    }
                    ui.separator();
                    let total = self.doc.as_ref().map(|d| d.pages.len()).unwrap_or(0);
                    ui.label(RichText::new(format!("/ {total}")).weak());
                    let mut page = self.doc.as_ref().map(|d| d.cur_page + 1).unwrap_or(1);
                    let r = ui.add(egui::DragValue::new(&mut page).range(1..=total.max(1)).speed(0.2));
                    if r.changed() {
                        self.goto_page(page - 1, None);
                    }
                }
            });
        });
        ui.add_space(3.0);
    }

    pub fn open_dialog(&mut self) {
        let mut dlg = rfd::FileDialog::new().add_filter("PDF", &["pdf"]);
        if let Some(f) = &self.settings.folder {
            dlg = dlg.set_directory(f);
        }
        if let Some(p) = dlg.pick_file() {
            self.open_file(p.to_string_lossy().into_owned(), 0, Vec::new(), Vec::new(), false);
        }
    }

    pub fn annot_count(&self) -> usize {
        self.doc.as_ref().map(|d| d.annots.len()).unwrap_or(0)
    }

    // ---------- zoom / navigation ----------

    /// Called when panels open/close so a fit-to-width view follows the new width.
    fn refit(&mut self) {
        if let Some(d) = &mut self.doc {
            d.viewport_w = 0.0; // forces re-evaluation next frame
        }
    }

    pub fn zoom_by(&mut self, f: f32) {
        if let Some(d) = &self.doc {
            let z = d.zoom * f;
            self.set_zoom(z, false);
        }
    }

    pub fn fit_now(&mut self) {
        if let Some(d) = &self.doc {
            let z = d.fit_zoom(d.viewport_w.max(400.0));
            self.set_zoom(z, true);
        }
    }

    pub fn set_zoom(&mut self, z: f32, fit: bool) {
        let Some(d) = &mut self.doc else { return };
        let z = z.clamp(MIN_ZOOM, MAX_ZOOM);
        let cur = d.cur_page.min(d.pages.len() - 1);
        let h_old = d.page_size(cur).y.max(1.0);
        let frac = (d.scroll_y - d.tops[cur]) / h_old;
        d.zoom = z;
        d.fit = fit;
        d.layout();
        let h_new = d.page_size(cur).y;
        d.scroll_to = Some((d.tops[cur] + frac * h_new).max(0.0));
        d.zoom_changed = Instant::now();
    }

    pub fn goto_page(&mut self, i: usize, rect: Option<Rect4>) {
        let Some(d) = &mut self.doc else { return };
        if i >= d.pages.len() {
            return;
        }
        let mut y = d.tops[i] - 10.0;
        if let Some(r) = rect {
            let (h, ph) = (d.page_size(i).y, d.pages[i][1]);
            y = d.tops[i] + (ph - r[3]) / ph * h - d.viewport_h * 0.3;
        }
        d.scroll_to = Some(y.max(0.0));
        d.cur_page = i;
        self.remember_at = Some(Instant::now() + Duration::from_millis(900));
    }

    // ---------- the page view ----------

    pub fn viewer_ui(&mut self, ui: &mut egui::Ui) {
        self.view_rect = ui.max_rect();
        let ppp = ui.ctx().pixels_per_point();
        let avail = ui.available_size();
        let Some(doc) = &mut self.doc else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(&self.message).weak());
            });
            return;
        };

        // fit-to-width follows the panel width
        if doc.fit && (doc.viewport_w - avail.x).abs() > 1.0 {
            let z = doc.fit_zoom(avail.x);
            if (z - doc.zoom).abs() > 0.001 {
                let cur = doc.cur_page.min(doc.pages.len() - 1);
                let frac = (doc.scroll_y - doc.tops[cur]) / doc.page_size(cur).y.max(1.0);
                doc.zoom = z;
                doc.layout();
                doc.scroll_to = Some((doc.tops[cur] + frac * doc.page_size(cur).y).max(0.0));
                doc.zoom_changed = Instant::now();
            }
        }
        doc.viewport_w = avail.x;
        doc.viewport_h = avail.y;

        let mut area = egui::ScrollArea::both().id_salt("pages").auto_shrink(false);
        if let Some(y) = doc.scroll_to.take() {
            area = area.vertical_scroll_offset(y);
        }

        let note_tool = self.note_tool;
        let settled = doc.zoom_changed.elapsed() > Duration::from_millis(150);
        let mut acts: Vec<ViewAct> = Vec::new();
        let mut want: Vec<(usize, u32)> = Vec::new();
        let mut visible = (usize::MAX, 0usize);

        let out = area.show_viewport(ui, |ui, viewport| {
            let max_w = doc.pages.iter().map(|p| p[0]).fold(0.0, f32::max) * PT * doc.zoom + 48.0;
            let content_w = max_w.max(viewport.width());
            let (content, _) = ui.allocate_exact_size(vec2(content_w, doc.total_h), Sense::hover());
            let painter = ui.painter_at(ui.clip_rect());

            let margin = viewport.height() * 0.35;
            let first = doc.tops.partition_point(|&t| t < viewport.min.y - margin).saturating_sub(1);
            let last = doc.tops.partition_point(|&t| t < viewport.max.y + margin).min(doc.pages.len() - 1);
            visible = (first, last);

            for i in first..=last {
                let size = doc.page_size(i);
                let rect = Rect::from_min_size(pos2(content.left() + (content_w - size.x) / 2.0, content.top() + doc.tops[i]), size);
                painter.rect_filled(rect.translate(vec2(0.0, 2.0)), 2.0, Color32::from_black_alpha(60));
                painter.rect_filled(rect, 0.0, Color32::WHITE);
                if let Some(t) = doc.tex.get(&i) {
                    painter.image(t.tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                let wanted = doc.wanted_width(i, ppp);
                let ver = doc.page_ver.get(&i).copied().unwrap_or(0);
                let fresh = doc.tex.get(&i).is_some_and(|t| t.width_px == wanted && t.ver == ver);
                if !fresh && settled && doc.inflight.get(&i) != Some(&(wanted + ver * 100_000)) {
                    want.push((i, wanted));
                }

                // overlays: search hits, selection, flash
                if let Some(rs) = doc.found.get(&i) {
                    for r in rs {
                        painter.rect_filled(from_pdf(rect, doc.pages[i], *r), 2.0, Color32::from_rgba_unmultiplied(255, 196, 0, 110));
                    }
                }
                if let Some(s) = doc.sel.as_ref().filter(|s| s.page == i) {
                    for r in &s.rects {
                        painter.rect_filled(from_pdf(rect, doc.pages[i], *r), 2.0, Color32::from_rgba_unmultiplied(37, 99, 235, 90));
                    }
                }
                if let Some((p, r, t)) = &doc.flash {
                    if *p == i && t.elapsed() < Duration::from_millis(2600) {
                        let k = 1.0 - (t.elapsed().as_secs_f32() / 2.6).powi(2);
                        let rr = from_pdf(rect, doc.pages[i], *r).expand(2.0);
                        painter.rect_filled(rr, 3.0, Color32::from_rgba_unmultiplied(110, 160, 255, (90.0 * k) as u8));
                        painter.rect_stroke(rr, 3.0, Stroke::new(2.0, Color32::from_rgba_unmultiplied(70, 120, 255, (255.0 * k) as u8)), egui::StrokeKind::Outside);
                    }
                }

                // interaction
                let resp = ui.interact(rect, ui.scope_id().with(("page", i)), Sense::click_and_drag());
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(if note_tool { egui::CursorIcon::Crosshair } else { egui::CursorIcon::Text });
                }
                let ptr = ui.input(|inp| inp.pointer.latest_pos());
                if resp.drag_started_by(egui::PointerButton::Primary) && !note_tool {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        acts.push(ViewAct::DragStart { page: i, pos, rect });
                    }
                } else if resp.dragged_by(egui::PointerButton::Primary) && !note_tool {
                    if let Some(pos) = ptr {
                        acts.push(ViewAct::DragMove { pos, rect });
                    }
                } else if resp.drag_stopped() {
                    if let Some(pos) = ptr {
                        acts.push(ViewAct::DragEnd { pos, rect });
                    }
                } else if resp.clicked() {
                    if let Some(pos) = resp.interact_pointer_pos() {
                        if note_tool {
                            acts.push(ViewAct::Sticky { page: i, pos, rect });
                        } else {
                            acts.push(ViewAct::DragStart { page: i, pos, rect }); // a plain click clears the selection
                            acts.push(ViewAct::DragEnd { pos, rect });
                        }
                    }
                }
            }
        });
        doc.scroll_y = out.state.clamped_offset().y;

        // current page = the one under the upper third of the view
        let y = doc.scroll_y + avail.y * 0.3;
        let cur = doc.tops.partition_point(|&t| t <= y).saturating_sub(1).min(doc.pages.len() - 1);
        let page_changed = cur != doc.cur_page && doc.scroll_to.is_none();
        if page_changed {
            doc.cur_page = cur;
        }

        // free textures far from the view
        let (first, last) = visible;
        if first != usize::MAX {
            let keep_lo = first.saturating_sub(1);
            let keep_hi = last + 1;
            doc.tex.retain(|i, _| *i >= keep_lo && *i <= keep_hi);
        }

        // request renders, nearest page first
        want.sort_by_key(|(i, _)| (*i as i64 - doc.cur_page as i64).abs());
        let doc_id = doc.id;
        for (i, wanted) in want {
            if doc.inflight.len() >= MAX_INFLIGHT {
                break;
            }
            let ver = doc.page_ver.get(&i).copied().unwrap_or(0);
            doc.inflight.insert(i, wanted + ver * 100_000);
            let sink = self.renders.clone();
            let ctx = self.ctx.clone();
            let _ = self.viewer.send(Cmd::RenderRgba {
                doc: doc_id,
                page: i as u32,
                width: wanted,
                reply: Box::new(move |image| {
                    sink.lock().unwrap().push(RenderDone { doc: doc_id, page: i, width: wanted, ver, image });
                    ctx.request_repaint();
                }),
            });
        }

        for a in acts {
            self.apply_view_act(a);
        }
        if page_changed {
            self.remember_at = Some(Instant::now() + Duration::from_millis(900));
        }
    }

    pub fn accept_render(&mut self, r: RenderDone) {
        let Some(d) = &mut self.doc else { return };
        if d.id != r.doc {
            return;
        }
        d.inflight.remove(&r.page);
        let Ok((w, h, bytes)) = r.image else { return };
        if d.page_ver.get(&r.page).copied().unwrap_or(0) != r.ver {
            return; // edited since this render was requested
        }
        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &bytes);
        let tex = self.ctx.load_texture(format!("page{}", r.page), img, egui::TextureOptions::LINEAR);
        d.tex.insert(r.page, PageTex { tex, width_px: r.width, ver: r.ver });
        self.ctx.request_repaint();
    }

    // ---------- selection ----------

    fn apply_view_act(&mut self, a: ViewAct) {
        let Some(d) = &mut self.doc else { return };
        match a {
            ViewAct::DragStart { page, pos, rect } => {
                d.sel = None;
                self.seltool = None;
                let p = to_pdf(rect, d.pages[page], pos);
                d.drag = Some(DragState { page, start: p, latest: p, sent: None, moved: false, finished: false, end_screen: pos });
            }
            ViewAct::DragMove { pos, rect } => {
                if let Some(g) = &mut d.drag {
                    let p = to_pdf(rect, d.pages[g.page], pos);
                    if !g.moved && ((p[0] - g.start[0]).powi(2) + (p[1] - g.start[1]).powi(2)).sqrt() < 3.0 {
                        return;
                    }
                    g.moved = true;
                    g.latest = p;
                    g.end_screen = pos;
                }
            }
            ViewAct::DragEnd { pos, rect } => {
                if let Some(g) = &mut d.drag {
                    if g.moved {
                        g.latest = to_pdf(rect, d.pages[g.page], pos);
                        g.end_screen = pos;
                        g.finished = true;
                    } else {
                        d.drag = None;
                    }
                }
            }
            ViewAct::Sticky { page, pos, rect } => {
                let p = to_pdf(rect, d.pages[page], pos);
                self.editor = Some(NoteEditor { ctx: NoteCtx::Sticky { page, x: p[0], y: p[1] }, text: String::new(), pos, focus: true });
            }
        }
    }

    /// Sends the latest drag position to the worker (one request in flight at a time).
    pub fn pump_selection(&mut self) {
        let in_flight = self.jobs.iter().any(|j| matches!(j, Job::Select(..)));
        let Some(d) = &mut self.doc else { return };
        let Some(g) = &mut d.drag else { return };
        if !g.moved || in_flight || g.sent == Some(g.latest) {
            // finished and nothing left to ask: show the toolbar
            if g.finished && !in_flight && g.sent == Some(g.latest) {
                let pos = g.end_screen;
                let has = d.sel.is_some();
                d.drag = None;
                if has {
                    self.seltool = Some(pos);
                }
            }
            return;
        }
        g.sent = Some(g.latest);
        let (tx, rx) = channel();
        let _ = self.viewer.send(Cmd::Select { doc: d.id, page: g.page as u32, a: g.start, b: g.latest, reply: tx });
        let id = d.id;
        self.jobs.push(Job::Select(rx, id));
    }

    pub fn finish_select(&mut self, res: Result<Selection, String>, doc: u32) {
        let Some(d) = &mut self.doc else { return };
        if d.id != doc {
            return;
        }
        let page = d.drag.as_ref().map(|g| g.page);
        if let (Ok(s), Some(page)) = (res, page) {
            d.sel = if s.rects.is_empty() { None } else { Some(SelState { page, rects: s.rects, text: s.text }) };
        }
    }

    pub fn request_highlights(&mut self, terms: &[String], pages: &[usize]) {
        let Some(d) = &mut self.doc else { return };
        d.found.clear();
        if terms.is_empty() {
            return;
        }
        let id = d.id;
        let mut seen = HashSet::new();
        for &p in pages.iter().filter(|p| seen.insert(**p)).take(25) {
            let (tx, rx) = channel();
            let _ = self.viewer.send(Cmd::Find { doc: id, page: p as u32, terms: terms.to_vec(), reply: tx });
            self.jobs.push(Job::Find(rx, id, p));
        }
    }

    pub fn finish_find(&mut self, res: Result<Vec<Rect4>, String>, doc: u32, page: usize) {
        let Some(d) = &mut self.doc else { return };
        if d.id == doc {
            if let Ok(r) = res {
                d.found.insert(page, r);
            }
        }
    }

    pub fn set_note_tool(&mut self, on: bool) {
        self.note_tool = on;
        if on {
            self.clear_selection();
        }
    }

    pub fn clear_selection(&mut self) {
        if let Some(d) = &mut self.doc {
            d.sel = None;
            d.drag = None;
        }
        self.seltool = None;
    }

    // ---------- floating tools ----------

    pub fn floating(&mut self, ctx: &egui::Context) {
        // selection toolbar
        if let (Some(pos), true) = (self.seltool, self.doc.as_ref().is_some_and(|d| d.sel.is_some())) {
            let mut act: Option<(&str, [u8; 3])> = None;
            let colors = self.colors;
            egui::Area::new(egui::Id::new("seltool")).fixed_pos(pos + vec2(-110.0, 14.0)).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for (i, c) in colors.iter().enumerate() {
                            let b = ui.add(egui::Button::new("").fill(*c).min_size(vec2(20.0, 20.0)).corner_radius(10.0));
                            if b.on_hover_text("Highlight").clicked() {
                                act = Some(("highlight", [c.r(), c.g(), c.b()]));
                                let _ = i;
                            }
                        }
                        ui.separator();
                        if ui.button(RichText::new("U").underline()).on_hover_text("Underline").clicked() {
                            act = Some(("underline", [220, 40, 40]));
                        }
                        if ui.button(RichText::new("S").strikethrough()).on_hover_text("Strike through").clicked() {
                            act = Some(("strikeout", [220, 40, 40]));
                        }
                        if ui.button("Comment").on_hover_text("Highlight and add a comment").clicked() {
                            act = Some(("comment", [255, 225, 77]));
                        }
                        if ui.button("Copy").on_hover_text("Copy text (Ctrl+C)").clicked() {
                            act = Some(("copy", [0, 0, 0]));
                        }
                    });
                });
            });
            if let Some((kind, rgb)) = act {
                let sel = self.doc.as_ref().and_then(|d| d.sel.as_ref()).map(|s| (s.page, s.rects.clone(), s.text.clone()));
                if let Some((page, rects, text)) = sel {
                    match kind {
                        "copy" => {
                            ctx.copy_text(text);
                            self.toast("Copied");
                            self.seltool = None;
                        }
                        "comment" => {
                            self.editor = Some(NoteEditor { ctx: NoteCtx::Highlight { page, rects }, text: String::new(), pos: pos + vec2(-110.0, 50.0), focus: true });
                            self.seltool = None;
                        }
                        k => self.add_markup(page, k, rgb, None, rects),
                    }
                }
            }
        }

        // note editor
        if self.editor.is_some() {
            let mut save = false;
            let mut cancel = false;
            if let Some(ed) = &mut self.editor {
                let title = match ed.ctx {
                    NoteCtx::Sticky { .. } => "New sticky note",
                    NoteCtx::Highlight { .. } => "Comment on highlight",
                    NoteCtx::Edit { .. } => "Edit comment",
                };
                egui::Window::new(title).fixed_pos(ed.pos).collapsible(false).resizable(false).default_width(300.0).show(ctx, |ui| {
                    let r = ui.add(egui::TextEdit::multiline(&mut ed.text).desired_rows(4).desired_width(f32::INFINITY).hint_text("Type your comment…"));
                    if ed.focus {
                        r.request_focus();
                        ed.focus = false;
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Ctrl+Enter to save").small().weak());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Save").clicked() {
                                save = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancel = true;
                            }
                        });
                    });
                    if r.has_focus() && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter)) {
                        save = true;
                    }
                });
            }
            if save {
                self.save_editor();
            } else if cancel {
                self.editor = None;
                self.note_tool = false;
            }
        }
    }

    pub fn add_markup(&mut self, page: usize, kind: &str, rgb: [u8; 3], note: Option<String>, rects: Vec<Rect4>) {
        let Some(d) = &self.doc else { return };
        let (tx, rx) = channel();
        let note = note.filter(|n| !n.trim().is_empty());
        let _ = self.viewer.send(Cmd::Edit {
            doc: d.id,
            op: EditOp::Markup { page: page as u32, kind: kind.to_string(), rects, rgb, note, author: self.author() },
            reply: tx,
        });
        self.jobs.push(Job::Edit(rx, page));
        self.clear_selection();
    }

    pub fn author(&self) -> String {
        self.settings
            .author
            .clone()
            .filter(|a| !a.trim().is_empty())
            .or_else(|| std::env::var("USERNAME").ok())
            .unwrap_or_else(|| "Reader".into())
    }

    pub fn paint_toast(&mut self, ctx: &egui::Context) {
        if let Some((msg, _)) = &self.toast {
            egui::Area::new(egui::Id::new("toast")).anchor(egui::Align2::RIGHT_BOTTOM, vec2(-16.0, -16.0)).order(egui::Order::Foreground).show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(msg);
                });
            });
        }
    }

    // ---------- keyboard ----------

    pub fn handle_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let typing = ctx.egui_wants_keyboard_input();
        let (mut k_search, mut k_open, mut k_side, mut k_save) = (false, false, false, false);
        let (mut zin, mut zout, mut zfit, mut z100, mut next, mut prev) = (false, false, false, false, false, false);
        ctx.input_mut(|i| {
            k_search = i.consume_key(Modifiers::COMMAND, Key::K) || i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::F);
            k_open = i.consume_key(Modifiers::COMMAND, Key::O);
            k_side = i.consume_key(Modifiers::COMMAND, Key::B);
            k_save = i.consume_key(Modifiers::COMMAND, Key::S);
            zin = i.consume_key(Modifiers::COMMAND, Key::Equals) || i.consume_key(Modifiers::COMMAND, Key::Plus);
            zout = i.consume_key(Modifiers::COMMAND, Key::Minus);
            zfit = i.consume_key(Modifiers::COMMAND, Key::Num0);
            z100 = i.consume_key(Modifiers::COMMAND, Key::Num1);
            next = i.consume_key(Modifiers::ALT, Key::ArrowRight);
            prev = i.consume_key(Modifiers::ALT, Key::ArrowLeft);
        });
        if k_search {
            self.side_open = true;
            self.focus_search = true;
        }
        if k_open {
            self.open_dialog();
        }
        if k_side {
            self.side_open = !self.side_open;
            self.refit();
        }
        if k_save {
            self.start_save();
        }
        if zin {
            self.zoom_by(1.15);
        }
        if zout {
            self.zoom_by(1.0 / 1.15);
        }
        if zfit {
            self.fit_now();
        }
        if z100 {
            self.set_zoom(1.0, false);
        }
        if next {
            self.step_file(1);
        }
        if prev {
            self.step_file(-1);
        }

        // Ctrl + wheel (or pinch) over the page view zooms
        let hover = ctx.input(|i| i.pointer.hover_pos());
        let zd = ctx.input(|i| i.zoom_delta());
        if (zd - 1.0).abs() > 0.001 && hover.is_some_and(|p| self.view_rect.contains(p)) {
            self.zoom_by(zd);
        }

        if !typing {
            if ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy))) {
                if let Some(s) = self.doc.as_ref().and_then(|d| d.sel.as_ref()) {
                    ctx.copy_text(s.text.clone());
                    self.toast("Copied");
                }
            }
            if ctx.input(|i| i.key_pressed(Key::N) && !i.modifiers.any()) {
                let on = !self.note_tool;
                self.set_note_tool(on);
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.editor.is_some() {
                self.editor = None;
                self.note_tool = false;
            } else {
                self.clear_selection();
                self.note_tool = false;
            }
        }
    }
}
