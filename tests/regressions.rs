use persona_core::*;

fn files(list: &[(&str, &[u8])]) -> Corpus {
    let owned: Vec<(String, Vec<u8>)> = list.iter().map(|(n, b)| (n.to_string(), b.to_vec())).collect();
    ingest_files(&owned, &ParseOptions::default()).unwrap()
}

fn id_of(list: &[(&str, &[u8])]) -> Identification {
    identify(&files(list), &IdentifyOptions::default(), &mut |_| true).unwrap()
}

fn one(name: &str, text: &str) -> Identification {
    id_of(&[(name, text.as_bytes())])
}

fn names(id: &Identification) -> Vec<&str> {
    id.characters.iter().map(|c| c.name.as_str()).collect()
}

fn said(id: &Identification, text: &str) -> String {
    let m = id.conversation.messages.iter().find(|m| m.text.contains(text));
    m.map_or_else(|| format!("<{text} missing>"), |m| m.sender.clone())
}

fn text_of(c: &Corpus) -> &str {
    match &c.documents[0].content {
        Content::Text(t) => t,
        Content::Speakers(_) => panic!("expected text"),
    }
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("persona_reg_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

#[test]
fn distinct_speakers_with_similar_names_stay_apart() {
    let id = one("chat.txt", "Ann: hi\nAnn Lee: hello\nAnn: how are you\nAnn Lee: fine\n");
    assert_eq!(names(&id), ["Ann", "Ann Lee"]);
    let id = one("k.txt", "민수: 안녕\n김민수: 응\n민수: 뭐해\n김민수: 쉬어\n");
    assert_eq!(id.characters.len(), 2);
}

#[test]
fn a_labelled_narrator_is_one_option() {
    let directions = "(a door)\n\n(a bell)\n\n(rain)\n\n(steps)\n\n(silence)\n\n";
    let play = "NARRATOR: Once upon a time.\nANA: Hello there.\nNARRATOR: She waited.\nANA: Anyone home?\n\n";
    let id = one("play.txt", &format!("{play}{directions}"));
    assert_eq!(names(&id).iter().filter(|n| **n == "Narrator").count(), 1);
    assert_eq!(id.characters.len(), 2);
}

#[test]
fn dialogue_without_any_name_still_gives_a_usable_option() {
    let id = one("d.txt", "“Are you there?”\n\n“Yes, I am.”\n\n“Good, come in.”\n\n“Thank you.”\n");
    assert_eq!(id.characters.len(), 1);
    assert!(id.characters[0].lines > 0);
    let root = tmp("nameless");
    PersonaStore::new(&root).create_persona("x", &id, &id.characters[0]).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn speech_running_over_paragraphs_keeps_its_speaker() {
    let id = one(
        "n.txt",
        "“First,” said Marta. “The lamp went out at nine.\n\n“Then the wind came up and the door blew in.\n\n“And that is all I know.”\n\n“I see,” said Tobias.\n",
    );
    assert_eq!(said(&id, "Then the wind"), "Marta");
    assert_eq!(said(&id, "that is all"), "Marta");
    assert_eq!(said(&id, "I see"), "Tobias");
}

#[test]
fn the_korean_listener_is_not_the_speaker() {
    let id = one("k.txt", "“어디 가?” 민수는 지수에게 물었다.\n\n“집에.” 지수가 대답했다.\n\n“같이 가자.” 민수가 말했다.\n");
    assert_eq!(said(&id, "어디 가"), "민수");
}

#[test]
fn korean_headings_need_a_number() {
    let id = one("k.txt", "제일 큰 시장\n\n그곳은 조용했다.\n\n제3장\n\n바람이 불었다.\n");
    assert!(id.conversation.messages.iter().any(|m| m.text.contains("제일 큰 시장")));
    assert!(!id.conversation.messages.iter().any(|m| m.text.contains("제3장")));
}

#[test]
fn transcript_labels_with_titles_and_digits() {
    let id = one("t.txt", "Dr. Kim: How long?\nPatient: A week.\nDr. Kim: Any pain?\nPatient: No.\n");
    assert_eq!(names(&id), ["Dr. Kim", "Patient"]);
    let id = one("t.txt", "player1: go\nuser42: ok\nplayer1: left\nuser42: right\n");
    assert_eq!(names(&id), ["player1", "user42"]);
    let id = one("w.txt", "\u{200e}[12/31/20, 10:23:45\u{202f}PM] Bob: hi\n\u{200e}[12/31/20, 10:24:00\u{202f}PM] Ana: yo\n[12/31/20, 10:25:00\u{202f}PM] Bob: ok\n[12/31/20, 10:26:00\u{202f}PM] Ana: bye\n");
    assert_eq!(names(&id), ["Ana", "Bob"]);
    let steps = one("s.txt", "Step 1: open it.\nStep 2: close it.\nStep 1: open again.\nStep 2: close again.\n");
    assert_eq!(steps.characters.len(), 1);
}

#[test]
fn a_persona_from_several_sources_is_not_tied_to_the_first() {
    let a = "“Hi,” said Marta.\n\n“Hello,” said Marta.\n\n“Bye,” said Marta.\n";
    let b = "“Hey,” said Otto.\n\n“Well,” said Otto.\n\n“Good,” said Otto.\n";
    let id = id_of(&[("first.txt", a.as_bytes()), ("second.txt", b.as_bytes())]);
    assert_ne!(id.title, "first");
}

#[test]
fn going_back_in_time_is_not_a_reply() {
    let newer = r#"{"participants":[{"name":"A"},{"name":"B"}],"messages":[{"sender_name":"B","timestamp_ms":1700000000000,"content":"newest question"}]}"#;
    let older = r#"{"participants":[{"name":"C"},{"name":"D"}],"messages":[{"sender_name":"C","timestamp_ms":1500000000000,"content":"oldest answer"}]}"#;
    let id = id_of(&[("1.json", newer.as_bytes()), ("2.json", older.as_bytes())]);
    let c = id.characters.iter().find(|c| c.name == "C").unwrap();
    let root = tmp("time");
    let p = PersonaStore::new(&root).create_persona("C", &id, c).unwrap();
    let ctx = get_injection_context(&p, "oldest answer", &[]);
    assert_eq!(ctx.exemplars[0].context, "");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn chat_exports_with_sender_objects_and_content_parts() {
    let discord = r#"{"messages":[{"author":{"name":"ana"},"content":"hi"},{"author":{"name":"bob"},"content":"yo"}]}"#;
    assert_eq!(names(&one("d.json", discord)), ["ana", "bob"]);
    let parts = r#"[{"role":"user","content":[{"type":"text","text":"hey"}]},{"role":"assistant","content":[{"type":"text","text":"hello"}]}]"#;
    assert_eq!(names(&one("p.json", parts)), ["Assistant", "User"]);
    let gpt = r#"[{"mapping":{"a":{"message":{"author":{"role":"user"},"content":{"content_type":"text","parts":["why"]}}},"b":{"message":{"author":{"role":"assistant"},"content":{"content_type":"text","parts":["because"]}}}}}]"#;
    assert_eq!(names(&one("g.json", gpt)), ["Assistant", "User"]);
}

#[test]
fn semicolon_csv() {
    assert_eq!(names(&one("l.csv", "speaker;text\nMarta;Hi, all\nOtto;Hello\n")), ["Marta", "Otto"]);
}

#[test]
fn subtitle_text_survives() {
    let c = files(&[("m.srt", b"1\n00:00:01,000 --> 00:00:02,000\n5 > 3, right?\n\n2\n00:00:03,000 --> 00:00:04,000\n1984\n\n3\n00:00:05,000 --> 00:00:06,000\nNOTEBOOKS are mine\n")]);
    let t = text_of(&c);
    assert!(t.contains("5 > 3, right?"), "{t}");
    assert!(t.contains("1984"), "{t}");
    assert!(t.contains("NOTEBOOKS"), "{t}");
}

#[test]
fn html_with_an_unclosed_head_and_latin_entities() {
    let c = files(&[("p.html", b"<!doctype html><html><head><title>x</title><body><p>caf&eacute; &Uuml;ber &copy;</p></body></html>")]);
    assert_eq!(text_of(&c), "café Über ©");
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, body) in entries {
        z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
        z.write_all(body).unwrap();
    }
    z.finish().unwrap().into_inner()
}

#[test]
fn word_text_boxes_are_not_doubled() {
    let xml = br#"<w:p><mc:AlternateContent><mc:Choice><w:p><w:r><w:t>Boxed</w:t></w:r></w:p></mc:Choice><mc:Fallback><w:p><w:r><w:t>Boxed</w:t></w:r></w:p></mc:Fallback></mc:AlternateContent></w:p>"#;
    assert_eq!(text_of(&files(&[("a.docx", &zip_of(&[("word/document.xml", xml)]))])), "Boxed");
}

#[test]
fn epub_chapters_with_encoded_names() {
    let epub = zip_of(&[
        ("META-INF/container.xml", br#"<container><rootfiles><rootfile full-path="b.opf"/></rootfiles></container>"#),
        ("b.opf", br#"<package><manifest><item id="1" href="%EC%9E%A5.xhtml"/></manifest><spine><itemref idref="1"/></spine></package>"#),
        ("장.xhtml", "<html><body><p>본문</p></body></html>".as_bytes()),
    ]);
    assert_eq!(text_of(&files(&[("b.epub", &epub)])), "본문");
}

#[test]
fn archives_skip_mac_junk_and_survive_wild_cells() {
    let c = files(&[("a.zip", &zip_of(&[("a.txt", b"alpha"), ("__MACOSX/._a.txt", b"\0\x05\x16\x07junk"), (".DS_Store", b"\0\0\0\x01Bud1")]))]);
    assert!(c.unreadable.is_empty());
    let sheet = br#"<worksheet><sheetData><row><c r="ZZZZZZZ1" t="inlineStr"><is><t>far</t></is></c></row><row><c r="A2" t="inlineStr"><is><t>near</t></is></c></row></sheetData></worksheet>"#;
    let x = files(&[("a.xlsx", &zip_of(&[("xl/workbook.xml", b"<workbook/>"), ("xl/worksheets/sheet1.xml", sheet)]))]);
    assert!(text_of(&x).contains("near"));
}

#[test]
fn any_name_gets_a_folder() {
    let id = one("chat.txt", "Ana: hi\nBob: hello\nAna: ok\nBob: bye\n");
    let root = tmp("names");
    let store = PersonaStore::new(&root);
    let long = "x".repeat(300);
    for n in ["🖤", "♡♡", "con", "NUL", long.as_str()] {
        store.create_persona(n, &id, &id.characters[0]).unwrap_or_else(|e| panic!("{n}: {e}"));
        assert_eq!(store.load_persona(n).unwrap().meta.name, n);
    }
    assert!(store.create_persona("  ", &id, &id.characters[0]).is_err());
    assert_eq!(store.list_personas().unwrap().len(), 5);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_renamed_persona_folder_still_works() {
    let id = one("chat.txt", "Ana: hi\nBob: hello\nAna: ok\nBob: bye\n");
    let root = tmp("moved");
    let store = PersonaStore::new(&root);
    store.create_persona("Ana", &id, &id.characters[0]).unwrap();
    std::fs::rename(root.join("ana"), root.join("ana_backup")).unwrap();
    let listed = store.list_personas().unwrap();
    assert_eq!(listed.len(), 1);
    assert!(store.load_persona(&listed[0].name).is_ok());
    assert!(store.path(&listed[0].name).is_ok());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_transcript_in_rows_links_what_was_said_to_the_answer() {
    let csv = "speaker,text\nMarta,Did you bring the oil?\nOtto,Two cans.\nMarta,And the ferry?\nOtto,Late as always.\n";
    let sharegpt = r#"{"conversations":[{"from":"human","value":"Did you bring the oil?"},{"from":"gpt","value":"Two cans."}]}"#;
    for (file, text, who) in [("t.csv", csv, "Otto"), ("t.json", sharegpt, "Assistant")] {
        let id = one(file, text);
        let root = tmp("rows");
        let c = id.characters.iter().find(|c| c.name == who).unwrap();
        let p = PersonaStore::new(&root).create_persona(who, &id, c).unwrap();
        let ctx = get_injection_context(&p, "did you bring the oil?", &[]);
        assert_eq!(ctx.exemplars[0].context, "Did you bring the oil?", "{file}");
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[test]
fn records_without_a_time_stay_beside_their_neighbours() {
    let id = one(
        "t.json",
        r#"[{"author":"A","text":"first","timestamp":1500000000},{"author":"B","text":"second"},{"author":"A","text":"third","timestamp":1500000060}]"#,
    );
    let texts: Vec<&str> = id.conversation.messages.iter().map(|m| m.text.as_str()).collect();
    assert_eq!(texts, ["first", "second", "third"]);
}

#[test]
fn phone_numbers_and_nicknames_beside_a_timestamp() {
    let id = one("w.txt", "12/31/20, 10:23 PM - +82 10-1234-5678: hi\n12/31/20, 10:24 PM - J.K ✨: yo\n12/31/20, 10:25 PM - +82 10-1234-5678: ok\n12/31/20, 10:26 PM - J.K ✨: bye\n");
    assert_eq!(names(&id), ["+82 10-1234-5678", "J.K ✨"]);
}

struct Recorder(std::sync::Mutex<Vec<ChatTurn>>);
impl ChatBackend for Recorder {
    fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![ModelInfo { name: "m".into(), capabilities: vec![] }])
    }
    fn chat(&self, _: &str, turns: &[ChatTurn], _: &GenParams, _: &mut dyn FnMut(&str)) -> Result<String> {
        *self.0.lock().unwrap() = turns.to_vec();
        Ok("응".into())
    }
}

#[test]
fn the_prompt_fits_the_context_window() {
    let long = "바람이 불고 파도가 높았다 ".repeat(18);
    let log: String = (0..40).map(|i| format!("민수: {i} {long}\n지수: {i} {long}\n")).collect();
    let id = one("k.txt", &log);
    let root = tmp("window");
    let c = id.characters.iter().find(|c| c.name == "민수").unwrap();
    let p = PersonaStore::new(&root).create_persona("민수", &id, c).unwrap();
    let backend = Recorder(Default::default());
    let settings = Settings { model: "m".into(), ..Default::default() };
    let history: Vec<ChatMessage> = (0..20).map(|_| ChatMessage { role: Role::User, text: long.clone() }).collect();
    Engine::new(&p, &backend, &settings).unwrap().reply(&history, "바람이 불어?", &mut |_| {}).unwrap();

    let sent = backend.0.lock().unwrap();
    let chars: usize = sent.iter().map(|t| t.content.chars().count()).sum();
    let room = (settings.gen.num_ctx as usize - settings.gen.max_tokens as usize) as f32;
    assert!((chars as f32) * 0.56 <= room, "{chars} characters");
    assert!(sent.len() > 2 && sent[0].content.contains("Real messages"));
    assert_eq!(sent.last().unwrap().content, "바람이 불어?");
    let _ = std::fs::remove_dir_all(&root);
}
