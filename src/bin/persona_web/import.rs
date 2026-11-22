//! The import session: start -> file* -> identify (again with a correction, as often as needed) -> create.
//! Uploads live in memory until read; a session ends when its personas exist or the user cancels.

use crate::httpd::{send_error, Request, Stream};
use crate::{body_json, body_str, lock, now_ms, App};
use persona_core::{
    identify as run_identify, ingest_files, resolve_model, Corpus, Error, Identification, IdentifyOptions, ModelAccess,
    ParseOptions, Result,
};
use serde_json::{json, Value};
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

/// Abandoned sessions hold whole uploads in memory, so only the latest few are kept.
const MAX_SESSIONS: usize = 4;

#[derive(Default)]
pub struct Session {
    files: Vec<(String, Vec<u8>)>,
    corpus: Option<Corpus>,
    identification: Option<Identification>,
}

fn session(app: &App, token: &str) -> Result<Arc<Mutex<Session>>> {
    lock(&app.imports).get(token).cloned().ok_or_else(|| Error::NotFound("this import has ended; start again".into()))
}

pub fn start(app: &App) -> Value {
    let token = format!("{:x}{:x}", now_ms(), app.counter.fetch_add(1, Ordering::Relaxed));
    let mut imports = lock(&app.imports);
    while imports.len() >= MAX_SESSIONS {
        // tokens start with the time; a session may be locked for minutes, so it must not be asked
        let oldest = imports.keys().min().cloned();
        imports.remove(&oldest.expect("sessions exist"));
    }
    imports.insert(token.clone(), Default::default());
    json!({ "token": token })
}

pub fn add_file(app: &App, req: &mut Request) -> Result<Value> {
    let session = session(app, req.query("token").unwrap_or(""))?;
    let name = req.query("name").unwrap_or("file").to_string();
    lock(&session).files.push((name, std::mem::take(&mut req.body)));
    Ok(json!({ "ok": true }))
}

pub fn cancel(app: &App, token: &str) -> Value {
    lock(&app.imports).remove(token);
    json!({ "ok": true })
}

/// `POST /api/import/identify {"token", "correction"?}` -> NDJSON stream: `{"stage", "done", "total"}`*
/// then `{"result": ..}` or `{"error": ..}`. A failed correction leaves the previous result in place.
pub fn identify(app: &App, req: &Request, stream: &TcpStream) -> std::io::Result<()> {
    let opened = (|| -> Result<_> {
        let body = body_json(req)?;
        let correction = body["correction"].as_str().map(str::trim).filter(|c| !c.is_empty()).map(String::from);
        Ok((session(app, body_str(&body, "token")?)?, correction))
    })();
    let (session, correction) = match opened {
        Ok(x) => x,
        Err(e) => return send_error(stream, &e),
    };
    let mut session = lock(&session);
    let mut out = Stream::start(stream)?;
    match run(app, &mut session, correction, &mut out) {
        Ok(result) => out.line(&json!({ "result": result })),
        Err(e) => out.line(&json!({ "error": e.to_string() })),
    }
}

fn run(app: &App, session: &mut Session, correction: Option<String>, out: &mut Stream) -> Result<Value> {
    if session.corpus.is_none() {
        let _ = out.line(&json!({ "stage": "Reading files" }));
        let files = std::mem::take(&mut session.files);
        session.corpus = Some(ingest_files(&files, &ParseOptions::default())?);
    }
    let settings = app.store.settings()?;
    let backend = settings.backend();
    let model = resolve_model(backend.as_ref(), None, &settings).ok();
    let corpus = session.corpus.as_ref().expect("read above");
    let options = IdentifyOptions {
        model: model.map(|model| ModelAccess { backend: backend.as_ref(), model, gen: settings.gen.clone() }),
        correction: correction.as_deref(),
        previous: session.identification.as_ref(),
    };
    let found = run_identify(corpus, &options, &mut |p| {
        out.line(&json!({ "stage": p.stage, "done": p.done, "total": p.total })).is_ok()
    })?;
    let result = describe(corpus, &found);
    session.identification = Some(found);
    Ok(result)
}

fn describe(corpus: &Corpus, id: &Identification) -> Value {
    let characters: Vec<Value> = id
        .characters
        .iter()
        .map(|c| {
            json!({
                "name": c.name, "aliases": c.aliases, "description": c.description, "voice": c.voice,
                "lines": c.lines, "certain": c.certain, "samples": c.samples,
            })
        })
        .collect();
    json!({
        "documents": corpus.documents.iter().map(|d| json!({ "name": d.name, "format": d.format })).collect::<Vec<_>>(),
        "unreadable": corpus.unreadable.iter().map(|u| json!({ "name": u.name, "reason": u.reason })).collect::<Vec<_>>(),
        "characters": characters,
        "mentioned": id.mentioned,
        "unattributed": id.unattributed,
        "notes": id.notes,
    })
}

/// `POST /api/import/create {"token", "characters": [names]}`: one persona per checked character.
pub fn create(app: &Arc<App>, req: &Request) -> Result<Value> {
    let body = body_json(req)?;
    let token = body_str(&body, "token")?;
    let picked: Vec<&str> = body["characters"].as_array().map_or(Vec::new(), |a| a.iter().filter_map(Value::as_str).collect());
    if picked.is_empty() {
        return Err(Error::Invalid("select at least one character".into()));
    }
    let session = session(app, token)?;
    let session = lock(&session);
    let id = session.identification.as_ref().ok_or_else(|| Error::NotFound("identification".into()))?;

    let (mut created, mut failed) = (Vec::new(), Vec::new());
    for name in picked {
        let persona = id
            .characters
            .iter()
            .find(|c| c.name == name)
            .ok_or_else(|| Error::NotFound(name.to_string()))
            .and_then(|c| app.store.create_persona(&app.free_name(&c.name), id, c));
        match persona {
            Ok(p) => created.push(p.meta.name),
            Err(e) => failed.push(json!({ "name": name, "error": e.to_string() })),
        }
    }
    drop(session);
    if failed.is_empty() {
        lock(&app.imports).remove(token);
    }
    for name in &created {
        app.start_embeddings(name.clone());
    }
    Ok(json!({ "created": created, "failed": failed }))
}
