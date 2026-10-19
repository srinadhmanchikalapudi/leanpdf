//! Looking for a newer LeanPDF on GitHub, downloading its setup program and running it.
//!
//! Like Job Tracker: one small request to the public "latest release" endpoint (never a draft or
//! pre-release), at most once a day unless asked. A setup program is only ever run if it came from
//! GitHub over HTTPS, arrived in full, and matches the checksum GitHub published for it.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

pub const OWNER: &str = "srinadhmanchikalapudi";
pub const REPO: &str = "leanpdf";
pub const INSTALLER_PREFIX: &str = "LeanPDF-Setup-";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// A visible progress window, no Windows restart, the running app closed for the update and started again at the end.
pub const SETUP_ARGS: [&str; 4] = ["/SILENT", "/NORESTART", "/CLOSEAPPLICATIONS", "/RESTARTAPP=1"];

static CHANGELOG: &str = include_str!("../../CHANGELOG.md");

#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub page: String,
    pub url: Option<String>,
    pub sha256: Option<String>,
    pub notes: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Idle,
    Checking,
    Available,
    Downloading,
    Failed,
}

pub enum Outcome {
    /// The setup program was started: the app should close now.
    Exit,
}

enum Ev {
    Checked { manual: bool, res: Result<Option<UpdateInfo>, String> },
    Downloaded(Result<PathBuf, String>),
}

pub struct Updater {
    pub phase: Phase,
    pub info: Option<UpdateInfo>,
    pub error: Option<String>,
    /// Shown in Settings after a manual check ("You have the latest version.").
    pub message: Option<String>,
    pub dismissed: bool,
    /// 0..=1000
    pub progress: Arc<AtomicU32>,
    /// Updates can be applied in place only for a copy that the setup program installed.
    pub installed_by_setup: bool,
    pub checked_ok: bool,
    rx: Option<Receiver<Ev>>,
}

impl Updater {
    pub fn new() -> Self {
        let installed_by_setup = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("unins000.exe").exists()))
            .unwrap_or(false);
        Updater {
            phase: Phase::Idle,
            info: None,
            error: None,
            message: None,
            dismissed: false,
            progress: Arc::new(AtomicU32::new(0)),
            installed_by_setup,
            checked_ok: false,
            rx: None,
        }
    }

    pub fn can_install_in_place(&self) -> bool {
        self.info.as_ref().is_some_and(|i| i.url.is_some()) && self.installed_by_setup
    }

    pub fn show_banner(&self) -> bool {
        !self.dismissed && matches!(self.phase, Phase::Available | Phase::Downloading | Phase::Failed) && self.info.is_some()
    }

    pub fn check(&mut self, manual: bool, wake: Arc<dyn Fn() + Send + Sync>) {
        if matches!(self.phase, Phase::Checking | Phase::Downloading) {
            return;
        }
        if manual {
            self.dismissed = false;
            self.message = None;
        }
        self.phase = Phase::Checking;
        let (tx, rx) = channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(Ev::Checked { manual, res: check_latest(CURRENT) });
            wake();
        });
    }

    /// Downloads the setup program and starts it; or, when this copy was not installed by the setup program, opens the release page.
    pub fn install(&mut self, wake: Arc<dyn Fn() + Send + Sync>) {
        let Some(info) = self.info.clone() else { return };
        if !self.can_install_in_place() {
            crate::util::open_url(&info.page);
            return;
        }
        let Some(url) = info.url.clone() else { return };
        self.phase = Phase::Downloading;
        self.error = None;
        self.progress.store(0, Ordering::Relaxed);
        let progress = self.progress.clone();
        let (tx, rx) = channel();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(Ev::Downloaded(download(&info, &url, &progress, &wake)));
            wake();
        });
    }

    /// Handles finished background work. Returns `Exit` when the setup program has been started.
    pub fn poll(&mut self) -> Option<Outcome> {
        let rx = self.rx.as_ref()?;
        let ev = rx.try_recv().ok()?;
        self.rx = None;
        match ev {
            Ev::Checked { manual, res } => match res {
                Ok(Some(info)) => {
                    self.checked_ok = true;
                    self.phase = Phase::Available;
                    self.info = Some(info);
                    if !manual && self.dismissed {
                        self.dismissed = false;
                    }
                }
                Ok(None) => {
                    self.checked_ok = true;
                    self.phase = Phase::Idle;
                    self.info = None;
                    if manual {
                        self.message = Some(format!("You have the latest version ({CURRENT})."));
                    }
                }
                Err(e) => {
                    // an automatic check that fails (offline, rate limited) stays quiet
                    self.phase = if self.info.is_some() { Phase::Available } else { Phase::Idle };
                    if manual {
                        self.message = Some(e);
                    }
                }
            },
            Ev::Downloaded(Ok(path)) => match launch(&path) {
                Ok(()) => return Some(Outcome::Exit),
                Err(e) => {
                    self.phase = Phase::Failed;
                    self.error = Some(e);
                }
            },
            Ev::Downloaded(Err(e)) => {
                self.phase = Phase::Failed;
                self.error = Some(e);
            }
        }
        None
    }

    pub fn busy(&self) -> bool {
        matches!(self.phase, Phase::Checking | Phase::Downloading)
    }
}

// ---------------------------------------------------------------- versions

pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.trim().trim_start_matches(['v', 'V']);
    if s.contains('-') {
        return None; // pre-release: not an update
    }
    let mut it = s.split('.').map(|p| p.parse::<u32>());
    let a = it.next()?.ok()?;
    let b = it.next().unwrap_or(Ok(0)).ok()?;
    let c = it.next().unwrap_or(Ok(0)).ok()?;
    Some((a, b, c))
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The "## x.y.z" section of CHANGELOG.md for a version (what is shown as "What's new").
pub fn changelog_section(version: &str) -> Option<String> {
    let mut out = String::new();
    let mut on = false;
    for line in CHANGELOG.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            if on {
                break;
            }
            on = h.trim().trim_start_matches(['v', 'V']).split_whitespace().next() == Some(version);
            continue;
        }
        if on {
            out.push_str(line);
            out.push('\n');
        }
    }
    let t = out.trim();
    if t.is_empty() { None } else { Some(t.to_string()) }
}

// ---------------------------------------------------------------- network

fn latest_url() -> String {
    std::env::var("LEANPDF_UPDATE_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{OWNER}/{REPO}/releases/latest"))
}

/// HTTPS on github.com (release pages and downloads) or githubusercontent.com (where GitHub serves the files from).
pub fn is_trusted_url(url: &str) -> bool {
    if std::env::var("LEANPDF_UPDATE_URL").is_ok() {
        return true; // a test server
    }
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").split('@').next_back().unwrap_or("").to_lowercase();
    host == "github.com" || host.ends_with(".github.com") || host.ends_with(".githubusercontent.com")
}

pub fn check_latest(current: &str) -> Result<Option<UpdateInfo>, String> {
    let mut resp = crate::llm::agent()
        .get(&latest_url())
        .header("accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("Could not reach GitHub to look for updates: {e}"))?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok(None); // nothing published yet
    }
    if status == 403 || status == 429 {
        return Err("GitHub is limiting requests from this connection right now. Try again later.".into());
    }
    if !(200..300).contains(&status) {
        return Err(format!("GitHub answered with an error ({status})."));
    }
    let text = resp.body_mut().read_to_string().map_err(|e| format!("GitHub's answer could not be read: {e}"))?;
    parse_release(&text, current)
}

pub fn parse_release(json: &str, current: &str) -> Result<Option<UpdateInfo>, String> {
    let v: Value = serde_json::from_str(json).map_err(|_| "GitHub's answer could not be read.".to_string())?;
    let tag = v["tag_name"].as_str().unwrap_or("");
    if !is_newer(tag, current) {
        return Ok(None);
    }
    let (mut url, mut sha) = (None, None);
    if let Some(assets) = v["assets"].as_array() {
        for a in assets {
            let name = a["name"].as_str().unwrap_or("");
            if name.starts_with(INSTALLER_PREFIX) && name.to_lowercase().ends_with(".exe") {
                url = a["browser_download_url"].as_str().map(str::to_string);
                sha = a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(|d| d.trim().to_lowercase());
                break;
            }
        }
    }
    Ok(Some(UpdateInfo {
        version: tag.trim_start_matches(['v', 'V']).to_string(),
        page: v["html_url"].as_str().map(str::to_string).unwrap_or_else(|| format!("https://github.com/{OWNER}/{REPO}/releases/latest")),
        url,
        sha256: sha,
        notes: v["body"].as_str().unwrap_or("").to_string(),
    }))
}

fn download(info: &UpdateInfo, url: &str, progress: &AtomicU32, wake: &Arc<dyn Fn() + Send + Sync>) -> Result<PathBuf, String> {
    if !is_trusted_url(url) {
        return Err("The update's download address is not on GitHub, so it was not used.".into());
    }
    let folder = std::env::temp_dir().join("LeanPDF-update");
    std::fs::create_dir_all(&folder).map_err(|e| format!("The download failed: {e}"))?;
    let path = folder.join(format!("{INSTALLER_PREFIX}{}.exe", info.version));
    let result = (|| -> Result<(), String> {
        let mut resp = crate::llm::agent().get(url).call().map_err(|e| format!("The download failed: {e}"))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("The download failed: GitHub answered with an error ({status})."));
        }
        let total: Option<u64> = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|s| s.parse().ok());
        let mut reader = resp.body_mut().with_config().limit(400 * 1024 * 1024).reader();
        let mut file = std::fs::File::create(&path).map_err(|e| format!("The download failed: {e}"))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut read: u64 = 0;
        loop {
            let n = reader.read(&mut buf).map_err(|e| format!("The download failed: {e}"))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| format!("The download failed: {e}"))?;
            hasher.update(&buf[..n]);
            read += n as u64;
            if let Some(t) = total.filter(|t| *t > 0) {
                progress.store((read.saturating_mul(1000) / t).min(1000) as u32, Ordering::Relaxed);
                wake();
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        if let Some(t) = total {
            if t != read {
                return Err("The download was cut short. Try again.".into());
            }
        }
        if let Some(expected) = &info.sha256 {
            let actual: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
            if !actual.eq_ignore_ascii_case(expected.trim()) {
                return Err("The downloaded file does not match the checksum GitHub published for it, so it was not run.".into());
            }
        }
        progress.store(1000, Ordering::Relaxed);
        Ok(())
    })();
    match result {
        Ok(()) => Ok(path),
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            Err(e)
        }
    }
}

fn launch(path: &std::path::Path) -> Result<(), String> {
    if let Ok(dry) = std::env::var("LEANPDF_UPDATE_DRYRUN") {
        // test hook: record what would be run instead of running it
        let line = format!("{} {}\n", path.display(), SETUP_ARGS.join(" "));
        return std::fs::write(dry, line).map_err(|e| e.to_string());
    }
    std::process::Command::new(path)
        .args(SETUP_ARGS)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("The update could not be started: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("1.0.0", "0.99.99"));
        assert!(!is_newer("v0.3.0", "0.3.0"));
        assert!(!is_newer("v0.2.9", "0.3.0"));
        assert!(!is_newer("v0.4.0-beta", "0.3.0"), "pre-releases are not updates");
        assert!(!is_newer("nightly", "0.3.0"));
    }

    #[test]
    fn releases_are_read() {
        let j = r#"{"tag_name":"v0.4.0","html_url":"https://github.com/o/r/releases/tag/v0.4.0","body":"- new","assets":[
            {"name":"LeanPDF-0.4.0-win-x64.zip","browser_download_url":"https://x/zip"},
            {"name":"LeanPDF-Setup-0.4.0.exe","browser_download_url":"https://github.com/o/r/releases/download/v0.4.0/LeanPDF-Setup-0.4.0.exe","digest":"sha256:ABCDEF"}]}"#;
        let i = parse_release(j, "0.3.0").unwrap().unwrap();
        assert_eq!(i.version, "0.4.0");
        assert!(i.url.unwrap().ends_with("LeanPDF-Setup-0.4.0.exe"));
        assert_eq!(i.sha256.as_deref(), Some("abcdef"));
        assert!(parse_release(j, "0.4.0").unwrap().is_none());
    }

    #[test]
    fn only_github_downloads_are_trusted() {
        assert!(is_trusted_url("https://github.com/o/r/releases/download/v1/a.exe"));
        assert!(is_trusted_url("https://objects.githubusercontent.com/x"));
        assert!(!is_trusted_url("http://github.com/x"));
        assert!(!is_trusted_url("https://github.com.evil.com/x"));
        assert!(!is_trusted_url("https://evil.com/github.com/x"));
        assert!(!is_trusted_url("https://github.com@evil.com/x"));
    }

    #[test]
    fn changelog_sections_are_found() {
        assert!(changelog_section(CURRENT).is_some(), "CHANGELOG.md needs a section for {CURRENT}");
    }
}
