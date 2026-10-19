//! Talking to OpenRouter and Anthropic: streamed chat completions and API-key storage.
//!
//! Keys are kept in Windows Credential Manager (service "LeanPDF"), never in a file. For
//! development a key can also come from LEANPDF_OPENROUTER_KEY / LEANPDF_ANTHROPIC_KEY, and
//! LEANPDF_AI_BASE can point both providers at a test server.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    OpenRouter,
    Anthropic,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::OpenRouter, Provider::Anthropic];

    pub fn id(self) -> &'static str {
        match self {
            Provider::OpenRouter => "openrouter",
            Provider::Anthropic => "anthropic",
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Provider::OpenRouter => "OpenRouter",
            Provider::Anthropic => "Anthropic",
        }
    }
    pub fn from_id(s: &str) -> Provider {
        if s == "anthropic" { Provider::Anthropic } else { Provider::OpenRouter }
    }
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::OpenRouter => "anthropic/claude-haiku-5.5",
            Provider::Anthropic => "claude-haiku-5-5",
        }
    }
    pub fn keys_url(self) -> &'static str {
        match self {
            Provider::OpenRouter => "https://openrouter.ai/keys",
            Provider::Anthropic => "https://console.anthropic.com/settings/keys",
        }
    }
    fn endpoint(self) -> String {
        if let Ok(base) = std::env::var("LEANPDF_AI_BASE") {
            return format!("{}/{}", base.trim_end_matches('/'), self.id());
        }
        match self {
            Provider::OpenRouter => "https://openrouter.ai/api/v1/chat/completions".into(),
            Provider::Anthropic => "https://api.anthropic.com/v1/messages".into(),
        }
    }
    fn env_key(self) -> &'static str {
        match self {
            Provider::OpenRouter => "LEANPDF_OPENROUTER_KEY",
            Provider::Anthropic => "LEANPDF_ANTHROPIC_KEY",
        }
    }
}

// ---------------------------------------------------------------- key storage

fn entry(p: Provider) -> Result<keyring::Entry, String> {
    keyring::Entry::new("LeanPDF", p.id()).map_err(|e| e.to_string())
}

pub fn get_key(p: Provider) -> Option<String> {
    if let Ok(k) = std::env::var(p.env_key()) {
        if !k.trim().is_empty() {
            return Some(k.trim().to_string());
        }
    }
    entry(p).ok()?.get_password().ok().filter(|k| !k.trim().is_empty())
}

/// Where the key in use comes from, for the settings dialog.
pub fn key_source(p: Provider) -> Option<&'static str> {
    if std::env::var(p.env_key()).map(|k| !k.trim().is_empty()).unwrap_or(false) {
        return Some("an environment variable");
    }
    entry(p).ok()?.get_password().ok().filter(|k| !k.trim().is_empty()).map(|_| "Windows Credential Manager")
}

pub fn set_key(p: Provider, key: &str) -> Result<(), String> {
    entry(p)?.set_password(key.trim()).map_err(|e| e.to_string())
}

pub fn delete_key(p: Provider) -> Result<(), String> {
    match entry(p)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

// ---------------------------------------------------------------- chat

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub role: Role,
    pub text: String,
}

pub enum ChatEvent {
    Delta(String),
    Done,
    Error(String),
}

pub fn agent() -> ureq::Agent {
    // Windows' own certificate store (Schannel). The default, a bundled Mozilla list, fails for some
    // certificate chains ("unable to find any user-specified roots in the final cert chain").
    let tls = ureq::tls::TlsConfig::builder()
        .provider(ureq::tls::TlsProvider::NativeTls)
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(90)))
        .timeout_recv_body(Some(Duration::from_secs(180)))
        .user_agent("LeanPDF")
        .build()
        .into()
}

fn request_body(p: Provider, model: &str, system: &str, history: &[Msg], max_tokens: u32, stream: bool) -> Value {
    let turns: Vec<Value> = history
        .iter()
        .map(|m| json!({ "role": if m.role == Role::User { "user" } else { "assistant" }, "content": m.text }))
        .collect();
    match p {
        Provider::Anthropic => json!({
            "model": model,
            "max_tokens": max_tokens,
            "system": system,
            "messages": turns,
            "stream": stream,
        }),
        Provider::OpenRouter => {
            let mut messages = vec![json!({ "role": "system", "content": system })];
            messages.extend(turns);
            json!({ "model": model, "max_tokens": max_tokens, "messages": messages, "stream": stream })
        }
    }
}

/// A readable message for a failed HTTP status.
fn status_message(p: Provider, status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["message"].as_str().map(str::to_string).or_else(|| v["error"].as_str().map(str::to_string))
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    let hint = match status {
        401 | 403 => format!("{} rejected the API key. Open AI settings and check it.", p.name()),
        402 => format!("{} says there are not enough credits.", p.name()),
        404 => "The model was not found. Check the model name in AI settings.".to_string(),
        429 => format!("{} is rate limiting requests. Wait a moment and try again.", p.name()),
        500..=599 => format!("{} had a server problem. Try again in a moment.", p.name()),
        _ => format!("{} returned an error.", p.name()),
    };
    if detail.is_empty() { format!("{hint} (HTTP {status})") } else { format!("{hint} (HTTP {status}: {detail})") }
}

fn send(p: Provider, key: &str, body: &Value) -> Result<ureq::http::Response<ureq::Body>, String> {
    let url = p.endpoint();
    let mut req = agent().post(&url).header("content-type", "application/json");
    match p {
        Provider::Anthropic => {
            req = req.header("x-api-key", key).header("anthropic-version", "2023-06-01");
        }
        Provider::OpenRouter => {
            req = req.header("authorization", &format!("Bearer {key}")).header("x-title", "LeanPDF");
        }
    }
    req.send(body.to_string()).map_err(|e| format!("Could not reach {}: {e}", p.name()))
}

/// Streams the model's answer into `tx`. Runs on its own thread; `cancel` stops it.
pub fn stream_chat(
    p: Provider,
    key: String,
    model: String,
    system: String,
    history: Vec<Msg>,
    tx: Sender<ChatEvent>,
    cancel: Arc<AtomicBool>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let result = (|| -> Result<(), String> {
        let body = request_body(p, &model, &system, &history, 2048, true);
        let mut resp = send(p, &key, &body)?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            return Err(status_message(p, status, &text));
        }
        let reader = BufReader::new(resp.body_mut().as_reader());
        for line in reader.lines() {
            if cancel.load(Ordering::Relaxed) {
                return Ok(());
            }
            let line = line.map_err(|e| format!("The connection dropped: {e}"))?;
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data.is_empty() {
                continue;
            }
            if data == "[DONE]" {
                break;
            }
            let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
            if let Some(err) = v.get("error") {
                let msg = err["message"].as_str().or_else(|| err.as_str()).unwrap_or("the provider reported an error");
                return Err(format!("{}: {msg}", p.name()));
            }
            let piece = match p {
                Provider::OpenRouter => v["choices"][0]["delta"]["content"].as_str().map(str::to_string),
                Provider::Anthropic => match v["type"].as_str() {
                    Some("content_block_delta") => v["delta"]["text"].as_str().map(str::to_string),
                    Some("message_stop") => break,
                    Some("error") => {
                        return Err(format!("{}: {}", p.name(), v["error"]["message"].as_str().unwrap_or("error")));
                    }
                    _ => None,
                },
            };
            if let Some(t) = piece.filter(|t| !t.is_empty()) {
                if tx.send(ChatEvent::Delta(t)).is_err() {
                    return Ok(());
                }
                wake();
            }
        }
        Ok(())
    })();
    let _ = tx.send(match result {
        Ok(()) => ChatEvent::Done,
        Err(e) => ChatEvent::Error(e),
    });
    wake();
}

/// A tiny non-streaming request to check that the key and model work.
pub fn test_connection(p: Provider, key: &str, model: &str) -> Result<String, String> {
    let body = request_body(p, model, "Reply with the single word OK.", &[Msg { role: Role::User, text: "Hello".into() }], 8, false);
    let mut resp = send(p, key, &body)?;
    let status = resp.status().as_u16();
    let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
    if !(200..300).contains(&status) {
        return Err(status_message(p, status, &text));
    }
    Ok(format!("Connected to {} using {}.", p.name(), model))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    /// What the dropdown shows.
    pub label: String,
}

fn models_url(p: Provider) -> String {
    if let Ok(base) = std::env::var("LEANPDF_AI_BASE") {
        return format!("{}/{}/models", base.trim_end_matches('/'), p.id());
    }
    match p {
        Provider::OpenRouter => "https://openrouter.ai/api/v1/models".into(),
        Provider::Anthropic => "https://api.anthropic.com/v1/models".into(),
    }
}

/// The models the provider offers. OpenRouter's list is public; Anthropic's needs the key.
pub fn list_models(p: Provider, key: Option<&str>) -> Result<Vec<ModelInfo>, String> {
    let mut out: Vec<ModelInfo> = Vec::new();
    match p {
        Provider::OpenRouter => {
            let mut resp = agent().get(&models_url(p)).call().map_err(|e| format!("Could not reach OpenRouter: {e}"))?;
            let status = resp.status().as_u16();
            let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
            if !(200..300).contains(&status) {
                return Err(status_message(p, status, &text));
            }
            out = parse_openrouter_models(&text)?;
        }
        Provider::Anthropic => {
            let key = key.ok_or("Add your Anthropic key first; Anthropic only lists models for a valid key.")?;
            let mut after: Option<String> = None;
            for _ in 0..6 {
                let mut url = format!("{}?limit=100", models_url(p));
                if let Some(a) = &after {
                    url.push_str(&format!("&after_id={a}"));
                }
                let mut resp = agent()
                    .get(&url)
                    .header("x-api-key", key)
                    .header("anthropic-version", "2023-06-01")
                    .call()
                    .map_err(|e| format!("Could not reach Anthropic: {e}"))?;
                let status = resp.status().as_u16();
                let text = resp.body_mut().read_to_string().map_err(|e| e.to_string())?;
                if !(200..300).contains(&status) {
                    return Err(status_message(p, status, &text));
                }
                let (mut page, more, last) = parse_anthropic_models(&text)?;
                out.append(&mut page);
                if more && last.is_some() {
                    after = last;
                } else {
                    break;
                }
            }
        }
    }
    if out.is_empty() {
        return Err("The provider returned no models.".into());
    }
    Ok(out)
}

pub fn parse_openrouter_models(json: &str) -> Result<Vec<ModelInfo>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("Could not read the model list: {e}"))?;
    let mut out: Vec<ModelInfo> = v["data"]
        .as_array()
        .ok_or("Unexpected model list.")?
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            if id.ends_with(":batch") || id.starts_with('~') {
                return None;
            }
            let name = m["name"].as_str().unwrap_or(&id);
            let price = |k: &str| m["pricing"][k].as_str().and_then(|s| s.parse::<f64>().ok()).map(|x| x * 1e6);
            let label = match (price("prompt"), price("completion")) {
                (Some(i), Some(o)) if i == 0.0 && o == 0.0 => format!("{name}  ·  free"),
                (Some(i), Some(o)) => format!("{name}  ·  ${i:.2} / ${o:.2} per M tokens"),
                _ => name.to_string(),
            };
            Some(ModelInfo { id, label })
        })
        .collect();
    // Anthropic models first (they are what the prompts are written for), then alphabetical.
    out.sort_by(|a, b| {
        let (aa, ba) = (a.id.starts_with("anthropic/"), b.id.starts_with("anthropic/"));
        ba.cmp(&aa).then_with(|| a.id.cmp(&b.id))
    });
    Ok(out)
}

/// (models, has_more, last_id)
pub fn parse_anthropic_models(json: &str) -> Result<(Vec<ModelInfo>, bool, Option<String>), String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("Could not read the model list: {e}"))?;
    let models = v["data"]
        .as_array()
        .ok_or("Unexpected model list.")?
        .iter()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let name = m["display_name"].as_str().unwrap_or(&id);
            Some(ModelInfo { label: format!("{name}  ·  {id}"), id })
        })
        .collect();
    Ok((models, v["has_more"].as_bool().unwrap_or(false), v["last_id"].as_str().map(str::to_string)))
}

#[cfg(test)]
mod tests {
    /// Real HTTPS to the real services (no key needed); run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_https_connections_work() {
        let models = super::list_models(super::Provider::OpenRouter, None).expect("OpenRouter model list over HTTPS");
        assert!(models.iter().any(|m| m.id.starts_with("anthropic/")), "{} models", models.len());
        // Anthropic answers a bogus key with 401, which proves the TLS handshake and the request worked.
        let e = super::list_models(super::Provider::Anthropic, Some("sk-ant-not-a-real-key")).unwrap_err();
        assert!(e.contains("rejected the API key") || e.contains("401"), "{e}");
        let e = super::test_connection(super::Provider::OpenRouter, "sk-or-not-a-real-key", "anthropic/claude-haiku-5.5").unwrap_err();
        assert!(e.contains("HTTP 401") || e.contains("rejected"), "{e}");
    }

    #[test]
    fn model_lists_are_parsed() {
        let or = r#"{"data":[{"id":"openai/gpt-x","name":"GPT X","pricing":{"prompt":"0.000001","completion":"0.000002"}},
            {"id":"anthropic/claude-haiku-5.5","name":"Claude Haiku 5.5","pricing":{"prompt":"0.0000001","completion":"0.0000005"}},
            {"id":"anthropic/claude-haiku-5.5:batch","name":"b","pricing":{}}]}"#;
        let m = super::parse_openrouter_models(or).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].id, "anthropic/claude-haiku-5.5"); // Anthropic first, :batch dropped
        assert!(m[0].label.contains("$0.10") && m[0].label.contains("$0.50"));
        let (a, more, last) = super::parse_anthropic_models(r#"{"data":[{"id":"claude-haiku-5-5","display_name":"Claude Haiku 5.5"}],"has_more":true,"last_id":"claude-haiku-5-5"}"#).unwrap();
        assert_eq!(a[0].id, "claude-haiku-5-5");
        assert!(more && last.as_deref() == Some("claude-haiku-5-5"));
    }
}

#[cfg(test)]
mod tests_old {
    use super::*;

    #[test]
    fn bodies_follow_each_providers_shape() {
        let h = [Msg { role: Role::User, text: "hi".into() }];
        let a = request_body(Provider::Anthropic, "m", "sys", &h, 10, true);
        assert_eq!(a["system"], "sys");
        assert_eq!(a["messages"][0]["role"], "user");
        let o = request_body(Provider::OpenRouter, "m", "sys", &h, 10, true);
        assert_eq!(o["messages"][0]["role"], "system");
        assert_eq!(o["messages"][1]["content"], "hi");
    }

    /// Touches the real Windows Credential Manager (under a test-only service name); run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn keys_round_trip_through_credential_manager() {
        let e = keyring::Entry::new("LeanPDF-test", "probe").unwrap();
        e.set_password("dummy-secret").unwrap();
        assert_eq!(e.get_password().unwrap(), "dummy-secret");
        e.delete_credential().unwrap();
        assert!(matches!(e.get_password(), Err(keyring::Error::NoEntry)));
    }

    #[test]
    fn errors_are_readable() {
        let m = status_message(Provider::OpenRouter, 401, r#"{"error":{"message":"No auth"}}"#);
        assert!(m.contains("rejected the API key") && m.contains("No auth"));
        assert!(status_message(Provider::Anthropic, 429, "").contains("rate limiting"));
    }
}
