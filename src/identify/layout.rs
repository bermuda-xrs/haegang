//! Paragraphs from hard-wrapped lines, headings, and splitting long text.

const CLOSERS: &[char] = &['"', '”', '’', '\'', '」', '』', '»', ')'];
const SENTENCE_ENDS: &[char] = &['.', '!', '?', '…', '。', '！', '？'];
const OPENERS: &[char] = &['"', '“', '‘', '「', '『', '«', '—', '―'];

pub(super) fn ends_sentence(s: &str) -> bool {
    s.trim_end().trim_end_matches(CLOSERS).ends_with(SENTENCE_ENDS)
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF)
}

fn strip_gutenberg(text: &str) -> &str {
    let body = match text.find("*** START OF") {
        Some(i) => text[i..].split_once('\n').map_or("", |x| x.1),
        None => text,
    };
    body.find("*** END OF").map_or(body, |i| &body[..i])
}

/// The width the text was wrapped at, if it was. Only lines that continue into the next one count:
/// a paragraph's last line can be any length.
fn wrap_width(lines: &[&str]) -> Option<usize> {
    let mut widths: Vec<usize> = lines
        .windows(2)
        .filter(|w| !w[0].trim().is_empty() && !w[1].trim().is_empty())
        .map(|w| w[0].trim().chars().count())
        .collect();
    if widths.len() < 4 {
        return None;
    }
    widths.sort_unstable();
    let p90 = widths[widths.len() * 9 / 10];
    let near = widths.iter().filter(|w| **w * 4 >= p90 * 3).count();
    (p90 >= 30 && near * 2 >= widths.len()).then_some(p90)
}

fn join_line(para: &mut String, line: &str) {
    if para.is_empty() {
        para.push_str(line);
        return;
    }
    let (before, after) = (para.chars().last().unwrap_or(' '), line.chars().next().unwrap_or(' '));
    let second_last = para.chars().rev().nth(1).unwrap_or(' ');
    if before == '-' && second_last.is_alphabetic() && after.is_lowercase() {
        para.pop(); // "conver-" + "sation"
    } else if !(is_cjk(before) && is_cjk(after)) {
        para.push(' ');
    }
    para.push_str(line);
}

/// Blank lines separate paragraphs. In hard-wrapped text (PDF, Gutenberg) lines are joined back up, and a
/// short sentence-ending line or a new indent also ends one. Unwrapped text has a paragraph per line.
pub(super) fn paragraphs(text: &str) -> Vec<String> {
    let body = strip_gutenberg(text);
    let lines: Vec<&str> = body.lines().map(str::trim_end).collect();
    let width = wrap_width(&lines);
    let mut out = Vec::new();
    let mut para = String::new();
    let mut prev: &str = "";
    let mut prev_indent = 0;
    for line in &lines {
        let t = line.trim();
        if t.is_empty() {
            if !para.is_empty() {
                out.push(std::mem::take(&mut para));
            }
            prev = "";
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let new_para = match width {
            None => true,
            Some(w) => {
                let prev_short = prev.chars().count() * 4 < w * 3;
                (prev_short && ends_sentence(prev))
                    || (indent >= prev_indent + 2 && !prev.is_empty())
                    || (t.starts_with(OPENERS) && ends_sentence(prev))
            }
        };
        if new_para && !para.is_empty() {
            out.push(std::mem::take(&mut para));
        }
        join_line(&mut para, t);
        prev = t;
        prev_indent = indent;
    }
    if !para.is_empty() {
        out.push(para);
    }
    out
}

/// Chapter titles and the like.
pub(super) fn is_heading(p: &str) -> bool {
    let n = p.chars().count();
    if n == 0 || n > 60 || p.contains(OPENERS) {
        return false;
    }
    let roman = p.chars().all(|c| "IVXLC. ".contains(c));
    if ends_sentence(p) {
        return roman;
    }
    let lower = p.to_lowercase();
    let word = ["chapter", "part ", "book ", "prologue", "epilogue", "act ", "scene "].iter().any(|w| lower.starts_with(w));
    let markdown = p.starts_with('#');
    let korean = p.starts_with('제') && p.ends_with(['장', '부', '화']);
    let letters: Vec<char> = p.chars().filter(|c| c.is_alphabetic()).collect();
    let capitals = letters.len() >= 2 && letters.iter().all(|c| c.is_uppercase());
    word || markdown || korean || capitals || roman
}

pub(super) fn chunks(text: &str, max: usize) -> Vec<String> {
    if text.chars().count() <= max {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    let mut sentence_start = 0;
    let flush = |current: &mut String, count: &mut usize, out: &mut Vec<String>| {
        let piece = current.trim();
        if !piece.is_empty() {
            out.push(piece.to_string());
        }
        current.clear();
        *count = 0;
    };
    let mut indices = text.char_indices().peekable();
    while let Some((i, c)) = indices.next() {
        let next = indices.peek().map(|x| x.1);
        let boundary = next.is_none() || c == '\n' || (SENTENCE_ENDS.contains(&c) && next.is_some_and(char::is_whitespace));
        if !boundary {
            continue;
        }
        let end = i + c.len_utf8();
        let sentence = &text[sentence_start..end];
        let len = sentence.chars().count();
        if count + len > max {
            flush(&mut current, &mut count, &mut out);
        }
        if len > max {
            let cs: Vec<char> = sentence.chars().collect();
            out.extend(cs.chunks(max).map(|c| c.iter().collect::<String>()));
        } else {
            current.push_str(sentence);
            count += len;
        }
        sentence_start = end;
    }
    flush(&mut current, &mut count, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_wrapped_blocks_are_joined() {
        let line = "the quick brown fox jumps over the lazy dog again and again";
        let mut text = String::new();
        for _ in 0..3 {
            text.push_str(&format!("{line}\n{line}\n{line}\nend of the para-\ngraph here.\n\n"));
        }
        let p = paragraphs(&text);
        assert_eq!(p.len(), 3);
        assert!(p[0].ends_with("end of the paragraph here."));
    }

    #[test]
    fn wrapped_without_blank_lines_breaks_at_short_lines_and_quotes() {
        let long = "the quick brown fox jumps over the lazy dog again and again";
        let text = format!("{long}\n{long}\n{long}\nshort end.\n{long}\n{long}\n{long}\n{long}.\n“A quote starts here.”\n{long}\n{long}\n");
        let p = paragraphs(&text);
        assert_eq!(p.len(), 4, "{p:?}");
        assert!(p[2].starts_with('“'));
    }

    #[test]
    fn unwrapped_lines_are_paragraphs() {
        let text = "한 줄입니다.\n짧다.\n조금 더 긴 줄이 여기에 있습니다. 이 줄은 앞의 줄보다 깁니다.\n네.\n그래요.\n아니요.\n좋아요.\n끝.\n";
        assert_eq!(paragraphs(text).len(), 8);
    }

    #[test]
    fn gutenberg_licence_is_removed() {
        let t = "Licence text\n*** START OF THE BOOK ***\nThe story.\n*** END OF THE BOOK ***\nMore licence";
        assert_eq!(paragraphs(t), ["The story."]);
    }

    #[test]
    fn headings() {
        for h in ["Chapter One", "CHAPTER XII", "제3장", "IV.", "Prologue", "# Lamp maintenance"] {
            assert!(is_heading(h), "{h}");
        }
        for p in ["“Hello,” she said.", "He left.", "Marta and the sea"] {
            assert!(!is_heading(p), "{p}");
        }
    }

    #[test]
    fn long_text_splits_at_sentences() {
        let text = "First sentence here. Second sentence here. Third one is here.";
        assert_eq!(chunks(text, 45), ["First sentence here. Second sentence here.", "Third one is here."]);
        assert_eq!(chunks(&"x".repeat(25), 10).len(), 3);
        assert_eq!(chunks("short", 100), ["short"]);
    }
}
