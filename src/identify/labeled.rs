//! Text that labels its speakers: chat logs (WhatsApp, KakaoTalk, IRC, `Name: text`), plays
//! (`HAMLET. Who's there?`) and screenplays (an all-caps cue above the dialogue).

use super::cast::is_title;
use std::collections::HashMap;

/// `speaker == None` is narration: stage directions and action lines.
pub(super) struct Turn {
    pub speaker: Option<String>,
    pub text: String,
}

const NOT_SPEAKERS: &[&str] = &[
    "note", "warning", "tip", "example", "step", "chapter", "part", "figure", "table", "summary", "abstract",
    "keywords", "see", "source", "http", "https", "date", "time", "from", "to", "subject", "re", "fwd", "section",
    "question", "day", "act", "scene", "verse",
];
/// Direction marks WhatsApp puts around names and lines.
const INVISIBLE: &[char] = &['\u{200e}', '\u{200f}', '\u{202a}', '\u{202c}', '\u{feff}'];

/// "Dr." or an initial, where a full stop is part of the name.
fn is_abbreviation(word: &str) -> bool {
    word.ends_with('.') && (is_title(word) || word.split('.').all(|part| part.chars().count() <= 1))
}

fn plausible_name(name: &str) -> bool {
    let words = name.split_whitespace().count();
    let unnumbered = name.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end().to_lowercase();
    (1..=4).contains(&words)
        && name.chars().count() <= 40
        && name.chars().next().is_some_and(char::is_alphabetic)
        && !name.contains([',', ';', '!', '?', '"', '“', '”', '=', '/'])
        && name.split_whitespace().filter(|w| w.contains('.')).all(is_abbreviation)
        && !NOT_SPEAKERS.contains(&unnumbered.as_str())
}

/// Beside a timestamp anything short can be a name: phone numbers, nicknames with symbols.
fn stamped_name(name: &str) -> bool {
    (1..=40).contains(&name.chars().count()) && (name.starts_with('+') || name.contains(char::is_alphabetic))
}

/// Drop a leading timestamp such as `[10:23]` or `12/31/20, 10:23 PM -`.
fn strip_time(line: &str) -> &str {
    if let Some(inner) = line.strip_prefix('[') {
        if let Some((stamp, rest)) = inner.split_once(']') {
            if stamp.contains(|c: char| c.is_ascii_digit()) {
                return rest.trim_start();
            }
        }
    }
    if line.starts_with(|c: char| c.is_ascii_digit()) {
        let head: String = line.chars().take(36).collect();
        for sep in [" - ", ", "] {
            if let Some(i) = head.find(sep) {
                if head[..i].contains(':') {
                    return line[i + sep.len()..].trim_start();
                }
            }
        }
        if let Some((stamp, rest)) = line.split_once(' ') {
            if stamp.contains(':') && stamp.chars().all(|c| c.is_ascii_digit() || c == ':') {
                let rest = rest.strip_prefix("AM ").or(rest.strip_prefix("PM ")).unwrap_or(rest);
                return rest.trim_start();
            }
        }
    }
    line
}

fn label(line: &str) -> Option<(String, String)> {
    let line = line.trim_start_matches(INVISIBLE);
    // KakaoTalk: [name] [time] text
    if let Some(inner) = line.strip_prefix('[') {
        if let Some((name, rest)) = inner.split_once(']') {
            if let Some(after) = rest.trim_start().strip_prefix('[') {
                if let Some((stamp, text)) = after.split_once(']') {
                    if stamp.contains(':') && stamp.contains(|c: char| c.is_ascii_digit()) && stamped_name(name) {
                        return Some((name.trim().to_string(), text.trim_start().to_string()));
                    }
                }
            }
        }
    }
    let unstamped = strip_time(line);
    let stamped = unstamped.len() < line.len();
    let line = unstamped.trim_start_matches(INVISIBLE);
    // IRC: <name> text
    if let Some(inner) = line.strip_prefix('<') {
        if let Some((name, text)) = inner.split_once('>') {
            if plausible_name(name) {
                return Some((name.to_string(), text.trim_start().to_string()));
            }
        }
    }
    // Markdown: **name:** text / **name**: text
    let unbold = line.strip_prefix("**").map(|l| l.replacen("**", "", 1));
    let line = unbold.as_deref().unwrap_or(line);
    // name: text
    let colon = line.find([':', '：'])?;
    let name = line[..colon].trim();
    let name = name.split(" (").next().unwrap_or(name).trim().trim_matches(INVISIBLE);
    let rest = line[colon..].trim_start_matches([':', '：']).trim_start();
    let is_name = plausible_name(name) || (stamped && stamped_name(name));
    (is_name && !rest.starts_with("//")).then(|| (name.to_string(), rest.to_string()))
}

/// A play's `NAME. text`; the capitals keep ordinary sentences out.
fn play_label(line: &str) -> Option<(String, String)> {
    let abbreviated = |i: usize| matches!(line[..i].rsplit(' ').next(), Some("MR" | "MRS" | "MS" | "DR" | "ST"));
    let end = line.match_indices(". ").map(|m| m.0).find(|&i| !abbreviated(i))?;
    let (name, rest) = (&line[..end], &line[end + 2..]);
    let letters: Vec<char> = name.chars().filter(|c| c.is_alphabetic()).collect();
    let capitals = letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase());
    (capitals && plausible_name(name)).then(|| (name.to_string(), rest.trim_start().to_string()))
}

/// A screenplay cue: capitals only, optionally with a note like `(V.O.)`.
fn cue(line: &str) -> Option<String> {
    let name = line.split('(').next().unwrap_or(line).trim();
    let letters: Vec<char> = name.chars().filter(|c| c.is_alphabetic()).collect();
    let capitals = letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase());
    let scene = ["INT.", "EXT.", "CUT TO", "FADE", "THE END"].iter().any(|s| name.starts_with(s));
    (capitals && !scene && !name.ends_with(':') && plausible_name(name)).then(|| name.to_string())
}

fn display_name(name: &str) -> String {
    if !name.chars().any(char::is_lowercase) && name.chars().any(char::is_uppercase) {
        let mut inside_word = false;
        let mut out = String::with_capacity(name.len());
        for c in name.chars() {
            if inside_word { out.extend(c.to_lowercase()) } else { out.push(c) }
            inside_word = c.is_alphabetic();
        }
        return out;
    }
    name.to_string()
}

type Labeler = fn(&str) -> Option<(String, String)>;

/// Recurring speakers must cover enough of the text for it to count as labelled.
fn enough(counts: &HashMap<String, usize>, lines: usize) -> bool {
    let recurring: usize = counts.values().filter(|n| **n >= 2).sum();
    recurring >= 3 && recurring * 4 >= lines && counts.len() <= 60.max(lines / 20)
}

pub(super) fn parse(text: &str) -> Option<Vec<Turn>> {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let filled = lines.iter().filter(|l| !l.trim().is_empty()).count();

    // screenplay: a cue, then its dialogue up to the next blank line
    let mut cues: HashMap<String, usize> = HashMap::new();
    for (i, l) in lines.iter().enumerate() {
        let followed = lines.get(i + 1).is_some_and(|n| !n.trim().is_empty());
        if let Some(name) = cue(l.trim()).filter(|_| followed) {
            *cues.entry(name).or_default() += 1;
        }
    }
    if enough(&cues, filled) {
        return Some(screenplay(&lines));
    }

    // chats and plays: a label on each line
    for labeler in [label as Labeler, play_label as Labeler] {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for l in lines.iter().filter_map(|l| labeler(l.trim())) {
            *counts.entry(l.0).or_default() += 1;
        }
        if enough(&counts, filled) {
            return Some(labeled(&lines, labeler, &counts));
        }
    }
    None
}

fn labeled(lines: &[&str], labeler: Labeler, counts: &HashMap<String, usize>) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut open = false; // an unlabelled line directly after a turn continues it
    for l in lines.iter().map(|l| l.trim()) {
        if l.is_empty() {
            open = false;
            continue;
        }
        match labeler(l).filter(|(name, _)| counts.get(name).is_some_and(|n| *n >= 2)) {
            Some((name, text)) => {
                turns.push(Turn { speaker: Some(display_name(&name)), text });
                open = true;
            }
            None if open => {
                let last = turns.last_mut().expect("open implies a turn");
                last.text.push('\n');
                last.text.push_str(l);
            }
            None => turns.push(Turn { speaker: None, text: l.to_string() }),
        }
    }
    turns
}

fn screenplay(lines: &[&str]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i].trim();
        i += 1;
        if l.is_empty() {
            continue;
        }
        let followed = lines.get(i).is_some_and(|n| !n.trim().is_empty());
        let Some(name) = cue(l).filter(|_| followed) else {
            turns.push(Turn { speaker: None, text: l.to_string() });
            continue;
        };
        let mut said = Vec::new();
        while let Some(d) = lines.get(i).map(|d| d.trim()).filter(|d| !d.is_empty()) {
            if !(d.starts_with('(') && d.ends_with(')')) {
                said.push(d);
            }
            i += 1;
        }
        turns.push(Turn { speaker: Some(display_name(&name)), text: said.join(" ") });
    }
    turns
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Vec<(Option<String>, String)> {
        parse(text).expect("labelled").into_iter().map(|t| (t.speaker, t.text)).collect()
    }

    fn speakers(text: &str) -> Vec<String> {
        let mut s: Vec<String> = parsed(text).into_iter().filter_map(|t| t.0).collect();
        s.sort();
        s.dedup();
        s
    }

    #[test]
    fn whatsapp_with_continuation_lines() {
        let log = "12/31/20, 10:23 PM - Bob: are you coming?\n12/31/20, 10:24 PM - Ana: maybe\n12/31/20, 10:25 PM - Bob: there is a train\nand if not we walk\n1/1/21, 9:02 AM - Ana: happy new year\n";
        let t = parsed(log);
        assert_eq!(t.len(), 4);
        assert_eq!(t[2], (Some("Bob".into()), "there is a train\nand if not we walk".into()));
    }

    #[test]
    fn other_chat_styles() {
        assert_eq!(speakers("[민수] [오후 3:21] 어디야?\n[지수] [오후 3:22] 거의 다 왔어\n[민수] [오후 3:22] 천천히 와\n[지수] [오후 3:25] 도착!\n"), ["민수", "지수"]);
        assert_eq!(speakers("<ana> hi\n<bob> hello\n<ana> ok\n<bob> bye\n"), ["ana", "bob"]);
        assert_eq!(speakers("[10:23] Ana: hi\n[10:24] Bob: hello\n[10:25] Ana: ok\n[10:26] Bob: bye\n"), ["Ana", "Bob"]);
        assert_eq!(speakers("**Ana:** hi\n**Bob:** hello\n**Ana:** ok\n**Bob:** bye\n"), ["Ana", "Bob"]);
        assert_eq!(speakers("Q: why?\nA: because\nQ: when?\nA: now\n"), ["A", "Q"]);
        assert_eq!(speakers("2021년 3월 1일 오후 3:21, 민수 : 안녕\n2021년 3월 1일 오후 3:22, 지수 : 응\n2021년 3월 1일 오후 3:23, 민수 : 뭐해\n2021년 3월 1일 오후 3:24, 지수 : 쉬어\n"), ["민수", "지수"]);
    }

    #[test]
    fn plays() {
        let t = parsed("ACT I.\n\nEnter HAMLET and HORATIO.\n\nHAMLET. Who's there?\nHORATIO. Friends to this ground.\nHAMLET. Good night.\nHORATIO. Fare you well.\n");
        assert_eq!(t[2], (Some("Hamlet".into()), "Who's there?".into()));
        assert_eq!(t[1].0, None);
    }

    #[test]
    fn screenplays() {
        let script = "INT. LIGHTHOUSE - NIGHT\n\nRain against the glass.\n\n        TOBIAS\n    You're late.\n\n        MARTA (O.S.)\n    (out of breath)\n    The ferry\n    was late.\n\n        TOBIAS\n    Oil?\n\n        MARTA\n    Two cans.\n";
        let t = parsed(script);
        assert_eq!(t[3], (Some("Marta".into()), "The ferry was late.".into()));
        assert_eq!(t[1].0, None);
    }

    #[test]
    fn ordinary_prose_is_not_labelled() {
        let prose = "Marta climbed the stairs. Note: this is prose.\nThe storm was building all afternoon.\nHe said nothing at all about it.\nHere is a list of things: rope, oil, bread.\nShe smiled and went on climbing.\n";
        assert!(parse(prose).is_none());
        assert!(parse("Note: check the oil.\nNote: clean the lens.\nWarning: hot.\nRead the manual first.\nKeep it dry.\nStore it well.\n").is_none());
    }
}
