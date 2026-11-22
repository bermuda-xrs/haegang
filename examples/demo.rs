//! End-to-end check: read files, identify the characters, build a persona, show its prompt.
//! `cargo run --example demo [files or folders...]` (default: the bundled sample).
//! Set MODEL=1 to let the configured model help with names. Your real personas are never touched.

use persona_core::*;

fn main() -> Result<()> {
    let mut paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        paths.push("examples/sample".into());
    }
    let corpus = ingest(&paths, &ParseOptions::default())?;
    for d in &corpus.documents {
        println!("read {} ({})", d.name, d.format);
    }
    for u in &corpus.unreadable {
        println!("could not read {}: {}", u.name, u.reason);
    }

    let settings = Settings::default();
    let backend = settings.backend();
    let model = if std::env::var("MODEL").is_ok() { Some(resolve_model(backend.as_ref(), None, &settings)?) } else { None };
    let options = IdentifyOptions {
        model: model.map(|model| ModelAccess { backend: backend.as_ref(), model, gen: settings.gen.clone() }),
        ..Default::default()
    };
    let id = identify(&corpus, &options, &mut |p| {
        println!("{} ({}/{})", p.stage, p.done + 1, p.total);
        true
    })?;

    for c in &id.characters {
        println!("[ ] {} - {} lines, {} certain {:?} {:?}", c.name, c.lines, c.certain, c.aliases, c.description);
        for s in &c.samples {
            println!("      {s}");
        }
    }
    println!("mentioned only: {:?}; unattributed quotes: {}", id.mentioned, id.unattributed);
    id.notes.iter().for_each(|n| println!("note: {n}"));

    let Some(first) = id.characters.first() else { return Ok(()) };
    let root = std::env::temp_dir().join("persona_core_demo");
    let _ = std::fs::remove_dir_all(&root);
    let persona = PersonaStore::new(&root).create_persona(&first.name, &id, first)?;
    let msg = "what happened?";
    let ctx = get_injection_context(&persona, msg, &[]);
    let hist = [ChatMessage { role: Role::User, text: msg.into() }];
    println!("--- prompt for {} ---\n{}", first.name, prepare_prompt(&persona, &hist, &ctx));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
