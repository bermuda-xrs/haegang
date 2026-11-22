//! Persona storage: one self-contained folder per persona, `<root>/<slug>/` holding `meta.json`,
//! `messages.json`, `system_prompt.txt` and optionally `embeddings.bin`. Copy the folder to move it.
//! Store-wide settings live in `<root>/settings.json`. The BM25 index is rebuilt in memory on load.

use crate::dense::{DenseIndex, HybridRetriever};
use crate::retrieval::build_exchanges;
use crate::settings::Settings;
use crate::{Bm25Retriever, Character, Embedder, Error, Identification, Message, Result, Retriever};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaMeta {
    pub name: String,
    /// The sender in `messages.json` this persona speaks as.
    pub target_sender: String,
    pub created_unix: u64,
    pub message_count: usize,
    pub persona_message_count: usize,
    pub first_timestamp_ms: i64,
    pub last_timestamp_ms: i64,
    /// Overrides the default chat model.
    #[serde(default)]
    pub model: Option<String>,
    /// Speaks as the voice of a text rather than as a character.
    #[serde(default)]
    pub general: bool,
    /// Name of the source text; empty for chats.
    #[serde(default)]
    pub source: String,
}

pub struct Persona {
    pub meta: PersonaMeta,
    pub system_prompt: String,
    pub messages: Vec<Message>,
    /// Indices of persona messages short enough to be style samples.
    style_pool: Vec<usize>,
    retriever: Box<dyn Retriever>,
}

impl Persona {
    pub fn new(meta: PersonaMeta, system_prompt: String, messages: Vec<Message>) -> Self {
        let retriever = Box::new(Bm25Retriever::new(build_exchanges(&messages, &meta.target_sender)));
        let style_pool = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.sender == meta.target_sender && (3..=300).contains(&m.text.chars().count()))
            .map(|(i, _)| i)
            .collect();
        Self { meta, system_prompt, messages, style_pool, retriever }
    }

    pub fn set_retriever(&mut self, r: Box<dyn Retriever>) {
        self.retriever = r;
    }

    pub fn retriever(&self) -> &dyn Retriever {
        self.retriever.as_ref()
    }

    /// `n` persona messages spread evenly over the history.
    pub fn style_samples(&self, n: usize) -> Vec<String> {
        let n = n.min(self.style_pool.len());
        (0..n).map(|i| self.messages[self.style_pool[i * self.style_pool.len() / n]].text.clone()).collect()
    }
}

/// A directory of personas.
pub struct PersonaStore {
    root: PathBuf,
}

impl PersonaStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn settings(&self) -> Result<Settings> {
        let p = self.root.join("settings.json");
        if p.exists() { read_json(&p) } else { Ok(Settings::default()) }
    }

    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        std::fs::create_dir_all(&self.root).map_err(io(&self.root))?;
        write_json(&self.root.join("settings.json"), s)
    }

    /// Create a persona for one of `identification`'s characters. Atomic: written to a temp folder, then renamed.
    pub fn create_persona(&self, name: &str, identification: &Identification, character: &Character) -> Result<Persona> {
        let dir = self.dir(name)?;
        if dir.exists() {
            return Err(Error::Exists(name.into()));
        }
        let messages = &identification.conversation.messages;
        let own: Vec<&Message> = messages.iter().filter(|m| m.sender == character.sender).collect();
        if own.is_empty() {
            return Err(Error::Invalid(format!("`{}` has no lines", character.name)));
        }
        let meta = PersonaMeta {
            name: name.trim().to_string(),
            target_sender: character.sender.clone(),
            created_unix: SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            message_count: messages.len(),
            persona_message_count: own.len(),
            first_timestamp_ms: messages.first().map_or(0, |m| m.timestamp_ms),
            last_timestamp_ms: messages.last().map_or(0, |m| m.timestamp_ms),
            model: None,
            general: character.voice,
            source: identification.title.clone(),
        };
        let prompt = generate_system_prompt(&meta, &own, &character.description);

        let tmp = dir.with_file_name(format!(".{}.tmp", dir.file_name().unwrap().to_string_lossy()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(io(&tmp))?;
        let written = (|| -> Result<()> {
            write_json(&tmp.join("meta.json"), &meta)?;
            write_json_compact(&tmp.join("messages.json"), messages)?;
            let sp = tmp.join("system_prompt.txt");
            std::fs::write(&sp, &prompt).map_err(io(&sp))?;
            std::fs::rename(&tmp, &dir).map_err(io(&dir))
        })();
        if let Err(e) = written {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
        Ok(Persona::new(meta, prompt, messages.clone()))
    }

    /// All personas by name; unreadable folders are skipped.
    pub fn list_personas(&self) -> Result<Vec<PersonaMeta>> {
        let mut out = Vec::new();
        if !self.root.exists() {
            return Ok(out);
        }
        for e in std::fs::read_dir(&self.root).map_err(io(&self.root))?.flatten() {
            if e.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            if let Ok(m) = read_json::<PersonaMeta>(&e.path().join("meta.json")) {
                out.push(m);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn load_persona(&self, name: &str) -> Result<Persona> {
        let dir = self.path(name)?;
        let meta: PersonaMeta = read_json(&dir.join("meta.json"))?;
        let messages: Vec<Message> = read_json(&dir.join("messages.json"))?;
        let sp = dir.join("system_prompt.txt");
        let prompt = std::fs::read_to_string(&sp).map_err(io(&sp))?;
        Ok(Persona::new(meta, prompt, messages))
    }

    /// Switch to hybrid retrieval, building `embeddings.bin` through `embedder` unless a current one exists.
    pub fn attach_dense(&self, persona: &mut Persona, embedder: Arc<dyn Embedder>, embed_model: &str) -> Result<()> {
        self.attach(persona, embedder, embed_model, true).map(|_| ())
    }

    /// Like [`attach_dense`](Self::attach_dense) but never builds: `Ok(false)` if no current index exists.
    pub fn attach_dense_cached(&self, persona: &mut Persona, embedder: Arc<dyn Embedder>, embed_model: &str) -> Result<bool> {
        self.attach(persona, embedder, embed_model, false)
    }

    fn attach(&self, persona: &mut Persona, embedder: Arc<dyn Embedder>, embed_model: &str, build: bool) -> Result<bool> {
        let dir = self.path(&persona.meta.name)?;
        let exchanges = build_exchanges(&persona.messages, &persona.meta.target_sender);
        let path = dir.join("embeddings.bin");
        let dense = match DenseIndex::load(&path).filter(|d| d.model == embed_model && d.len() == exchanges.len()) {
            Some(d) => d,
            None if build => {
                let d = DenseIndex::build(&exchanges, embedder.as_ref(), embed_model)?;
                d.save(&path)?;
                d
            }
            None => return Ok(false),
        };
        persona.set_retriever(Box::new(HybridRetriever::new(Bm25Retriever::new(exchanges), dense, embedder)?));
        Ok(true)
    }

    pub fn save_system_prompt(&self, name: &str, prompt: &str) -> Result<()> {
        let sp = self.path(name)?.join("system_prompt.txt");
        std::fs::write(&sp, prompt).map_err(io(&sp))
    }

    /// Set or clear the persona's model; [`crate::resolve_model`] validates it when chatting.
    pub fn set_persona_model(&self, name: &str, model: Option<&str>) -> Result<()> {
        let p = self.path(name)?.join("meta.json");
        let mut meta: PersonaMeta = read_json(&p)?;
        meta.model = model.map(String::from);
        write_json(&p, &meta)
    }

    pub fn delete_persona(&self, name: &str) -> Result<()> {
        let dir = self.path(name)?;
        std::fs::remove_dir_all(&dir).map_err(io(&dir))
    }

    /// Folder of an existing persona.
    pub fn path(&self, name: &str) -> Result<PathBuf> {
        let dir = self.dir(name)?;
        if dir.join("meta.json").exists() { Ok(dir) } else { Err(Error::NotFound(name.into())) }
    }

    /// The slug has no separators or dots, so it cannot escape `root`.
    fn dir(&self, name: &str) -> Result<PathBuf> {
        let slug: String = name
            .trim()
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        if slug.trim_matches('_').is_empty() {
            return Err(Error::Invalid(format!("bad persona name `{name}`")));
        }
        Ok(self.root.join(slug))
    }
}

fn io(p: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |e| Error::Io(format!("{}: {e}", p.display()))
}

/// Pretty-printed, for files people may edit.
fn write_json<T: Serialize + ?Sized>(p: &Path, v: &T) -> Result<()> {
    let s = serde_json::to_vec_pretty(v).map_err(|e| Error::Json(e.to_string()))?;
    std::fs::write(p, s).map_err(io(p))
}

fn write_json_compact<T: Serialize + ?Sized>(p: &Path, v: &T) -> Result<()> {
    let f = std::fs::File::create(p).map_err(io(p))?;
    let mut w = std::io::BufWriter::new(f);
    serde_json::to_writer(&mut w, v).map_err(|e| Error::Json(e.to_string()))?;
    std::io::Write::flush(&mut w).map_err(io(p))
}

fn read_json<T: for<'de> Deserialize<'de>>(p: &Path) -> Result<T> {
    let b = std::fs::read(p).map_err(io(p))?;
    serde_json::from_slice(&b).map_err(|e| Error::Json(format!("{}: {e}", p.display())))
}

fn is_emoji(c: char) -> bool {
    matches!(c as u32, 0x1F300..=0x1FAFF | 0x2600..=0x27BF)
}

/// A starting point for the system prompt, from measured style; editable afterwards.
fn generate_system_prompt(meta: &PersonaMeta, own: &[&Message], description: &str) -> String {
    let name = &meta.name;
    let n = own.len() as f32;
    let mut lens: Vec<usize> = own.iter().map(|m| m.text.split_whitespace().count()).collect();
    lens.sort_unstable();
    let median = lens[lens.len() / 2];
    let pct = |f: &dyn Fn(&str) -> bool| (own.iter().filter(|m| f(&m.text)).count() as f32 / n * 100.0).round();
    let lower_start = pct(&|t| t.chars().next().is_some_and(|c| c.is_lowercase()));
    let end_punct = pct(&|t| t.ends_with(['.', '!', '?']));
    let has_emoji = pct(&|t| t.chars().any(is_emoji));

    let mut emojis: HashMap<char, usize> = HashMap::new();
    let mut words: HashMap<String, usize> = HashMap::new();
    for m in own {
        for c in m.text.chars().filter(|c| is_emoji(*c)) {
            *emojis.entry(c).or_default() += 1;
        }
        for w in m.text.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3) {
            *words.entry(w.to_lowercase()).or_default() += 1;
        }
    }
    let top = |mut v: Vec<(String, usize)>, k: usize| {
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.into_iter().take(k).map(|x| x.0).collect::<Vec<_>>().join(", ")
    };
    let top_emoji = top(emojis.into_iter().map(|(c, n)| (c.to_string(), n)).collect(), 8);
    let top_words = top(words.into_iter().collect(), 15);

    let origin = if meta.source.is_empty() { String::new() } else { format!(" from \"{}\"", meta.source) };
    let intro = if meta.general {
        format!(
            "You speak in the voice of the text{origin}. Respond in its style and on its subject matter, drawing \
             on the real passages below, never as a generic AI assistant."
        )
    } else {
        format!(
            "You are {name}{origin}. Stay in character: reply exactly as {name} would, in their voice, \
             never as an AI assistant."
        )
    };
    let about = if description.is_empty() { String::new() } else { format!("\nAbout {name}: {description}\n") };
    let mut s = format!(
        "{intro}\n{about}\nStyle (measured from {} real messages):\n\
         - Typical message length: about {median} words (median).\n\
         - {lower_start}% of messages start lowercase; {end_punct}% end with . ! or ?.\n\
         - {has_emoji}% of messages contain emoji.\n",
        own.len()
    );
    if !top_emoji.is_empty() {
        s.push_str(&format!("- Favorite emoji: {top_emoji}\n"));
    }
    if !top_words.is_empty() {
        s.push_str(&format!("- Frequent words: {top_words}\n"));
    }
    s.push_str(
        "\nRules: output only the message text itself, with no quotes, name prefix or narration. \
         Write in the language the real messages below use. Treat them as ground truth for tone, \
         vocabulary and rhythm, and keep replies as short and natural as they are.\n",
    );
    s
}
