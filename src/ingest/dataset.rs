//! JSON and table records -> speaker-labelled messages.
//!
//! An Instagram DM export (`{participants, messages: [{sender_name, timestamp_ms, content}]}`) is
//! deserialized directly. Anything else is walked as a JSON tree — JSON Lines and concatenated values
//! included — looking for message objects, prompt/response pairs and plain strings.

use super::text::{clean, fix_mojibake, is_system_line};
use super::ParseOptions;
use crate::{Conversation, Error, Message, Result};
use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

const GENERAL_SENDER: &str = "Dataset";
const MAX_DEPTH: usize = 12;

pub(crate) const SYNTH_BASE_MS: i64 = 1_700_000_000_000;
pub(crate) const SYNTH_RECORD_GAP_MS: i64 = 2 * 3_600_000; // longer than retrieval's 1 h turn gap

// Recognised field names, matched case-insensitively, first hit wins.
const TEXT_KEYS: &[&str] =
    &["content", "text", "message", "utterance", "value", "body", "msg", "sentence", "dialogue", "line", "quote"];
const SENDER_KEYS: &[&str] = &["sender_name", "sender", "author", "speaker", "character", "from", "username", "user"];
const PROMPT_KEYS: &[&str] = &["instruction", "prompt", "question", "query", "input"];
const ANSWER_KEYS: &[&str] = &["output", "response", "answer", "completion", "reply"];
/// Arrays under these keys hold the messages of one conversation.
const CONVERSATION_KEYS: &[&str] = &[
    "messages", "conversation", "conversations", "chat_messages", "dialog", "dialogue", "turns", "history", "chat",
];
const TIME_KEYS: &[&str] = &["timestamp", "create_time", "created_at", "date_unixtime"];

/// A parsed file with its name and format.
pub(crate) type ParsedFile = (String, &'static str, Parsed);

/// The files of one conversation, merged.
pub(crate) struct Merged {
    pub name: String,
    pub format: &'static str,
    pub conversation: Conversation,
    /// False when no message says who wrote it.
    pub has_senders: bool,
}

/// Files listing the same participants form one conversation, and so do files listing none.
/// Each conversation comes back chronological and deduplicated.
pub(crate) fn merge(files: Vec<ParsedFile>) -> Vec<Merged> {
    let mut groups: Vec<(Option<Vec<String>>, Vec<ParsedFile>)> = Vec::new();
    for file in files {
        match groups.iter_mut().find(|g| g.0 == file.2.participants) {
            Some(group) => group.1.push(file),
            None => groups.push((file.2.participants.clone(), vec![file])),
        }
    }
    groups.into_iter().map(|(participants, files)| merge_group(participants, files)).collect()
}

fn merge_group(participants: Option<Vec<String>>, files: Vec<ParsedFile>) -> Merged {
    let (name, format) = (files[0].0.clone(), files[0].1);
    let mut conv = Conversation::default();
    let mut has_senders = false;
    let mut next_synthetic = SYNTH_BASE_MS;
    for (_, _, mut file) in files {
        conv.skipped += file.skipped;
        has_senders |= file.saw_sender;
        if !file.real_timestamps {
            // place such files one after another instead of letting their synthetic clocks interleave
            let shift = next_synthetic - file.messages.first().map_or(0, |m| m.timestamp_ms);
            file.messages.iter_mut().for_each(|m| m.timestamp_ms += shift);
            next_synthetic = file.messages.last().map_or(next_synthetic, |m| m.timestamp_ms + SYNTH_RECORD_GAP_MS);
        }
        conv.messages.extend(file.messages);
    }

    // stable, so equal timestamps keep the order they were read in
    conv.messages.sort_by_key(|m| m.timestamp_ms);
    let total = conv.messages.len();
    conv.messages = dedup_sorted(std::mem::take(&mut conv.messages));
    conv.skipped += total - conv.messages.len();

    conv.participants = participants.unwrap_or_else(|| {
        let mut senders: Vec<String> = conv.messages.iter().map(|m| m.sender.clone()).collect();
        senders.sort();
        senders.dedup();
        senders
    });
    Merged { name, format, conversation: conv, has_senders }
}

/// Drop identical repeats, e.g. from overlapping export files. Compares only within a run of equal
/// timestamps, so it stays linear.
fn dedup_sorted(msgs: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(msgs.len());
    let mut run_start = 0;
    for m in msgs {
        if out.last().is_some_and(|l| l.timestamp_ms != m.timestamp_ms) {
            run_start = out.len();
        }
        if !out[run_start..].iter().any(|x| x.sender == m.sender && x.text == m.text) {
            out.push(m);
        }
    }
    out
}

/// Treat a value of the wrong type as absent instead of failing the whole file.
fn lenient<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> std::result::Result<Option<T>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Maybe<T> {
        Value(T),
        Other(IgnoredAny),
    }
    Ok(match Maybe::<T>::deserialize(d)? {
        Maybe::Value(v) => Some(v),
        Maybe::Other(_) => None,
    })
}

#[derive(Deserialize)]
struct ExportFile {
    #[serde(default)]
    participants: Vec<ExportParticipant>,
    messages: Vec<ExportMessage>,
}

#[derive(Deserialize)]
struct ExportParticipant {
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
}

#[derive(Deserialize)]
struct ExportMessage {
    #[serde(default, deserialize_with = "lenient")]
    sender_name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    timestamp_ms: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    content: Option<String>,
}

fn export_message(m: ExportMessage, opts: &ParseOptions) -> Option<Message> {
    let sender = fix_mojibake(&m.sender_name?);
    let content = fix_mojibake(&m.content?);
    if is_system_line(&content, opts) {
        return None;
    }
    Some(Message { sender, timestamp_ms: m.timestamp_ms?, text: clean(&content)? })
}

/// Timestamps for records that carry none: a second apart within a group, two hours between groups,
/// so retrieval sees each group as its own turn.
#[derive(Default)]
struct Clock {
    group: i64,
    index: i64,
}

impl Clock {
    fn new_group(&mut self) {
        self.group += 1;
        self.index = 0;
    }

    fn tick(&mut self) -> i64 {
        self.index += 1;
        SYNTH_BASE_MS + self.group * SYNTH_RECORD_GAP_MS + (self.index - 1) * 1000
    }
}

/// A message-shaped thing found while walking a JSON tree.
enum Record {
    Msg { sender: String, explicit: bool, text: String, ts: Option<i64> },
    Pair { prompt: String, answer: String },
    /// System prompts and tool output: no one's words.
    NotSpeech,
}

/// One parsed file.
#[derive(Default)]
pub(crate) struct Parsed {
    /// `None` when the file has no participants list; then they are derived from the senders.
    pub participants: Option<Vec<String>>,
    pub messages: Vec<Message>,
    pub skipped: usize,
    /// Some message carried real sender info: an export, a sender field or a role.
    pub saw_sender: bool,
    real_timestamps: bool,
    last_time: Option<i64>,
    clock: Clock,
}

/// Parse one JSON / JSON Lines file.
pub(crate) fn parse_json(bytes: &[u8], opts: &ParseOptions) -> Result<Parsed> {
    let mut parsed = Parsed::default();
    if let Ok(export) = serde_json::from_slice::<ExportFile>(bytes) {
        if parsed.add_export(export, opts)? {
            return Ok(parsed);
        }
    }
    let values: Vec<Value> = serde_json::Deserializer::from_slice(bytes)
        .into_iter()
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| Error::Json(e.to_string()))?;
    match &values[..] {
        [one] => parsed.walk(one, 0, opts)?,
        lines => parsed.walk_array(lines, false, 0, opts)?,
    }
    if parsed.messages.is_empty() {
        return Err(Error::Invalid(
            "no usable text found. Supported: Instagram exports, role+content messages, \
             prompt+response pairs, objects with a text/content field, or plain lists of strings"
                .into(),
        ));
    }
    Ok(parsed)
}

/// Parse table rows given as objects keyed by column name.
pub(crate) fn parse_rows(rows: &[Value], opts: &ParseOptions) -> Parsed {
    let mut parsed = Parsed::default();
    // cannot fail: only the export layout, which rows never have, reports errors
    let _ = parsed.walk_array(rows, false, 0, opts);
    parsed
}

impl Parsed {
    /// Add an export-layout object. `Ok(false)` when its messages are not export messages at all,
    /// e.g. `{"messages": [{"role": .., "content": ..}]}`: the caller then walks it generically.
    fn add_export(&mut self, export: ExportFile, opts: &ParseOptions) -> Result<bool> {
        let total = export.messages.len();
        // the export is newest-first
        let msgs: Vec<Message> = export.messages.into_iter().rev().filter_map(|m| export_message(m, opts)).collect();
        if msgs.is_empty() && total > 0 {
            return Ok(false);
        }
        let mut names: Vec<String> =
            export.participants.into_iter().filter_map(|p| p.name).map(|n| fix_mojibake(&n)).collect();
        names.sort();
        match &self.participants {
            Some(prev) if *prev != names => {
                return Err(Error::Invalid("this file contains more than one conversation".into()))
            }
            _ => self.participants = Some(names),
        }
        self.saw_sender = true;
        self.real_timestamps = true;
        self.skipped += total - msgs.len();
        self.messages.extend(msgs);
        Ok(true)
    }

    fn walk(&mut self, v: &Value, depth: usize, opts: &ParseOptions) -> Result<()> {
        if depth > MAX_DEPTH {
            return Ok(());
        }
        match v {
            Value::String(s) => {
                self.clock.new_group();
                self.add_text(GENERAL_SENDER, false, s, None, opts);
            }
            Value::Array(items) => self.walk_array(items, false, depth, opts)?,
            Value::Object(o) => {
                let export_shaped = matches!(o.get("participants"), Some(Value::Array(_)))
                    && matches!(o.get("messages"), Some(Value::Array(_)));
                if export_shaped {
                    if let Ok(export) = ExportFile::deserialize(v) {
                        if self.add_export(export, opts)? {
                            return Ok(());
                        }
                    }
                }
                if let Some(r) = record(o) {
                    self.clock.new_group();
                    self.add_record(r, opts);
                    return Ok(());
                }
                for (key, child) in o {
                    match child {
                        Value::Array(items) => {
                            let one_conversation = CONVERSATION_KEYS.iter().any(|c| key.eq_ignore_ascii_case(c));
                            self.walk_array(items, one_conversation, depth + 1, opts)?
                        }
                        Value::Object(_) => self.walk(child, depth + 1, opts)?,
                        _ => {} // metadata
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// `one_conversation`: every element belongs to one group; otherwise each is its own, unless they
    /// read as people talking to each other.
    fn walk_array(&mut self, items: &[Value], one_conversation: bool, depth: usize, opts: &ParseOptions) -> Result<()> {
        let one_conversation = one_conversation || is_conversation(items);
        if one_conversation {
            self.clock.new_group();
        }
        for item in items {
            let found = match item {
                Value::String(s) => {
                    Some(Record::Msg { sender: GENERAL_SENDER.into(), explicit: false, text: s.clone(), ts: None })
                }
                Value::Object(o) => record(o),
                _ => None,
            };
            match found {
                Some(r) => {
                    if !one_conversation {
                        self.clock.new_group();
                    }
                    self.add_record(r, opts);
                }
                None if item.is_object() || item.is_array() => self.walk(item, depth + 1, opts)?,
                None => {}
            }
        }
        Ok(())
    }

    fn add_record(&mut self, r: Record, opts: &ParseOptions) {
        match r {
            Record::Msg { sender, explicit, text, ts } => self.add_text(&sender, explicit, &text, ts, opts),
            Record::Pair { prompt, answer } => {
                self.add_text("User", true, &prompt, None, opts);
                self.add_text("Assistant", true, &answer, None, opts);
            }
            Record::NotSpeech => self.skipped += 1,
        }
    }

    fn add_text(&mut self, sender: &str, explicit: bool, text: &str, ts: Option<i64>, opts: &ParseOptions) {
        let content = fix_mojibake(text);
        let Some(text) = (if is_system_line(&content, opts) { None } else { clean(&content) }) else {
            self.skipped += 1;
            return;
        };
        self.real_timestamps |= ts.is_some();
        self.saw_sender |= explicit;
        self.last_time = ts.or(self.last_time); // a record without a time stays beside the one before it
        let timestamp_ms = self.last_time.unwrap_or_else(|| self.clock.tick());
        self.messages.push(Message { sender: sender.to_string(), timestamp_ms, text });
    }
}

fn pick<'a>(o: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| o.iter().find(|(n, v)| n.eq_ignore_ascii_case(k) && !v.is_null()).map(|(_, v)| v))
}

/// A string, an array of strings or typed parts (`{"type": "text", "text": ..}`), or an object holding such `parts`.
fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => {
            let typed = |p: &Value| p.get("type").is_some_and(Value::is_string);
            let parts: Vec<&str> = a
                .iter()
                .filter_map(|p| p.as_str().or_else(|| p.get("text").filter(|_| typed(p)).and_then(Value::as_str)))
                .collect();
            // plain and typed parts together are one line cut up by its formatting
            let one_line = a.iter().any(Value::is_string) && a.iter().any(Value::is_object);
            (!parts.is_empty()).then(|| parts.join(if one_line { "" } else { "\n" }))
        }
        Value::Object(o) => o.get("parts").and_then(text_of),
        _ => None,
    }
}

/// A sender given as a string or as an object describing them.
fn name_of(v: &Value) -> Option<&str> {
    let text = |v: &Value| v.as_str().is_some_and(|s| !s.trim().is_empty());
    match v {
        Value::Object(o) => {
            let role = o.get("role").and_then(Value::as_str).filter(|r| is_non_speaker(r));
            role.or_else(|| {
                ["name", "display_name", "username", "first_name", "role"]
                    .iter()
                    .find_map(|k| o.get(*k).filter(|v| text(v)).and_then(Value::as_str))
            })
        }
        _ => v.as_str(),
    }
    .map(str::trim)
    .filter(|s| !s.is_empty())
}

/// Milliseconds, from a number (or a string holding one) in seconds or milliseconds.
fn time_of(o: &Map<String, Value>) -> Option<i64> {
    let number = |v: &Value| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok());
    pick(o, &["timestamp_ms"]).and_then(Value::as_i64).or_else(|| {
        let t = TIME_KEYS.iter().find_map(|k| pick(o, &[k]).and_then(number)).filter(|t| *t >= 1e8)?;
        Some(if t < 1e11 { (t * 1000.0) as i64 } else { t as i64 })
    })
}

/// Do a few people keep coming back in these records?
fn is_conversation(items: &[Value]) -> bool {
    const LOOK_AT: usize = 50;
    let senders: Vec<String> = items
        .iter()
        .take(LOOK_AT)
        .filter_map(|v| match record(v.as_object()?)? {
            Record::Msg { sender, explicit: true, .. } => Some(sender),
            _ => None,
        })
        .collect();
    let distinct = senders.iter().collect::<std::collections::HashSet<_>>().len();
    distinct >= 2 && (distinct <= 4 || distinct * 2 <= senders.len())
}

/// Chat roles used by common dataset formats -> display sender.
fn role_sender(r: &str) -> Option<&'static str> {
    match r.trim().to_ascii_lowercase().as_str() {
        "user" | "human" | "customer" | "prompter" | "client" => Some("User"),
        "assistant" | "gpt" | "bot" | "model" | "ai" | "chatbot" => Some("Assistant"),
        _ => None,
    }
}

/// System prompts and tool output are not anyone's voice.
fn is_non_speaker(r: &str) -> bool {
    matches!(r.trim().to_ascii_lowercase().as_str(), "system" | "tool" | "function" | "developer")
}

fn record(o: &Map<String, Value>) -> Option<Record> {
    if let Some(answer) = pick(o, ANSWER_KEYS).and_then(text_of) {
        let prompt: Vec<String> = PROMPT_KEYS
            .iter()
            .filter_map(|k| pick(o, &[k]).and_then(text_of))
            .filter(|s| !s.trim().is_empty())
            .collect();
        if !prompt.is_empty() {
            return Some(Record::Pair { prompt: prompt.join("\n"), answer });
        }
    }
    let text = pick(o, TEXT_KEYS).and_then(text_of)?;
    let ts = time_of(o);
    let named = SENDER_KEYS.iter().find_map(|k| pick(o, &[k]).and_then(name_of));
    let role = pick(o, &["role"]).and_then(Value::as_str);
    let (sender, explicit) = match named.or(role) {
        Some(s) if is_non_speaker(s) => return Some(Record::NotSpeech),
        Some(s) => match role_sender(s) {
            Some(mapped) => (mapped.to_string(), true),
            None if named.is_some() => (fix_mojibake(s), true),
            None => (GENERAL_SENDER.to_string(), false), // an unknown role value
        },
        None => (GENERAL_SENDER.to_string(), false),
    };
    Some(Record::Msg { sender, explicit, text, ts })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(content: &str) -> Merged {
        let parsed = parse_json(content.as_bytes(), &ParseOptions::default()).unwrap();
        merge(vec![("t.json".to_string(), "JSON", parsed)]).remove(0)
    }

    fn texts(m: &Merged) -> Vec<&str> {
        m.conversation.messages.iter().map(|m| m.text.as_str()).collect()
    }

    #[test]
    fn export_malformed_entries_drop_only_themselves() {
        let m = one(
            r#"{"participants":[{"name":"A"},{"name":"B"}],"messages":[
              {"sender_name":"B","timestamp_ms":3,"content":"last"},
              {"sender_name":"B","timestamp_ms":"oops","content":"bad ts"},
              {"sender_name":"B","timestamp_ms":2,"content":42},
              {"timestamp_ms":2,"content":"no sender"},
              {"sender_name":"A","timestamp_ms":1,"content":"first"},
              {"sender_name":"A","timestamp_ms":1,"content":"first"}]}"#,
        );
        assert_eq!(texts(&m), ["first", "last"]);
        assert_eq!(m.conversation.skipped, 4);
        assert!(m.has_senders);
        assert!(parse_json(br#"{"participants":[]}"#, &ParseOptions::default()).is_err());
    }

    #[test]
    fn json_lines_arrays_and_concatenation() {
        let m = one("{\"sender_name\":\"A\",\"timestamp_ms\":1,\"content\":\"one\"}\n{\"sender_name\":\"B\",\"timestamp_ms\":2,\"content\":\"two\"}\n");
        assert_eq!(m.conversation.participants, ["A", "B"]); // derived from the senders
        let m = one(r#"[{"sender_name":"A","timestamp_ms":2,"content":"b"},{"sender_name":"A","timestamp_ms":1,"content":"a"}]"#);
        assert_eq!(texts(&m), ["a", "b"]);
        let two = r#"{"participants":[{"name":"A"},{"name":"B"}],"messages":[{"sender_name":"B","timestamp_ms":5,"content":"hi"}]}
{"participants":[{"name":"A"},{"name":"B"}],"messages":[{"sender_name":"A","timestamp_ms":6,"content":"yo"}]}"#;
        assert_eq!(texts(&one(two)).len(), 2);
        let mixed = r#"{"participants":[{"name":"A"}],"messages":[{"sender_name":"A","timestamp_ms":1,"content":"x"}]}
{"participants":[{"name":"Z"}],"messages":[{"sender_name":"Z","timestamp_ms":2,"content":"y"}]}"#;
        assert!(matches!(parse_json(mixed.as_bytes(), &ParseOptions::default()), Err(Error::Invalid(_))));
        assert!(matches!(parse_json(b"not json", &ParseOptions::default()), Err(Error::Json(_))));
    }

    #[test]
    fn records_without_senders() {
        let m = one(r#"["first text", "second text", ""]"#);
        assert!(!m.has_senders);
        assert_eq!((m.conversation.messages.len(), m.conversation.skipped), (2, 1));
        let gap = m.conversation.messages[1].timestamp_ms - m.conversation.messages[0].timestamp_ms;
        assert!(gap > 3_600_000); // separate turns
        let m = one(r#"{"name":"x","data":[{"text":"alpha"},{"text":"beta","id":3}]}"#);
        assert!(!m.has_senders);
        assert_eq!(texts(&m), ["alpha", "beta"]);
    }

    #[test]
    fn role_based_chats() {
        // a top-level {"messages": [role/content]} shares the export's key but is not an export
        let m = one(r#"{"messages":[{"role":"system","content":"be nice"},{"role":"user","content":"hey"},{"role":"assistant","content":"yo"}]}"#);
        assert!(m.has_senders);
        assert_eq!(m.conversation.messages.iter().map(|m| m.sender.as_str()).collect::<Vec<_>>(), ["User", "Assistant"]);
        let m = one(r#"[{"conversations":[{"from":"system","value":"s"},{"from":"human","value":"q"},{"from":"gpt","value":"a"}]}]"#);
        assert_eq!(texts(&m), ["q", "a"]);
    }

    #[test]
    fn prompt_response_pairs() {
        let m = one(r#"[{"instruction":"say hi","input":"politely","output":"hello"}]"#);
        assert!(m.has_senders);
        assert_eq!(m.conversation.participants, ["Assistant", "User"]);
        assert_eq!(texts(&m), ["say hi\npolitely", "hello"]);
    }

    #[test]
    fn explicit_senders_and_second_timestamps() {
        let m = one(r#"[{"author":"Mina","text":"hi","timestamp":1700000000},{"author":"Joon","text":"yo","timestamp":1700000005}]"#);
        assert_eq!(m.conversation.participants, ["Joon", "Mina"]);
        assert_eq!(m.conversation.messages[0].timestamp_ms, 1_700_000_000_000);
    }

    #[test]
    fn files_are_grouped_and_never_interleaved() {
        let o = ParseOptions::default();
        let pair = |q: &str, a: &str| parse_json(format!(r#"[{{"prompt":"{q}","response":"{a}"}}]"#).as_bytes(), &o).unwrap();
        let merged = merge(vec![("a".into(), "JSON", pair("q1", "a1")), ("b".into(), "JSON", pair("q2", "a2"))]);
        assert_eq!(merged.len(), 1);
        assert_eq!(texts(&merged[0]), ["q1", "a1", "q2", "a2"]);

        let chat = |who: &str| {
            let j = format!(r#"{{"participants":[{{"name":"{who}"}}],"messages":[{{"sender_name":"{who}","timestamp_ms":1,"content":"x"}}]}}"#);
            parse_json(j.as_bytes(), &o).unwrap()
        };
        assert_eq!(merge(vec![("a".into(), "JSON", chat("A")), ("b".into(), "JSON", chat("B"))]).len(), 2);
    }
}
