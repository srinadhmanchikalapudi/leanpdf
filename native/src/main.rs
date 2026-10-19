#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod comments;
mod side;
mod util;
mod view;

use std::sync::atomic::{AtomicU64, Ordering};

/// Milliseconds since process start at the last frame the UI actually ran.
static LAST_ACTIVE_MS: AtomicU64 = AtomicU64::new(0);
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn now_ms() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// When the app has been idle for a few seconds, hand unused pages back to Windows.
/// The data stays valid (pages simply fault back in on demand), but Task Manager's
/// Memory column drops to what the app is actually touching.
#[cfg(windows)]
fn spawn_idle_trimmer() {
    std::thread::Builder::new()
        .name("idle-trim".into())
        .spawn(|| {
            let mut trimmed_at = 0u64;
            loop {
                std::thread::sleep(Duration::from_millis(1000));
                let last = LAST_ACTIVE_MS.load(Ordering::Relaxed);
                if now_ms().saturating_sub(last) > 3000 && trimmed_at < last {
                    unsafe {
                        windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet(windows_sys::Win32::System::Threading::GetCurrentProcess());
                    }
                    trimmed_at = now_ms().max(last + 1);
                }
            }
        })
        .ok();
}

#[cfg(not(windows))]
fn spawn_idle_trimmer() {}

use eframe::egui;
use leanpdf_core::edit::Selection;
use leanpdf_core::index::{self, FileEntry, FileHits, IndexEvent, Indexer, Summary};
use leanpdf_core::pdf::{self, Annot, Cmd, DocInfo};
use leanpdf_core::settings::{self, Settings};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type Rect4 = [f32; 4]; // left, bottom, right, top in PDF points

/// A rendered page that has finished on the worker thread.
pub struct RenderDone {
    pub doc: u32,
    pub page: usize,
    pub width: u32,
    pub ver: u32,
    pub image: Result<(u32, u32, Vec<u8>), String>,
}

pub struct PageTex {
    pub tex: egui::TextureHandle,
    pub width_px: u32,
    pub ver: u32,
}

pub struct SelState {
    pub page: usize,
    pub rects: Vec<Rect4>,
    pub text: String,
}

pub struct DragState {
    pub page: usize,
    pub start: [f32; 2],
    pub latest: [f32; 2],
    pub sent: Option<[f32; 2]>,
    pub moved: bool,
    pub finished: bool,
    pub end_screen: egui::Pos2,
}

pub struct Doc {
    pub id: u32,
    pub path: String,
    pub name: String,
    pub pages: Vec<[f32; 2]>,
    pub zoom: f32,
    pub fit: bool,
    pub tops: Vec<f32>,
    pub total_h: f32,
    pub tex: HashMap<usize, PageTex>,
    pub inflight: HashMap<usize, u32>,
    pub page_ver: HashMap<usize, u32>,
    pub scroll_y: f32,
    pub scroll_to: Option<f32>,
    pub cur_page: usize,
    pub viewport_w: f32,
    pub viewport_h: f32,
    pub zoom_changed: Instant,
    pub annots: Vec<Annot>,
    pub annots_next: u32,
    pub annots_at: Option<Instant>,
    pub sel: Option<SelState>,
    pub drag: Option<DragState>,
    pub found: HashMap<usize, Vec<Rect4>>,
    pub flash: Option<(usize, Rect4, Instant)>,
}

pub enum NoteCtx {
    Sticky { page: usize, x: f32, y: f32 },
    Highlight { page: usize, rects: Vec<Rect4> },
    Edit { page: usize, annot: u32 },
}

pub struct NoteEditor {
    pub ctx: NoteCtx,
    pub text: String,
    pub pos: egui::Pos2,
    pub focus: bool,
}

pub struct OpenCtx {
    pub path: String,
    pub page: usize,
    pub terms: Vec<String>,
    pub hit_pages: Vec<usize>,
    pub quiet: bool,
    pub seq: u64,
}

pub struct StepCtx {
    pub dir: i32,
    pub pos: i32,
    pub skipped: u32,
}

pub enum Job {
    Open(Receiver<Result<DocInfo, String>>, OpenCtx),
    Annots(Receiver<Result<Vec<Annot>, String>>, u32, Option<usize>),
    Select(Receiver<Result<Selection, String>>, u32),
    Edit(Receiver<Result<(), String>>, usize),
    Save(Receiver<Result<bool, String>>),
    Find(Receiver<Result<Vec<Rect4>, String>>, u32, usize),
    Search(Receiver<Result<Vec<FileHits>, String>>, u64),
}

pub struct App {
    pub ctx: egui::Context,
    pub viewer: Sender<Cmd>,
    pub indexer: Indexer,
    pub index_rx: Receiver<IndexEvent>,
    pub renders: Arc<Mutex<Vec<RenderDone>>>,
    pub jobs: Vec<Job>,
    pub dir: PathBuf,
    pub settings: Settings,

    // sidebar
    pub side_open: bool,
    pub files: Vec<FileEntry>,
    pub tree: side::Tree,
    pub order: Vec<usize>,
    pub expanded: HashSet<String>,
    pub scope: String,
    pub query: String,
    pub search_at: Option<Instant>,
    pub search_seq: u64,
    pub results: Vec<FileHits>,
    pub results_query: String,
    pub results_more: HashSet<usize>,
    pub status: String,
    pub progress: Option<(usize, usize, String)>,
    pub indexing: bool,
    pub reveal: bool,
    pub focus_search: bool,
    pub open_seq: u64,
    pub view_rect: egui::Rect,

    // viewer
    pub doc: Option<Doc>,
    pub step: Option<StepCtx>,
    pub comments_open: bool,
    pub note_tool: bool,
    pub editor: Option<NoteEditor>,
    pub seltool: Option<egui::Pos2>,
    pub message: String,
    pub toast: Option<(String, Instant)>,
    pub dirty: bool,
    pub save_at: Option<Instant>,
    pub save_state: String,
    pub remember_at: Option<Instant>,
    pub last_rescan: Instant,
    pub colors: [egui::Color32; 4],

    // debugging aid: LEANPDF_SHOT=<path> saves a screenshot and exits
    pub shot_path: Option<String>,
    pub shot_after: Instant,
    pub shot_rx: Option<Receiver<Arc<egui::ColorImage>>>,
    pub started: Instant,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let dir = PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("app.leanpdf.desktop");
        let settings = settings::load(&dir);

        let (itx, index_rx) = channel::<IndexEvent>();
        let itx = Mutex::new(itx);
        let c2 = ctx.clone();
        let emit: index::Emit = Arc::new(move |ev| {
            let _ = itx.lock().unwrap().send(ev);
            c2.request_repaint();
        });
        let indexer = Indexer::new(emit, dir.join("index.db")).expect("index database");
        if let Some(f) = settings.folder.clone() {
            indexer.scan(f, true);
        }

        let mut app = App {
            ctx,
            viewer: pdf::spawn_worker(),
            indexer,
            index_rx,
            renders: Arc::new(Mutex::new(Vec::new())),
            jobs: Vec::new(),
            dir,
            settings,
            side_open: true,
            files: Vec::new(),
            tree: side::Tree::default(),
            order: Vec::new(),
            expanded: HashSet::new(),
            scope: String::new(),
            query: String::new(),
            search_at: None,
            search_seq: 0,
            results: Vec::new(),
            results_query: String::new(),
            results_more: HashSet::new(),
            status: String::new(),
            progress: None,
            indexing: true,
            reveal: false,
            focus_search: false,
            open_seq: 0,
            view_rect: egui::Rect::NOTHING,
            doc: None,
            step: None,
            comments_open: false,
            note_tool: false,
            editor: None,
            seltool: None,
            message: "Choose a PDF from the sidebar, or press Ctrl+O.".into(),
            toast: None,
            dirty: false,
            save_at: None,
            save_state: String::new(),
            remember_at: None,
            last_rescan: Instant::now(),
            colors: [
                egui::Color32::from_rgb(255, 225, 77),
                egui::Color32::from_rgb(123, 224, 107),
                egui::Color32::from_rgb(107, 196, 255),
                egui::Color32::from_rgb(255, 143, 184),
            ],
            shot_path: std::env::var("LEANPDF_SHOT").ok(),
            shot_after: Instant::now(),
            shot_rx: None,
            started: Instant::now(),
        };
        spawn_idle_trimmer();
        app.load_ui_state();
        app.refresh_files();
        let launch = std::env::args().skip(1).find(|a| a.to_lowercase().ends_with(".pdf") && PathBuf::from(a).is_file());
        let restore = std::env::var("LEANPDF_NO_RESTORE").is_err();
        if let Some(p) = launch.or_else(|| if restore { app.settings.last_file.clone() } else { None }) {
            let page = if app.settings.last_file.as_deref() == Some(p.as_str()) { app.settings.last_page.unwrap_or(0) as usize } else { 0 };
            app.open_file(p, page, Vec::new(), Vec::new(), false);
        }
        app
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn ui_state_path(&self) -> PathBuf {
        self.dir.join("native-ui.json")
    }

    fn load_ui_state(&mut self) {
        if let Ok(s) = std::fs::read_to_string(self.ui_state_path()) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                if v["folder"].as_str() == self.settings.folder.as_deref() {
                    self.scope = v["scope"].as_str().unwrap_or("").to_string();
                    if let Some(a) = v["expanded"].as_array() {
                        self.expanded = a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect();
                    }
                }
            }
        }
    }

    pub fn save_ui_state(&self) {
        let v = serde_json::json!({
            "folder": self.settings.folder,
            "scope": self.scope,
            "expanded": self.expanded.iter().collect::<Vec<_>>(),
        });
        let _ = std::fs::write(self.ui_state_path(), v.to_string());
    }

    /// Persists the open file and page so the next launch resumes there.
    pub fn remember(&mut self) {
        if let Some(d) = &self.doc {
            self.settings.last_file = Some(d.path.clone());
            self.settings.last_page = Some(d.cur_page as u32);
            let _ = settings::save(&self.dir, &self.settings);
        }
    }

    // ---------- background results ----------

    fn poll(&mut self) {
        // indexer events
        while let Ok(ev) = self.index_rx.try_recv() {
            match ev {
                IndexEvent::Files => {
                    self.indexing = true;
                    self.refresh_files();
                }
                IndexEvent::Progress(p) => {
                    self.indexing = true;
                    self.progress = Some((p.done, p.total, p.current));
                }
                IndexEvent::Done(s) => {
                    self.indexing = false;
                    self.progress = None;
                    self.set_summary(&s);
                    self.refresh_files();
                    if !self.query.trim().is_empty() {
                        self.start_search();
                    }
                }
                IndexEvent::Error(e) => {
                    self.indexing = false;
                    self.progress = None;
                    self.status = e;
                }
            }
        }

        // finished page renders
        let done: Vec<RenderDone> = std::mem::take(&mut *self.renders.lock().unwrap());
        for r in done {
            self.accept_render(r);
        }

        // jobs
        let jobs = std::mem::take(&mut self.jobs);
        for job in jobs {
            match job {
                Job::Open(rx, c) => match rx.try_recv() {
                    Ok(res) => self.finish_open(res, c),
                    Err(std::sync::mpsc::TryRecvError::Empty) => self.jobs.push(Job::Open(rx, c)),
                    Err(_) => {}
                },
                Job::Annots(rx, doc, page) => match rx.try_recv() {
                    Ok(res) => self.finish_annots(res, doc, page),
                    Err(std::sync::mpsc::TryRecvError::Empty) => self.jobs.push(Job::Annots(rx, doc, page)),
                    Err(_) => {}
                },
                Job::Select(rx, doc) => match rx.try_recv() {
                    Ok(res) => self.finish_select(res, doc),
                    Err(std::sync::mpsc::TryRecvError::Empty) => self.jobs.push(Job::Select(rx, doc)),
                    Err(_) => {}
                },
                Job::Edit(rx, page) => match rx.try_recv() {
                    Ok(res) => self.finish_edit(res, page),
                    Err(std::sync::mpsc::TryRecvError::Empty) => self.jobs.push(Job::Edit(rx, page)),
                    Err(_) => {}
                },
                Job::Save(rx) => match rx.try_recv() {
                    Ok(res) => self.finish_save(res),
                    Err(std::sync::mpsc::TryRecvError::Empty) => self.jobs.push(Job::Save(rx)),
                    Err(_) => {}
                },
                Job::Find(rx, doc, page) => match rx.try_recv() {
                    Ok(res) => self.finish_find(res, doc, page),
                    Err(std::sync::mpsc::TryRecvError::Empty) => self.jobs.push(Job::Find(rx, doc, page)),
                    Err(_) => {}
                },
                Job::Search(rx, seq) => match rx.try_recv() {
                    Ok(res) => self.finish_search(res, seq),
                    Err(std::sync::mpsc::TryRecvError::Empty) => self.jobs.push(Job::Search(rx, seq)),
                    Err(_) => {}
                },
            }
        }

        // timers
        let now = Instant::now();
        if self.search_at.is_some_and(|t| now >= t) {
            self.search_at = None;
            self.start_search();
        }
        if self.save_at.is_some_and(|t| now >= t) {
            self.save_at = None;
            self.start_save();
        }
        if self.remember_at.is_some_and(|t| now >= t) {
            self.remember_at = None;
            self.remember();
        }
        self.maybe_load_annots();
        self.pump_selection();
        if let Some((_, t)) = &self.toast {
            if t.elapsed() > Duration::from_secs(5) {
                self.toast = None;
            }
        }
    }

    pub fn set_summary(&mut self, s: &Summary) {
        let mut parts = vec![format!("{} PDF{} indexed", s.total, if s.total == 1 { "" } else { "s" })];
        if s.no_text > 0 {
            parts.push(format!("{} scanned (no text)", s.no_text));
        }
        if s.errors > 0 {
            parts.push(format!("{} unreadable", s.errors));
        }
        self.status = parts.join(" · ");
    }

    fn busy(&self) -> bool {
        !self.jobs.is_empty()
            || self.search_at.is_some()
            || self.save_at.is_some()
            || self.remember_at.is_some()
            || self.doc.as_ref().is_some_and(|d| !d.inflight.is_empty() || d.annots_at.is_some() || d.drag.is_some())
            || self.toast.is_some()
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        LAST_ACTIVE_MS.store(now_ms(), Ordering::Relaxed);
        let ctx = ui.ctx().clone();
        self.handle_keys(&ctx);

        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui));
        if self.side_open {
            egui::Panel::left("side").resizable(true).default_size(330.0).size_range(220.0..=640.0).show(ui, |ui| self.sidebar(ui));
        }
        if self.comments_open {
            egui::Panel::right("comments").resizable(true).default_size(320.0).size_range(220.0..=560.0).show(ui, |ui| self.comments_panel(ui));
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(egui::Color32::from_gray(22))).show(ui, |ui| self.viewer_ui(ui));

        self.floating(&ctx);
        self.paint_toast(&ctx);

        // Re-run soon while anything is in flight; otherwise sleep (no idle CPU).
        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }

        // debugging aid
        if let Some(path) = self.shot_path.clone() {
            let after = std::env::var("LEANPDF_SHOT_MS").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(2500);
            if self.started.elapsed() > Duration::from_millis(after) {
                if let Some(rx) = &self.shot_rx {
                    if let Ok(img) = rx.try_recv() {
                        save_png(&path, &img);
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                } else {
                    let (tx, rx) = channel();
                    let c2 = ctx.clone();
                    self.shot_rx = Some(rx);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                        let _ = tx.send(img);
                        c2.request_repaint();
                    })));
                }
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Closing with unsaved comments: write them first.
        if self.dirty {
            if let Some(d) = &self.doc {
                let (tx, rx) = channel();
                let _ = self.viewer.send(Cmd::Save { doc: d.id, reply: tx });
                let _ = rx.recv_timeout(Duration::from_secs(10));
            }
        }
        self.remember();
        self.save_ui_state();
    }
}

fn save_png(path: &str, img: &egui::ColorImage) {
    if let Ok(f) = std::fs::File::create(path) {
        let mut enc = png::Encoder::new(std::io::BufWriter::new(f), img.width() as u32, img.height() as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        if let Ok(mut w) = enc.write_header() {
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            let _ = w.write_image_data(&bytes);
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("LeanPDF").with_inner_size([1280.0, 860.0]).with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native("LeanPDF", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
