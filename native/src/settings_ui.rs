//! The Settings window (General, AI, Updates), the update banner and the "What's new" window.

use crate::chat::pidx;
use crate::llm::{self, ModelInfo, Provider};
use crate::update::{self, Phase};
use crate::*;
use eframe::egui::{self, Color32, RichText};

pub const DEFAULT_SCROLL_SPEED: f32 = 3.0;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsTab {
    #[default]
    General,
    Ai,
    Updates,
}

#[derive(Default)]
pub struct SettingsWin {
    pub open: bool,
    pub tab: SettingsTab,
}

pub struct WhatsNew {
    pub title: String,
    pub body: String,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl App {
    pub fn scroll_speed(&self) -> f32 {
        self.settings.scroll_speed.unwrap_or(DEFAULT_SCROLL_SPEED).clamp(0.25, 12.0)
    }

    pub fn save_settings(&mut self) {
        let _ = settings::save(&self.dir, &self.settings);
    }

    /// Applies theme and interface size (at start and when they change).
    pub fn apply_look(&self) {
        let pref = match self.settings.theme.as_deref() {
            Some("dark") => egui::ThemePreference::Dark,
            Some("light") => egui::ThemePreference::Light,
            _ => egui::ThemePreference::System,
        };
        self.ctx.set_theme(pref);
        self.ctx.set_zoom_factor(self.settings.ui_scale.unwrap_or(1.0).clamp(0.7, 2.0));
    }

    pub fn open_settings(&mut self, tab: SettingsTab, notice: Option<String>) {
        self.settings_win.open = true;
        self.settings_win.tab = tab;
        self.ai.notice = notice;
        self.ai.loaded = false;
        self.ai.test_msg = None;
    }

    pub fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings_win.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings").anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).open(&mut open).collapsible(false).resizable(false).default_width(470.0).show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (t, name) in [(SettingsTab::General, "General"), (SettingsTab::Ai, "AI"), (SettingsTab::Updates, "Updates")] {
                    let label = if t == SettingsTab::Updates && self.updater.info.is_some() { format!("{name} •") } else { name.to_string() };
                    if ui.selectable_label(self.settings_win.tab == t, label).clicked() {
                        self.settings_win.tab = t;
                    }
                }
            });
            ui.separator();
            match self.settings_win.tab {
                SettingsTab::General => self.general_tab(ui),
                SettingsTab::Ai => self.ai_tab(ui),
                SettingsTab::Updates => self.updates_tab(ui),
            }
        });
        if !open {
            self.settings_win.open = false;
            self.ai.key_input.clear();
        }
    }

    // ---------------------------------------------------------------- General

    fn general_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Scrolling").strong());
        let mut speed = self.scroll_speed();
        let r = ui.add(egui::Slider::new(&mut speed, 0.5..=10.0).step_by(0.25).text("Mouse wheel speed"));
        if r.changed() {
            self.settings.scroll_speed = Some(speed);
            self.save_settings();
        }
        ui.label(RichText::new(format!("Higher scrolls further per notch. Default {DEFAULT_SCROLL_SPEED}.")).small().weak());
        ui.add_space(8.0);

        ui.label(RichText::new("Appearance").strong());
        let mut theme = self.settings.theme.clone().unwrap_or_else(|| "system".into());
        let before = theme.clone();
        ui.horizontal(|ui| {
            ui.label("Theme");
            ui.radio_value(&mut theme, "system".to_string(), "Follow Windows");
            ui.radio_value(&mut theme, "dark".to_string(), "Dark");
            ui.radio_value(&mut theme, "light".to_string(), "Light");
        });
        if theme != before {
            self.settings.theme = Some(theme);
            self.save_settings();
            self.apply_look();
        }
        let mut scale = self.settings.ui_scale.unwrap_or(1.0);
        let r = ui.add(egui::Slider::new(&mut scale, 0.8..=1.6).step_by(0.05).custom_formatter(|v, _| format!("{:.0}%", v * 100.0)).text("Interface size"));
        if r.changed() {
            self.settings.ui_scale = Some(scale);
        }
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            self.save_settings();
            self.apply_look();
        }
        ui.add_space(8.0);

        ui.label(RichText::new("Comments").strong());
        let mut author = self.settings.author.clone().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("Author name");
            if ui.add(egui::TextEdit::singleline(&mut author).hint_text(std::env::var("USERNAME").unwrap_or_default()).desired_width(220.0)).changed() {
                self.settings.author = if author.trim().is_empty() { None } else { Some(author.clone()) };
                self.save_settings();
            }
        });
        ui.label(RichText::new("Shown in Acrobat as the author of comments you add. Empty uses your Windows user name.").small().weak());
        ui.add_space(8.0);

        ui.label(RichText::new("Starting up").strong());
        let mut reopen = self.settings.reopen_tabs.unwrap_or(true);
        if ui.checkbox(&mut reopen, "Reopen the tabs from last time").changed() {
            self.settings.reopen_tabs = Some(reopen);
            self.save_settings();
        }
        ui.add_space(8.0);
        if ui.button("Reset these to defaults").clicked() {
            self.settings.scroll_speed = None;
            self.settings.theme = None;
            self.settings.ui_scale = None;
            self.settings.author = None;
            self.settings.reopen_tabs = None;
            self.save_settings();
            self.apply_look();
        }
    }

    // ---------------------------------------------------------------- Updates

    fn updates_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(format!("LeanPDF {}", update::CURRENT)).strong());
        ui.label(RichText::new(if self.updater.installed_by_setup { "Installed with the setup program: updates install themselves." } else { "This copy was not installed with the setup program, so updates open the download page." }).small().weak());
        ui.add_space(6.0);
        let mut auto = self.settings.auto_update.unwrap_or(true);
        if ui.checkbox(&mut auto, "Look for a new version when LeanPDF starts (once a day)").changed() {
            self.settings.auto_update = Some(auto);
            self.save_settings();
        }
        ui.horizontal(|ui| {
            if ui.add_enabled(!self.updater.busy(), egui::Button::new("Check for updates now")).clicked() {
                let wake = self.wake();
                self.updater.check(true, wake);
            }
            if self.updater.phase == Phase::Checking {
                ui.label(RichText::new("Checking…").weak());
            }
        });
        if let Some(m) = &self.updater.message {
            ui.label(RichText::new(m).color(Color32::from_rgb(130, 200, 140)));
        }
        if let Some(info) = self.updater.info.clone() {
            ui.add_space(6.0);
            ui.label(RichText::new(format!("Version {} is available.", info.version)).strong());
            if self.updater.phase == Phase::Downloading {
                ui.add(egui::ProgressBar::new(self.updater.progress.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0).show_percentage());
            } else if ui.button(if self.updater.can_install_in_place() { "Update now" } else { "Download" }).clicked() {
                let wake = self.wake();
                self.updater.install(wake);
            }
            if let Some(e) = &self.updater.error {
                ui.label(RichText::new(e).color(Color32::from_rgb(230, 100, 90)));
            }
            if !info.notes.trim().is_empty() {
                ui.add_space(4.0);
                egui::ScrollArea::vertical().max_height(140.0).id_salt("upd-notes").show(ui, |ui| crate::chat::markdown(ui, &info.notes));
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("What's new in this version").clicked() {
                self.show_whats_new();
            }
            if ui.button("Open the GitHub page").clicked() {
                util::open_url(&format!("https://github.com/{}/{}", update::OWNER, update::REPO));
            }
        });
    }

    pub fn wake(&self) -> Arc<dyn Fn() + Send + Sync> {
        let ctx = self.ctx.clone();
        Arc::new(move || ctx.request_repaint())
    }

    // ---------------------------------------------------------------- update bar and What's new

    pub fn update_banner(&mut self, ui: &mut egui::Ui) {
        let Some(info) = self.updater.info.clone() else { return };
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            match self.updater.phase {
                Phase::Downloading => {
                    ui.label("Downloading the update…");
                    ui.add(egui::ProgressBar::new(self.updater.progress.load(std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0).desired_width(180.0));
                }
                Phase::Failed => {
                    ui.label(RichText::new(self.updater.error.clone().unwrap_or_else(|| "The update failed.".into())).color(Color32::from_rgb(230, 100, 90)));
                    if ui.button("Download manually").clicked() {
                        util::open_url(&info.page);
                    }
                }
                _ => {
                    ui.label(RichText::new(format!("LeanPDF {} is available.", info.version)).strong());
                    if ui.button(if self.updater.can_install_in_place() { "Update now" } else { "Download" }).clicked() {
                        let wake = self.wake();
                        self.updater.install(wake);
                    }
                    if ui.button("What's new").clicked() {
                        self.whatsnew = Some(WhatsNew { title: format!("What's new in LeanPDF {}", info.version), body: if info.notes.trim().is_empty() { "No release notes were published for this version.".into() } else { info.notes.clone() } });
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.updater.phase != Phase::Downloading && ui.button("Later").clicked() {
                    self.updater.dismissed = true;
                }
            });
        });
        ui.add_space(3.0);
    }

    pub fn show_whats_new(&mut self) {
        let body = update::changelog_section(update::CURRENT).unwrap_or_else(|| "No notes for this version.".into());
        self.whatsnew = Some(WhatsNew { title: format!("What's new in LeanPDF {}", update::CURRENT), body });
    }

    pub fn whats_new_window(&mut self, ctx: &egui::Context) {
        let Some(w) = &self.whatsnew else { return };
        let (title, body) = (w.title.clone(), w.body.clone());
        let mut open = true;
        egui::Window::new(&title).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).open(&mut open).collapsible(false).resizable(false).default_width(440.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| crate::chat::markdown(ui, &body));
            ui.add_space(6.0);
            if ui.button("Close").clicked() {
                self.whatsnew = None;
            }
        });
        if !open {
            self.whatsnew = None;
        }
    }

    /// Start-up: the daily update check, and "What's new" once after an update.
    pub fn startup_update_tasks(&mut self) {
        if self.settings.auto_update.unwrap_or(true) {
            let due = self.settings.last_update_check.map_or(true, |t| unix_now().saturating_sub(t) > 24 * 3600);
            if due {
                let wake = self.wake();
                self.updater.check(false, wake);
            }
        }
        match self.settings.last_seen_version.clone() {
            Some(v) if v != update::CURRENT => self.show_whats_new(),
            _ => {}
        }
        if self.settings.last_seen_version.as_deref() != Some(update::CURRENT) {
            self.settings.last_seen_version = Some(update::CURRENT.to_string());
            self.save_settings();
        }
    }

    /// Called every frame: finishes update work and records a successful check.
    pub fn poll_updates(&mut self) {
        if let Some(update::Outcome::Exit) = self.updater.poll() {
            self.quit = true;
        }
        if self.updater.checked_ok {
            self.updater.checked_ok = false;
            self.settings.last_update_check = Some(unix_now());
            self.save_settings();
        }
    }

    // ---------------------------------------------------------------- AI

    fn start_models_fetch(&mut self, p: Provider) {
        if self.ai.models_rx.is_some() {
            return;
        }
        let key = llm::get_key(p);
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        self.ai.models_rx = Some((pidx(p), rx));
        self.ai.models_err = None;
        std::thread::spawn(move || {
            let _ = tx.send(llm::list_models(p, key.as_deref()));
            ctx.request_repaint();
        });
    }

    fn ai_tab(&mut self, ui: &mut egui::Ui) {
        if !self.ai.loaded {
            for p in Provider::ALL {
                self.ai.key_present[pidx(p)] = llm::get_key(p).is_some();
            }
            self.ai.loaded = true;
        }
        let mut provider = self.provider();
        let mut save_key: Option<(Provider, String)> = None;
        let mut remove_key: Option<Provider> = None;
        let mut test: Option<Provider> = None;
        let mut changed_provider = false;
        let mut open_link: Option<&'static str> = None;
        let mut refresh = false;

        if let Some(n) = &self.ai.notice {
            ui.label(RichText::new(n).color(Color32::from_rgb(230, 150, 70)));
            ui.add_space(4.0);
        }
        ui.label("Which service answers your questions?");
        ui.horizontal(|ui| {
            for p in Provider::ALL {
                if ui.radio_value(&mut provider, p, p.name()).changed() {
                    changed_provider = true;
                }
            }
        });
        ui.separator();
        let i = pidx(provider);
        ui.label(RichText::new(format!("{} API key", provider.name())).strong());
        if let Some(src) = llm::key_source(provider).filter(|_| self.ai.key_present[i]) {
            ui.label(RichText::new(format!("A key is in use (from {src}).")).small().weak());
        } else {
            ui.label(RichText::new("No key saved yet.").small().color(Color32::from_rgb(230, 150, 70)));
        }
        ui.horizontal(|ui| {
            let te = ui.add(egui::TextEdit::singleline(&mut self.ai.key_input).password(true).hint_text("Paste your key here (Ctrl+V)").desired_width(260.0));
            if te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.ai.key_input.trim().is_empty() {
                save_key = Some((provider, self.ai.key_input.trim().to_string()));
            }
            if ui.add_enabled(!self.ai.key_input.trim().is_empty(), egui::Button::new("Save key")).clicked() {
                save_key = Some((provider, self.ai.key_input.trim().to_string()));
            }
            if self.ai.key_present[i] && ui.button("Remove").clicked() {
                remove_key = Some(provider);
            }
        });
        if ui.link(format!("Get a key at {}", provider.keys_url())).clicked() {
            open_link = Some(provider.keys_url());
        }

        ui.add_space(8.0);
        ui.label(RichText::new("Model").strong());
        let current = self.model(provider);
        let loaded = !self.ai.models[i].is_empty();
        let list: Vec<ModelInfo> = if loaded { self.ai.models[i].clone() } else { fallback_models(provider) };
        let selected_label = list.iter().find(|m| m.id == current).map(|m| m.label.clone()).unwrap_or_else(|| current.clone());
        let mut picked: Option<String> = None;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("model_combo").width(330.0).selected_text(truncate(&selected_label, 46)).show_ui(ui, |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.ai.model_filter).hint_text("Filter models…").desired_width(300.0));
                let f = self.ai.model_filter.to_lowercase();
                egui::ScrollArea::vertical().max_height(300.0).id_salt("model-list").show(ui, |ui| {
                    let mut shown = 0;
                    for m in list.iter().filter(|m| f.is_empty() || m.id.to_lowercase().contains(&f) || m.label.to_lowercase().contains(&f)) {
                        shown += 1;
                        if ui.selectable_label(m.id == current, &m.label).clicked() {
                            picked = Some(m.id.clone());
                        }
                    }
                    if shown == 0 {
                        ui.label(RichText::new("No model matches.").weak());
                    }
                });
            });
            if ui.button("Refresh").on_hover_text("Reload the list from the provider").clicked() {
                refresh = true;
            }
            if self.ai.models_rx.is_some() {
                ui.label(RichText::new("Loading…").small().weak());
            }
        });
        if let Some(e) = &self.ai.models_err {
            ui.label(RichText::new(e).small().color(Color32::from_rgb(230, 150, 70)));
        } else if loaded {
            ui.label(RichText::new(format!("{} models available from {}.", list.len(), provider.name())).small().weak());
        }
        ui.label(RichText::new(format!("Default: {}  (fast and inexpensive; pick Sonnet or Opus for harder questions)", provider.default_model())).small().weak());
        ui.collapsing("Enter a model id by hand", |ui| {
            let mut manual = current.clone();
            if ui.add(egui::TextEdit::singleline(&mut manual).desired_width(330.0)).changed() {
                picked = Some(manual.trim().to_string());
            }
        });
        if let Some(id) = picked {
            let v = if id.is_empty() || id == provider.default_model() { None } else { Some(id) };
            match provider {
                Provider::OpenRouter => self.settings.ai_model_openrouter = v,
                Provider::Anthropic => self.settings.ai_model_anthropic = v,
            }
            self.save_settings();
        }

        ui.add_space(8.0);
        let mut folder = self.settings.ai_use_folder.unwrap_or(false);
        if ui.checkbox(&mut folder, "Also search the other PDFs in the folder when answering").changed() {
            self.settings.ai_use_folder = Some(folder);
            self.save_settings();
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(self.ai.key_present[i] && self.ai.test.is_none(), egui::Button::new("Test connection")).clicked() {
                test = Some(provider);
            }
            if self.ai.test.is_some() {
                ui.label(RichText::new("Testing…").weak());
            }
        });
        if let Some((m, ok)) = &self.ai.test_msg {
            ui.label(RichText::new(m).color(if *ok { Color32::from_rgb(110, 200, 120) } else { Color32::from_rgb(230, 100, 90) }));
        }
        ui.add_space(6.0);
        ui.label(RichText::new("Only the pages needed to answer a question are sent, as text, when you ask. Keys never leave this PC except to the service you chose.").small().weak());

        // ---- apply what was clicked
        if changed_provider {
            self.ai.test_msg = None;
            self.ai.models_err = None;
            self.settings.ai_provider = Some(provider.id().to_string());
            self.save_settings();
        }
        if let Some((p, k)) = save_key {
            match llm::set_key(p, &k) {
                Ok(()) => {
                    self.ai.key_present[pidx(p)] = true;
                    self.ai.key_input.clear();
                    self.ai.notice = None;
                    self.settings.ai_provider = Some(p.id().to_string());
                    self.save_settings();
                    self.toast(format!("{} key saved", p.name()));
                    refresh = true;
                }
                Err(e) => self.toast(format!("Could not save the key: {e}")),
            }
        }
        if let Some(p) = remove_key {
            let _ = llm::delete_key(p);
            self.ai.key_present[pidx(p)] = llm::get_key(p).is_some();
            self.ai.test_msg = None;
        }
        if let Some(p) = test {
            if let Some(key) = llm::get_key(p) {
                let model = self.model(p);
                let (tx, rx) = channel();
                let ctx = self.ctx.clone();
                self.ai.test = Some(rx);
                self.ai.test_msg = None;
                std::thread::spawn(move || {
                    let _ = tx.send(llm::test_connection(p, &key, &model));
                    ctx.request_repaint();
                });
            }
        }
        if let Some(url) = open_link {
            util::open_url(url);
        }
        // load the model list the first time, after a provider/key change, or on request
        let needs = self.ai.models[pidx(provider)].is_empty() && self.ai.models_err.is_none() && (provider == Provider::OpenRouter || self.ai.key_present[pidx(provider)]);
        if refresh {
            self.ai.models[pidx(provider)].clear();
            self.ai.models_err = None;
        }
        if (needs || refresh) && self.ai.models_rx.is_none() {
            self.start_models_fetch(provider);
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n { s.chars().take(n - 1).collect::<String>() + "…" } else { s.to_string() }
}

/// Shown until the provider's own list arrives (or when it cannot be fetched).
fn fallback_models(p: Provider) -> Vec<ModelInfo> {
    let ids: [(&str, &str); 3] = match p {
        Provider::OpenRouter => [
            ("anthropic/claude-haiku-5.5", "Claude Haiku 5.5"),
            ("anthropic/claude-sonnet-5.5", "Claude Sonnet 5.5"),
            ("anthropic/claude-opus-5.5", "Claude Opus 5.5"),
        ],
        Provider::Anthropic => [("claude-haiku-5-5", "Claude Haiku 5.5"), ("claude-sonnet-5-5", "Claude Sonnet 5.5"), ("claude-opus-5-5", "Claude Opus 5.5")],
    };
    ids.iter().map(|(id, name)| ModelInfo { id: id.to_string(), label: format!("{name}  ·  {id}") }).collect()
}
