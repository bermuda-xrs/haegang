//! Settings (`<root>/settings.json`) and model selection.

use crate::backend::{ChatBackend, Embedder, GenParams, OllamaBackend, OllamaEmbedder, OpenAiBackend};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendKind {
    Ollama,
    OpenAi,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub backend: BackendKind,
    /// Include the version prefix for OpenAI-compatible servers, e.g. `http://127.0.0.1:8080/v1`.
    pub base_url: String,
    /// Stored in plain text.
    pub api_key: Option<String>,
    pub model: String,
    /// `None` turns semantic retrieval off.
    pub embed_model: Option<String>,
    pub gen: GenParams,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            backend: BackendKind::Ollama,
            base_url: "http://127.0.0.1:11434".into(),
            api_key: None,
            model: "huihui_ai/qwen3.5-abliterated:4b".into(),
            embed_model: Some("nomic-embed-text".into()),
            gen: GenParams::default(),
        }
    }
}

impl Settings {
    pub fn backend(&self) -> Box<dyn ChatBackend> {
        match self.backend {
            BackendKind::Ollama => Box::new(OllamaBackend::new(&self.base_url)),
            BackendKind::OpenAi => Box::new(OpenAiBackend::new(&self.base_url, self.api_key.clone())),
        }
    }

    pub fn embedder(&self) -> Option<Arc<dyn Embedder>> {
        match (self.backend, &self.embed_model) {
            (BackendKind::Ollama, Some(m)) => Some(Arc::new(OllamaEmbedder::new(&self.base_url, m))),
            _ => None,
        }
    }
}

/// The persona's model, else the default, checked against what the server has installed.
pub fn resolve_model(backend: &dyn ChatBackend, persona_model: Option<&str>, settings: &Settings) -> Result<String> {
    let wanted = persona_model.unwrap_or(&settings.model);
    let models = backend.list_models()?;
    let found = models
        .iter()
        .find(|m| m.name == wanted || m.name == format!("{wanted}:latest"))
        .filter(|m| m.can_chat());
    match found {
        Some(m) => Ok(m.name.clone()),
        None => Err(Error::NotFound(format!(
            "model `{wanted}`; available chat models: {:?}",
            models.iter().filter(|m| m.can_chat()).map(|m| &m.name).collect::<Vec<_>>()
        ))),
    }
}
