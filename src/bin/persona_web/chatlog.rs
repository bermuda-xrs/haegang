//! Chat history (`<persona>/chat.json`) and the streaming reply endpoint.

use crate::httpd::{send_error, Request, Stream};
use crate::{body_json, body_str, lock, now_ms, App};
use persona_core::{ChatMessage, Engine, Error, Result, Role};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::TcpStream;

#[derive(Serialize, Deserialize, Clone)]
pub struct Line {
    pub role: String, // "user" | "persona"
    pub text: String,
    pub ts: i64,
}

pub fn load(app: &App, name: &str) -> Result<Vec<Line>> {
    let p = app.store.path(name)?.join("chat.json");
    if !p.exists() {
        return Ok(Vec::new());
    }
    let b = std::fs::read(&p).map_err(|e| Error::Io(e.to_string()))?;
    serde_json::from_slice(&b).map_err(|e| Error::Json(e.to_string()))
}

pub fn save(app: &App, name: &str, chat: &[Line]) -> Result<()> {
    let p = app.store.path(name)?.join("chat.json");
    let tmp = p.with_extension("json.tmp");
    let bytes = serde_json::to_vec(chat).map_err(|e| Error::Json(e.to_string()))?;
    std::fs::write(&tmp, bytes).map_err(|e| Error::Io(e.to_string()))?;
    std::fs::rename(&tmp, &p).map_err(|e| Error::Io(e.to_string()))
}

fn append(app: &App, name: &str, line: Line) -> Result<()> {
    let _g = lock(&app.chat_lock);
    let mut chat = load(app, name)?;
    chat.push(line);
    save(app, name, &chat)
}

/// `POST /api/personas/{name}/chat {"text"}` -> NDJSON stream: `{"t": token}`* then
/// `{"done": true, "text", "ts"}` or `{"error"}`. Errors before generation starts are plain JSON.
pub fn reply(app: &App, name: &str, req: &Request, stream: &TcpStream) -> std::io::Result<()> {
    let prepared = (|| -> Result<_> {
        let text = body_str(&body_json(req)?, "text")?.trim().to_string();
        if text.is_empty() {
            return Err(Error::Invalid("empty message".into()));
        }
        let history: Vec<ChatMessage> = load(app, name)?
            .into_iter()
            .map(|l| ChatMessage { role: if l.role == "user" { Role::User } else { Role::Persona }, text: l.text })
            .collect();
        Ok((text, history, app.persona(name)?, app.store.settings()?))
    })();
    let (text, history, persona, settings) = match prepared {
        Ok(x) => x,
        Err(e) => return send_error(stream, &e),
    };
    let backend = settings.backend();
    let engine = match Engine::new(&persona, backend.as_ref(), &settings) {
        Ok(e) => e,
        Err(e) => return send_error(stream, &e),
    };
    // saved first so it survives a failed reply
    if let Err(e) = append(app, name, Line { role: "user".into(), text: text.clone(), ts: now_ms() }) {
        return send_error(stream, &e);
    }

    let mut out = Stream::start(stream)?;
    let result = engine.reply(&history, &text, &mut |t| {
        let _ = out.line(&json!({ "t": t })); // a closed tab must not abort (and lose) the reply
    });
    match result {
        Ok(r) if !r.text.is_empty() => {
            let ts = now_ms();
            if let Err(e) = append(app, name, Line { role: "persona".into(), text: r.text.clone(), ts }) {
                return out.line(&json!({ "error": e.to_string() }));
            }
            out.line(&json!({ "done": true, "text": r.text, "ts": ts }))
        }
        Ok(_) => out.line(&json!({ "error": "the model returned an empty reply" })),
        Err(e) => out.line(&json!({ "error": e.to_string() })),
    }
}
