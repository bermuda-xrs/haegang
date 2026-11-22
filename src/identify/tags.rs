//! Speech tags: reading "said Marta" or "지수가 물었다" out of the narration beside a quote.

use super::cast::is_title;

#[derive(Debug, PartialEq)]
pub(super) enum Tag {
    Name(String),
    /// "I said": the first-person narrator.
    First,
    /// "he said": someone, but the tag does not say who.
    Pronoun,
}

const VERBS: &[&str] = &[
    "said", "says", "asked", "asks", "replied", "replies", "answered", "answers", "cried", "shouted", "yelled",
    "whispered", "murmured", "muttered", "mumbled", "exclaimed", "called", "added", "continued", "demanded",
    "inquired", "enquired", "observed", "remarked", "returned", "rejoined", "responded", "screamed", "sobbed",
    "sighed", "laughed", "growled", "snapped", "hissed", "wondered", "insisted", "repeated", "declared", "suggested",
    "retorted", "interrupted", "stammered", "explained", "agreed", "pleaded", "begged", "warned", "ordered",
    "shrieked", "roared", "protested", "began", "went", "thought", "told", "breathed", "gasped", "groaned",
];
/// Adverbs and the like may sit between the subject and the verb: "Tom quietly said".
const BEFORE_VERB: &[&str] = &["had", "then", "only", "just", "also", "and"];
/// Capitalized at the start of a sentence, but never a name.
const NOT_NAMES: &[&str] = &[
    "The", "A", "An", "He", "She", "It", "They", "We", "You", "I", "His", "Her", "Their", "My", "Our", "Your", "This",
    "That", "These", "Those", "Then", "There", "Here", "When", "While", "As", "But", "And", "Or", "So", "If", "Yet",
    "At", "In", "On", "For", "To", "With", "From", "By", "After", "Before", "Now", "Once", "Still", "Just", "Even",
    "What", "Who", "Why", "How", "Where", "Yes", "No", "Oh", "Ah", "Well", "One", "Someone", "Everyone", "Nobody",
    "Somebody", "Everybody", "Finally", "Suddenly", "Again", "Instead",
];

const KO_VERBS: &[&str] = &[
    "말했", "말하", "말씀", "물었", "물으", "묻", "대답", "답했", "외쳤", "외치", "소리쳤", "소리질", "속삭", "중얼",
    "덧붙", "되물", "소리쳐", "말을",
];
const KO_PARTICLES: &[&str] = &["께서는", "께서", "이가", "은", "는", "이", "가", "도"];
/// Who is spoken to, even when a topic particle follows: 지수에게는.
const KO_LISTENER: &[&str] = &["에게", "한테", "께", "더러", "보고"];
const KO_FIRST: &[&str] = &["나", "내", "저", "제"];
const KO_PRONOUNS: &[&str] = &[
    "그", "그녀", "그들", "너", "네", "당신", "자기", "누군가", "사람", "남자", "여자", "아이", "소년", "소녀",
    "노인", "모두", "다들", "우리", "저희",
];

/// The tag opening a narration fragment: the one that follows a quote.
pub(super) fn opening(text: &str) -> Option<Tag> {
    read(&first_clause(text))
}

/// The tag closing a narration fragment: the one that precedes a quote.
pub(super) fn closing(text: &str) -> Option<Tag> {
    read(&last_clause(text))
}

fn read(tokens: &[&str]) -> Option<Tag> {
    english(tokens).or_else(|| korean(tokens))
}

fn word(token: &str) -> &str {
    token.trim_matches(|c: char| !c.is_alphanumeric())
}

fn ends_clause(token: &str) -> bool {
    let closing = token.trim_end_matches(['"', '”', '’', '\'']);
    closing.ends_with(['.', '!', '?', '…', '。', '！', '？', ':', ';']) && !is_title(word(token))
}

fn first_clause(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for t in s.split_whitespace().take(12) {
        out.push(t);
        if ends_clause(t) {
            break;
        }
    }
    out
}

fn last_clause(s: &str) -> Vec<&str> {
    let tokens: Vec<&str> = s.split_whitespace().collect();
    let tail = &tokens[tokens.len().saturating_sub(14)..];
    let body = tail.split_last().map_or(tail, |x| x.1); // the clause's own final token ends it
    let start = body.iter().rposition(|t| ends_clause(t)).map_or(0, |i| i + 1);
    tail[start..].to_vec()
}

fn can_be_name(token: &str) -> bool {
    let w = word(token);
    w.chars().next().is_some_and(char::is_uppercase) && !NOT_NAMES.contains(&w)
}

fn name_from(tokens: &[&str]) -> Option<Tag> {
    let name = tokens
        .iter()
        .map(|t| if is_title(word(t)) { t.trim_end_matches([',', ';', ':']) } else { word(t) })
        .collect::<Vec<_>>()
        .join(" ");
    (!name.is_empty()).then_some(Tag::Name(name))
}

/// Capitalized words from the start of `tokens`: `Mr. Darcy, smiling` -> `Mr. Darcy`.
fn run_forward(tokens: &[&str]) -> Option<Tag> {
    let mut end = 0;
    while let Some(t) = tokens.get(end).filter(|t| can_be_name(t) && end < 4) {
        end += 1;
        if !is_title(word(t)) && t.ends_with([',', ';', ':', '.', '!', '?']) {
            break;
        }
    }
    name_from(&tokens[..end])
}

/// Capitalized words ending just before `end`: `Then Mr. Darcy` + `said` -> `Mr. Darcy`.
fn run_backward(tokens: &[&str], end: usize) -> Option<Tag> {
    let mut start = end;
    while start > 0 && end - start < 4 && can_be_name(tokens[start - 1]) && !ends_clause(tokens[start - 1]) {
        start -= 1;
    }
    name_from(&tokens[start..end])
}

fn english(tokens: &[&str]) -> Option<Tag> {
    let verb = tokens.iter().position(|t| VERBS.contains(&word(t).to_lowercase().as_str()))?;
    // subject before the verb: "Tom said", "Tom quietly said", "he said"
    let mut subject = verb;
    if subject > 0 {
        let w = word(tokens[subject - 1]);
        if w.ends_with("ly") || BEFORE_VERB.contains(&w) {
            subject -= 1;
        }
    }
    if subject > 0 {
        match word(tokens[subject - 1]) {
            "I" => return Some(Tag::First),
            "he" | "she" | "they" | "He" | "She" | "They" => return Some(Tag::Pronoun),
            _ => {}
        }
        if let Some(tag) = run_backward(tokens, subject) {
            return Some(tag);
        }
    }
    // subject after the verb: "said Tom", "said he"
    let after = tokens.get(verb + 1..).unwrap_or_default();
    match after.first().map(|t| word(t)) {
        Some("I") => Some(Tag::First),
        Some("he" | "she" | "they") => Some(Tag::Pronoun),
        _ => run_forward(after),
    }
}

fn korean(tokens: &[&str]) -> Option<Tag> {
    let verb = tokens.iter().position(|t| KO_VERBS.iter().any(|v| word(t).starts_with(v)))?;
    // the nearest word before the verb carrying a subject particle: 지수가 조용히 물었다
    for t in tokens[..verb].iter().rev().take(3) {
        let w = word(t);
        let Some(stem) = KO_PARTICLES.iter().find_map(|p| w.strip_suffix(p)) else { continue };
        if KO_FIRST.contains(&stem) {
            return Some(Tag::First);
        }
        if KO_PRONOUNS.contains(&stem) {
            return Some(Tag::Pronoun);
        }
        // one syllable is an adverb: 같이, 깊이, 많이
        if stem.chars().count() >= 2 && !KO_LISTENER.iter().any(|p| stem.ends_with(p)) {
            return Some(Tag::Name(stem.to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> Option<Tag> {
        Some(Tag::Name(n.to_string()))
    }

    #[test]
    fn english_tags() {
        assert_eq!(opening(" said Tobias, not looking up."), name("Tobias"));
        assert_eq!(opening(" Marta replied."), name("Marta"));
        assert_eq!(opening(" said Mrs. Halloran, “you look thin.”"), name("Mrs. Halloran"));
        assert_eq!(opening(" Captain Reyes said quietly."), name("Captain Reyes"));
        assert_eq!(opening(" Tom quietly said."), name("Tom"));
        assert_eq!(opening(" he said."), Some(Tag::Pronoun));
        assert_eq!(opening(" said she."), Some(Tag::Pronoun));
        assert_eq!(opening(" I said."), Some(Tag::First));
        assert_eq!(opening(" The door closed."), None);
        assert_eq!(opening(" The man said."), None);
        assert_eq!(closing("She turned away. Marta said,"), name("Marta"));
    }

    #[test]
    fn korean_tags() {
        assert_eq!(opening(" 지수가 물었다."), name("지수"));
        assert_eq!(opening(" 민수는 조용히 대답했다."), name("민수"));
        assert_eq!(opening("라고 할머니께서 말씀하셨다."), name("할머니"));
        assert_eq!(opening(" 나는 말했다."), Some(Tag::First));
        assert_eq!(opening(" 그가 말했다."), Some(Tag::Pronoun));
        assert_eq!(opening(" 바다는 조용했다."), None);
    }
}
