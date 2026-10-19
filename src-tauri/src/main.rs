#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]


use leanpdf_core::index::{self, FileEntry, FileHits, IndexEvent, Indexer, Summary};
use leanpdf_core::pdf::{self, Annot, Cmd, DocInfo, EditOp};
use leanpdf_core::settings::{self, Settings};
use leanpdf_core::edit;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Mutex;
use tauri::http::Response;
use tauri::{Emitter, Manager, State};

struct AppState {
    viewer: Sender<Cmd>,
    indexer: Indexer,
    dir: PathBuf,
    settings: Mutex<Settings>,
}

/// Sends a command to the viewer thread and waits for its answer without
/// blocking the async runtime.
async fn ask<T: Send + 'static>(
    tx: &Sender<Cmd>,
    make: impl FnOnce(Sender<Result<T, String>>) -> Cmd,
) -> Result<T, String> {
    let (rtx, rrx) = std::sync::mpsc::channel();
    tx.send(make(rtx)).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || rrx.recv().map_err(|e| e.to_string())?)
        .await
        .map_err(|e| e.to_string())?
}

fn current_folder(state: &AppState) -> Result<String, String> {
    state.settings.lock().unwrap().folder.clone().ok_or_else(|| "No folder chosen".to_string())
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().unwrap().clone()
}

/// A .pdf passed on the command line (Open with / default app), if any.
#[tauri::command]
fn launch_file() -> Option<String> {
    std::env::args().skip(1).find(|a| a.to_lowercase().ends_with(".pdf") && PathBuf::from(a).is_file())
}

#[tauri::command]
fn set_folder(path: String, state: State<AppState>) -> Result<(), String> {
    {
        let mut s = state.settings.lock().unwrap();
        s.folder = Some(path.clone());
        settings::save(&state.dir, &s)?;
    }
    state.indexer.scan(path, true);
    Ok(())
}

#[tauri::command]
fn rescan(retry_errors: bool, state: State<AppState>) -> Result<(), String> {
    let folder = current_folder(&state)?;
    state.indexer.scan(folder, retry_errors);
    Ok(())
}

#[tauri::command]
fn remember_last(path: String, page: u32, state: State<AppState>) -> Result<(), String> {
    let mut s = state.settings.lock().unwrap();
    s.last_file = Some(path);
    s.last_page = Some(page);
    settings::save(&state.dir, &s)
}

#[tauri::command]
async fn list_files(state: State<'_, AppState>) -> Result<Vec<FileEntry>, String> {
    let folder = current_folder(&state)?;
    let db = state.indexer.db.clone();
    tauri::async_runtime::spawn_blocking(move || index::list_files(&db, &folder))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn index_summary(state: State<'_, AppState>) -> Result<Summary, String> {
    let folder = current_folder(&state)?;
    let db = state.indexer.db.clone();
    tauri::async_runtime::spawn_blocking(move || index::summary(&db, &folder))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn search(query: String, scope: String, state: State<'_, AppState>) -> Result<Vec<FileHits>, String> {
    let folder = current_folder(&state)?;
    let db = state.indexer.db.clone();
    tauri::async_runtime::spawn_blocking(move || index::search(&db, &folder, &query, &scope))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn open_pdf(path: String, state: State<'_, AppState>) -> Result<DocInfo, String> {
    ask(&state.viewer, |reply| Cmd::Open { path, reply }).await
}

#[tauri::command]
async fn annotations(doc: u32, from: u32, to: u32, state: State<'_, AppState>) -> Result<Vec<Annot>, String> {
    ask(&state.viewer, |reply| Cmd::Annots { doc, from, to, reply }).await
}

#[tauri::command]
async fn find_text(
    doc: u32,
    page: u32,
    terms: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Vec<[f32; 4]>, String> {
    ask(&state.viewer, |reply| Cmd::Find { doc, page, terms, reply }).await
}

fn author(state: &AppState) -> String {
    state
        .settings
        .lock()
        .unwrap()
        .author
        .clone()
        .filter(|a| !a.trim().is_empty())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "Reader".to_string())
}

#[tauri::command]
async fn select_text(
    doc: u32,
    page: u32,
    a: [f32; 2],
    b: [f32; 2],
    state: State<'_, AppState>,
) -> Result<edit::Selection, String> {
    ask(&state.viewer, |reply| Cmd::Select { doc, page, a, b, reply }).await
}

#[tauri::command]
async fn add_markup(
    doc: u32,
    page: u32,
    kind: String,
    rects: Vec<[f32; 4]>,
    rgb: [u8; 3],
    note: Option<String>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let author = author(&state);
    let note = note.filter(|n| !n.trim().is_empty());
    ask(&state.viewer, |reply| Cmd::Edit { doc, op: EditOp::Markup { page, kind, rects, rgb, note, author }, reply }).await
}

#[tauri::command]
async fn add_note(doc: u32, page: u32, x: f32, y: f32, text: String, state: State<'_, AppState>) -> Result<(), String> {
    let author = author(&state);
    ask(&state.viewer, |reply| Cmd::Edit { doc, op: EditOp::Note { page, x, y, text, author }, reply }).await
}

#[tauri::command]
async fn set_annot_contents(doc: u32, page: u32, annot: u32, text: String, state: State<'_, AppState>) -> Result<(), String> {
    ask(&state.viewer, |reply| Cmd::Edit { doc, op: EditOp::SetContents { page, annot, text }, reply }).await
}

#[tauri::command]
async fn delete_annot(doc: u32, page: u32, annot: u32, state: State<'_, AppState>) -> Result<(), String> {
    ask(&state.viewer, |reply| Cmd::Edit { doc, op: EditOp::Delete { page, annot }, reply }).await
}

#[tauri::command]
async fn save_doc(doc: u32, state: State<'_, AppState>) -> Result<bool, String> {
    ask(&state.viewer, |reply| Cmd::Save { doc, reply }).await
}

fn main() {
    let viewer = pdf::spawn_worker();
    let protocol_tx = viewer.clone();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        // pdfpage://localhost/<doc>/<page>/<pixel width> -> PNG of that page
        .register_asynchronous_uri_scheme_protocol("pdfpage", move |_ctx, request, responder| {
            let parts: Vec<u32> = request
                .uri()
                .path()
                .split('/')
                .filter(|s| !s.is_empty())
                .filter_map(|s| s.parse().ok())
                .collect();
            let reply = |status: u16, ctype: &str, body: Vec<u8>| {
                Response::builder()
                    .status(status)
                    .header("Content-Type", ctype)
                    .header("Cache-Control", "no-store")
                    .header("Access-Control-Allow-Origin", "*")
                    .body(body)
                    .unwrap()
            };
            let [doc, page, width, ..] = parts[..] else {
                responder.respond(reply(400, "text/plain", b"bad request".to_vec()));
                return;
            };
            let _ = protocol_tx.send(Cmd::Render {
                doc,
                page,
                width,
                reply: Box::new(move |result| match result {
                    Ok(png) => responder.respond(reply(200, "image/png", png)),
                    Err(e) => responder.respond(reply(500, "text/plain", e.into_bytes())),
                }),
            });
        })
        .setup(move |app| {
            let dir = app.path().app_data_dir()?;
            let settings = settings::load(&dir);
            let handle = app.handle().clone();
            let emit: index::Emit = std::sync::Arc::new(move |ev| {
                let _ = match ev {
                    IndexEvent::Files => handle.emit("index-files", ()),
                    IndexEvent::Progress(p) => handle.emit("index-progress", p),
                    IndexEvent::Done(s) => handle.emit("index-done", s),
                    IndexEvent::Error(e) => handle.emit("index-error", e),
                };
            });
            let indexer = Indexer::new(emit, dir.join("index.db"))?;
            if let Some(folder) = settings.folder.clone() {
                indexer.scan(folder, true);
            }
            app.manage(AppState { viewer, indexer, dir, settings: Mutex::new(settings) });
            // Safety net: the window starts hidden and the page shows it when ready.
            if let Some(win) = app.get_webview_window("main") {
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(4));
                    let _ = win.show();
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings,
            launch_file,
            set_folder,
            rescan,
            remember_last,
            list_files,
            index_summary,
            search,
            open_pdf,
            annotations,
            find_text,
            select_text,
            add_markup,
            add_note,
            set_annot_contents,
            delete_annot,
            save_doc,
        ])
        .run(tauri::generate_context!())
        .expect("error while running LeanPDF");
}
