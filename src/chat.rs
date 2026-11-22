//! One turn at a time: retrieval, prompt, model. The caller owns the history.

use crate::backend::{ChatBackend, GenParams};
use crate::retrieval::{get_injection_context_with, InjectionConfig, InjectionContext};
use crate::settings::{resolve_model, Settings};
use crate::prompt::system_block;
use crate::{prepare_chat, ChatMessage, Persona, Result, Role};

/// An estimate, on the high side: Korean measures 0.56 tokens per character, English about 0.25.
fn tokens(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 3 } else { 7 }).sum::<usize>() / 10 + 4
}

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
        let mut injection = get_injection_context_with(
            self.persona, self.persona.retriever(), user_text, history, &self.injection,
        );
        // what does not fit the context window the server would cut off, the persona itself included
        let reserved = self.gen.max_tokens.max(0) as usize + tokens(user_text);
        let budget = (self.gen.num_ctx as usize).saturating_sub(reserved);
        let mut system = tokens(&system_block(self.persona, &injection));
        while system > budget * 3 / 4 {
            let exemplars_lead = injection.exemplars.len() >= injection.style_samples.len();
            let dropped = match exemplars_lead {
                true => injection.exemplars.pop().is_some(),
                false => injection.style_samples.pop().is_some(),
            };
            if !dropped {
                break;
            }
            system = tokens(&system_block(self.persona, &injection));
        }
        let mut left = budget.saturating_sub(system);
        let fitting = history
            .iter()
            .rev()
            .take_while(|m| {
                let rest = left.checked_sub(tokens(&m.text));
                left = rest.unwrap_or(0);
                rest.is_some()
            })
            .count();
        let mut msgs = history[history.len() - fitting..].to_vec();
        msgs.push(ChatMessage { role: Role::User, text: user_text.to_string() });
        let turns = prepare_chat(self.persona, &msgs, &injection);
        let text = self.backend.chat(&self.model, &turns, &self.gen, on_token)?;
        Ok(Reply { text, injection })
    }
}
