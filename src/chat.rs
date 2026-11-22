//! One turn at a time: retrieval, prompt, model. The caller owns the history.

use crate::backend::{ChatBackend, GenParams};
use crate::retrieval::{get_injection_context_with, InjectionConfig, InjectionContext};
use crate::settings::{resolve_model, Settings};
use crate::{prepare_chat, ChatMessage, Persona, Result, Role};

pub struct Reply {
    pub text: String,
    pub injection: InjectionContext,
}

pub struct Engine<'a> {
    pub persona: &'a Persona,
    pub backend: &'a dyn ChatBackend,
    pub model: String,
    pub gen: GenParams,
    pub injection: InjectionConfig,
    /// Only this many recent messages are sent.
    pub max_history: usize,
}

impl<'a> Engine<'a> {
    pub fn new(persona: &'a Persona, backend: &'a dyn ChatBackend, settings: &Settings) -> Result<Self> {
        Ok(Self {
            persona,
            backend,
            model: resolve_model(backend, persona.meta.model.as_deref(), settings)?,
            gen: settings.gen.clone(),
            injection: InjectionConfig::default(),
            max_history: 20,
        })
    }

    /// `history` excludes `user_text`; append both it and the reply afterwards.
    pub fn reply(&self, history: &[ChatMessage], user_text: &str, on_token: &mut dyn FnMut(&str)) -> Result<Reply> {
        let history = &history[history.len().saturating_sub(self.max_history)..];
        let injection = get_injection_context_with(
            self.persona, self.persona.retriever(), user_text, history, &self.injection,
        );
        let mut msgs = history.to_vec();
        msgs.push(ChatMessage { role: Role::User, text: user_text.to_string() });
        let turns = prepare_chat(self.persona, &msgs, &injection);
        let text = self.backend.chat(&self.model, &turns, &self.gen, on_token)?;
        Ok(Reply { text, injection })
    }
}
