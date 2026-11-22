//! Asking the chat model who is in a text. Rules find speakers only where the text marks them; the
//! model also knows that "Lizzy" is Elizabeth, reads names in any language, and understands corrections.

use super::cast::Member;
use super::edits::Edits;
use crate::{ChatBackend, ChatTurn, Error, GenParams, Result, TurnRole};
use serde_json::{json, Value};

/// A chat model to use for identification.
pub struct ModelAccess<'a> {
    pub backend: &'a dyn ChatBackend,
    pub model: String,
    pub gen: GenParams,
}

const MAX_REPLY_TOKENS: i32 = 700;

const CAST_SYSTEM: &str = "You identify the characters in a text. You reply with JSON only.";
const EDIT_SYSTEM: &str = "You turn a user's correction into edits of a list of characters. You reply with JSON only.";

const EDIT_GUIDE: &str = r#"Turn exactly what the user said into edits of the list. Change nothing the user did not mention.
- "remove": names the user says should not be listed
- "merge": names the user says are one person; "keep" is the name to keep
- "rename": a listed name the user says is wrong, and the right one
- "add": characters the user says are missing
- "aliases": other names the user gives for a listed character

Examples, for a list with Ann and Bob:
"Ann is not a character" -> {"remove":["Ann"],"merge":[],"rename":[],"add":[],"aliases":[]}
"Ann and Bob are the same person" -> {"remove":[],"merge":[{"keep":"Ann","names":["Bob"]}],"rename":[],"add":[],"aliases":[]}
"Bob is really called Rob" -> {"remove":[],"merge":[],"rename":[{"from":"Bob","to":"Rob"}],"add":[],"aliases":[]}
"Cleo is missing" -> {"remove":[],"merge":[],"rename":[],"add":[{"name":"Cleo","aliases":[],"role":""}],"aliases":[]}
"Ann is also known as Annie" -> {"remove":[],"merge":[],"rename":[],"add":[],"aliases":[{"name":"Ann","aliases":["Annie"]}]}"#;

fn ask(access: &ModelAccess, system: &str, user: String, schema: Value) -> Result<String> {
    let gen = GenParams {
        temperature: 0.1,
        repeat_penalty: 1.0,
        max_tokens: MAX_REPLY_TOKENS,
        think: false,
        json_schema: Some(schema),
        ..access.gen.clone()
    };
    let turns = [
        ChatTurn { role: TurnRole::System, content: system.to_string() },
        ChatTurn { role: TurnRole::User, content: user },
    ];
    access.backend.chat(&access.model, &turns, &gen, &mut |_| {})
}

/// The characters in one excerpt.
pub(super) fn propose(access: &ModelAccess, title: &str, excerpt: &str) -> Result<Vec<Member>> {
    let user = format!(
        "Below is an excerpt from \"{title}\". List the characters in it: everyone who speaks or acts.\n\
         Write names exactly as they appear in the text. If one character goes by several names, give one \
         entry with the other names as aliases. Leave out places, organisations and the author. \
         \"role\" says who they are in a few words, in the language of the text.\n\
         Reply as {{\"characters\":[{{\"name\":\"...\",\"aliases\":[\"...\"],\"role\":\"...\"}}]}}.\n\n\
         Excerpt:\n{excerpt}"
    );
    parse_cast(&ask(access, CAST_SYSTEM, user, cast_schema())?)
}

/// What the user's correction means for the list.
pub(super) fn correct(access: &ModelAccess, listed: &[Member], correction: &str) -> Result<Edits> {
    let list: Vec<String> = listed
        .iter()
        .map(|m| {
            let also = if m.aliases.is_empty() { String::new() } else { format!(" (also: {})", m.aliases.join(", ")) };
            format!("- {}{also}: {}", m.name, m.description)
        })
        .collect();
    let user = format!(
        "Characters identified in a text:\n{}\n\nThe user says this is incorrect:\n\"{correction}\"\n\n{EDIT_GUIDE}",
        list.join("\n")
    );
    parse_edits(&ask(access, EDIT_SYSTEM, user, edit_schema())?)
}

fn person_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "aliases": { "type": "array", "items": { "type": "string" } },
            "role": { "type": "string" },
        },
        "required": ["name", "aliases", "role"],
    })
}

fn cast_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "characters": { "type": "array", "items": person_schema() } },
        "required": ["characters"],
    })
}

fn edit_schema() -> Value {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    let text = json!({ "type": "string" });
    let pairs = |key: &str, other: &str, value: &Value| {
        json!({
            "type": "array",
            "items": { "type": "object", "properties": { key: text, other: value }, "required": [key, other] },
        })
    };
    json!({
        "type": "object",
        "properties": {
            "remove": strings,
            "merge": pairs("keep", "names", &strings),
            "rename": pairs("from", "to", &text),
            "add": { "type": "array", "items": person_schema() },
            "aliases": pairs("name", "aliases", &strings),
        },
        "required": ["remove", "merge", "rename", "add", "aliases"],
    })
}

/// Tolerates text around the JSON, and a bare list of names.
fn parse_cast(reply: &str) -> Result<Vec<Member>> {
    let value = json_in(reply).ok_or_else(|| bad_reply("a character list", reply))?;
    let list = value.get("characters").unwrap_or(&value).as_array().ok_or_else(|| bad_reply("a character list", reply))?;
    Ok(members_in(list))
}

pub(super) fn parse_edits(reply: &str) -> Result<Edits> {
    let v = json_in(reply).ok_or_else(|| bad_reply("a list of changes", reply))?;
    let items = |key: &str| v[key].as_array().cloned().unwrap_or_default();
    Ok(Edits {
        remove: strings(&v["remove"]),
        merge: items("merge").iter().map(|m| (text(&m["keep"]), strings(&m["names"]))).filter(|m| !m.0.is_empty()).collect(),
        rename: items("rename")
            .iter()
            .map(|r| (text(&r["from"]), text(&r["to"])))
            .filter(|(from, to)| !from.is_empty() && !to.is_empty())
            .collect(),
        add: members_in(&items("add")),
        aliases: items("aliases")
            .iter()
            .map(|a| (text(&a["name"]), strings(&a["aliases"])))
            .filter(|(name, aliases)| !name.is_empty() && !aliases.is_empty())
            .collect(),
    })
}

fn bad_reply(wanted: &str, reply: &str) -> Error {
    Error::Backend(format!("the model did not return {wanted}: {reply:?}"))
}

fn json_in(reply: &str) -> Option<Value> {
    let start = reply.find(['{', '['])?;
    let end = reply.rfind(['}', ']'])?;
    serde_json::from_str(reply.get(start..=end)?).ok()
}

fn text(v: &Value) -> String {
    v.as_str().map(|s| s.trim().to_string()).unwrap_or_default()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().map(text).filter(|s| !s.is_empty()).collect()).unwrap_or_default()
}

fn members_in(list: &[Value]) -> Vec<Member> {
    list.iter()
        .filter_map(|c| {
            let name = if c.is_string() { text(c) } else { text(&c["name"]) };
            let usable = !name.is_empty() && name.chars().count() <= 60;
            usable.then(|| Member { name, aliases: strings(&c["aliases"]), description: text(&c["role"]) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cast_replies() {
        let reply = "Sure!\n```json\n{\"characters\":[{\"name\":\"Marta\",\"aliases\":[\"Miss Marta\"],\"role\":\"the keeper's daughter\"},{\"name\":\"\"}]}\n```";
        let m = parse_cast(reply).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].name.as_str(), m[0].aliases.len()), ("Marta", 1));
        assert_eq!(parse_cast(r#"["Marta", "Tobias"]"#).unwrap().len(), 2);
        assert!(parse_cast("I cannot help with that.").is_err());
    }

    #[test]
    fn edit_replies() {
        let e = parse_edits(r#"{"remove":["A",""],"merge":[{"keep":"","names":["X"]}],"rename":[{"from":"B","to":""}],"add":[],"aliases":[{"name":"C","aliases":[]}]}"#).unwrap();
        assert_eq!(e.remove, ["A"]);
        assert!(e.merge.is_empty() && e.rename.is_empty() && e.aliases.is_empty());
        assert!(parse_edits("no idea").is_err());
    }
}
