//! Plain-text formats and the cleanup every format goes through.

use super::ParseOptions;
use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{Encoding, WINDOWS_1252};
use std::collections::HashMap;

/// BOM, then UTF-8 (a few damaged bytes allowed), then a statistical guess for legacy encodings (EUC-KR, Shift_JIS, ...).
pub(super) fn decode(bytes: &[u8]) -> String {
    if let Some((enc, bom)) = Encoding::for_bom(bytes) {
        return enc.decode_without_bom_handling(&bytes[bom..]).0.into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    let lossy = String::from_utf8_lossy(bytes);
    let foreign = lossy.chars().filter(|c| !c.is_ascii()).count();
    let damaged = lossy.chars().filter(|c| *c == '\u{FFFD}').count();
    if damaged * 5 < foreign {
        return lossy.into_owned();
    }
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Deny);
    detector.feed(bytes, true);
    detector.guess(None, Utf8Detection::Allow).decode(bytes).0.into_owned()
}

/// Control or replacement characters in quantity: this was never text.
pub(super) fn looks_binary(s: &str) -> bool {
    let sample: Vec<char> = s.chars().take(65_536).collect();
    let bad = sample
        .iter()
        .filter(|c| **c == '\u{FFFD}' || (c.is_control() && !matches!(**c, '\n' | '\r' | '\t' | '\u{c}')))
        .count();
    bad * 50 > sample.len()
}

/// Instagram writes UTF-8 bytes as if each byte were a Latin-1 char. Undo that when possible.
pub(crate) fn fix_mojibake(s: &str) -> String {
    if s.is_ascii() {
        return s.to_string();
    }
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        match u8::try_from(c as u32) {
            Ok(b) => bytes.push(b),
            Err(_) => return s.to_string(), // already proper Unicode
        }
    }
    String::from_utf8(bytes).unwrap_or_else(|_| s.to_string())
}

/// Chat-app chatter that nobody said.
pub(crate) fn is_system_line(c: &str, opts: &ParseOptions) -> bool {
    c == "Liked a message"
        || c.ends_with(" sent an attachment.")
        || c.ends_with("unsent a message")
        || c.ends_with("missed a call.")
        || c.ends_with(" left the group.")
        || c.ends_with(" joined the group via invite link.")
        || c.contains(" named the group ")
        || c.contains(" changed the group photo.")
        || (c.starts_with("Reacted ") && c.contains(" to ") && c.contains("message"))
        || opts.system_markers.iter().any(|m| !m.is_empty() && c.contains(m.as_str()))
}

/// Strip URLs and collapse whitespace, keeping newlines. `None` if nothing remains.
pub(crate) fn clean(s: &str) -> Option<String> {
    let out: String = s
        .lines()
        .map(|l| {
            l.split_whitespace()
                .filter(|w| !(w.starts_with("http://") || w.starts_with("https://")))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!out.is_empty()).then_some(out)
}

/// Spreadsheets in many locales write "CSV" with semicolons; the first line shows which it is.
pub(super) fn delimiter(s: &str) -> char {
    let first = s.lines().next().unwrap_or("");
    let count = |d: char| first.matches(d).count();
    [';', '\t'].into_iter().fold(',', |best, d| if count(d) > count(best) { d } else { best })
}

/// RFC 4180-style rows: quoted fields, doubled quotes, newlines inside quotes.
pub(super) fn parse_delimited(s: &str, delimiter: char) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut field) = (Vec::new(), Vec::new(), String::new());
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match (quoted, c) {
            (true, '"') if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            (true, '"') => quoted = false,
            (true, _) => field.push(c),
            (false, '"') if field.is_empty() => quoted = true,
            (false, '\r') => {}
            (false, '\n') => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            (false, _) if c == delimiter => row.push(std::mem::take(&mut field)),
            (false, _) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r: &Vec<String>| r.iter().any(|f| !f.trim().is_empty()));
    rows
}

/// SRT / WebVTT cue text. A WebVTT voice tag `<v Bob>` becomes a `Bob:` speaker label.
pub(super) fn subtitles(s: &str) -> String {
    let mut cues: Vec<Vec<String>> = Vec::new();
    let (mut after_blank, mut last_after_blank, mut in_note) = (true, false, false);
    for line in s.lines().map(str::trim) {
        if line.is_empty() {
            (after_blank, in_note) = (true, false);
            continue;
        }
        if line.contains("-->") {
            // the line above a timing line is the cue's number or id
            if let Some(cue) = cues.last_mut() {
                if last_after_blank || cue.last().is_some_and(|l| l.chars().all(|c| c.is_ascii_digit())) {
                    cue.pop();
                }
            }
            cues.push(Vec::new());
            (after_blank, last_after_blank) = (false, false);
            continue;
        }
        in_note |= after_blank && (line == "NOTE" || line.starts_with("NOTE "));
        // nothing before the first cue is dialogue
        if let Some(cue) = cues.last_mut().filter(|_| !in_note) {
            cue.push(strip_cue_tags(line));
            last_after_blank = after_blank;
        }
        after_blank = false;
    }
    cues.iter().filter(|c| !c.is_empty()).map(|c| c.join("\n")).collect::<Vec<_>>().join("\n\n")
}

fn strip_cue_tags(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let is_tag = rest[1..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '/');
        let Some(end) = rest.find('>').filter(|_| is_tag) else {
            out.push('<');
            rest = &rest[1..];
            continue;
        };
        if let Some(name) = rest[1..end].strip_prefix("v ") {
            out.push_str(name.trim());
            out.push_str(": ");
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[derive(Clone, Copy)]
struct Group {
    skip: bool,
    unicode_skip: usize,
}

/// Text of an RTF document.
pub(super) fn rtf(bytes: &[u8]) -> String {
    let mut stack = vec![Group { skip: false, unicode_skip: 1 }];
    let mut out = String::new();
    let mut pending: Vec<u8> = Vec::new(); // `\'hh` bytes: one code point can span several escapes
    let mut encoding = WINDOWS_1252;
    let mut skip_chars = 0usize; // ASCII fallback following a `\uN`
    let mut high = 0u32; // first half of a surrogate pair
    let mut i = 0;

    fn flush(pending: &mut Vec<u8>, out: &mut String, encoding: &'static Encoding) {
        if !pending.is_empty() {
            out.push_str(&encoding.decode_without_bom_handling(pending).0);
            pending.clear();
        }
    }

    while i < bytes.len() {
        let top = *stack.last().unwrap();
        match bytes[i] {
            b'{' => {
                flush(&mut pending, &mut out, encoding);
                stack.push(top);
                i += 1;
            }
            b'}' => {
                flush(&mut pending, &mut out, encoding);
                if stack.len() > 1 {
                    stack.pop();
                }
                i += 1;
            }
            b'\\' => {
                i += 1;
                let Some(&next) = bytes.get(i) else { break };
                if next == b'\'' {
                    let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
                    if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                        if skip_chars > 0 {
                            skip_chars -= 1;
                        } else if !top.skip {
                            pending.push(byte);
                        }
                    }
                    i += 3;
                    continue;
                }
                flush(&mut pending, &mut out, encoding);
                if !next.is_ascii_alphabetic() {
                    i += 1;
                    match next {
                        b'*' => stack.last_mut().unwrap().skip = true,
                        b'\\' | b'{' | b'}' if !top.skip => out.push(next as char),
                        b'_' if !top.skip => out.push('-'),
                        b'~' if !top.skip => out.push(' '),
                        b'\n' | b'\r' if !top.skip => out.push('\n'),
                        _ => {}
                    }
                    continue;
                }
                let start = i;
                while bytes.get(i).is_some_and(u8::is_ascii_alphabetic) {
                    i += 1;
                }
                let word = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
                let digits = i;
                if bytes.get(i) == Some(&b'-') {
                    i += 1;
                }
                while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                let param: Option<i32> = std::str::from_utf8(&bytes[digits..i]).ok().and_then(|p| p.parse().ok());
                if bytes.get(i) == Some(&b' ') {
                    i += 1;
                }
                match word {
                    "u" => {
                        let v = param.unwrap_or(0);
                        let code = if v < 0 { v + 65536 } else { v } as u32;
                        match code {
                            0xD800..=0xDBFF => high = code,
                            _ if top.skip => {}
                            0xDC00..=0xDFFF if high != 0 => {
                                let pair = 0x10000 + ((high - 0xD800) << 10) + (code - 0xDC00);
                                out.push(char::from_u32(pair).unwrap_or('\u{FFFD}'));
                                high = 0;
                            }
                            _ => out.push(char::from_u32(code).unwrap_or('\u{FFFD}')),
                        }
                        skip_chars = top.unicode_skip;
                    }
                    "uc" => stack.last_mut().unwrap().unicode_skip = param.unwrap_or(1).max(0) as usize,
                    "ansicpg" => encoding = codepage(param.unwrap_or(1252)),
                    "fonttbl" | "colortbl" | "stylesheet" | "info" | "pict" | "object" | "header" | "footer"
                    | "headerl" | "headerr" | "footerl" | "footerr" | "footnote" | "themedata" | "datastore" => {
                        stack.last_mut().unwrap().skip = true
                    }
                    _ if top.skip => {}
                    "par" | "line" | "sect" | "page" | "row" => out.push_str("\n\n"),
                    "tab" | "cell" => out.push(' '),
                    "emdash" => out.push('—'),
                    "endash" => out.push('–'),
                    "lquote" => out.push('‘'),
                    "rquote" => out.push('’'),
                    "ldblquote" => out.push('“'),
                    "rdblquote" => out.push('”'),
                    _ => {}
                }
            }
            b'\r' | b'\n' => i += 1,
            c => {
                if skip_chars > 0 {
                    skip_chars -= 1;
                } else if !top.skip {
                    flush(&mut pending, &mut out, encoding);
                    out.push(c as char);
                }
                i += 1;
            }
        }
    }
    flush(&mut pending, &mut out, encoding);
    out
}

fn codepage(cp: i32) -> &'static Encoding {
    let label = match cp {
        949 => "euc-kr".to_string(),
        932 => "shift_jis".to_string(),
        936 => "gbk".to_string(),
        950 => "big5".to_string(),
        n => format!("windows-{n}"),
    };
    Encoding::for_label(label.as_bytes()).unwrap_or(WINDOWS_1252)
}

/// Page text as one, without page numbers and running heads: lines at the edges of pages that keep coming back.
pub(super) fn join_pages(pages: &[String]) -> String {
    const EDGE: usize = 2;
    const SHORT: usize = 60;
    const REPEATS: usize = 4;
    let unnumbered = |l: &str| l.chars().filter(|c| !c.is_ascii_digit()).collect::<String>();
    let pages: Vec<Vec<&str>> =
        pages.iter().map(|p| p.trim().lines().map(str::trim_end).collect::<Vec<_>>()).collect();

    let edges_of = |page: &[&str]| -> Vec<usize> {
        let filled: Vec<usize> = (0..page.len()).filter(|&i| !page[i].trim().is_empty()).collect();
        let tail = filled.len().saturating_sub(EDGE).max(EDGE.min(filled.len()));
        filled[..EDGE.min(filled.len())].iter().chain(&filled[tail..]).copied().collect()
    };
    let mut exact: HashMap<&str, usize> = HashMap::new();
    let mut numbered: HashMap<String, usize> = HashMap::new();
    for page in &pages {
        for i in edges_of(page) {
            *exact.entry(page[i].trim()).or_default() += 1;
            *numbered.entry(unnumbered(page[i].trim())).or_default() += 1;
        }
    }
    // "Title 12" is on most pages; "Chapter 12" on few, and must stay
    let is_furniture = |l: &str| {
        let numbers = numbered.get(&unnumbered(l)).copied().unwrap_or(0);
        l.chars().all(|c| c.is_ascii_digit())
            || (l.chars().count() <= SHORT
                && (exact.get(l).is_some_and(|n| *n >= REPEATS) || (numbers >= REPEATS && numbers * 4 >= pages.len())))
    };

    let mut out = String::new();
    for page in &pages {
        let edges = edges_of(page);
        let body: Vec<&str> =
            page.iter().enumerate().filter(|(i, l)| !(edges.contains(i) && is_furniture(l.trim()))).map(|x| *x.1).collect();
        let body = body.join("\n");
        let body = body.trim_matches('\n');
        if body.is_empty() {
            continue;
        }
        if !out.is_empty() {
            // a sentence can run over the page break
            let ended = out.ends_with(['.', '!', '?', '…', '。', '"', '”', '’', '」', '』']);
            out.push_str(if ended { "\n\n" } else { "\n" });
        }
        out.push_str(body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding() {
        assert_eq!(decode("héllo".as_bytes()), "héllo");
        assert_eq!(decode(b"\xEF\xBB\xBFbom"), "bom");
        assert_eq!(decode(b"\xFF\xFEh\0i\0"), "hi");
        let (euc_kr, _, _) = encoding_rs::EUC_KR.encode("폭풍이 지나간 다음 날, 민수는 항구에서 지수를 기다리고 있었다.");
        assert!(decode(&euc_kr).starts_with("폭풍이 지나간"));
        assert!(looks_binary("\u{1}\u{2}\u{3}ab"));
        assert!(!looks_binary("plain text\n\twith tabs"));
    }

    #[test]
    fn cleanup() {
        let bad: String = "héllo 😀".bytes().map(|b| b as char).collect();
        assert_eq!(fix_mojibake(&bad), "héllo 😀");
        assert_eq!(fix_mojibake("해강"), "해강");
        assert_eq!(clean("  hi   there \n\n see https://x.y/z ").as_deref(), Some("hi there\nsee"));
        assert_eq!(clean("https://only.link"), None);
        let o = ParseOptions { system_markers: vec!["좋아요".into()] };
        assert!(is_system_line("Bob sent an attachment.", &o));
        assert!(is_system_line("Reacted ❤ to your message", &o));
        assert!(is_system_line("메시지에 좋아요를 표시했습니다", &o));
        assert!(!is_system_line("I reacted badly", &o));
    }

    #[test]
    fn delimited() {
        let rows = parse_delimited("a,b\n\"x, \"\"y\"\"\",\"two\nlines\"\r\n\n", ',');
        assert_eq!(rows, [vec!["a", "b"], vec!["x, \"y\"", "two\nlines"]]);
    }

    #[test]
    fn subtitle_cues() {
        let srt = "1\n00:00:01,000 --> 00:00:03,000\n<i>You're late.</i>\n\n2\n00:00:04,000 --> 00:00:06,000\nDid you\nbring it?\n";
        assert_eq!(subtitles(srt), "You're late.\n\nDid you\nbring it?");
        let vtt = "WEBVTT\n\n00:01.000 --> 00:02.000\n<v Bob>Hello there\n";
        assert_eq!(subtitles(vtt), "Bob: Hello there");
    }

    #[test]
    fn rtf_text() {
        let doc = br"{\rtf1\ansi\ansicpg949{\fonttbl{\f0 Arial;}}{\*\generator x;}\f0 Hi \'c6\'f8\'c7\'b3\par \u-10892?\u-21483?!\par {\b bold} \{x\}\emdash y}";
        let text = rtf(doc);
        assert_eq!(text.split_whitespace().collect::<Vec<_>>().join(" "), "Hi 폭풍 해강! bold {x}—y");
    }

    #[test]
    fn page_furniture() {
        let pages: Vec<String> = (1..=5)
            .map(|p| format!("THE KEEPER {p}\n\nOn page {p}{}.\n“No.”\n1984\nThe end of page {p}{}.\n\n{p}\n", "x".repeat(p * 9), "y".repeat(p * 9)))
            .collect();
        let clean = join_pages(&pages);
        assert!(clean.contains("On page 3"));
        assert!(!clean.contains("THE KEEPER"));
        assert!(!clean.lines().any(|l| l == "3"));
        // lines inside a page are content however often they come back
        assert_eq!(clean.matches("“No.”").count(), 5);
        assert_eq!(clean.matches("1984").count(), 5);

        let chapters: Vec<String> = (1..=40)
            .map(|p| if p % 10 == 0 { format!("Chapter {}\nText {p}.", p / 10) } else { format!("Body {}\nmore {}", "a".repeat(p), "b".repeat(p)) })
            .collect();
        assert_eq!(join_pages(&chapters).matches("Chapter").count(), 4);
    }

    #[test]
    fn damage_does_not_change_the_encoding() {
        let mut bytes = "폭풍이 지나간 다음 날, 민수는 항구에서 지수를 기다리고 있었다.".as_bytes().to_vec();
        bytes[1] = b'x';
        assert!(decode(&bytes).contains("민수는 항구에서"));
    }

    #[test]
    fn rtf_emoji() {
        assert_eq!(rtf(br"{\rtf1 \u-10179?\u-8704?}").trim(), "😀");
    }
}
