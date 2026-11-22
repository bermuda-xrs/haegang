//! Model backends: [`ChatBackend`] is the plug point, with Ollama and OpenAI-compatible servers provided.

use crate::http::{self, Response};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::BufRead;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRole {
    System,
    User,
    Assistant,
}

impl TurnRole {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnRole::System => "system",
            TurnRole::User => "user",
            TurnRole::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatTurn {
    pub role: TurnRole,
    pub content: String,
}

/// Defaults suit short chat replies from a small local model.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GenParams {
    pub temperature: f32,
    pub top_p: f32,
    pub max_tokens: i32,
    pub num_ctx: u32,
    pub repeat_penalty: f32,
    /// Let thinking models reason first; off keeps replies fast.
    pub think: bool,
    /// Constrains the reply where the server supports it (Ollama).
    #[serde(skip)]
    pub json_schema: Option<Value>,
}

impl Default for GenParams {
    fn default() -> Self {
        Self {
            temperature: 0.8,
            top_p: 0.95,
            max_tokens: 256,
            num_ctx: 4096,
            repeat_penalty: 1.1,
            think: false,
            json_schema: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub capabilities: Vec<String>,
}

impl ModelInfo {
    /// False for embedding-only models.
    pub fn can_chat(&self) -> bool {
        self.capabilities.is_empty() || self.capabilities.iter().any(|c| c == "completion")
    }
}

pub trait ChatBackend: Send + Sync {
    fn list_models(&self) -> Result<Vec<ModelInfo>>;
    /// Streams pieces through `on_token`; returns the full reply without any reasoning.
    fn chat(&self, model: &str, turns: &[ChatTurn], p: &GenParams, on_token: &mut dyn FnMut(&str)) -> Result<String>;
}

pub trait Embedder: Send + Sync {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
}

const CHAT_TIMEOUT: Duration = Duration::from_secs(300); // first call may load the model
const META_TIMEOUT: Duration = Duration::from_secs(10);

fn messages_json(turns: &[ChatTurn]) -> Vec<Value> {
    turns.iter().map(|t| json!({"role": t.role.as_str(), "content": t.content})).collect()
}

fn parse_line(line: &str) -> Result<Value> {
    serde_json::from_str(line).map_err(|e| Error::Backend(format!("bad stream line {line:?}: {e}")))
}

/// Drop `<think>` reasoning, including a dangling close tag.
pub(crate) fn strip_think(s: &str) -> String {
    let mut s = s;
    if let Some(i) = s.rfind("</think>") {
        s = &s[i + "</think>".len()..];
    }
    s.trim().to_string()
}

fn base_url(u: &str) -> String {
    u.trim_end_matches('/').to_string()
}

pub struct OllamaBackend {
    base: String,
}

impl OllamaBackend {
    pub fn new(base: &str) -> Self {
        Self { base: base_url(base) }
    }

    fn post(&self, path: &str, body: &Value, timeout: Duration) -> Result<Response> {
        http::send(&self.base, "POST", path, None, Some(body.to_string().as_bytes()), timeout)
    }

    fn chat_request(&self, model: &str, turns: &[ChatTurn], p: &GenParams, send_think: bool) -> Result<Response> {
        let mut body = json!({
            "model": model,
            "messages": messages_json(turns),
            "stream": true,
            "options": {
                "temperature": p.temperature, "top_p": p.top_p, "num_predict": p.max_tokens,
                "num_ctx": p.num_ctx, "repeat_penalty": p.repeat_penalty,
            },
        });
        if send_think {
            body["think"] = json!(p.think);
        }
        if let Some(schema) = &p.json_schema {
            body["format"] = schema.clone();
        }
        self.post("/api/chat", &body, CHAT_TIMEOUT)
    }
}

impl ChatBackend for OllamaBackend {
    fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let r = http::send(&self.base, "GET", "/api/tags", None, None, META_TIMEOUT)?.ok()?.text()?;
        let v: Value = serde_json::from_str(&r).map_err(|e| Error::Backend(e.to_string()))?;
        Ok(v["models"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| {
                        Some(ModelInfo {
                            name: m["name"].as_str()?.to_string(),
                            capabilities: m["capabilities"]
                                .as_array()
                                .map(|c| c.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn chat(&self, model: &str, turns: &[ChatTurn], p: &GenParams, on_token: &mut dyn FnMut(&str)) -> Result<String> {
        let resp = match self.chat_request(model, turns, p, true)?.ok() {
            // Models without thinking support reject the `think` field: retry without it.
            Err(Error::Http(400, b)) if b.contains("think") => self.chat_request(model, turns, p, false)?.ok()?,
            r => r?,
        };
        let mut out = String::new();
        for line in resp.body.lines() {
            let line = line.map_err(|e| Error::Io(e.to_string()))?;
            if line.trim().is_empty() {
                continue;
            }
            let v = parse_line(&line)?;
            if let Some(e) = v["error"].as_str() {
                return Err(Error::Backend(e.to_string()));
            }
            if let Some(t) = v["message"]["content"].as_str().filter(|t| !t.is_empty()) {
                out.push_str(t);
                on_token(t);
            }
            if v["done"].as_bool() == Some(true) {
                break;
            }
        }
        Ok(strip_think(&out))
    }
}

pub struct OllamaEmbedder {
    base: String,
    model: String,
}

impl OllamaEmbedder {
    pub fn new(base: &str, model: &str) -> Self {
        Self { base: base_url(base), model: model.to_string() }
    }
}

impl Embedder for OllamaEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let body = json!({"model": self.model, "input": texts});
        let r = http::send(&self.base, "POST", "/api/embed", None, Some(body.to_string().as_bytes()), CHAT_TIMEOUT)?
            .ok()?
            .text()?;
        let v: Value = serde_json::from_str(&r).map_err(|e| Error::Backend(e.to_string()))?;
        let out: Vec<Vec<f32>> = v["embeddings"]
            .as_array()
            .ok_or_else(|| Error::Backend("no `embeddings` in reply".into()))?
            .iter()
            .map(|e| e.as_array().map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect()).unwrap_or_default())
            .collect();
        if out.len() != texts.len() {
            return Err(Error::Backend(format!("asked {} embeddings, got {}", texts.len(), out.len())));
        }
        Ok(out)
    }
}

/// llama.cpp, vLLM, LM Studio, Ollama's `/v1`: anything OpenAI-compatible over plain http.
pub struct OpenAiBackend {
    base: String,
    api_key: Option<String>,
}

impl OpenAiBackend {
    pub fn new(base: &str, api_key: Option<String>) -> Self {
        Self { base: base_url(base), api_key }
    }
}

impl ChatBackend for OpenAiBackend {
    fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let r = http::send(&self.base, "GET", "/models", self.api_key.as_deref(), None, META_TIMEOUT)?.ok()?.text()?;
        let v: Value = serde_json::from_str(&r).map_err(|e| Error::Backend(e.to_string()))?;
        Ok(v["data"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["id"].as_str())
                    .map(|id| ModelInfo { name: id.to_string(), capabilities: vec![] })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn chat(&self, model: &str, turns: &[ChatTurn], p: &GenParams, on_token: &mut dyn FnMut(&str)) -> Result<String> {
        let body = json!({
            "model": model, "messages": messages_json(turns), "stream": true,
            "temperature": p.temperature, "top_p": p.top_p, "max_tokens": p.max_tokens,
        });
        let resp = http::send(
            &self.base, "POST", "/chat/completions", self.api_key.as_deref(),
            Some(body.to_string().as_bytes()), CHAT_TIMEOUT,
        )?
        .ok()?;
        let mut out = String::new();
        for line in resp.body.lines() {
            let line = line.map_err(|e| Error::Io(e.to_string()))?;
            let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
            if data == "[DONE]" {
                break;
            }
            let v = parse_line(data)?;
            if let Some(e) = v["error"]["message"].as_str() {
                return Err(Error::Backend(e.to_string()));
            }
            if let Some(t) = v["choices"][0]["delta"]["content"].as_str().filter(|t| !t.is_empty()) {
                out.push_str(t);
                on_token(t);
            }
        }
        Ok(strip_think(&out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_stripping() {
        assert_eq!(strip_think("<think>hmm</think>\n\nhello"), "hello");
        assert_eq!(strip_think("reasoning</think> hi"), "hi");
        assert_eq!(strip_think("  plain "), "plain");
    }
}
