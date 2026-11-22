//! Local web UI: `cargo run --release --bin persona_web`, then open http://127.0.0.1:7860
//! Env: PORT (default 7860), PERSONA_DIR (default ./personas). Binds to 127.0.0.1 only.

mod avatar;
mod chatlog;
mod httpd;
mod import;

use httpd::{percent_decode, read_request, send, send_error, send_json, Request};
use persona_core::{Error, Persona, PersonaStore, Result, Settings};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const INDEX: &str = include_str!("../../../web/index.html");
const STYLE: &str = include_str!("../../../web/style.css");
const SCRIPT: &str = include_str!("../../../web/app.js");

pub struct App {
    pub store: PersonaStore,
    root: PathBuf,
    hosts: [String; 2],
    cache: Mutex<HashMap<PathBuf, Arc<Persona>>>,
    imports: Mutex<HashMap<String, Arc<Mutex<import::Session>>>>,
    building: Mutex<HashSet<String>>,
    pub chat_lock: Mutex<()>,
    pub counter: AtomicU64,
}

fn main() {
    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(7860);
    let root = std::env::var("PERSONA_DIR").unwrap_or_else(|_| "personas".into());
    let app = Arc::new(App {
        store: PersonaStore::new(&root),
        root: PathBuf::from(&root),
        hosts: [format!("127.0.0.1:{port}"), format!("localhost:{port}")],
        cache: Default::default(),
        imports: Default::default(),
        building: Default::default(),
        chat_lock: Default::default(),
        counter: AtomicU64::new(0),
    });
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap_or_else(|e| {
        eprintln!("cannot bind 127.0.0.1:{port}: {e}");
        std::process::exit(1);
    });
    println!("persona_web: http://127.0.0.1:{port}  (personas in {root})");
    for conn in listener.incoming().flatten() {
        let app = app.clone();
        std::thread::spawn(move || {
            conn.set_read_timeout(Some(Duration::from_secs(120))).ok();
            match read_request(&conn) {
                Ok(Some(mut req)) => {
                    let _ = handle(&app, &mut req, &conn);
                }
                Ok(None) => {}
                Err(e) => {
                    let _ = send_json(&conn, 400, &json!({ "error": e.to_string() }));
                }
            }
        });
    }
}

/// A poisoned lock only means another request panicked; the data is still fine.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn body_json(req: &Request) -> Result<Value> {
    serde_json::from_slice(&req.body).map_err(|e| Error::Json(e.to_string()))
}

pub fn body_str<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().ok_or_else(|| Error::Invalid(format!("missing `{k}`")))
}

fn ok() -> Result<Value> {
    Ok(json!({ "ok": true }))
}

impl App {
    /// Rejects DNS rebinding (foreign Host) and cross-site writes (foreign Origin).
    fn request_allowed(&self, req: &Request) -> bool {
        self.hosts.contains(&req.host)
            && (req.method == "GET" || req.origin.as_deref().is_none_or(|o| o == format!("http://{}", req.host)))
    }

    /// Cached persona; semantic retrieval is attached only if its index already exists.
    pub fn persona(&self, name: &str) -> Result<Arc<Persona>> {
        let key = self.store.path(name)?;
        if let Some(p) = lock(&self.cache).get(&key) {
            return Ok(p.clone());
        }
        let mut p = self.store.load_persona(name)?;
        let s = self.store.settings()?;
        if let (Some(e), Some(m)) = (s.embedder(), s.embed_model.as_deref()) {
            if let Err(err) = self.store.attach_dense_cached(&mut p, e, m) {
                eprintln!("semantic retrieval off for {name}: {err}");
            }
        }
        let p = Arc::new(p);
        lock(&self.cache).insert(key, p.clone());
        Ok(p)
    }

    fn forget(&self, name: &str) {
        if let Ok(key) = self.store.path(name) {
            lock(&self.cache).remove(&key);
        }
    }

    pub fn start_embeddings(self: &Arc<Self>, name: String) {
        if !lock(&self.building).insert(name.clone()) {
            return;
        }
        let app = self.clone();
        std::thread::spawn(move || {
            let r = (|| -> Result<()> {
                let s = app.store.settings()?;
                let (Some(e), Some(m)) = (s.embedder(), s.embed_model.clone()) else { return Ok(()) };
                let mut p = app.store.load_persona(&name)?;
                app.store.attach_dense(&mut p, e, &m)
            })();
            if let Err(e) = r {
                eprintln!("semantic index for {name} failed: {e}");
            }
            app.forget(&name);
            lock(&app.building).remove(&name);
        });
    }

    /// `name`, or `name 2`, `name 3`... when taken.
    pub fn free_name(&self, name: &str) -> String {
        let free = |n: &String| self.store.path(n).is_err();
        std::iter::once(name.to_string()).chain((2..).map(|i| format!("{name} {i}"))).find(free).expect("endless names")
    }
}

fn handle(app: &Arc<App>, req: &mut Request, stream: &TcpStream) -> std::io::Result<()> {
    if !app.request_allowed(req) {
        return send_json(stream, 403, &json!({ "error": "forbidden" }));
    }
    let segs: Vec<String> =
        req.path.split('/').filter(|s| !s.is_empty()).map(|s| percent_decode(s, false)).collect();
    let segs: Vec<&str> = segs.iter().map(String::as_str).collect();

    match (req.method.as_str(), segs.as_slice()) {
        ("GET", []) => return send(stream, 200, "text/html; charset=utf-8", INDEX.as_bytes()),
        ("GET", ["style.css"]) => return send(stream, 200, "text/css; charset=utf-8", STYLE.as_bytes()),
        ("GET", ["app.js"]) => return send(stream, 200, "text/javascript; charset=utf-8", SCRIPT.as_bytes()),
        ("POST", ["api", "personas", name, "chat"]) => return chatlog::reply(app, name, req, stream),
        ("POST", ["api", "import", "identify"]) => return import::identify(app, req, stream),
        ("GET", ["api", "personas", name, "avatar"]) => return serve_avatar(app.store.path(name).ok(), "avatar", stream),
        ("GET", ["api", "me", "avatar"]) => return serve_avatar(Some(app.root.clone()), "me", stream),
        _ => {}
    }
    match api(app, req, &segs) {
        Ok(v) => send_json(stream, 200, &v),
        Err(e) => send_error(stream, &e),
    }
}

fn serve_avatar(dir: Option<PathBuf>, stem: &str, stream: &TcpStream) -> std::io::Result<()> {
    let found = dir.and_then(|d| avatar::find(&d, stem));
    match found.and_then(|(p, ctype, _)| Some((std::fs::read(p).ok()?, ctype))) {
        Some((bytes, ctype)) => send(stream, 200, ctype, &bytes),
        None => send_json(stream, 404, &json!({ "error": "no picture" })),
    }
}

fn api(app: &Arc<App>, req: &mut Request, segs: &[&str]) -> Result<Value> {
    match (req.method.as_str(), segs) {
        ("GET", ["api", "personas"]) => list_personas(app),
        ("DELETE", ["api", "personas", name]) => {
            app.forget(name);
            app.store.delete_persona(name)?;
            ok()
        }
        ("GET", ["api", "personas", name, "chat"]) => Ok(json!(chatlog::load(app, name)?)),
        ("DELETE", ["api", "personas", name, "chat"]) => {
            let _g = lock(&app.chat_lock);
            chatlog::save(app, name, &[])?;
            ok()
        }
        ("GET", ["api", "personas", name, "prompt"]) => Ok(json!({ "prompt": app.persona(name)?.system_prompt })),
        ("PUT", ["api", "personas", name, "prompt"]) => {
            app.store.save_system_prompt(name, body_str(&body_json(req)?, "prompt")?)?;
            app.forget(name);
            ok()
        }
        ("PUT", ["api", "personas", name, "model"]) => {
            app.store.set_persona_model(name, body_json(req)?["model"].as_str())?;
            app.forget(name);
            ok()
        }
        ("POST", ["api", "personas", name, "embeddings"]) => {
            app.store.path(name)?;
            app.start_embeddings(name.to_string());
            ok()
        }
        ("POST", ["api", "personas", name, "avatar"]) => {
            Ok(json!({ "avatar": avatar::save(&app.store.path(name)?, "avatar", &req.body)? }))
        }
        ("DELETE", ["api", "personas", name, "avatar"]) => {
            avatar::delete(&app.store.path(name)?, "avatar");
            ok()
        }

        // the user's own picture
        ("GET", ["api", "me"]) => Ok(json!({ "avatar": avatar::version(&app.root, "me") })),
        ("POST", ["api", "me", "avatar"]) => Ok(json!({ "avatar": avatar::save(&app.root, "me", &req.body)? })),
        ("DELETE", ["api", "me", "avatar"]) => {
            avatar::delete(&app.root, "me");
            ok()
        }

        // import
        ("POST", ["api", "import", "start"]) => Ok(import::start(app)),
        ("POST", ["api", "import", "file"]) => import::add_file(app, req),
        ("POST", ["api", "import", "create"]) => import::create(app, req),
        ("DELETE", ["api", "import", token]) => Ok(import::cancel(app, token)),

        // settings & models
        ("GET", ["api", "settings"]) => serde_json::to_value(app.store.settings()?).map_err(|e| Error::Json(e.to_string())),
        ("PUT", ["api", "settings"]) => {
            let s: Settings = serde_json::from_slice(&req.body).map_err(|e| Error::Json(e.to_string()))?;
            app.store.save_settings(&s)?;
            lock(&app.cache).clear(); // backend / embedder may have changed
            ok()
        }
        ("GET", ["api", "models"]) => {
            let s = app.store.settings()?;
            Ok(match s.backend().list_models() {
                Ok(ms) => json!({
                    "online": true, "default": s.model,
                    "models": ms.iter().filter(|m| m.can_chat()).map(|m| &m.name).collect::<Vec<_>>(),
                }),
                Err(e) => json!({ "online": false, "default": s.model, "models": [], "error": e.to_string() }),
            })
        }

        _ => Err(Error::NotFound(format!("{} {}", req.method, req.path))),
    }
}

fn list_personas(app: &App) -> Result<Value> {
    let building = lock(&app.building).clone();
    let mut out = Vec::new();
    for m in app.store.list_personas()? {
        let dir = app.store.path(&m.name)?;
        let embeddings = if building.contains(&m.name) {
            "building"
        } else if dir.join("embeddings.bin").exists() {
            "ready"
        } else {
            "none"
        };
        let last = chatlog::load(app, &m.name).ok().and_then(|c| c.last().cloned());
        out.push(json!({
            "name": m.name, "target": m.target_sender, "general": m.general, "source": m.source,
            "messages": m.message_count, "persona_messages": m.persona_message_count,
            "model": m.model, "embeddings": embeddings, "avatar": avatar::version(&dir, "avatar"),
            "last": last,
        }));
    }
    Ok(json!(out))
}
