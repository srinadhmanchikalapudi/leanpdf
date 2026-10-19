//! Full-text index of every PDF under the chosen folder, stored in SQLite FTS5.
//!
//! One FTS row per page. The rowid encodes `file_id * PAGE_STRIDE + page_index`,
//! so deleting a file's pages is a cheap rowid range delete and a hit maps back
//! to (file, page) without a lookup column. Files are re-indexed only when their
//! size or modified time changes.

use crate::pdf::{load_document, page_text, pdfium};
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};
use walkdir::WalkDir;

const PAGE_STRIDE: i64 = 100_000;

pub struct Indexer {
    tx: Sender<(String, bool)>,
    generation: Arc<AtomicU64>,
    pub db: PathBuf,
}

/// What the indexer reports while it works. The UI layer decides how to show it.
#[derive(Clone)]
pub enum IndexEvent {
    /// The file list changed (new/removed/changed files discovered).
    Files,
    Progress(Progress),
    Done(Summary),
    Error(String),
}

pub type Emit = Arc<dyn Fn(IndexEvent) + Send + Sync>;

#[derive(Serialize, Clone)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub current: String,
}

#[derive(Serialize, Clone)]
pub struct Summary {
    pub total: u32,
    pub pending: u32,
    pub no_text: u32,
    pub errors: u32,
}

#[derive(Serialize)]
pub struct FileEntry {
    pub path: String,
    pub rel: String,
    pub status: String,
    pub pages: u32,
    pub note: Option<String>,
}

#[derive(Serialize)]
pub struct Hit {
    pub page: u32,
    pub snippet: String,
}

#[derive(Serialize)]
pub struct FileHits {
    pub path: String,
    pub rel: String,
    pub name_match: bool,
    pub hits: Vec<Hit>,
}

fn open(db: &Path) -> Result<Connection, String> {
    let conn = Connection::open(db).map_err(|e| e.to_string())?;
    conn.busy_timeout(Duration::from_secs(10)).map_err(|e| e.to_string())?;
    Ok(conn)
}

fn init_db(db: &Path) -> Result<(), String> {
    let conn = open(db)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS files(
             id INTEGER PRIMARY KEY,
             root TEXT NOT NULL,
             path TEXT NOT NULL UNIQUE,
             rel TEXT NOT NULL,
             size INTEGER NOT NULL,
             mtime INTEGER NOT NULL,
             pages INTEGER NOT NULL DEFAULT 0,
             status TEXT NOT NULL DEFAULT 'pending');
         CREATE INDEX IF NOT EXISTS files_root ON files(root);
         CREATE VIRTUAL TABLE IF NOT EXISTS pages_fts USING fts5(
             text, tokenize='unicode61 remove_diacritics 2');",
    )
    .map_err(|e| e.to_string())?;
    // Older databases lack the column that records why a file could not be read.
    let has_note = conn
        .prepare("SELECT 1 FROM pragma_table_info('files') WHERE name='note'")
        .and_then(|mut s| s.exists([]))
        .map_err(|e| e.to_string())?;
    if !has_note {
        conn.execute("ALTER TABLE files ADD COLUMN note TEXT", []).map_err(|e| e.to_string())?;
    }
    Ok(())
}

impl Indexer {
    pub fn new(emit: Emit, db: PathBuf) -> Result<Self, String> {
        if let Some(dir) = db.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        init_db(&db)?;
        let (tx, rx) = channel::<(String, bool)>();
        let generation = Arc::new(AtomicU64::new(0));

        let cancel = generation.clone();
        let db_path = db.clone();
        std::thread::Builder::new()
            .name("pdf-indexer".into())
            .spawn(move || {
                while let Ok((mut folder, mut retry)) = rx.recv() {
                    // Only the newest requested folder matters, but a retry request sticks.
                    while let Ok((f, r)) = rx.try_recv() {
                        folder = f;
                        retry |= r;
                    }
                    let my_gen = cancel.load(Ordering::SeqCst);
                    if let Err(e) = scan(&emit, &db_path, &folder, retry, &cancel, my_gen) {
                        emit(IndexEvent::Error(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        Ok(Indexer { tx, generation, db })
    }

    /// Starts (or restarts) a scan of `folder`, cancelling any scan in progress.
    /// `retry_errors` re-attempts files that failed last time.
    pub fn scan(&self, folder: String, retry_errors: bool) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send((folder, retry_errors));
    }
}

fn scan(emit: &Emit, db: &Path, folder: &str, retry_errors: bool, cancel: &AtomicU64, my_gen: u64) -> Result<(), String> {
    let alive = || cancel.load(Ordering::SeqCst) == my_gen;
    let root = Path::new(folder);
    if !root.is_dir() {
        return Err(format!("Folder not found: {folder}"));
    }

    // Phase 1: walk the folder and reconcile the file table.
    let mut found: Vec<(String, String, i64, i64)> = Vec::new();
    for entry in WalkDir::new(root).follow_links(false).into_iter().filter_map(Result::ok) {
        if !alive() {
            return Ok(());
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let is_pdf = entry
            .path()
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
        if !is_pdf {
            continue;
        }
        let Ok(md) = entry.metadata() else { continue };
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_string_lossy()
            .replace('\\', "/");
        found.push((entry.path().to_string_lossy().into_owned(), rel, md.len() as i64, mtime));
    }

    let mut conn = open(db)?;
    {
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        let mut existing: HashMap<String, (i64, i64, i64, String)> = HashMap::new();
        {
            let mut stmt = tx
                .prepare("SELECT path, id, size, mtime, status FROM files WHERE root = ?1")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([folder], |r| {
                    Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, String>(4)?)))
                })
                .map_err(|e| e.to_string())?;
            for row in rows.flatten() {
                existing.insert(row.0, row.1);
            }
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for (path, rel, size, mtime) in &found {
            seen.insert(path);
            match existing.get(path) {
                Some((_, s, m, status))
                    if s == size && m == mtime && status != "pending" && !(retry_errors && status == "error") => {}
                Some((id, ..)) => {
                    delete_pages(&tx, *id)?;
                    tx.execute(
                        "UPDATE files SET size=?1, mtime=?2, rel=?3, status='pending', pages=0, note=NULL WHERE id=?4",
                        params![size, mtime, rel, id],
                    )
                    .map_err(|e| e.to_string())?;
                }
                None => {
                    tx.execute(
                        "INSERT INTO files(root, path, rel, size, mtime) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![folder, path, rel, size, mtime],
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
        }
        for (path, (id, ..)) in &existing {
            if !seen.contains(path.as_str()) {
                delete_pages(&tx, *id)?;
                tx.execute("DELETE FROM files WHERE id=?1", [id]).map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
    }
    emit(IndexEvent::Files);

    // Phase 2: extract text from files that are new or changed.
    let pending: Vec<(i64, String, String)> = {
        let mut stmt = conn
            .prepare("SELECT id, path, rel FROM files WHERE root=?1 AND status='pending' ORDER BY id")
            .map_err(|e| e.to_string())?;
        stmt.query_map([folder], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?
            .flatten()
            .collect()
    };
    let total = pending.len();
    if total > 0 {
        let pdfium = pdfium()?;
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        for (i, (id, path, rel)) in pending.iter().enumerate() {
            if !alive() {
                return Ok(());
            }
            if last_emit.elapsed() > Duration::from_millis(120) {
                last_emit = Instant::now();
                emit(IndexEvent::Progress(Progress { done: i, total, current: rel.clone() }));
            }
            let (status, pages, note) = match load_document(pdfium, path) {
                Ok(doc) => {
                    let (s, p) = index_file(&mut conn, *id, &doc, &alive)?;
                    (s, p, None)
                }
                Err(reason) => ("error".to_string(), 0, Some(reason)),
            };
            if status == "cancelled" {
                return Ok(());
            }
            conn.execute(
                "UPDATE files SET status=?1, pages=?2, note=?3 WHERE id=?4",
                params![status, pages, note, id],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    emit(IndexEvent::Done(summary(db, folder)?));
    Ok(())
}

fn delete_pages(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute(
        "DELETE FROM pages_fts WHERE rowid BETWEEN ?1 AND ?2",
        [id * PAGE_STRIDE, id * PAGE_STRIDE + PAGE_STRIDE - 1],
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

fn index_file(
    conn: &mut Connection,
    id: i64,
    doc: &pdfium_render::prelude::PdfDocument<'_>,
    alive: &dyn Fn() -> bool,
) -> Result<(String, u32), String> {
    let pages = doc.pages().len();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    delete_pages(&tx, id)?;
    let mut any = false;
    {
        let mut stmt = tx
            .prepare("INSERT INTO pages_fts(rowid, text) VALUES (?1, ?2)")
            .map_err(|e| e.to_string())?;
        for i in 0..pages {
            if i % 25 == 24 && !alive() {
                return Ok(("cancelled".into(), 0));
            }
            let Some(text) = page_text(doc, i) else { continue };
            let clean: String = text
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            if clean.is_empty() {
                continue;
            }
            stmt.execute(params![id * PAGE_STRIDE + i as i64, clean]).map_err(|e| e.to_string())?;
            any = true;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok((if any { "done" } else { "notext" }.to_string(), pages as u32))
}

pub fn summary(db: &Path, root: &str) -> Result<Summary, String> {
    let conn = open(db)?;
    conn.query_row(
        "SELECT COUNT(*),
                COALESCE(SUM(status='pending'),0),
                COALESCE(SUM(status='notext'),0),
                COALESCE(SUM(status='error'),0)
         FROM files WHERE root=?1",
        [root],
        |r| Ok(Summary { total: r.get(0)?, pending: r.get(1)?, no_text: r.get(2)?, errors: r.get(3)? }),
    )
    .map_err(|e| e.to_string())
}

pub fn list_files(db: &Path, root: &str) -> Result<Vec<FileEntry>, String> {
    let conn = open(db)?;
    let mut stmt = conn
        .prepare("SELECT path, rel, status, pages, note FROM files WHERE root=?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([root], |r| {
            Ok(FileEntry { path: r.get(0)?, rel: r.get(1)?, status: r.get(2)?, pages: r.get(3)?, note: r.get(4)? })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

/// Turns what the user typed into an FTS5 query. A quoted string is a phrase;
/// otherwise every word must appear (as a prefix, so "contr" finds "contract").
fn fts_query(q: &str) -> Option<String> {
    let q = q.trim();
    let tokens: Vec<String> = q
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    if q.len() >= 2 && q.starts_with('"') && q.ends_with('"') {
        return Some(format!("\"{}\"", tokens.join(" ")));
    }
    Some(tokens.iter().map(|t| format!("\"{t}\"*")).collect::<Vec<_>>().join(" "))
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

pub fn search(db: &Path, root: &str, query: &str, scope: &str) -> Result<Vec<FileHits>, String> {
    let conn = open(db)?;
    let mut order: Vec<FileHits> = Vec::new();
    let mut slot: HashMap<String, usize> = HashMap::new();

    // Restrict to a sub-folder: rel starts with "<scope>/".
    let scope_like = if scope.is_empty() { "%".to_string() } else { format!("{}/%", like_escape(scope)) };

    // File-name matches first.
    let like = format!("%{}%", like_escape(query.trim()));
    if query.trim().len() >= 2 {
        let mut stmt = conn
            .prepare(
                "SELECT path, rel FROM files WHERE root=?1 AND rel LIKE ?2 ESCAPE '\\' AND rel LIKE ?3 ESCAPE '\\' ORDER BY rel LIMIT 50",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![root, like, scope_like], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(|e| e.to_string())?;
        for (path, rel) in rows.flatten() {
            slot.insert(path.clone(), order.len());
            order.push(FileHits { path, rel, name_match: true, hits: Vec::new() });
        }
    }

    if let Some(fts) = fts_query(query) {
        let mut stmt = conn
            .prepare(
                "SELECT f.path, f.rel, pages_fts.rowid,
                        snippet(pages_fts, 0, char(1), char(2), '…', 16)
                 FROM pages_fts JOIN files f ON f.id = pages_fts.rowid / 100000
                 WHERE pages_fts MATCH ?1 AND f.root = ?2 AND f.rel LIKE ?3 ESCAPE '\\'
                 ORDER BY rank LIMIT 600",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map(params![fts, root, scope_like], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?))
            })
            .map_err(|e| e.to_string())?;
        for (path, rel, rowid, snippet) in rows.flatten() {
            let idx = *slot.entry(path.clone()).or_insert_with(|| {
                order.push(FileHits { path, rel, name_match: false, hits: Vec::new() });
                order.len() - 1
            });
            order[idx].hits.push(Hit { page: (rowid % PAGE_STRIDE) as u32 + 1, snippet });
        }
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_words_become_prefix_terms() {
        assert_eq!(fts_query("Contract renew").as_deref(), Some("\"contract\"* \"renew\"*"));
    }

    #[test]
    fn quoted_text_is_a_phrase() {
        assert_eq!(fts_query("\"payment terms\"").as_deref(), Some("\"payment terms\""));
    }

    #[test]
    fn punctuation_only_is_no_query() {
        assert_eq!(fts_query("  -- "), None);
    }

    #[test]
    fn like_patterns_are_escaped() {
        assert_eq!(like_escape("100%_x\\y"), "100\\%\\_x\\\\y");
    }

    #[test]
    fn search_finds_pages_and_respects_scope() {
        let dir = std::env::temp_dir().join(format!("leanpdf-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        init_db(&db).unwrap();
        let conn = open(&db).unwrap();
        for (id, path, rel) in [(1i64, "/r/a/x.pdf", "a/x.pdf"), (2, "/r/b/y.pdf", "b/y.pdf")] {
            conn.execute(
                "INSERT INTO files(id, root, path, rel, size, mtime, status) VALUES (?1,'/r',?2,?3,1,1,'done')",
                params![id, path, rel],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO pages_fts(rowid, text) VALUES (?1, 'the contract renewal is due')",
                params![id * PAGE_STRIDE + 3],
            )
            .unwrap();
        }
        let all = search(&db, "/r", "renew", "").unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].hits[0].page, 4); // page index 3 is shown as page 4
        let scoped = search(&db, "/r", "renew", "a").unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].rel, "a/x.pdf");
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
