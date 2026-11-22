//! Scale check: `cargo run --release --example bench [n_messages]` (default 400000).
//! Generates a synthetic export in the temp dir, then times reading, persona create/load and retrieval.

use persona_core::*;
use std::time::Instant;

fn main() -> Result<()> {
    let n: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(400_000);
    let dir = std::env::temp_dir().join("persona_core_bench");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let vocab: Vec<String> = (0..5000).map(|i| format!("w{}x{}", i, i * 7 % 13)).collect();
    let (files, per) = (8, n / 8);
    let mut seed = 12345u64;
    let mut rnd = move |m: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % m
    };
    for f in 0..files {
        let mut s = String::from(r#"{"participants":[{"name":"Me"},{"name":"Them"}],"messages":["#);
        for i in 0..per {
            let words: Vec<&str> = (0..4 + rnd(10)).map(|_| vocab[rnd(vocab.len())].as_str()).collect();
            let ts = 1_600_000_000_000i64 + ((files - f) * per - i) as i64 * 20_000; // newest first
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                r#"{{"sender_name":"{}","timestamp_ms":{ts},"content":"{}","photos":[{{"uri":"x"}}]}}"#,
                if rnd(2) == 0 { "Me" } else { "Them" },
                words.join(" ")
            ));
        }
        s.push_str("]}");
        std::fs::write(dir.join(format!("message_{}.json", f + 1)), s).unwrap();
    }
    let mb: u64 = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum::<u64>() >> 20;

    let t = Instant::now();
    let corpus = ingest(&[&dir], &ParseOptions::default())?;
    let id = identify(&corpus, &IdentifyOptions::default(), &mut |_| true)?;
    println!("read + identify {} msgs ({mb} MB): {:?}", id.conversation.messages.len(), t.elapsed());

    let store = PersonaStore::new(dir.join("store"));
    let them = id.characters.iter().find(|c| c.name == "Them").ok_or(Error::NotFound("Them".into()))?;
    let t = Instant::now();
    store.create_persona("them", &id, them)?;
    println!("create (write): {:?}", t.elapsed());
    let t = Instant::now();
    let p = store.load_persona("them")?;
    println!("load + index: {:?}", t.elapsed());

    let t = Instant::now();
    let mut hits = 0;
    for i in 0..200 {
        let q = format!("{} {} {}", vocab[i * 7 % 5000], vocab[i * 13 % 5000], vocab[i * 29 % 5000]);
        hits += get_injection_context(&p, &q, &[]).exemplars.len();
    }
    println!("200 retrievals: {:?} total ({} hits)", t.elapsed(), hits);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
