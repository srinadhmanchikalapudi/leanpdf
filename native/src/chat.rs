//! "Ask AI": a chat panel that answers questions about the open PDF (and optionally the rest
//! of the folder) using OpenRouter or Anthropic, with clickable page sources.

use crate::llm::{self, ChatEvent, Msg, Provider, Role};
use crate::rag::{self, Cite, Excerpt};
use crate::util::*;
use crate::*;
use eframe::egui::{self, text::LayoutJob, Color32, RichText, TextFormat};
use std::sync::atomic::{AtomicBool, Ordering};

const TEXT_CHUNK: u32 = 40;

pub struct ChatMsg {
    pub role: Role,
    pub text: String,
    pub error: bool,
    /// What was sent, shown under the answer ("Sent 6 pages ... to OpenRouter").
    pub note: Option<String>,
    /// File display name -> path, so a citation of another file can be opened.
    pub files: Vec<(String, String)>,
    pub cites: Vec<Cite>,
}

pub struct Stream {
    rx: Receiver<ChatEvent>,
    cancel: Arc<AtomicBool>,
}

impl Stream {
    pub fn cancel_now(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct Chat {
    pub msgs: Vec<ChatMsg>,
    pub input: String,
    pub stream: Option<Stream>,
    /// Page text of this document, filled the first time a question is asked.
    pub texts: Vec<(u32, String)>,
    pub texts_done: bool,
    pub texts_next: u32,
    pub pending: Option<String>,
}

#[derive(Default)]
pub struct AiUi {
    pub open: bool,
    pub key_input: String,
    pub notice: Option<String>,
    pub key_present: [bool; 2],
    pub models: [Vec<llm::ModelInfo>; 2],
    pub models_rx: Option<(usize, Receiver<Result<Vec<llm::ModelInfo>, String>>)>,
    pub models_err: Option<String>,
    pub model_filter: String,
    pub test: Option<Receiver<Result<String, String>>>,
    pub test_msg: Option<(String, bool)>,
    pub loaded: bool,
}

pub fn pidx(p: Provider) -> usize {
    if p == Provider::OpenRouter { 0 } else { 1 }
}

enum ChatAct {
    Send,
    Stop,
    Clear,
    Settings,
    Cite(usize, usize), // message index, citation index
    Copy(usize),
}

impl App {
    pub fn provider(&self) -> Provider {
        match self.settings.ai_provider.as_deref() {
            Some(id) => Provider::from_id(id),
            None => {
                if llm::get_key(Provider::Anthropic).is_some() && llm::get_key(Provider::OpenRouter).is_none() {
                    Provider::Anthropic
                } else {
                    Provider::OpenRouter
                }
            }
        }
    }

    pub fn model(&self, p: Provider) -> String {
        let m = match p {
            Provider::OpenRouter => self.settings.ai_model_openrouter.clone(),
            Provider::Anthropic => self.settings.ai_model_anthropic.clone(),
        };
        m.filter(|m| !m.trim().is_empty()).unwrap_or_else(|| p.default_model().to_string())
    }

    pub fn toggle_chat(&mut self) {
        self.chat_open = !self.chat_open;
        if self.chat_open {
            self.comments_open = false;
        }
        if let Some(d) = &mut self.doc {
            d.viewport_w = 0.0;
        }
    }

    // ---------------------------------------------------------------- panel

    pub fn chat_panel(&mut self, ui: &mut egui::Ui) {
        let Some(doc_id) = self.doc.as_ref().map(|d| d.id) else {
            ui.add_space(8.0);
            ui.label(RichText::new("Open a PDF to ask questions about it.").weak());
            return;
        };
        let p = self.provider();
        let model = self.model(p);
        let has_key = llm::get_key(p).is_some();
        let use_folder = self.settings.ai_use_folder.unwrap_or(false);
        let mut acts: Vec<ChatAct> = Vec::new();
        let mut folder_toggle = use_folder;

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Ask AI").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Settings").clicked() {
                    acts.push(ChatAct::Settings);
                }
                if ui.small_button("Clear").on_hover_text("Clear this conversation").clicked() {
                    acts.push(ChatAct::Clear);
                }
            });
        });
        if has_key {
            ui.label(RichText::new(format!("Excerpts of the PDF you ask about are sent to {} ({}).", p.name(), model)).small().weak());
        } else {
            ui.label(RichText::new(format!("Add your {} API key in Settings to start.", p.name())).small().color(Color32::from_rgb(230, 150, 70)));
        }
        ui.separator();

        let chat = self.chats.entry(doc_id).or_default();
        let busy = chat.stream.is_some() || chat.pending.is_some();
        let accent = ui.visuals().hyperlink_color;
        let footer_h = 128.0;
        let avail = ui.available_height();

        egui::ScrollArea::vertical().id_salt("chat-msgs").stick_to_bottom(true).auto_shrink(false).max_height((avail - footer_h).max(80.0)).show(ui, |ui| {
            if chat.msgs.is_empty() {
                ui.add_space(6.0);
                ui.label(RichText::new("Ask about this PDF, for example:").weak());
                for ex in ["Summarize this document", "What does it say about <a topic>?", "Explain the section on this page"] {
                    ui.label(RichText::new(format!("   · {ex}")).weak().small());
                }
            }
            for (mi, m) in chat.msgs.iter().enumerate() {
                ui.add_space(6.0);
                match m.role {
                    Role::User => {
                        egui::Frame::new()
                            .fill(ui.visuals().widgets.noninteractive.weak_bg_fill)
                            .corner_radius(8.0)
                            .inner_margin(egui::Margin::symmetric(8, 6))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(&m.text);
                            });
                    }
                    Role::Assistant => {
                        if m.error {
                            ui.label(RichText::new(&m.text).color(Color32::from_rgb(230, 100, 90)));
                        } else if m.text.is_empty() {
                            ui.label(RichText::new("Thinking…").weak());
                        } else {
                            markdown(ui, &m.text);
                        }
                        if !m.cites.is_empty() {
                            ui.add_space(2.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("Sources:").small().weak());
                                for (ci, c) in m.cites.iter().enumerate() {
                                    let label = match &c.file {
                                        Some(f) => format!("{} p.{}", f.trim_end_matches(".pdf"), c.page),
                                        None => format!("p.{}", c.page),
                                    };
                                    if ui.add(egui::Button::new(RichText::new(label).small().color(accent)).small()).clicked() {
                                        acts.push(ChatAct::Cite(mi, ci));
                                    }
                                }
                            });
                        }
                        if let Some(n) = &m.note {
                            ui.label(RichText::new(n).small().weak());
                        }
                        if !m.error && !m.text.is_empty() && ui.small_button("Copy").clicked() {
                            acts.push(ChatAct::Copy(mi));
                        }
                    }
                }
            }
        });

        ui.add_space(4.0);
        ui.separator();
        if ui.checkbox(&mut folder_toggle, "Also search other PDFs in this folder").on_hover_text("Uses the folder index to add the best-matching pages from your other PDFs").changed() {
            self.settings.ai_use_folder = Some(folder_toggle);
            let _ = settings::save(&self.dir, &self.settings);
        }
        let chat = self.chats.entry(doc_id).or_default();
        let input_id = egui::Id::new("chat_input");
        let focused = ui.memory(|m| m.has_focus(input_id));
        let enter = focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        ui.add(egui::TextEdit::multiline(&mut chat.input).id(input_id).desired_rows(3).desired_width(f32::INFINITY).hint_text("Ask a question…  (Enter to send, Shift+Enter for a new line)"));
        ui.horizontal(|ui| {
            if busy {
                if ui.button("Stop").clicked() {
                    acts.push(ChatAct::Stop);
                }
                ui.label(RichText::new("Answering…").small().weak());
            } else if ui.add_enabled(!chat.input.trim().is_empty(), egui::Button::new("Send")).clicked() || (enter && !chat.input.trim().is_empty()) {
                acts.push(ChatAct::Send);
            }
        });

        for a in acts {
            match a {
                ChatAct::Send => self.ask(),
                ChatAct::Stop => self.stop_chat(doc_id),
                ChatAct::Clear => {
                    self.stop_chat(doc_id);
                    if let Some(c) = self.chats.get_mut(&doc_id) {
                        c.msgs.clear();
                    }
                }
                ChatAct::Settings => self.open_settings(crate::settings_ui::SettingsTab::Ai, None),
                ChatAct::Cite(mi, ci) => self.cite_click(doc_id, mi, ci),
                ChatAct::Copy(mi) => {
                    if let Some(m) = self.chats.get(&doc_id).and_then(|c| c.msgs.get(mi)) {
                        self.ctx.copy_text(m.text.clone());
                        self.toast("Copied");
                    }
                }
            }
        }
    }

    fn cite_click(&mut self, doc_id: u32, mi: usize, ci: usize) {
        let Some((cite, files)) = self.chats.get(&doc_id).and_then(|c| c.msgs.get(mi)).and_then(|m| m.cites.get(ci).map(|c| (c.clone(), m.files.clone()))) else { return };
        let page = cite.page.saturating_sub(1) as usize;
        let current = self.doc.as_ref().map(|d| (d.name.clone(), d.path.clone()));
        let same = match (&cite.file, &current) {
            (None, _) => true,
            (Some(f), Some((name, _))) => name.eq_ignore_ascii_case(f) || name.to_lowercase().trim_end_matches(".pdf") == f.to_lowercase().trim_end_matches(".pdf"),
            _ => false,
        };
        if same {
            self.goto_page(page, None);
            return;
        }
        let want = cite.file.unwrap_or_default().to_lowercase();
        let path = files
            .iter()
            .find(|(n, _)| n.to_lowercase() == want || n.to_lowercase().trim_end_matches(".pdf") == want.trim_end_matches(".pdf"))
            .map(|(_, p)| p.clone());
        match path {
            Some(p) => self.open_file(p, page, Vec::new(), Vec::new(), false),
            None => self.toast("That file is not among the excerpts that were sent."),
        }
    }

    pub fn stop_chat(&mut self, doc_id: u32) {
        if let Some(c) = self.chats.get_mut(&doc_id) {
            if let Some(s) = c.stream.take() {
                s.cancel.store(true, Ordering::Relaxed);
            }
            c.pending = None;
            if let Some(m) = c.msgs.last_mut() {
                if m.role == Role::Assistant && m.text.is_empty() {
                    c.msgs.pop();
                }
            }
        }
    }

    // ---------------------------------------------------------------- asking

    fn ask(&mut self) {
        let Some(doc_id) = self.doc.as_ref().map(|d| d.id) else { return };
        let p = self.provider();
        if llm::get_key(p).is_none() {
            self.open_settings(crate::settings_ui::SettingsTab::Ai, Some(format!("Add your {} API key to start asking questions.", p.name())));
            return;
        }
        let page_count = self.doc.as_ref().map(|d| d.pages.len()).unwrap_or(0) as u32;
        let chat = self.chats.entry(doc_id).or_default();
        if chat.stream.is_some() || chat.pending.is_some() {
            return;
        }
        let q = chat.input.trim().to_string();
        if q.is_empty() {
            return;
        }
        chat.input.clear();
        chat.msgs.push(ChatMsg { role: Role::User, text: q.clone(), error: false, note: None, files: Vec::new(), cites: Vec::new() });
        if chat.texts_done {
            self.dispatch(doc_id, q);
        } else {
            chat.pending = Some(q);
            if chat.texts_next == 0 || chat.texts_next >= page_count {
                chat.texts_next = chat.texts.last().map(|(i, _)| i + 1).unwrap_or(0).min(page_count);
            }
            self.load_text_chunk(doc_id);
        }
    }

    fn load_text_chunk(&mut self, doc_id: u32) {
        let Some(d) = self.doc_mut(doc_id) else { return };
        let count = d.pages.len() as u32;
        let Some(chat) = self.chats.get_mut(&doc_id) else { return };
        if chat.texts_next >= count {
            chat.texts_done = true;
            if let Some(q) = chat.pending.take() {
                self.dispatch(doc_id, q);
            }
            return;
        }
        let from = chat.texts_next;
        chat.texts_next = (from + TEXT_CHUNK).min(count);
        let (tx, rx) = channel();
        let _ = self.viewer.send(Cmd::Text { doc: doc_id, from, to: from + TEXT_CHUNK, reply: tx });
        self.jobs.push(Job::Text(rx, doc_id, from));
    }

    pub fn finish_text(&mut self, res: Result<Vec<(u32, String)>, String>, doc: u32, _from: u32) {
        let Some(chat) = self.chats.get_mut(&doc) else { return };
        match res {
            Ok(mut pages) => {
                chat.texts.append(&mut pages);
                self.load_text_chunk(doc);
            }
            Err(e) => {
                chat.pending = None;
                chat.msgs.push(ChatMsg { role: Role::Assistant, text: format!("Could not read the PDF's text: {e}"), error: true, note: None, files: Vec::new(), cites: Vec::new() });
            }
        }
    }

    fn dispatch(&mut self, doc_id: u32, question: String) {
        let Some((path, name, cur)) = self.doc_mut(doc_id).map(|d| (d.path.clone(), d.name.clone(), d.cur_page as u32)) else { return };
        let p = self.provider();
        let Some(key) = llm::get_key(p) else { return };
        let model = self.model(p);
        let use_folder = self.settings.ai_use_folder.unwrap_or(false);
        let folder = self.settings.folder.clone();
        let db = self.indexer.db.clone();
        let Some(chat) = self.chats.get_mut(&doc_id) else { return };

        if chat.texts.is_empty() {
            chat.msgs.push(ChatMsg { role: Role::Assistant, text: "This PDF has no text layer (it looks like a scanned image), so there is nothing for me to read.".into(), error: true, note: None, files: Vec::new(), cites: Vec::new() });
            return;
        }

        let budget = if use_folder { 36_000 } else { 60_000 };
        let picked = rag::select_pages(&chat.texts, &question, cur, budget);
        let mut excerpts: Vec<Excerpt> =
            picked.iter().map(|&i| Excerpt { file: name.clone(), path: path.clone(), page: chat.texts[i].0, text: chat.texts[i].1.clone() }).collect();
        let own_pages = excerpts.len();
        if use_folder {
            if let Some(root) = folder {
                if let Ok(hits) = index::page_texts(&db, &root, &rag::terms(&question), "", 14) {
                    let mut chars = 0usize;
                    for h in hits {
                        if h.path == path || chars > 24_000 {
                            continue;
                        }
                        chars += h.text.len().min(6000);
                        let file = base_name(&h.rel).to_string();
                        excerpts.push(Excerpt { file, path: h.path, page: h.page, text: h.text });
                    }
                }
            }
        }
        let mut files: Vec<(String, String)> = Vec::new();
        for e in &excerpts {
            if !files.iter().any(|(n, _)| *n == e.file) {
                files.push((e.file.clone(), e.path.clone()));
            }
        }
        let chars: usize = excerpts.iter().map(|e| e.text.len().min(6000)).sum();
        let note = format!(
            "Sent {} page{}{} (about {}k tokens) to {}.",
            excerpts.len(),
            if excerpts.len() == 1 { "" } else { "s" },
            if excerpts.len() > own_pages { format!(", {} from other PDFs", excerpts.len() - own_pages) } else { String::new() },
            (chars / 4 / 1000).max(1),
            p.name()
        );

        // earlier turns without their excerpts, then this question with its excerpts
        let mut history: Vec<Msg> = chat
            .msgs
            .iter()
            .rev()
            .skip(1) // the question just added
            .filter(|m| !m.error && !m.text.is_empty())
            .take(8)
            .map(|m| Msg { role: m.role, text: m.text.chars().take(4000).collect() })
            .collect();
        history.reverse();
        history.push(Msg { role: Role::User, text: rag::user_prompt(&question, &excerpts) });

        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        chat.msgs.push(ChatMsg { role: Role::Assistant, text: String::new(), error: false, note: Some(note), files, cites: Vec::new() });
        chat.stream = Some(Stream { rx, cancel: cancel.clone() });
        let ctx = self.ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        std::thread::spawn(move || llm::stream_chat(p, key, model, rag::SYSTEM_PROMPT.to_string(), history, tx, cancel, wake));
    }

    /// Moves streamed text into the conversation. Called every frame.
    pub fn poll_chats(&mut self) {
        for chat in self.chats.values_mut() {
            let Some(s) = &chat.stream else { continue };
            let mut finished = false;
            loop {
                match s.rx.try_recv() {
                    Ok(ChatEvent::Delta(t)) => {
                        if let Some(m) = chat.msgs.last_mut() {
                            m.text.push_str(&t);
                        }
                    }
                    Ok(ChatEvent::Done) => {
                        finished = true;
                        break;
                    }
                    Ok(ChatEvent::Error(e)) => {
                        if let Some(m) = chat.msgs.last_mut() {
                            if m.text.is_empty() {
                                m.text = e;
                                m.error = true;
                            } else {
                                m.text.push_str(&format!("\n\n(The answer stopped early: {e})"));
                            }
                            m.note = None;
                        }
                        finished = true;
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(_) => {
                        finished = true;
                        break;
                    }
                }
            }
            if finished {
                chat.stream = None;
                if let Some(m) = chat.msgs.last_mut() {
                    if m.role == Role::Assistant && !m.error {
                        m.cites = rag::parse_citations(&m.text);
                    }
                }
            }
        }
        let got = self.ai.models_rx.as_ref().and_then(|(i, rx)| rx.try_recv().ok().map(|r| (*i, r)));
        if let Some((i, res)) = got {
            self.ai.models_rx = None;
            match res {
                Ok(list) => self.ai.models[i] = list,
                Err(e) => self.ai.models_err = Some(e),
            }
        }
        if let Some(rx) = &self.ai.test {
            if let Ok(res) = rx.try_recv() {
                self.ai.test = None;
                self.ai.test_msg = Some(match res {
                    Ok(m) => (m, true),
                    Err(e) => (e, false),
                });
            }
        }
    }

    pub fn chat_busy(&self) -> bool {
        self.ai.test.is_some() || self.ai.models_rx.is_some() || self.chats.values().any(|c| c.stream.is_some() || c.pending.is_some())
    }
}

// ---------------------------------------------------------------- rendering answers

/// A small markdown subset: headings, bullets, **bold**, `code`.
pub fn markdown(ui: &mut egui::Ui, text: &str) {
    for raw in text.lines() {
        let line = raw.trim_end();
        let t = line.trim_start();
        if t.is_empty() {
            ui.add_space(4.0);
        } else if let Some(h) = t.strip_prefix("### ").or_else(|| t.strip_prefix("## ")).or_else(|| t.strip_prefix("# ")) {
            ui.label(RichText::new(h.replace("**", "")).strong().size(15.0));
        } else if let Some(b) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("• ")) {
            ui.horizontal_top(|ui| {
                ui.add_space(6.0);
                ui.label("•");
                ui.add(egui::Label::new(inline_job(ui, b)).wrap());
            });
        } else {
            ui.add(egui::Label::new(inline_job(ui, t)).wrap());
        }
    }
}

fn inline_job(ui: &egui::Ui, s: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    let font = egui::FontId::proportional(14.0);
    let normal = TextFormat { font_id: font.clone(), color: ui.visuals().text_color(), ..Default::default() };
    let bold = TextFormat { font_id: font.clone(), color: ui.visuals().strong_text_color(), underline: egui::Stroke::NONE, ..Default::default() };
    let code = TextFormat { font_id: egui::FontId::monospace(13.0), color: ui.visuals().text_color(), background: Color32::from_white_alpha(18), ..Default::default() };
    let (mut is_bold, mut is_code) = (false, false);
    let mut buf = String::new();
    let flush = |job: &mut LayoutJob, buf: &mut String, bold_on: bool, code_on: bool| {
        if !buf.is_empty() {
            let f = if code_on { code.clone() } else if bold_on { bold.clone() } else { normal.clone() };
            job.append(buf, 0.0, f);
            buf.clear();
        }
    };
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if !is_code && chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            flush(&mut job, &mut buf, is_bold, is_code);
            is_bold = !is_bold;
            i += 2;
        } else if chars[i] == '`' {
            flush(&mut job, &mut buf, is_bold, is_code);
            is_code = !is_code;
            i += 1;
        } else {
            buf.push(chars[i]);
            i += 1;
        }
    }
    flush(&mut job, &mut buf, is_bold, is_code);
    job
}
