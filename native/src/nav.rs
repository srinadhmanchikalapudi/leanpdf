//! Left panel pages for the open PDF: its outline (bookmarks) and page thumbnails,
//! and turning the view.

use crate::*;
use eframe::egui::{self, pos2, vec2, Color32, Rect, RichText, Sense, Stroke};
use leanpdf_core::pdf::OutlineItem;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum SideTab {
    #[default]
    Files,
    Outline,
    Pages,
}

/// A finished thumbnail render.
pub struct ThumbDone {
    pub doc: u32,
    pub page: usize,
    pub rot: u8,
    pub image: Result<(u32, u32, Vec<u8>), String>,
}

const THUMB_MAX_INFLIGHT: usize = 4;
const THUMB_ROW_PAD: f32 = 28.0;

impl App {
    /// The Files / Outline / Pages switch at the top of the left panel.
    pub fn side_tabs(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            for (tab, label, tip) in [
                (SideTab::Files, "Files", "The folder tree and search"),
                (SideTab::Outline, "Outline", "This PDF's bookmarks"),
                (SideTab::Pages, "Pages", "Page thumbnails"),
            ] {
                if ui.selectable_label(self.side_tab == tab, label).on_hover_text(tip).clicked() {
                    self.side_tab = tab;
                }
            }
        });
        ui.separator();
    }

    pub fn outline_panel(&mut self, ui: &mut egui::Ui) {
        let need = self.doc.as_ref().is_some_and(|d| d.outline.is_none() && !d.outline_asked);
        if need {
            if let Some(d) = &mut self.doc {
                d.outline_asked = true;
                let id = d.id;
                let (tx, rx) = channel();
                let _ = self.viewer.send(Cmd::Outline { doc: id, reply: tx });
                self.jobs.push(Job::Outline(rx, id));
            }
        }
        let Some(d) = &self.doc else {
            ui.label(RichText::new("Open a PDF to see its outline.").weak());
            return;
        };
        let Some(items) = &d.outline else {
            ui.label(RichText::new("Reading the outline…").weak());
            return;
        };
        if items.is_empty() {
            ui.label(RichText::new("This PDF has no outline (bookmarks). Use Pages to browse by thumbnail.").weak());
            return;
        }
        // the entry being read: the last one that starts on or before the current page
        let cur = items.iter().rposition(|it| it.page.is_some_and(|p| p as usize <= d.cur_page));
        let mut goto = None;
        let row_h = ui.text_style_height(&egui::TextStyle::Button) + ui.spacing().item_spacing.y + 4.0;
        egui::ScrollArea::vertical().id_salt("outline").auto_shrink(false).show_rows(ui, row_h, items.len(), |ui, range| {
            for i in range {
                let it: &OutlineItem = &items[i];
                ui.horizontal(|ui| {
                    ui.add_space(it.depth as f32 * 14.0);
                    let mut text = RichText::new(&it.title);
                    if Some(i) == cur {
                        text = text.strong();
                    }
                    let r = ui.add(egui::Button::selectable(Some(i) == cur, text).truncate());
                    let r = match it.page {
                        Some(p) => r.on_hover_text(format!("Page {}", p + 1)),
                        None => r.on_hover_text("This entry does not point to a page in this PDF"),
                    };
                    if r.clicked() {
                        goto = it.page;
                    }
                });
            }
        });
        if let Some(p) = goto {
            self.goto_page(p as usize, None);
        }
    }

    pub fn finish_outline(&mut self, res: Result<Vec<OutlineItem>, String>, doc: u32) {
        if let Some(d) = self.doc_mut(doc) {
            d.outline = Some(res.unwrap_or_default());
        }
    }

    pub fn thumbs_panel(&mut self, ui: &mut egui::Ui) {
        let ppp = ui.ctx().pixels_per_point();
        let Some(d) = &mut self.doc else {
            ui.label(RichText::new("Open a PDF to see its pages.").weak());
            return;
        };
        let tw = (ui.available_width() - 28.0).clamp(80.0, 200.0);
        let n = d.pages.len();
        let mut tops = Vec::with_capacity(n);
        let mut y = 6.0;
        for i in 0..n {
            tops.push(y);
            let dp = d.disp(i);
            y += tw * dp[1] / dp[0].max(1.0) + THUMB_ROW_PAD;
        }
        let total = y + 6.0;
        let cur = d.cur_page.min(n.saturating_sub(1));
        let mut area = egui::ScrollArea::vertical().id_salt("thumbs").auto_shrink(false);
        if d.thumb_seen_page != cur {
            // keep the page being read in view
            let dp = d.disp(cur);
            let h = tw * dp[1] / dp[0].max(1.0);
            area = area.vertical_scroll_offset((tops[cur] + h / 2.0 - ui.available_height() / 2.0).max(0.0));
            d.thumb_seen_page = cur;
        }
        let accent = ui.visuals().hyperlink_color;
        let mut goto = None;
        let mut want: Vec<usize> = Vec::new();
        let ctx = ui.ctx().clone();
        let out = area.show_viewport(ui, |ui, viewport| {
            let (content, _) = ui.allocate_exact_size(vec2(ui.available_width(), total), Sense::hover());
            let painter = ui.painter_at(ui.clip_rect());
            let first = tops.partition_point(|&t| t < viewport.min.y - 120.0).saturating_sub(1);
            let last = tops.partition_point(|&t| t < viewport.max.y + 120.0).min(n.saturating_sub(1));
            for i in first..=last.max(first) {
                if i >= n {
                    break;
                }
                let dp = d.disp(i);
                let th = tw * dp[1] / dp[0].max(1.0);
                let x = content.left() + (content.width() - tw) / 2.0;
                let rect = Rect::from_min_size(pos2(x, content.top() + tops[i]), vec2(tw, th));
                painter.rect_filled(rect.translate(vec2(0.0, 1.5)), 2.0, Color32::from_black_alpha(60));
                painter.rect_filled(rect, 1.0, Color32::WHITE);
                match d.thumbs.get(&i) {
                    Some(t) => {
                        painter.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    None => want.push(i),
                }
                let is_cur = i == cur;
                if is_cur {
                    painter.rect_stroke(rect.expand(2.0), 2.0, Stroke::new(2.0, accent), egui::StrokeKind::Outside);
                }
                painter.text(
                    pos2(rect.center().x, rect.bottom() + 12.0),
                    egui::Align2::CENTER_CENTER,
                    format!("{}", i + 1),
                    egui::FontId::proportional(12.5),
                    if is_cur { accent } else { ui.visuals().weak_text_color() },
                );
                let r = ui.interact(rect.expand(3.0), ui.id().with(("thumb", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                if r.clicked() {
                    goto = Some(i);
                }
            }
            (first, last)
        });
        let (first, last) = out.inner;

        // ask for the thumbnails that are showing and free those far away
        if !want.is_empty() {
            let doc_id = d.id;
            let rot = d.rot;
            for i in want {
                if d.thumb_inflight.len() >= THUMB_MAX_INFLIGHT {
                    break;
                }
                if !d.thumb_inflight.insert(i) {
                    continue;
                }
                let sink = self.thumb_sink.clone();
                let c = ctx.clone();
                let width = ((tw * ppp).round() as u32).clamp(100, 600);
                let _ = self.viewer.send(Cmd::RenderRgba {
                    doc: doc_id,
                    page: i as u32,
                    width,
                    rot,
                    reply: Box::new(move |image| {
                        sink.lock().unwrap().push(ThumbDone { doc: doc_id, page: i, rot, image });
                        c.request_repaint();
                    }),
                });
            }
        }
        let (lo, hi) = (first.saturating_sub(10), last + 10);
        d.thumbs.retain(|i, _| *i >= lo && *i <= hi);
        if let Some(i) = goto {
            self.goto_page(i, None);
        }
    }

    pub fn accept_thumb(&mut self, t: ThumbDone) {
        let ctx = self.ctx.clone();
        let Some(d) = self.doc_mut(t.doc) else { return };
        d.thumb_inflight.remove(&t.page);
        if t.rot != d.rot {
            return; // the view was turned since this was asked for
        }
        let Ok((w, h, bytes)) = t.image else { return };
        let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &bytes);
        let tex = ctx.load_texture(format!("thumb{}-{}", t.doc, t.page), img, egui::TextureOptions::LINEAR);
        d.thumbs.insert(t.page, tex);
        ctx.request_repaint();
    }

    /// Turns the view of the open PDF by quarter turns (clockwise when positive). The file is not changed.
    pub fn rotate_view(&mut self, quarter_turns: i32) {
        let Some(d) = &mut self.doc else { return };
        if d.pages.is_empty() {
            return;
        }
        let cur = d.cur_page.min(d.pages.len() - 1);
        let frac = (d.scroll_y - d.tops[cur]) / d.page_size(cur).y.max(1.0);
        d.rot = ((d.rot as i32 + quarter_turns).rem_euclid(4)) as u8;
        d.tex.clear();
        // answers to renders asked for before the turn must be ignored when they arrive
        let stale: Vec<usize> = d.inflight.drain().map(|(p, _)| p).collect();
        for p in stale {
            *d.page_ver.entry(p).or_insert(0) += 1;
        }
        d.thumbs.clear();
        d.thumb_inflight.clear();
        d.sel = None;
        d.drag = None;
        d.viewport_w = 0.0; // a fit-to-width view recomputes its zoom for the new shape
        d.layout();
        d.scroll_to = Some((d.tops[cur] + frac * d.page_size(cur).y).max(0.0));
        d.zoom_changed = Instant::now();
        self.seltool = None;
    }
}
