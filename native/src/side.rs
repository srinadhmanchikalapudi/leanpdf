//! Left panel: remembered folder, folder tree, full-text search results.

use crate::util::*;
use crate::*;
use eframe::egui::{self, text::LayoutJob, Color32, RichText, TextFormat};

#[derive(Default)]
pub struct Tree {
    pub name: String,
    pub path: String,
    pub folders: Vec<Tree>,
    pub files: Vec<usize>,
    pub count: usize,
}

struct TreeView<'a> {
    files: &'a [FileEntry],
    expanded: &'a HashSet<String>,
    scope: &'a str,
    active: Option<&'a str>,
    reveal: bool,
}

#[derive(Default)]
struct SideActions {
    open: Option<usize>,
    folder: Option<String>,
    revealed: bool,
}

impl App {
    pub fn refresh_files(&mut self) {
        let Some(folder) = self.settings.folder.clone() else { return };
        let Ok(files) = index::list_files(&self.indexer.db, &folder) else { return };
        self.files = files;

        let mut root = Tree::default();
        for (i, f) in self.files.iter().enumerate() {
            let parts: Vec<&str> = f.rel.split('/').collect();
            let mut node = &mut root;
            node.count += 1;
            for (d, part) in parts[..parts.len() - 1].iter().enumerate() {
                let pos = match node.folders.iter().position(|c| c.name == *part) {
                    Some(p) => p,
                    None => {
                        node.folders.push(Tree { name: part.to_string(), path: parts[..=d].join("/"), ..Default::default() });
                        node.folders.len() - 1
                    }
                };
                node = &mut node.folders[pos];
                node.count += 1;
            }
            node.files.push(i);
        }
        let files = &self.files;
        fn sort(n: &mut Tree, files: &[FileEntry]) {
            n.folders.sort_by(|a, b| natural_cmp(&a.name, &b.name));
            n.files.sort_by(|a, b| natural_cmp(base_name(&files[*a].rel), base_name(&files[*b].rel)));
            for c in &mut n.folders {
                sort(c, files);
            }
        }
        sort(&mut root, files);
        fn flatten(n: &Tree, out: &mut Vec<usize>) {
            for c in &n.folders {
                flatten(c, out);
            }
            out.extend(&n.files);
        }
        self.order.clear();
        flatten(&root, &mut self.order);
        self.tree = root;
        if !self.scope.is_empty() && !self.files.iter().any(|f| f.rel.starts_with(&format!("{}/", self.scope))) {
            self.scope.clear();
        }
    }

    pub fn choose_folder(&mut self) {
        let mut dlg = rfd::FileDialog::new().set_title("Choose the folder that holds your PDFs");
        if let Some(f) = &self.settings.folder {
            dlg = dlg.set_directory(f);
        }
        let Some(picked) = dlg.pick_folder() else { return };
        let picked = picked.to_string_lossy().into_owned();
        self.settings.folder = Some(picked.clone());
        let _ = settings::save(&self.dir, &self.settings);
        self.files.clear();
        self.tree = Tree::default();
        self.order.clear();
        self.expanded.clear();
        self.scope.clear();
        self.results.clear();
        self.indexing = true;
        self.status = "Scanning folder…".into();
        self.indexer.scan(picked, true);
        self.save_ui_state();
        self.focus_search = true;
    }

    pub fn rescan(&mut self, retry_errors: bool) {
        if let Some(f) = self.settings.folder.clone() {
            self.last_rescan = Instant::now();
            self.indexing = true;
            self.indexer.scan(f, retry_errors);
        }
    }

    pub fn sidebar(&mut self, ui: &mut egui::Ui) {
        let folder_name = self
            .settings
            .folder
            .as_deref()
            .map(|f| f.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(f).to_string())
            .unwrap_or_else(|| "Choose a folder…".into());

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let b = ui.add(egui::Button::new(RichText::new(format!("Folder: {folder_name}")).strong()).frame(false));
            if b.on_hover_text(self.settings.folder.clone().unwrap_or_else(|| "Choose the folder to search".into())).clicked() {
                self.choose_folder();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Rescan").on_hover_text("Rescan folder for new or changed PDFs").clicked() {
                    self.rescan(true);
                }
            });
        });

        let hint = if self.scope.is_empty() {
            "Search inside all PDFs   (Ctrl+K)".to_string()
        } else {
            format!("Search in “{}”   (Ctrl+K)", base_name(&self.scope))
        };
        let resp = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text(hint).desired_width(f32::INFINITY));
        if self.focus_search {
            resp.request_focus();
            self.focus_search = false;
        }
        if resp.changed() {
            self.search_at = Some(Instant::now() + Duration::from_millis(180));
            if self.query.trim().is_empty() {
                self.search_at = None;
                self.results.clear();
            }
        }
        if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            if let Some(first) = self.results.first() {
                let (path, page, terms, pages) = (
                    first.path.clone(),
                    first.hits.first().map(|h| h.page as usize - 1).unwrap_or(0),
                    terms_from_query(&self.query),
                    first.hits.iter().map(|h| h.page as usize - 1).collect::<Vec<_>>(),
                );
                self.open_file(path, page, terms, pages, false);
            }
        }
        if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.query.is_empty() {
                resp.surrender_focus();
            } else {
                self.query.clear();
                self.results.clear();
            }
        }

        // status / progress
        if let Some((done, total, current)) = &self.progress {
            ui.label(RichText::new(format!("Indexing {} / {} · {}", done + 1, total, base_name(current))).small().weak());
            ui.add(egui::ProgressBar::new(*done as f32 / (*total).max(1) as f32).desired_height(4.0));
        } else if !self.status.is_empty() {
            ui.label(RichText::new(&self.status).small().weak());
        }
        ui.add_space(2.0);
        ui.separator();

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            if self.settings.folder.is_none() {
                ui.add_space(24.0);
                ui.vertical_centered(|ui| {
                    ui.label("Pick the folder that holds your PDFs.");
                    ui.label(RichText::new("It is remembered, and everything inside it becomes searchable.").weak());
                    ui.add_space(8.0);
                    if ui.button("Choose folder…").clicked() {
                        self.choose_folder();
                    }
                });
            } else if self.query.trim().is_empty() {
                self.tree_ui(ui);
            } else {
                self.results_ui(ui);
            }
        });
    }

    fn tree_ui(&mut self, ui: &mut egui::Ui) {
        if self.files.is_empty() {
            ui.label(RichText::new(if self.indexing { "Scanning folder…" } else { "No PDFs found in this folder." }).weak());
            return;
        }
        let mut acts = SideActions::default();
        let active = self.doc.as_ref().map(|d| d.path.clone());
        {
            let view = TreeView { files: &self.files, expanded: &self.expanded, scope: &self.scope, active: active.as_deref(), reveal: self.reveal };
            let root_name = self.settings.folder.as_deref().map(|f| f.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(f)).unwrap_or("");
            let r = ui.add(egui::Button::selectable(self.scope.is_empty(), (RichText::new(format!("{}  ({})", root_name, self.tree.count)).strong(), egui::Atom::grow())).min_size(egui::vec2(ui.available_width(), 0.0)));
            if r.clicked() {
                acts.folder = Some(String::new());
            }
            tree_node(ui, &self.tree, &view, &mut acts);
        }
        if acts.revealed {
            self.reveal = false;
        }
        if let Some(path) = acts.folder {
            if path.is_empty() {
                self.scope.clear();
            } else if self.scope == path && self.expanded.contains(&path) {
                self.expanded.remove(&path);
            } else {
                self.expanded.insert(path.clone());
                self.scope = path;
            }
            self.save_ui_state();
            if !self.query.trim().is_empty() {
                self.start_search();
            }
        }
        if let Some(i) = acts.open {
            let path = self.files[i].path.clone();
            self.open_file(path, 0, Vec::new(), Vec::new(), false);
        }
    }

    fn results_ui(&mut self, ui: &mut egui::Ui) {
        if self.results.is_empty() {
            let msg = if self.search_at.is_some() || self.jobs.iter().any(|j| matches!(j, Job::Search(..))) {
                "Searching…".to_string()
            } else {
                format!("No matches for “{}”.{}", self.query.trim(), if self.indexing { "\nIndexing is still running, so results may be incomplete." } else { "" })
            };
            ui.label(RichText::new(msg).weak());
            return;
        }
        let total: usize = self.results.iter().map(|f| f.hits.len()).sum();
        ui.label(RichText::new(format!("{} file{}, {} page match{}", self.results.len(), if self.results.len() == 1 { "" } else { "s" }, total, if total == 1 { "" } else { "es" })).small().weak());
        let terms = terms_from_query(&self.results_query);
        let mut open: Option<(usize, usize)> = None; // (file idx, hit idx or usize::MAX)
        let mut more: Option<usize> = None;
        let accent = ui.visuals().hyperlink_color;
        for (fi, f) in self.results.iter().enumerate() {
            ui.add_space(4.0);
            let r = ui.add(egui::Button::new(RichText::new(base_name(&f.rel)).strong()).frame(false));
            if r.clicked() {
                open = Some((fi, usize::MAX));
            }
            let dir = dir_name(&f.rel);
            if !dir.is_empty() || f.name_match {
                ui.label(RichText::new(format!("{}{}", dir, if f.name_match { "   · name match" } else { "" })).small().weak());
            }
            let shown = if self.results_more.contains(&fi) { f.hits.len() } else { f.hits.len().min(4) };
            for (hi, h) in f.hits.iter().take(shown).enumerate() {
                ui.horizontal_top(|ui| {
                    ui.label(RichText::new(format!("p.{}", h.page)).color(accent).small());
                    let resp = ui.add(egui::Label::new(snippet_job(ui, &h.snippet)).sense(egui::Sense::click()).wrap());
                    if resp.clicked() {
                        open = Some((fi, hi));
                    }
                    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                });
            }
            if f.hits.len() > shown && ui.add(egui::Button::new(RichText::new(format!("+ {} more pages", f.hits.len() - shown)).small().color(accent)).frame(false)).clicked() {
                more = Some(fi);
            }
            ui.separator();
        }
        if let Some(fi) = more {
            self.results_more.insert(fi);
        }
        if let Some((fi, hi)) = open {
            let f = &self.results[fi];
            let page = if hi == usize::MAX { f.hits.first().map(|h| h.page as usize - 1).unwrap_or(0) } else { f.hits[hi].page as usize - 1 };
            let pages: Vec<usize> = f.hits.iter().map(|h| h.page as usize - 1).collect();
            let path = f.path.clone();
            let t = if f.hits.is_empty() { Vec::new() } else { terms };
            self.open_file(path, page, t, pages, false);
        }
    }

    pub fn start_search(&mut self) {
        let q = self.query.trim().to_string();
        if q.is_empty() {
            self.results.clear();
            return;
        }
        let Some(folder) = self.settings.folder.clone() else { return };
        self.search_seq += 1;
        let seq = self.search_seq;
        let (db, scope) = (self.indexer.db.clone(), self.scope.clone());
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        self.results_query = q.clone();
        std::thread::spawn(move || {
            let _ = tx.send(index::search(&db, &folder, &q, &scope));
            ctx.request_repaint();
        });
        self.jobs.push(Job::Search(rx, seq));
    }

    pub fn finish_search(&mut self, res: Result<Vec<FileHits>, String>, seq: u64) {
        if seq != self.search_seq {
            return;
        }
        match res {
            Ok(mut list) => {
                for f in &mut list {
                    f.hits.sort_by_key(|h| h.page);
                }
                self.results = list;
                self.results_more.clear();
            }
            Err(e) => {
                self.results.clear();
                self.status = e;
            }
        }
    }

    // ---------- opening documents ----------

    pub fn open_file(&mut self, path: String, page: usize, terms: Vec<String>, hit_pages: Vec<usize>, quiet: bool) {
        if let Some(d) = &self.doc {
            if d.path == path {
                let pages = if hit_pages.is_empty() { vec![page] } else { hit_pages };
                self.goto_page(page, None);
                self.request_highlights(&terms, &pages);
                return;
            }
        }
        self.open_seq += 1;
        let (tx, rx) = channel();
        let _ = self.viewer.send(Cmd::Open { path: path.clone(), reply: tx });
        self.jobs.push(Job::Open(rx, OpenCtx { path, page, terms, hit_pages, quiet, seq: self.open_seq }));
    }

    pub fn finish_open(&mut self, res: Result<DocInfo, String>, c: OpenCtx) {
        if c.seq != self.open_seq {
            return; // a newer open superseded this one
        }
        match res {
            Ok(info) => {
                let name = base_name(&c.path).to_string();
                let n = info.pages.len();
                let mut d = Doc {
                    id: info.id,
                    path: c.path.clone(),
                    name,
                    pages: info.pages,
                    zoom: 1.0,
                    fit: true,
                    tops: Vec::new(),
                    total_h: 0.0,
                    tex: HashMap::new(),
                    inflight: HashMap::new(),
                    page_ver: HashMap::new(),
                    scroll_y: 0.0,
                    scroll_to: None,
                    cur_page: 0,
                    viewport_w: 0.0,
                    viewport_h: 0.0,
                    zoom_changed: Instant::now(),
                    annots: Vec::new(),
                    annots_next: 0,
                    annots_at: Some(Instant::now() + Duration::from_millis(350)),
                    sel: None,
                    drag: None,
                    found: HashMap::new(),
                    flash: None,
                };
                d.layout();
                self.doc = Some(d);
                self.message.clear();
                self.dirty = false;
                self.save_at = None;
                self.seltool = None;
                self.editor = None;
                if let Some(e) = self.files.iter_mut().find(|f| f.path == c.path) {
                    if e.status == "error" {
                        e.status = "done".into();
                        e.note = None;
                    }
                }
                if self.step.take().is_some_and(|s| s.skipped > 0) {
                    self.toast("Skipped PDFs that could not be opened.");
                }
                self.reveal = true;
                self.reveal_current();
                let page = c.page.min(n.saturating_sub(1));
                self.goto_page(page, None);
                let pages = if c.hit_pages.is_empty() { vec![page] } else { c.hit_pages };
                self.request_highlights(&c.terms, &pages);
                self.remember();
            }
            Err(e) => {
                let e = e.trim_start_matches("Could not open PDF: ").to_string();
                if let Some(f) = self.files.iter_mut().find(|f| f.path == c.path) {
                    f.status = "error".into();
                    f.note = Some(e.clone());
                }
                if let Some(s) = &mut self.step {
                    s.skipped += 1;
                    s.pos += s.dir;
                    self.open_step();
                    return;
                }
                if !c.quiet {
                    self.toast(format!("{}: {}", base_name(&c.path.replace('\\', "/")), e));
                }
                if self.doc.is_none() {
                    self.message = e;
                }
            }
        }
    }

    fn reveal_current(&mut self) {
        let Some(d) = &self.doc else { return };
        let Some(f) = self.files.iter().find(|f| f.path == d.path) else { return };
        let parts: Vec<&str> = f.rel.split('/').collect();
        for i in 1..parts.len() {
            self.expanded.insert(parts[..i].join("/"));
        }
        self.save_ui_state();
    }

    /// Opens the next/previous PDF in tree order, skipping any that cannot be opened.
    pub fn step_file(&mut self, dir: i32) {
        if self.step.is_some() {
            return;
        }
        let Some(d) = &self.doc else { return };
        let Some(cur) = self.order.iter().position(|&i| self.files[i].path == d.path) else { return };
        self.step = Some(StepCtx { dir, pos: cur as i32 + dir, skipped: 0 });
        self.open_step();
    }

    fn open_step(&mut self) {
        let Some(s) = &self.step else { return };
        if s.pos < 0 || s.pos as usize >= self.order.len() {
            let msg = if s.skipped > 0 {
                format!("No more readable PDFs (skipped {}).", s.skipped)
            } else if s.dir > 0 {
                "That was the last PDF.".into()
            } else {
                "That was the first PDF.".into()
            };
            self.step = None;
            self.toast(msg);
            return;
        }
        let path = self.files[self.order[s.pos as usize]].path.clone();
        self.open_file(path, 0, Vec::new(), Vec::new(), true);
    }

    pub fn nav_state(&self) -> (bool, bool) {
        let Some(d) = &self.doc else { return (false, false) };
        match self.order.iter().position(|&i| self.files[i].path == d.path) {
            Some(i) => (i > 0, i + 1 < self.order.len()),
            None => (false, false),
        }
    }
}

fn tree_node(ui: &mut egui::Ui, node: &Tree, v: &TreeView, acts: &mut SideActions) {
    for c in &node.folders {
        let open = v.expanded.contains(&c.path);
        let label = format!("{}  ({})", c.name, c.count);
        let r = ui
            .horizontal(|ui| {
                let (_, icon) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                egui::collapsing_header::paint_default_icon(ui, if open { 1.0 } else { 0.0 }, &icon);
                ui.add(egui::Button::selectable(v.scope == c.path, (RichText::new(label).strong(), egui::Atom::grow())).min_size(egui::vec2(ui.available_width(), 0.0)))
            })
            .inner;
        if r.on_hover_text(&c.path).clicked() {
            acts.folder = Some(c.path.clone());
        }
        if open {
            ui.indent(("tree", &c.path), |ui| tree_node(ui, c, v, acts));
        }
    }
    for &i in &node.files {
        let f = &v.files[i];
        let is_active = v.active == Some(f.path.as_str());
        let mut text = RichText::new(base_name(&f.rel));
        if is_active {
            text = text.strong();
        }
        let tag = match f.status.as_str() {
            "notext" => "  · scan",
            "error" => "  · unreadable",
            _ => "",
        };
        let label = format!("{}{}", text.text(), tag);
        let mut rt = RichText::new(label);
        if f.status == "error" {
            rt = rt.color(Color32::from_rgb(220, 90, 80));
        }
        if is_active {
            rt = rt.strong();
        }
        let r = ui.add(egui::Button::selectable(is_active, (rt, egui::Atom::grow())).min_size(egui::vec2(ui.available_width(), 0.0)));
        let r = match (&f.status[..], &f.note) {
            ("error", Some(n)) => r.on_hover_text(format!("{n}. Press Rescan to retry.")),
            ("notext", _) => r.on_hover_text("No text layer, so it can't be searched (scanned image)"),
            _ => r.on_hover_text(&f.rel),
        };
        if is_active && v.reveal {
            r.scroll_to_me(Some(egui::Align::Center));
            acts.revealed = true;
        }
        if r.clicked() {
            acts.open = Some(i);
        }
    }
}

/// Search snippet with the matched words highlighted (marked \u{1}..\u{2} by SQLite).
fn snippet_job(ui: &egui::Ui, s: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    let normal = TextFormat { font_id: egui::FontId::proportional(12.5), color: ui.visuals().weak_text_color(), ..Default::default() };
    let hit = TextFormat {
        font_id: egui::FontId::proportional(12.5),
        color: ui.visuals().strong_text_color(),
        background: Color32::from_rgba_unmultiplied(255, 200, 40, 90),
        ..Default::default()
    };
    let mut on = false;
    for part in s.split_inclusive(['\u{1}', '\u{2}']) {
        let (text, marker) = match part.chars().last() {
            Some(c @ ('\u{1}' | '\u{2}')) => (&part[..part.len() - 1], Some(c)),
            _ => (part, None),
        };
        if !text.is_empty() {
            job.append(text, 0.0, if on { hit.clone() } else { normal.clone() });
        }
        match marker {
            Some('\u{1}') => on = true,
            Some('\u{2}') => on = false,
            _ => {}
        }
    }
    job
}
