//! One forgiving scanner for HTML, e-books and the XML inside office files.

use std::borrow::Cow;

#[derive(Debug, PartialEq)]
pub(super) enum Event<'a> {
    Open(&'a str),
    Close(&'a str),
    Text(&'a str),
    Raw(&'a str),
}

pub(super) struct Events<'a> {
    rest: &'a str,
}

pub(super) fn events(src: &str) -> Events<'_> {
    Events { rest: src }
}

pub(super) fn local_name(tag: &str) -> &str {
    let end = tag.find(|c: char| c.is_whitespace() || c == '/' || c == '>').unwrap_or(tag.len());
    let name = &tag[..end];
    name.rsplit(':').next().unwrap_or(name)
}

pub(super) fn is_self_closed(tag: &str) -> bool {
    tag.ends_with('/')
}

fn after<'a>(s: &'a str, pat: &str) -> &'a str {
    s.find(pat).map_or("", |i| &s[i + pat.len()..])
}

fn tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices().skip(1) {
        match (quote, c) {
            (None, '>') => return Some(i),
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if q == c => quote = None,
            _ => {}
        }
    }
    s.find('>') // unbalanced quotes in sloppy HTML
}

fn skip_to_close<'a>(mut s: &'a str, name: &str) -> &'a str {
    while let Some(i) = s.find("</") {
        if local_name(&s[i + 2..]).eq_ignore_ascii_case(name) {
            return &s[i..];
        }
        s = &s[i + 2..];
    }
    ""
}

impl<'a> Iterator for Events<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        loop {
            let r = self.rest;
            if r.is_empty() {
                return None;
            }
            if !r.starts_with('<') {
                let end = r.find('<').unwrap_or(r.len());
                self.rest = &r[end..];
                return Some(Event::Text(&r[..end]));
            }
            if let Some(body) = r.strip_prefix("<!--") {
                self.rest = after(body, "-->");
                continue;
            }
            if let Some(body) = r.strip_prefix("<![CDATA[") {
                let end = body.find("]]>").unwrap_or(body.len());
                self.rest = after(&body[end..], "]]>");
                return Some(Event::Raw(&body[..end]));
            }
            if r.starts_with("<?") || r.starts_with("<!") {
                self.rest = after(r, ">");
                continue;
            }
            if !r[1..].starts_with(|c: char| c.is_alphabetic() || c == '/' || c == '_') {
                self.rest = &r[1..]; // a lone `<` in text
                return Some(Event::Text("<"));
            }
            let end = tag_end(r)?;
            let tag = &r[1..end];
            self.rest = &r[end + 1..];
            if let Some(close) = tag.strip_prefix('/') {
                return Some(Event::Close(local_name(close.trim_end())));
            }
            let name = local_name(tag);
            let raw_text = name.eq_ignore_ascii_case("script") || name.eq_ignore_ascii_case("style");
            if raw_text && !is_self_closed(tag) {
                self.rest = skip_to_close(self.rest, name);
            }
            return Some(Event::Open(tag));
        }
    }
}

pub(super) fn attr(tag: &str, key: &str) -> Option<String> {
    let mut rest = tag.split_once(char::is_whitespace)?.1;
    loop {
        rest = rest.trim_start();
        let eq = rest.find('=')?;
        let name = rest[..eq].trim();
        let value = rest[eq + 1..].trim_start();
        let (found, tail) = match value.chars().next()? {
            q @ ('"' | '\'') => {
                let body = &value[1..];
                let end = body.find(q)?;
                (&body[..end], &body[end + 1..])
            }
            _ => {
                let end = value.find(char::is_whitespace).unwrap_or(value.len());
                (&value[..end], &value[end..])
            }
        };
        if name == key {
            return Some(decode_entities(found).into_owned());
        }
        rest = tail;
    }
}

pub(super) fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let decoded = rest[1..].find(';').filter(|&end| end <= 10).and_then(|end| {
            let name = &rest[1..end + 1];
            let c = match name.strip_prefix('#') {
                Some(n) => match n.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)?,
                    None => n.parse().ok().and_then(char::from_u32)?,
                },
                None => named_entity(name)?,
            };
            Some((c, end + 2))
        });
        match decoded {
            Some((c, len)) => {
                out.push(c);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        _ => return None,
    })
}

/// What to keep from a document; tag names are local and case-insensitive.
pub(super) struct Profile<'a> {
    /// Keep text only inside these; empty keeps all.
    pub text: &'a [&'a str],
    /// Paragraph breaks; `["*"]` is every tag.
    pub blocks: &'a [&'a str],
    /// Line breaks.
    pub breaks: &'a [&'a str],
    /// Spaces.
    pub spaces: &'a [&'a str],
    /// Dropped with their contents.
    pub skip: &'a [&'a str],
}

pub(super) const HTML: Profile = Profile {
    text: &[],
    blocks: &[
        "p", "div", "section", "article", "aside", "header", "footer", "main", "h1", "h2", "h3", "h4", "h5", "h6",
        "li", "ul", "ol", "tr", "table", "blockquote", "pre", "dd", "dt", "figcaption", "hr",
    ],
    breaks: &["br"],
    spaces: &["td", "th"],
    skip: &["head", "script", "style", "noscript", "template", "svg", "nav"],
};

/// Any other XML: every tag separates text.
pub(super) const GENERIC_XML: Profile =
    Profile { text: &[], blocks: &["*"], breaks: &[], spaces: &[], skip: &["binary", "stylesheet"] };

fn has(list: &[&str], name: &str) -> bool {
    list.iter().any(|n| *n == "*" || n.eq_ignore_ascii_case(name))
}

fn push_collapsed(out: &mut String, text: &str) {
    if text.is_empty() {
        return;
    }
    if text.starts_with(char::is_whitespace) && !out.ends_with(char::is_whitespace) && !out.is_empty() {
        out.push(' ');
    }
    let mut words = text.split_whitespace();
    if let Some(first) = words.next() {
        out.push_str(first);
        for w in words {
            out.push(' ');
            out.push_str(w);
        }
        if text.ends_with(char::is_whitespace) {
            out.push(' ');
        }
    }
}

pub(super) fn extract(src: &str, p: &Profile) -> String {
    let mut out = String::with_capacity(src.len() / 2);
    let mut inside = 0usize;
    let mut skipping: Option<(&str, usize)> = None;
    for ev in events(src) {
        if let Some((name, depth)) = skipping.as_mut() {
            match ev {
                Event::Open(t) if local_name(t).eq_ignore_ascii_case(name) && !is_self_closed(t) => *depth += 1,
                Event::Close(n) if n.eq_ignore_ascii_case(name) => *depth -= 1,
                _ => {}
            }
            if *depth == 0 {
                skipping = None;
            }
            continue;
        }
        match ev {
            Event::Open(tag) => {
                let name = local_name(tag);
                if has(p.skip, name) {
                    if !is_self_closed(tag) {
                        skipping = Some((name, 1));
                    }
                    continue;
                }
                if has(p.blocks, name) {
                    out.push_str("\n\n");
                } else if has(p.breaks, name) {
                    out.push('\n');
                } else if has(p.spaces, name) {
                    out.push(' ');
                }
                if !is_self_closed(tag) && has(p.text, name) {
                    inside += 1;
                }
            }
            Event::Close(name) => {
                if has(p.text, name) {
                    inside = inside.saturating_sub(1);
                }
                if has(p.blocks, name) {
                    out.push_str("\n\n");
                }
            }
            Event::Text(t) if p.text.is_empty() || inside > 0 => push_collapsed(&mut out, &decode_entities(t)),
            Event::Raw(t) if p.text.is_empty() || inside > 0 => push_collapsed(&mut out, t),
            _ => {}
        }
    }
    tidy(&out)
}

fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank = true;
    for line in s.lines().map(str::trim) {
        if line.is_empty() {
            if !blank {
                out.push('\n');
            }
            blank = true;
        } else {
            out.push_str(line);
            out.push('\n');
            blank = false;
        }
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_keeps_paragraphs_and_drops_noise() {
        let html = r#"<!doctype html><html><head><title>t</title><style>p > a {color:red}</style></head>
            <body><script>if (a<b) { x("</p>"); }</script><!-- c -->
            <p>One &amp; <b>two</b>&#33;</p><p>Line<br>break &ldquo;quoted&rdquo;</p><div data-x="a>b">three < four</div></body></html>"#;
        assert_eq!(extract(html, &HTML), "One & two!\n\nLine\nbreak “quoted”\n\nthree < four");
    }

    #[test]
    fn xml_text_only_mode() {
        let xml = r#"<w:p><w:pPr><w:rFonts w:ascii="Arial"/></w:pPr><w:r><w:t xml:space="preserve">Hello </w:t></w:r><w:r><w:t>world</w:t></w:r></w:p><w:p><w:r><w:t>Next</w:t><w:tab/><w:t>tab</w:t></w:r></w:p>"#;
        let p = Profile { text: &["t"], blocks: &["p"], breaks: &["br"], spaces: &["tab"], skip: &[] };
        assert_eq!(extract(xml, &p), "Hello world\n\nNext tab");
    }

    #[test]
    fn attributes_and_cdata() {
        assert_eq!(attr(r#"item id="a" href='x&amp;y.xhtml' media-type="t""#, "href").as_deref(), Some("x&y.xhtml"));
        assert_eq!(attr(r#"item id="a""#, "href"), None);
        assert_eq!(extract("<a><![CDATA[1 < 2]]></a>", &GENERIC_XML), "1 < 2");
    }
}
