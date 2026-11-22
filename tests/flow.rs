use persona_core::*;
use std::sync::{Arc, Mutex};

fn tmp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("persona_core_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

fn identified(path: &std::path::Path) -> Identification {
    let corpus = ingest(&[path], &ParseOptions::default()).unwrap();
    identify(&corpus, &IdentifyOptions::default(), &mut |_| true).unwrap()
}

fn sample() -> Identification {
    identified(std::path::Path::new("examples/sample"))
}

fn character<'a>(id: &'a Identification, name: &str) -> &'a Character {
    id.characters.iter().find(|c| c.name == name).unwrap()
}

#[test]
fn full_flow() {
    let id = sample();
    // 12 raw: 1 media-only + 1 system + 1 liked + 1 duplicate dropped
    assert_eq!(id.conversation.messages.len(), 8);
    assert_eq!(id.conversation.skipped, 4);
    assert!(id.conversation.messages.windows(2).all(|w| w[0].timestamp_ms <= w[1].timestamp_ms));
    assert_eq!((character(&id, "Alex").lines, character(&id, "Sam").lines), (4, 4));
    // mojibake fixed, URL stripped
    assert!(id.conversation.messages.iter().any(|m| m.text == "morning! coffee first 😀"));
    assert!(id.conversation.messages.iter().any(|m| m.text == "pepperoni obviously, no pineapple. never"));

    let root = tmp("flow");
    let store = PersonaStore::new(&root);
    let alex = character(&id, "Alex");
    let p = store.create_persona("Alex", &id, alex).unwrap();
    assert_eq!(p.meta.persona_message_count, 4);
    assert!(matches!(store.create_persona("alex", &id, alex), Err(Error::Exists(_))));

    let loaded = store.load_persona("Alex").unwrap();
    assert_eq!(store.list_personas().unwrap().len(), 1);

    let ctx = get_injection_context(&loaded, "what pizza toppings?", &[]);
    assert!(ctx.exemplars[0].reply.contains("pepperoni"), "{ctx:?}");
    let ctx = get_injection_context(&loaded, "bring snacks on the hike?", &[]);
    assert!(ctx.exemplars[0].reply.contains("trail mix"), "{ctx:?}");
    assert!(get_injection_context(&loaded, "zzzqqq", &[]).exemplars.is_empty());

    let hist = [ChatMessage { role: Role::User, text: "what pizza toppings?".into() }];
    let prompt = prepare_prompt(&loaded, &hist, &ctx);
    assert!(prompt.ends_with("Alex:") && prompt.contains("User: what pizza toppings?"));

    store.save_system_prompt("Alex", "edited").unwrap();
    assert_eq!(store.load_persona("Alex").unwrap().system_prompt, "edited");
    store.delete_persona("Alex").unwrap();
    assert!(matches!(store.load_persona("Alex"), Err(Error::NotFound(_))));
    let _ = std::fs::remove_dir_all(&root);
}

struct FakeBackend(Mutex<Vec<ChatTurn>>);

impl ChatBackend for FakeBackend {
    fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![
            ModelInfo { name: "embed:latest".into(), capabilities: vec!["embedding".into()] },
            ModelInfo { name: "chat:latest".into(), capabilities: vec!["completion".into()] },
        ])
    }
    fn chat(&self, _m: &str, turns: &[ChatTurn], _p: &GenParams, on_token: &mut dyn FnMut(&str)) -> Result<String> {
        *self.0.lock().unwrap() = turns.to_vec();
        on_token("ok");
        Ok("ok".into())
    }
}

/// Hashes character trigrams into a 64-dim vector: crude but deterministic and similarity-preserving.
struct FakeEmbedder;
impl Embedder for FakeEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|t| {
                let cs: Vec<char> = t.to_lowercase().chars().collect();
                let mut v = vec![0f32; 64];
                for w in cs.windows(3) {
                    v[w.iter().map(|c| *c as usize).sum::<usize>() * 31 % 64] += 1.0;
                }
                v
            })
            .collect())
    }
}

#[test]
fn engine_model_selection_and_dense() {
    let id = sample();
    let root = tmp("engine");
    let store = PersonaStore::new(&root);
    let mut p = store.create_persona("Alex", &id, character(&id, "Alex")).unwrap();

    // settings round-trip; model resolution: default missing -> error listing chat models only
    let mut s = store.settings().unwrap();
    assert_eq!(s.model, "huihui_ai/qwen3.5-abliterated:4b");
    s.model = "chat".into();
    store.save_settings(&s).unwrap();
    let s = store.settings().unwrap();
    let be = FakeBackend(Default::default());
    assert_eq!(resolve_model(&be, None, &s).unwrap(), "chat:latest");
    assert!(matches!(resolve_model(&be, Some("embed"), &s), Err(Error::NotFound(_))));
    store.set_persona_model("Alex", Some("nope")).unwrap();
    assert_eq!(store.load_persona("Alex").unwrap().meta.model.as_deref(), Some("nope"));
    store.set_persona_model("Alex", None).unwrap();

    // dense: builds embeddings.bin, second attach reuses it
    store.attach_dense(&mut p, Arc::new(FakeEmbedder), "fake").unwrap();
    assert!(root.join("alex/embeddings.bin").exists());
    store.attach_dense(&mut p, Arc::new(FakeEmbedder), "fake").unwrap();
    let ctx = get_injection_context(&p, "pepperoni pizza?", &[]);
    assert!(ctx.exemplars[0].reply.contains("pepperoni"), "{ctx:?}");

    // engine: system turn carries injected real messages; history is capped and ordered
    let mut eng = Engine::new(&p, &be, &s).unwrap();
    assert_eq!(eng.model, "chat:latest");
    eng.max_history = 1;
    let hist = [
        ChatMessage { role: Role::User, text: "old".into() },
        ChatMessage { role: Role::Persona, text: "newer".into() },
    ];
    let mut streamed = String::new();
    let r = eng.reply(&hist, "bring snacks on the hike?", &mut |t| streamed.push_str(t)).unwrap();
    assert_eq!((r.text.as_str(), streamed.as_str()), ("ok", "ok"));
    let sent = be.0.lock().unwrap().clone();
    assert_eq!(sent.len(), 3); // system + 1 history + new user
    assert_eq!(sent[0].role, TurnRole::System);
    assert!(sent[0].content.contains("trail mix"));
    assert_eq!((sent[1].role, sent[1].content.as_str()), (TurnRole::Assistant, "newer"));
    assert_eq!(sent[2].role, TurnRole::User);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_text_without_characters_becomes_its_own_voice() {
    let dir = tmp("voice");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("harbor notes.txt");
    std::fs::write(&file, "The moon rose over the quiet harbor.\n\nShe wrote letters she never sent.\n\nCoffee at midnight again.\n").unwrap();
    let id = identified(&file);
    assert_eq!(id.characters.len(), 1);
    let voice = &id.characters[0];
    assert!(voice.voice && voice.name == "harbor notes");

    let store = PersonaStore::new(dir.join("store"));
    let p = store.create_persona(&voice.name, &id, voice).unwrap();
    assert!(p.meta.general && p.system_prompt.contains("voice of the text"));
    assert!(store.load_persona("harbor notes").unwrap().meta.general);
    let ctx = get_injection_context(&p, "harbor moon", &[]);
    assert!(ctx.exemplars[0].reply.contains("harbor"), "{ctx:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_book_character_speaks_their_own_lines_in_context() {
    let dir = tmp("book");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("The Keeper.txt");
    std::fs::write(
        &file,
        "“Did you bring the oil?” asked Tobias.\n\n“Two cans,” said Marta. “The third was leaking.”\n\n“And the ferry?”\n\n“Late, as always.”\n",
    )
    .unwrap();
    let id = identified(&file);
    let store = PersonaStore::new(dir.join("store"));
    let marta = store.create_persona("Marta", &id, character(&id, "Marta")).unwrap();
    assert!(marta.system_prompt.starts_with("You are Marta from \"The Keeper\""));
    assert_eq!(marta.meta.source, "The Keeper");

    let ctx = get_injection_context(&marta, "did you bring the oil?", &[]);
    assert_eq!(ctx.exemplars[0].context, "Did you bring the oil?");
    assert!(ctx.exemplars[0].reply.contains("Two cans") && ctx.exemplars[0].reply.contains("leaking"), "{ctx:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
