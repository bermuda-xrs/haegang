//! Plain-text formats and the cleanup every format goes through.

use super::ParseOptions;
use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::{Encoding, WINDOWS_1252};
use std::collections::HashMap;

/// BOM, then UTF-8, then a statistical guess for legacy encodings (EUC-KR, Shift_JIS, ...).
pub(super) fn decode(bytes: &[u8]) -> String {
    if let Some((enc, bom)) = Encoding::for_bom(bytes) {
        return enc.decode_without_bom_handling(&bytes[bom..]).0.into_owned();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
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
    let normal = s.replace("\r\n", "\n");
    let mut cues = Vec::new();
    for block in normal.split("\n\n") {
        let lines: Vec<String> = block
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.contains("-->") && !l.chars().all(|c| c.is_ascii_digit()))
            .filter(|l| !l.starts_with("WEBVTT") && !l.starts_with("NOTE"))
            .map(strip_cue_tags)
            .collect();
        if !lines.is_empty() {
            cues.push(lines.join("\n"));
        }
    }
    cues.join("\n\n")
}

fn strip_cue_tags(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let Some(end) = rest[i..].find('>') else { break };
        let tag = &rest[i + 1..i + end];
        if let Some(name) = tag.strip_prefix("v ") {
            out.push_str(name.trim());
            out.push_str(": ");
        }
        rest = &rest[i + end + 1..];
    }
    out.push_str(rest.rsplit('>').next().unwrap_or(rest));
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
                        if !top.skip {
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
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

/// Drop page numbers and running heads from PDF text: a short line that keeps coming back,
/// ignoring the digits in it, is furniture rather than content.
pub(super) fn strip_page_furniture(text: &str) -> String {
    const SHORT: usize = 60;
    const REPEATS: usize = 4;
    let key = |l: &str| l.trim().chars().filter(|c| !c.is_ascii_digit()).collect::<String>();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in text.lines().filter(|l| l.trim().chars().count() <= SHORT) {
        *counts.entry(key(line)).or_default() += 1;
    }
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let t = line.trim();
        let page_number = !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
        let repeated =
            !t.is_empty() && t.chars().count() <= SHORT && counts.get(&key(line)).is_some_and(|n| *n >= REPEATS);
        if !page_number && !repeated {
            out.push_str(line);
            out.push('\n');
        }
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
        let mut pdf = String::new();
        for page in 1..=5 {
            pdf.push_str(&format!("THE KEEPER {page}\nBody text of page {page} is different: {}\n{page}\n", "x".repeat(page * 15)));
        }
        let clean = strip_page_furniture(&pdf);
        assert!(clean.contains("Body text of page 3"));
        assert!(!clean.contains("THE KEEPER"));
        assert!(!clean.lines().any(|l| l == "3"));
    }
}
