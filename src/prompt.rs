use crate::backend::{ChatTurn, TurnRole};
use crate::{InjectionContext, Persona};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Persona,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub text: String,
}

/// One line, capped so long retrieved turns cannot fill the context window.
fn flat(s: &str) -> String {
    const MAX: usize = 300;
    let one = s.replace('\n', " / ");
    if one.chars().count() <= MAX {
        return one;
    }
    let mut t: String = one.chars().take(MAX).collect();
    t.push('…');
    t
}

pub(crate) fn system_block(persona: &Persona, injection: &InjectionContext) -> String {
    let name = &persona.meta.name;
    let mut s = format!("{}\n", persona.system_prompt.trim());
    if !injection.style_samples.is_empty() {
        s.push_str(&format!("\n## Real messages by {name} (style reference)\n"));
        for m in &injection.style_samples {
            s.push_str(&format!("- {}\n", flat(m)));
        }
    }
    if !injection.exemplars.is_empty() {
        s.push_str("\n## Relevant real past exchanges\n");
        for e in &injection.exemplars {
            if !e.context.is_empty() {
                s.push_str(&format!("They: {}\n", flat(&e.context)));
            }
            s.push_str(&format!("{name}: {}\n\n", flat(&e.reply)));
        }
    }
    s
}

/// A system turn, then the conversation; `messages` should end with the user's newest message.
pub fn prepare_chat(persona: &Persona, messages: &[ChatMessage], injection: &InjectionContext) -> Vec<ChatTurn> {
    let mut turns = vec![ChatTurn { role: TurnRole::System, content: system_block(persona, injection) }];
    turns.extend(messages.iter().map(|m| ChatTurn {
        role: if m.role == Role::User { TurnRole::User } else { TurnRole::Assistant },
        content: m.text.clone(),
    }));
    turns
}

/// For completion-style backends; ends with the persona's turn left open.
pub fn prepare_prompt(persona: &Persona, messages: &[ChatMessage], injection: &InjectionContext) -> String {
    let name = &persona.meta.name;
    let mut s = system_block(persona, injection);
    s.push_str("\n## Conversation\n");
    for m in messages {
        let who = if m.role == Role::User { "User" } else { name };
        s.push_str(&format!("{who}: {}\n", m.text));
    }
    s.push_str(&format!("{name}:"));
    s
}
