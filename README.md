# Haegang

A local multi-persona chat app. Give it a book, a document, a chat log or a dataset: it works out who is in it,
you tick the characters you want, and you chat with them. Replies come from a local model (Ollama by default);
every turn injects real lines retrieved from that character's text ("constant injection").

Written in Rust, with a hand-written HTTP client and server, BM25, vector search and quote attribution on `std`.
Crates are used only for what cannot sensibly be hand-written: `pdf-extract`, `zip`, `cfb` (legacy Office/Hangul
containers), `flate2`, `encoding_rs` + `chardetng` (legacy text encodings), `serde` + `serde_json`.

## Run

```
start.cmd
```

Then open http://127.0.0.1:7860. `start.cmd` works from any folder. Equivalent, from this folder:

```
cargo run --release --bin persona_web
```

Env: `PORT` (default 7860), `PERSONA_DIR` (default `./personas`). The server binds to `127.0.0.1` only and
rejects foreign `Host`/`Origin` headers. The first reply after a cold start can take ~30 s on CPU while the
model loads.

### Build requirements (Windows)

`rust-toolchain.toml` pins `stable-x86_64-pc-windows-gnu`, which needs MinGW-w64 on `PATH`
(`winget install BrechtSanders.WinLibs.POSIX.UCRT`). With MSVC Build Tools installed you can delete that file.
The web frontend (`web/`) is compiled into the binary, so rebuild after editing it.

## Importing

Drop files or a folder into the import dialog. The format is detected from the content, so names do not matter.

| Kind | Formats |
|---|---|
| Documents | PDF with a text layer, Word (`.docx`, `.doc`), OpenDocument, PowerPoint (`.pptx`), Hangul (`.hwp`, `.hwpx`), RTF |
| Books | EPUB, FictionBook, plain text (Project Gutenberg boilerplate is removed) |
| Text | Markdown, HTML, XML, subtitles (`.srt`, `.vtt`), any other text in UTF-8, UTF-16 or a legacy encoding (EUC-KR, Shift_JIS, ...) |
| Data | JSON / JSON Lines (chat exports, role+content, prompt+response, text records, lists of strings), CSV, TSV, Excel (`.xlsx`) |
| Archives | `.zip` of any of the above |

Not readable, and reported per file: images, audio, video, scanned PDFs (no text layer), Kindle files, password-protected
documents, old `.ppt` / `.xls`. An unreadable file never stops the others.

### Characters

Identification offers a checkbox for each character. Each ticked character becomes a persona.

- **Sources that name their speakers** are certain: chat exports and datasets, chat logs (WhatsApp, KakaoTalk, IRC, `Name: text`),
  plays and screenplays.
- **Prose** has to be worked out, so each option shows how many lines were found and how many are certain. Names come
  from speech tags (English and Korean) plus the model, which reads any language and knows that "Lizzy" is Elizabeth.
  Attribution then runs over the whole text with rules, most reliable first: speech tags, the only character named beside a
  quote, vocatives, turn-taking ([Muzny et al. 2017](https://aclanthology.org/E17-1044)). A 200,000-word novel takes
  under a second; the model pass adds about 30 seconds per excerpt (up to four) on CPU.
- **The text's own voice** is an option too: the narrator of a book, or the only option for a text without dialogue.

If the list is wrong, tick **Identification incorrect** and say how ("remove X", "A and B are the same person", "X is really
called Y", "Z is missing"). The model turns that into edits; only names you mentioned are touched, and it needs the model
server to be running.

## Checks

```
cargo test                                  # unit + integration tests, no network needed
cargo run --example demo [files or folders]  # read -> identify -> persona -> prompt (MODEL=1 uses the model)
cargo run --release --example bench         # 400k-message scale check
```

## Layout

```
src/                    library (persona_core)
  ingest/               any file -> plain text or labelled messages
    mod.rs              format detection, parallel reading, merging
    text.rs markup.rs   encodings, RTF, subtitles, CSV; HTML and XML
    office.rs           zip containers (docx, pptx, xlsx, odf, epub, hwpx) and OLE (.doc, .hwp)
    dataset.rs          JSON and table records
  identify/             who is in it
    mod.rs              orchestration, the character list
    labeled.rs          chat logs, plays, screenplays
    layout.rs prose.rs  paragraphs and quotes; who said what
    tags.rs cast.rs     speech tags; names and aliases
    model.rs edits.rs   asking the model; applying the user's corrections
  persona.rs            persona folders, system-prompt generation
  retrieval.rs          exchanges, BM25 retriever, injection context
  dense.rs              embedding index + hybrid (BM25 + dense, RRF) retriever
  prompt.rs             chat / flat prompt rendering
  chat.rs               Engine: retrieval -> prompt -> backend
  backend.rs            Ollama + OpenAI-compatible backends, embedder
  settings.rs           settings.json, model selection
  http.rs               minimal HTTP client
src/bin/persona_web/    web server: routing, import session, chat log, profile pictures
web/                    UI (vanilla HTML/CSS/JS)
```

Data on disk (each persona folder is portable; copy it to move a persona):

```
personas/settings.json            backend, default model, generation params
personas/me.<ext>                 your profile picture
personas/<persona>/meta.json      metadata
                   messages.json  the whole source, in order, with speakers
                   system_prompt.txt   generated, editable in the UI
                   embeddings.bin semantic index (optional, rebuilt when stale)
                   chat.json      your conversation with the persona
                   avatar.<ext>   profile picture
```

## Library use

```rust
let corpus = ingest(&["The Keeper.pdf"], &ParseOptions::default())?;   // files or folders, any format
let store = PersonaStore::new("personas");
let settings = store.settings()?;
let backend = settings.backend();
let model = resolve_model(backend.as_ref(), None, &settings)?;
let options = IdentifyOptions {
    model: Some(ModelAccess { backend: backend.as_ref(), model, gen: settings.gen.clone() }),
    ..Default::default()
};
let found = identify(&corpus, &options, &mut |_progress| true)?;        // returning false cancels
let marta = found.characters.iter().find(|c| c.name == "Marta").unwrap();
let mut persona = store.create_persona("Marta", &found, marta)?;
if let (Some(e), Some(m)) = (settings.embedder(), settings.embed_model.as_deref()) {
    store.attach_dense(&mut persona, e, m)?;                            // optional semantic retrieval
}
let engine = Engine::new(&persona, backend.as_ref(), &settings)?;       // resolves + verifies the model
let reply = engine.reply(&history, "hey!", &mut |tok| print!("{tok}"))?;
```

| Item | Purpose |
|---|---|
| `ingest(paths, &ParseOptions)` / `ingest_files(named bytes, ..)` | Read anything into a `Corpus` of documents, plus a list of what could not be read. `ParseOptions::system_markers` adds localized system lines to drop. |
| `identify(&corpus, &options, progress)` | `Identification { characters, mentioned, unattributed, notes, conversation }`. `IdentifyOptions { correction, previous }` applies what the user said was wrong. |
| `PersonaStore` | `create_persona`, `list_personas`, `load_persona`, `delete_persona`, `save_system_prompt`, `set_persona_model`, `settings` / `save_settings`, `attach_dense`. Creation is atomic. |
| `get_injection_context(persona, msg, history)` / `_with(.., retriever, cfg)` | `InjectionContext { exemplars, style_samples }`. |
| `prepare_chat` -> `Vec<ChatTurn>`, `prepare_prompt` -> `String` | Chat-role or flat-text prompt. |
| `Engine::{new, reply}` | Retrieval -> prompt -> backend; streams tokens through a callback. |
| `resolve_model`, `ChatBackend::list_models` | Model selection: persona override, else `Settings::model`, checked against the server. |
| `ChatBackend`, `Embedder`, `Retriever` | Extension traits for new backends and retrieval methods. |

## Models

Default chat model: `huihui_ai/qwen3.5-abliterated:4b`; embeddings: `nomic-embed-text`. Change the default in
Settings, or per persona in its details panel. `BackendKind::OpenAi` targets any OpenAI-compatible server over
plain `http://` (e.g. `http://127.0.0.1:8080/v1`); HTTPS needs a local proxy.

## Design notes

- **Retrieval unit**: a persona turn plus the turn before it (same sender, gaps ≤ 1 h). BM25 over the
  answered turn (weight 1) and the reply (0.4) with an inverted index and a small recency boost. Non-ASCII words
  also index character bigrams (Hangul, CJK). With an embedding model, BM25 and cosine rankings are fused (RRF).
- **Style samples**: real persona messages spread evenly over the history, injected on every turn.
- **Long text** is split at sentence ends into passages of at most 500 characters, never cut off.
- **Limits**: speech tags without a model cover English and Korean; built-in chat system-line filters are English (add
  `system_markers` for other languages); the embedder is Ollama-only; attribution in prose is inference, so some lines
  will land on the wrong character.
