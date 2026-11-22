//! Who is in these documents? Identification turns a [`Corpus`] into selectable [`Character`]s, each
//! with the lines they speak.
//!
//! Sources that name their speakers — chat exports, datasets, chat logs, scripts — give certain
//! answers. In prose the speakers have to be worked out ([`prose`]), so the result is a set of options
//! the user can correct: the model turns [`IdentifyOptions::correction`] into edits of the list.

mod cast;
mod edits;
mod labeled;
mod layout;
mod model;
mod prose;
mod tags;

pub use model::ModelAccess;

use crate::ingest::{Content, Corpus, SYNTH_BASE_MS, SYNTH_RECORD_GAP_MS};
use crate::{Conversation, Error, Message, Result};
use cast::{Cast, Member};
use labeled::Turn;
use prose::{Para, Who};
use std::collections::HashSet;

const NARRATOR: &str = "Narrator";
const UNKNOWN: &str = "Unknown";
/// Long text is split at sentence ends: retrieval and prompts work on passages, not whole chapters.
const MAX_LINE_CHARS: usize = 500;
const MIN_QUOTES_FOR_DIALOGUE: usize = 3;
const MIN_NARRATION_LINES: usize = 5;
const SAMPLES: usize = 2;
const EXCERPTS: usize = 4;
const EXCERPT_CHARS: usize = 3000;

/// One selectable identity.
#[derive(Debug, Clone)]
pub struct Character {
    pub name: String,
    pub aliases: Vec<String>,
    /// Who they are, when the model said so.
    pub description: String,
    /// Sender of this character's lines in [`Identification::conversation`].
    pub sender: String,
    pub lines: usize,
    /// Lines the source explicitly gives them; the rest are inferred.
    pub certain: usize,
    pub samples: Vec<String>,
    /// Not a speaker but the voice of the text itself: a narrator, or a document without characters.
    pub voice: bool,
}

pub struct Identification {
    /// Name of the source; empty for chat logs.
    pub title: String,
    pub characters: Vec<Character>,
    /// Names known to be in the text with no lines of their own.
    pub mentioned: Vec<String>,
    /// Quotes that could not be attributed.
    pub unattributed: usize,
    pub notes: Vec<String>,
    /// Everything in order with senders resolved: the material personas are built from.
    pub conversation: Conversation,
}

/// A step of a long identification, for display.
pub struct Progress {
    pub stage: String,
    pub done: usize,
    pub total: usize,
}

#[derive(Default)]
pub struct IdentifyOptions<'a> {
    pub model: Option<ModelAccess<'a>>,
    /// What the user says is wrong with `previous`.
    pub correction: Option<&'a str>,
    /// The result being corrected.
    pub previous: Option<&'a Identification>,
}

/// Identify the characters of `corpus`. `progress` returns false to cancel.
pub fn identify(
    corpus: &Corpus,
    opts: &IdentifyOptions,
    progress: &mut dyn FnMut(Progress) -> bool,
) -> Result<Identification> {
    let mut sources: Vec<Source> = corpus.documents.iter().map(|d| Source::read(&d.content)).collect();
    let labelled_only = sources.iter().all(|s| !matches!(s, Source::Prose(_)));
    let source_name = corpus.documents.first().map_or(String::new(), |d| file_stem(&d.name));
    let mut notes = Vec::new();

    let members = match opts.correction {
        Some(correction) => corrected_members(opts, correction, progress)?,
        None => found_members(opts, &source_name, &sources, &mut notes, progress)?,
    };

    let cast = Cast::new(cast::merge(members));
    for source in sources.iter_mut() {
        if let Source::Prose(paras) = source {
            prose::attribute(paras, &cast);
        }
    }
    let mut built = Builder::new(cast.members.len());
    for source in &sources {
        built.add(source, &cast);
    }
    Ok(built.finish(cast, &source_name, labelled_only, notes))
}

/// The previous list with the user's correction applied.
fn corrected_members(
    opts: &IdentifyOptions,
    correction: &str,
    progress: &mut dyn FnMut(Progress) -> bool,
) -> Result<Vec<Member>> {
    let access = opts
        .model
        .as_ref()
        .ok_or_else(|| Error::Invalid("the model is needed to apply corrections, but it is not available".into()))?;
    if !progress(Progress { stage: "Applying your correction".into(), done: 0, total: 1 }) {
        return Err(Error::Invalid("cancelled".into()));
    }
    let listed = previous_members(opts.previous);
    let edits = model::correct(access, &listed, correction)?;
    edits.apply(listed, correction).ok_or_else(|| {
        Error::Invalid("that could not be turned into a change; try naming the characters you mean".into())
    })
}

/// Speakers the sources reveal, plus whoever the model finds in text that has dialogue.
fn found_members(
    opts: &IdentifyOptions,
    title: &str,
    sources: &[Source],
    notes: &mut Vec<String>,
    progress: &mut dyn FnMut(Progress) -> bool,
) -> Result<Vec<Member>> {
    let mut members = names_in_sources(sources);
    if !sources.iter().any(Source::has_dialogue) {
        return Ok(members);
    }
    let Some(access) = &opts.model else {
        notes.push("No model is available, so names come from speech patterns only.".to_string());
        return Ok(members);
    };
    let windows = excerpts(sources, EXCERPTS, EXCERPT_CHARS);
    let mut found = Vec::new();
    for (i, window) in windows.iter().enumerate() {
        let stage = Progress { stage: "Asking the model who is in the text".into(), done: i, total: windows.len() };
        if !progress(stage) {
            return Err(Error::Invalid("cancelled".into()));
        }
        match model::propose(access, title, window) {
            Ok(mut proposed) => found.append(&mut proposed),
            Err(e) => {
                notes.push(format!("The model could not finish, so some names may be missing: {e}"));
                break;
            }
        }
    }
    members.extend(occurring_in_text(found, sources));
    Ok(members)
}

enum Source<'a> {
    Speakers(&'a Conversation),
    Labeled(Vec<Turn>),
    Prose(Vec<Para>),
}

impl<'a> Source<'a> {
    fn read(content: &'a Content) -> Self {
        match content {
            Content::Speakers(conv) => Source::Speakers(conv),
            Content::Text(text) => match labeled::parse(text) {
                Some(turns) => Source::Labeled(turns),
                None => Source::Prose(prose::parse(&layout::paragraphs(text))),
            },
        }
    }

    fn has_dialogue(&self) -> bool {
        matches!(self, Source::Prose(paras) if prose::quote_count(paras) >= MIN_QUOTES_FOR_DIALOGUE)
    }

    /// Text in order, each passage flagged for whether it holds speech.
    fn passages(&self) -> Vec<(String, bool)> {
        match self {
            Source::Speakers(conv) => {
                conv.messages.iter().map(|m| (format!("{}: {}", m.sender, m.text), true)).collect()
            }
            Source::Labeled(turns) => turns
                .iter()
                .map(|t| {
                    let line = t.speaker.as_ref().map_or(t.text.clone(), |s| format!("{s}: {}", t.text));
                    (line, t.speaker.is_some())
                })
                .collect(),
            Source::Prose(paras) => paras
                .iter()
                .filter(|p| !p.heading)
                .map(|p| {
                    let parts: Vec<String> =
                        p.pieces.iter().map(|q| if q.quote { format!("“{}”", q.text) } else { q.text.clone() }).collect();
                    (parts.join(" "), p.pieces.iter().any(|q| q.quote))
                })
                .collect(),
        }
    }
}

/// `count` excerpts spread over the text, each starting at speech.
fn excerpts(sources: &[Source], count: usize, chars: usize) -> Vec<String> {
    let passages: Vec<(String, bool)> = sources.iter().flat_map(Source::passages).collect();
    let mut out: Vec<String> = Vec::new();
    let mut used_until = 0;
    for i in 0..count {
        let anchor = (i * passages.len() / count).max(used_until);
        let speech = (anchor..passages.len()).find(|&p| passages[p].1);
        let Some(start) = speech.or((anchor < passages.len()).then_some(anchor)) else { break };
        let (mut excerpt, mut size, mut p) = (String::new(), 0, start);
        while p < passages.len() && size < chars {
            excerpt.push_str(&passages[p].0);
            excerpt.push('\n');
            size += passages[p].0.chars().count();
            p += 1;
        }
        used_until = p;
        out.push(excerpt);
    }
    out
}

fn names_in_sources(sources: &[Source]) -> Vec<Member> {
    let mut names: Vec<String> = Vec::new();
    for s in sources {
        match s {
            Source::Speakers(conv) => names.extend(conv.participants.iter().cloned()),
            Source::Labeled(turns) => names.extend(turns.iter().filter_map(|t| t.speaker.clone())),
            Source::Prose(paras) => {
                // in a long text a lone tag could be a misreading; in a short one it is all there is
                let once_is_enough = prose::quote_count(paras) < 40;
                let tagged = prose::tagged_names(paras);
                names.extend(tagged.into_iter().filter(|(_, n)| once_is_enough || *n >= 2).map(|(name, _)| name));
            }
        }
    }
    let mut unique: Vec<String> = names.into_iter().collect::<HashSet<_>>().into_iter().collect();
    unique.sort();
    unique.into_iter().map(|name| Member { name, ..Default::default() }).collect()
}

fn previous_members(previous: Option<&Identification>) -> Vec<Member> {
    let Some(p) = previous else { return Vec::new() };
    p.characters
        .iter()
        .filter(|c| !c.voice)
        .map(|c| Member { name: c.name.clone(), aliases: c.aliases.clone(), description: c.description.clone() })
        .chain(p.mentioned.iter().map(|n| Member { name: n.clone(), ..Default::default() }))
        .collect()
}

/// Models invent names; keep only those that really occur in the text.
fn occurring_in_text(mut found: Vec<Member>, sources: &[Source]) -> Vec<Member> {
    let text: String = sources.iter().flat_map(Source::passages).map(|p| p.0.to_lowercase() + "\n").collect();
    let occurs = |name: &str| text.contains(&name.to_lowercase());
    found.iter_mut().for_each(|m| m.aliases.retain(|a| occurs(a)));
    found.retain(|m| occurs(&m.name) && !m.name.eq_ignore_ascii_case(NARRATOR));
    found
}

fn file_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.rsplit_once('.').map_or(base, |x| x.0).to_string()
}

#[derive(Default)]
struct Tally {
    lines: usize,
    certain: usize,
    sure_samples: Vec<String>,
    other_samples: Vec<String>,
}

impl Tally {
    fn add(&mut self, text: &str, certain: bool) {
        self.lines += 1;
        self.certain += usize::from(certain);
        if !(15..=160).contains(&text.chars().count()) {
            return;
        }
        let line = text.replace('\n', " ");
        let seen = self.sure_samples.contains(&line) || self.other_samples.contains(&line);
        let samples = if certain { &mut self.sure_samples } else { &mut self.other_samples };
        if samples.len() < SAMPLES && !seen {
            samples.push(line);
        }
    }

    fn samples(self) -> Vec<String> {
        self.sure_samples.into_iter().chain(self.other_samples).take(SAMPLES).collect()
    }
}

/// Lays every source out as one message stream and counts who said what.
struct Builder {
    messages: Vec<Message>,
    clock: i64,
    tallies: Vec<Tally>,
    narrator: Tally,
    skipped: usize,
    unattributed: usize,
}

impl Builder {
    fn new(members: usize) -> Self {
        Builder {
            messages: Vec::new(),
            clock: SYNTH_BASE_MS,
            tallies: (0..members).map(|_| Tally::default()).collect(),
            narrator: Tally::default(),
            skipped: 0,
            unattributed: 0,
        }
    }

    fn push(&mut self, who: Who, sender: &str, text: &str, certain: bool, timestamp: Option<i64>) {
        for piece in layout::chunks(text, MAX_LINE_CHARS) {
            match who {
                Who::Character(i) => self.tallies[i].add(&piece, certain),
                Who::Narrator => self.narrator.add(&piece, certain),
                Who::Unknown => {}
            }
            self.clock += 1000;
            let timestamp_ms = timestamp.unwrap_or(self.clock);
            self.messages.push(Message { sender: sender.to_string(), timestamp_ms, text: piece });
        }
    }

    fn scene_break(&mut self) {
        self.clock += SYNTH_RECORD_GAP_MS;
    }

    fn add(&mut self, source: &Source, cast: &Cast) {
        self.scene_break();
        match source {
            Source::Speakers(conv) => {
                self.skipped += conv.skipped;
                for m in &conv.messages {
                    let (who, sender) = resolve(cast, &m.sender);
                    self.push(who, &sender, &m.text, true, Some(m.timestamp_ms));
                }
            }
            Source::Labeled(turns) => {
                for t in turns {
                    match &t.speaker {
                        Some(name) => {
                            let (who, sender) = resolve(cast, name);
                            self.push(who, &sender, &t.text, true, None);
                        }
                        None => self.push(Who::Narrator, NARRATOR, &t.text, false, None),
                    }
                }
            }
            Source::Prose(paras) => self.add_prose(paras, cast, source.has_dialogue()),
        }
    }

    fn add_prose(&mut self, paras: &[Para], cast: &Cast, dialogue: bool) {
        for para in paras {
            if para.heading {
                self.scene_break();
            }
            for p in &para.pieces {
                if !p.quote {
                    // a fragment that is only a speech tag belongs to the quote, not to the story
                    if !prose::is_bare_tag(&p.text) || para.pieces.len() == 1 {
                        self.push(Who::Narrator, NARRATOR, &p.text, false, None);
                    }
                    continue;
                }
                match p.who {
                    Who::Character(i) => {
                        let name = cast.members[i].name.clone();
                        self.push(p.who, &name, &p.text, p.certain, None);
                    }
                    Who::Narrator => self.push(p.who, NARRATOR, &p.text, p.certain, None),
                    Who::Unknown if dialogue => {
                        self.unattributed += 1;
                        self.push(p.who, UNKNOWN, &p.text, false, None);
                    }
                    // without dialogue a stray quotation is just part of the prose
                    Who::Unknown => self.push(Who::Narrator, NARRATOR, &p.text, false, None),
                }
            }
        }
    }

    fn finish(self, cast: Cast, source_name: &str, labelled_only: bool, notes: Vec<String>) -> Identification {
        let mut characters = Vec::new();
        let mut mentioned = Vec::new();
        for (member, tally) in cast.members.into_iter().zip(self.tallies) {
            if tally.lines == 0 {
                mentioned.push(member.name);
                continue;
            }
            characters.push(Character {
                sender: member.name.clone(),
                name: member.name,
                aliases: member.aliases,
                description: member.description,
                lines: tally.lines,
                certain: tally.certain,
                samples: tally.samples(),
                voice: false,
            });
        }
        characters.sort_by(|a, b| b.lines.cmp(&a.lines).then_with(|| a.name.cmp(&b.name)));

        // the text's own voice: an option beside the characters, or the only one
        let alone = characters.is_empty();
        if alone || self.narrator.lines >= MIN_NARRATION_LINES {
            let (name, description) = match alone {
                true => (source_name.to_string(), "The text as a whole"),
                false => (NARRATOR.to_string(), "The narrative voice of the text"),
            };
            characters.push(Character {
                name,
                aliases: Vec::new(),
                description: description.to_string(),
                sender: NARRATOR.to_string(),
                lines: self.narrator.lines,
                certain: self.narrator.certain,
                samples: self.narrator.samples(),
                voice: true,
            });
        }

        let mut participants: Vec<String> = self.messages.iter().map(|m| m.sender.clone()).collect();
        participants.sort();
        participants.dedup();
        Identification {
            title: if labelled_only { String::new() } else { source_name.to_string() },
            characters,
            mentioned,
            unattributed: self.unattributed,
            notes,
            conversation: Conversation { participants, messages: self.messages, skipped: self.skipped },
        }
    }
}

fn resolve(cast: &Cast, name: &str) -> (Who, String) {
    match cast.find(name) {
        Some(i) => (Who::Character(i), cast.members[i].name.clone()),
        None => (Who::Unknown, name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::ingest_files;
    use crate::{ChatBackend, ChatTurn, GenParams, ModelInfo, ParseOptions};
    use std::sync::Mutex;

    fn corpus(name: &str, text: &str) -> Corpus {
        ingest_files(&[(name.to_string(), text.as_bytes().to_vec())], &ParseOptions::default()).unwrap()
    }

    fn run(c: &Corpus) -> Identification {
        identify(c, &IdentifyOptions::default(), &mut |_| true).unwrap()
    }

    fn names(id: &Identification) -> Vec<&str> {
        id.characters.iter().map(|c| c.name.as_str()).collect()
    }

    const STORY: &str = "“You’re late,” said Tobias.\n\n“The ferry was late,” Marta replied.\n\n“Did you bring the oil?”\n\n\
        “Two cans.”\n\nThe storm kept building over the water all through the afternoon.\n\n“Good,” said Tobias.\n\n\
        Night came early. The gulls had gone. The sea was grey. The lamp was lit. Nobody spoke for a long while.\n\n\
        The wind rose. The stairs creaked. The rain began. The old tower shook. The night went on.\n\n\
        The sea rose. The door banged. Morning came slowly. The storm passed.\n\nThey ate in silence by the stove.";

    #[test]
    fn prose_offers_characters_and_the_narrator() {
        let id = run(&corpus("keeper.txt", STORY));
        assert_eq!(names(&id), ["Tobias", "Marta", "Narrator"]);
        let tobias = &id.characters[0];
        assert_eq!((tobias.lines, tobias.certain), (3, 2)); // one line inferred from turn-taking
        assert!(id.characters[2].voice);
        assert_eq!(id.title, "keeper");
        assert_eq!(id.unattributed, 0);
        assert!(id.notes[0].contains("No model"));
    }

    #[test]
    fn text_without_dialogue_is_its_own_voice() {
        let id = run(&corpus("Lamp Manual.md", "# Lamps\n\nClean the lens weekly.\n\nCheck the oil before dusk."));
        assert_eq!(names(&id), ["Lamp Manual"]);
        assert!(id.characters[0].voice);
        assert_eq!(id.characters[0].sender, "Narrator");
    }

    #[test]
    fn labelled_text_is_certain() {
        let id = run(&corpus("chat.txt", "Ana: hi\nBob: hello\nAna: ok\nBob: bye\n"));
        assert_eq!(names(&id), ["Ana", "Bob"]);
        assert_eq!(id.characters[0].certain, 2);
        assert_eq!(id.title, "");
    }

    #[test]
    fn long_narration_is_split_not_cut() {
        let long = "A sentence of moderate length follows here. ".repeat(40);
        let id = run(&corpus("essay.txt", &long));
        let total: usize = id.conversation.messages.iter().map(|m| m.text.len()).sum();
        assert!(id.conversation.messages.len() >= 3);
        assert!(id.conversation.messages.iter().all(|m| m.text.chars().count() <= MAX_LINE_CHARS));
        assert!(total + id.conversation.messages.len() >= long.trim().len());
    }

    struct FakeModel {
        replies: Mutex<Vec<String>>,
        prompts: Mutex<Vec<String>>,
    }

    impl ChatBackend for FakeModel {
        fn list_models(&self) -> Result<Vec<ModelInfo>> {
            Ok(vec![])
        }
        fn chat(&self, _m: &str, turns: &[ChatTurn], p: &GenParams, _t: &mut dyn FnMut(&str)) -> Result<String> {
            assert!(p.json_schema.is_some());
            self.prompts.lock().unwrap().push(turns[1].content.clone());
            Ok(self.replies.lock().unwrap().pop().unwrap_or_else(|| r#"{"characters":[]}"#.to_string()))
        }
    }

    fn model(replies: &[&str]) -> FakeModel {
        FakeModel { replies: Mutex::new(replies.iter().map(|r| r.to_string()).collect()), prompts: Mutex::new(vec![]) }
    }

    fn access(m: &FakeModel) -> ModelAccess<'_> {
        ModelAccess { backend: m, model: "m".into(), gen: GenParams::default() }
    }

    #[test]
    fn the_model_adds_names_and_aliases_but_not_inventions() {
        let text = "“Hello,” Lizzy said.\n\n“Good day,” said Elizabeth.\n\n“Who is there?” Darcy asked.\n";
        let reply = r#"{"characters":[{"name":"Elizabeth","aliases":["Lizzy","Betsy"],"role":"the heroine"},{"name":"Zorro","aliases":[],"role":"invented"}]}"#;
        let m = model(&[reply]);
        let opts = IdentifyOptions { model: Some(access(&m)), ..Default::default() };
        let id = identify(&corpus("b.txt", text), &opts, &mut |_| true).unwrap();
        let e = id.characters.iter().find(|c| c.name == "Elizabeth").unwrap();
        assert_eq!(e.lines, 2); // Lizzy merged into Elizabeth
        assert_eq!(e.description, "the heroine");
        assert!(!e.aliases.contains(&"Betsy".to_string()));
        assert!(!names(&id).contains(&"Zorro"));
    }

    #[test]
    fn corrections_edit_the_list_and_need_the_model() {
        let c = corpus("keeper.txt", STORY);
        let first = run(&c);
        let correction = Some("Marta is not a character, and Tobias is the keeper");

        let offline = IdentifyOptions { correction, previous: Some(&first), model: None };
        assert!(identify(&c, &offline, &mut |_| true).is_err());

        let m = model(&[r#"{"remove":["Marta"],"merge":[],"rename":[],"add":[],"aliases":[{"name":"Tobias","aliases":["the keeper"]}]}"#]);
        let opts = IdentifyOptions { correction, previous: Some(&first), model: Some(access(&m)) };
        let id = identify(&c, &opts, &mut |_| true).unwrap();
        assert!(names(&id).contains(&"Tobias"));
        assert!(!names(&id).contains(&"Marta"));
        assert_eq!(id.characters[0].aliases, ["the keeper"]);
        let prompt = &m.prompts.lock().unwrap()[0];
        assert!(prompt.contains("Marta is not a character") && prompt.contains("- Marta"));

        // a correction builds on the previous result: what was removed stays removed
        let m = model(&[r#"{"remove":[],"merge":[],"rename":[{"from":"Tobias","to":"Old Tobias"}],"add":[],"aliases":[]}"#]);
        let opts = IdentifyOptions { correction: Some("he is Old Tobias"), previous: Some(&id), model: Some(access(&m)) };
        let again = identify(&c, &opts, &mut |_| true).unwrap();
        assert!(names(&again).contains(&"Old Tobias") && !names(&again).contains(&"Marta"));
    }

    #[test]
    fn a_correction_with_no_meaning_is_an_error() {
        let c = corpus("keeper.txt", STORY);
        let first = run(&c);
        let m = model(&[r#"{"remove":[],"merge":[],"rename":[],"add":[],"aliases":[]}"#]);
        let opts = IdentifyOptions { correction: Some("hmm"), previous: Some(&first), model: Some(access(&m)) };
        assert!(identify(&c, &opts, &mut |_| true).is_err());
    }

    #[test]
    fn progress_can_cancel() {
        let m = model(&[]);
        let opts = IdentifyOptions { model: Some(access(&m)), ..Default::default() };
        assert!(identify(&corpus("k.txt", STORY), &opts, &mut |_| false).is_err());
    }

    #[test]
    fn excerpts_spread_and_start_at_speech() {
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("Plain narration number {i} goes along here.\n\n“Spoken line {i},” said Tobias.\n\n"));
        }
        let c = corpus("long.txt", &text);
        let sources: Vec<Source> = c.documents.iter().map(|d| Source::read(&d.content)).collect();
        let windows = excerpts(&sources, 4, 500);
        assert_eq!(windows.len(), 4);
        assert!(windows.iter().all(|w| w.starts_with('“')));
        assert!(windows[3].contains("Spoken line 1"));
    }
}
