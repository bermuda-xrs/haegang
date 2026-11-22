//! Prose: find quoted speech, then work out who said it.
//!
//! A lean take on the sieve of Muzny et al. 2017, "A Two-Stage Sieve Approach", applied from most to
//! least reliable: speech tags ("said Marta"), the only character named beside a quote, vocatives
//! ("..., Tobias?" is answered by Tobias), and turn-taking. Only speech tags count as certain.

use super::cast::Cast;
use super::layout::is_heading;
use super::tags::{self, Tag};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Who {
    Character(usize),
    Narrator,
    Unknown,
}

pub(super) struct Piece {
    pub quote: bool,
    pub text: String,
    pub who: Who,
    pub certain: bool,
}

pub(super) struct Para {
    pub pieces: Vec<Piece>,
    pub heading: bool,
}

impl Para {
    fn has_quote(&self) -> bool {
        self.pieces.iter().any(|p| p.quote)
    }

    fn who(&self) -> Option<Who> {
        self.pieces.iter().find(|p| p.quote && p.who != Who::Unknown).map(|p| p.who)
    }

    fn narration(&self) -> impl Iterator<Item = &str> {
        self.pieces.iter().filter(|p| !p.quote).map(|p| p.text.as_str())
    }

    fn quotes(&self) -> impl Iterator<Item = &Piece> {
        self.pieces.iter().filter(|p| p.quote)
    }

    fn assign(&mut self, who: Who) {
        for p in self.pieces.iter_mut().filter(|p| p.quote && p.who == Who::Unknown) {
            p.who = who;
        }
    }
}

#[derive(Clone, Copy)]
struct Style {
    /// Dialogue in single quotes (British typesetting) rather than double.
    singles: bool,
    /// Dialogue opened by a dash (French, Spanish, Russian typesetting).
    dashes: bool,
}

const DASHES: &[char] = &['—', '―', '–'];

fn detect_style(paragraphs: &[String]) -> Style {
    let mut doubles = 0usize;
    let mut singles = 0usize;
    for c in paragraphs.iter().flat_map(|p| p.chars()) {
        match c {
            '“' | '"' | '«' | '「' | '『' => doubles += 1,
            '‘' => singles += 1,
            _ => {}
        }
    }
    let straight_singles = paragraphs.iter().filter(|p| p.starts_with('\'')).count();
    let dash_lines = paragraphs.iter().filter(|p| p.starts_with(DASHES)).count();
    Style {
        singles: (singles + straight_singles) >= 2 && doubles * 4 < singles + straight_singles,
        dashes: dash_lines >= 2 && doubles + singles < dash_lines,
    }
}

fn closer_for(c: char, prev: Option<char>, next: Option<char>, style: Style) -> Option<char> {
    let starts_word = !prev.is_some_and(char::is_alphanumeric) && next.is_some_and(|n| !n.is_whitespace());
    match c {
        '“' => Some('”'),
        '"' => Some('"'),
        '«' => Some('»'),
        '「' => Some('」'),
        '『' => Some('』'),
        '‘' if style.singles => Some('’'),
        '\'' if style.singles && starts_word => Some('\''),
        _ => None,
    }
}

/// ’ and ' are also apostrophes: don't, the dogs' bones.
fn closes(c: char, prev: Option<char>, rest: &[(usize, char)]) -> bool {
    if !matches!(c, '’' | '\'') {
        return true;
    }
    if rest.first().is_some_and(|n| n.1.is_alphanumeric()) {
        return false;
    }
    // after a letter it closes only if the speech does not close again further on
    let closes_later = rest.iter().map(|x| x.1).take_while(|x| *x != '‘').any(|x| x == c);
    !(prev.is_some_and(char::is_alphabetic) && closes_later)
}

fn piece(quote: bool, text: &str) -> Option<Piece> {
    let t = text.trim();
    let t = if quote { t.trim_end_matches(',').trim_end() } else { t };
    (!t.is_empty()).then(|| Piece { quote, text: t.to_string(), who: Who::Unknown, certain: false })
}

/// A quoted term inside a sentence is not speech: `He called it "the beast" and left.`
fn is_scare_quote(text: &str) -> bool {
    text.split_whitespace().count() <= 3 && !text.trim_end().ends_with(['.', ',', '!', '?', '…', '。', '！', '？'])
}

fn split_quotes(par: &str, style: Style) -> Vec<Piece> {
    if style.dashes && par.starts_with(DASHES) {
        let body = par.trim_start_matches(DASHES).trim_start();
        return match body.split_once(DASHES) {
            Some((said, tag)) => piece(true, said).into_iter().chain(piece(false, tag)).collect(),
            None => piece(true, body).into_iter().collect(),
        };
    }
    let chars: Vec<(usize, char)> = par.char_indices().collect();
    let mut pieces: Vec<Piece> = Vec::new();
    let mut start = 0;
    let mut open: Option<char> = None;
    for (k, &(i, c)) in chars.iter().enumerate() {
        let prev = k.checked_sub(1).map(|j| chars[j].1);
        let next = chars.get(k + 1).map(|x| x.1);
        match open {
            None => {
                if let Some(closer) = closer_for(c, prev, next, style) {
                    pieces.extend(piece(false, &par[start..i]));
                    start = i + c.len_utf8();
                    open = Some(closer);
                }
            }
            Some(closer) if c == closer && closes(c, prev, &chars[k + 1..]) => {
                pieces.extend(piece(true, &par[start..i]));
                start = i + c.len_utf8();
                open = None;
            }
            _ => {}
        }
    }
    pieces.extend(piece(open.is_some(), &par[start..]));

    // fold scare quotes back into the narration around them
    let mut merged: Vec<Piece> = Vec::with_capacity(pieces.len());
    let mut rest = pieces.into_iter().peekable();
    while let Some(p) = rest.next() {
        let surrounded = merged.last().is_some_and(|m| !m.quote) && rest.peek().is_some_and(|n| !n.quote);
        if p.quote && surrounded && is_scare_quote(&p.text) {
            let after = rest.next().expect("peeked above");
            let before = merged.last_mut().expect("checked above");
            before.text = format!("{} “{}” {}", before.text, p.text, after.text);
        } else {
            merged.push(p);
        }
    }
    merged
}

pub(super) fn parse(paragraphs: &[String]) -> Vec<Para> {
    let style = detect_style(paragraphs);
    paragraphs
        .iter()
        .map(|p| match is_heading(p) {
            true => Para { pieces: Vec::new(), heading: true },
            false => Para { pieces: split_quotes(p, style), heading: false },
        })
        .collect()
}

pub(super) fn quote_count(paras: &[Para]) -> usize {
    paras.iter().map(|p| p.quotes().count()).sum()
}

/// The speech tag next to quote `i` of a paragraph: right after it, else right before it.
fn tag_of(pieces: &[Piece], i: usize) -> Option<Tag> {
    let after = pieces.get(i + 1).filter(|p| !p.quote);
    let before = i.checked_sub(1).map(|j| &pieces[j]).filter(|p| !p.quote);
    after.and_then(|p| tags::opening(&p.text)).or_else(|| before.and_then(|p| tags::closing(&p.text)))
}

/// Names used in speech tags, with how often. This is how speakers are found without a model.
pub(super) fn tagged_names(paras: &[Para]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for para in paras {
        for (i, p) in para.pieces.iter().enumerate() {
            if p.quote {
                if let Some(Tag::Name(name)) = tag_of(&para.pieces, i) {
                    *counts.entry(name).or_insert(0) += 1;
                }
            }
        }
    }
    counts
}

/// A short narration fragment opening with a speech tag belongs to the quote, not to the story.
pub(super) fn is_bare_tag(text: &str) -> bool {
    text.split_whitespace().count() <= 12 && tags::opening(text).is_some()
}

/// Set `who` on every quote that can be attributed; the rest stay `Unknown`.
pub(super) fn attribute(paras: &mut [Para], cast: &Cast) {
    for para in paras.iter_mut() {
        explicit_tags(para, cast);
    }
    let turns: Vec<usize> = (0..paras.len()).filter(|&i| paras[i].has_quote()).collect();
    only_named_character(paras, &turns, cast);
    vocatives(paras, &turns, cast);
    turn_taking(paras, &turns);
}

fn explicit_tags(para: &mut Para, cast: &Cast) {
    for i in 0..para.pieces.len() {
        if !para.pieces[i].quote {
            continue;
        }
        let who = match tag_of(&para.pieces, i) {
            Some(Tag::Name(name)) => cast.find(&name).map(Who::Character),
            Some(Tag::First) => Some(Who::Narrator),
            _ => None,
        };
        if let Some(who) = who {
            para.pieces[i].who = who;
            para.pieces[i].certain = true;
        }
    }
    // the other quotes of the paragraph continue the same speech
    if let Some(who) = para.who() {
        para.assign(who);
    }
}

fn only_named_character(paras: &mut [Para], turns: &[usize], cast: &Cast) {
    for &t in turns {
        if paras[t].who().is_some() {
            continue;
        }
        let mut named: Vec<usize> = paras[t].narration().flat_map(|n| cast.mentions(n)).collect();
        named.sort_unstable();
        named.dedup();
        let [only] = named[..] else { continue };
        // someone spoken to in the quote is the listener, not the speaker
        if !paras[t].quotes().any(|p| cast.addressed(&p.text) == Some(only)) {
            paras[t].assign(Who::Character(only));
        }
    }
}

/// Quotes count as one conversation when at most one narration paragraph separates them.
fn adjacent(paras: &[Para], a: usize, b: usize) -> bool {
    b - a <= 2 && !paras[a..=b].iter().any(|p| p.heading)
}

/// A quote addressing someone by name is answered by them, and answers them.
fn vocatives(paras: &mut [Para], turns: &[usize], cast: &Cast) {
    for k in 0..turns.len() {
        let t = turns[k];
        let Some(addressed) = paras[t].quotes().find_map(|p| cast.addressed(&p.text)) else { continue };
        if paras[t].who() == Some(Who::Character(addressed)) {
            continue;
        }
        for neighbour in [k.checked_sub(1), Some(k + 1)].into_iter().flatten() {
            let Some(&o) = turns.get(neighbour) else { continue };
            if paras[o].who().is_none() && adjacent(paras, t.min(o), t.max(o)) {
                paras[o].assign(Who::Character(addressed));
            }
        }
    }
}

/// In a run of alternating speech every speaker talks every second turn. Sweeping both ways
/// propagates known speakers through the run from either end.
fn turn_taking(paras: &mut [Para], turns: &[usize]) {
    let mut filled = true;
    while filled {
        filled = false;
        for k in (0..turns.len()).chain((0..turns.len()).rev()) {
            filled |= fill_from_alternation(paras, turns, k);
        }
    }
}

/// Give turn `k` the speaker of the turn two places away, if that whole stretch is one conversation
/// and the turn between them is somebody else.
fn fill_from_alternation(paras: &mut [Para], turns: &[usize], k: usize) -> bool {
    if paras[turns[k]].who().is_some() {
        return false;
    }
    let linked = |a: usize, b: usize| adjacent(paras, turns[a], turns[b]);
    let before = (k >= 2 && linked(k - 2, k - 1) && linked(k - 1, k)).then(|| (k - 2, k - 1));
    let after = (k + 2 < turns.len() && linked(k, k + 1) && linked(k + 1, k + 2)).then_some((k + 2, k + 1));
    let found = [before, after].into_iter().flatten().find_map(|(same, other)| {
        let who = paras[turns[same]].who()?;
        (paras[turns[other]].who() != Some(who)).then_some(who)
    });
    match found {
        Some(who) => {
            paras[turns[k]].assign(who);
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::cast::{merge, Member};
    use super::*;

    fn cast(names: &[&str]) -> Cast {
        Cast::new(merge(names.iter().map(|n| Member { name: n.to_string(), ..Default::default() }).collect()))
    }

    fn quotes_of(par: &str) -> Vec<String> {
        let style = Style { singles: false, dashes: false };
        split_quotes(par, style).into_iter().filter(|p| p.quote).map(|p| p.text).collect()
    }

    #[test]
    fn quote_splitting() {
        assert_eq!(quotes_of("“Yes,” said Tom. “I will.”"), ["Yes", "I will."]);
        assert_eq!(quotes_of("He called it \"the beast\" and left."), Vec::<String>::new());
        assert_eq!(quotes_of("“Unclosed speech runs on"), ["Unclosed speech runs on"]);
        assert_eq!(quotes_of("「元気?」と彼は言った。"), ["元気?"]);
    }

    #[test]
    fn single_quotes_keep_apostrophes() {
        let paras = parse(&["‘I don’t know,’ she said.".to_string(), "‘The dogs’ bones.’".to_string()]);
        assert_eq!(paras[0].pieces[0].text, "I don’t know");
        assert_eq!(paras[1].pieces[0].text, "The dogs’ bones.");
    }

    #[test]
    fn dash_dialogue() {
        let paras = parse(&["— Hola — dijo Juan.".to_string(), "— ¿Qué tal?".to_string()]);
        assert!(paras[0].pieces[0].quote);
        assert_eq!(paras[0].pieces[1].text, "dijo Juan.");
        assert!(paras[1].pieces[0].quote);
    }

    fn scene(lines: &[&str], names: &[&str]) -> Vec<(String, Who)> {
        let paragraphs: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        let mut paras = parse(&paragraphs);
        attribute(&mut paras, &cast(names));
        paras.into_iter().flat_map(|p| p.pieces).filter(|p| p.quote).map(|p| (p.text, p.who)).collect()
    }

    #[test]
    fn turn_taking_fills_untagged_lines() {
        let got = scene(
            &["“You’re late,” said Tobias.", "“The ferry was late,” Marta replied.", "“Did you bring the oil?”", "“Two cans.”", "“Good.”"],
            &["Marta", "Tobias"],
        );
        let c = cast(&["Marta", "Tobias"]);
        let (t, m) = (Who::Character(c.find("Tobias").unwrap()), Who::Character(c.find("Marta").unwrap()));
        assert_eq!(got.iter().map(|x| x.1).collect::<Vec<_>>(), [t, m, t, m, t]);
    }

    #[test]
    fn turn_taking_reaches_back_from_a_late_anchor() {
        let got = scene(&["“One.”", "“Two.”", "“Three.”", "“Four,” said Marta."], &["Marta", "Tobias"]);
        let c = cast(&["Marta", "Tobias"]);
        let m = Who::Character(c.find("Marta").unwrap());
        assert_eq!(got[1].1, m);
        assert_eq!(got[3].1, m);
    }

    #[test]
    fn conversations_do_not_run_across_chapters() {
        let got = scene(&["“One,” said Tobias.", "“Two,” said Marta.", "Chapter Two", "“Three.”"], &["Marta", "Tobias"]);
        assert_eq!(got[2].1, Who::Unknown);
    }

    #[test]
    fn the_character_named_beside_a_quote_is_the_speaker() {
        let got = scene(&["Marta set her bag on the floor. “I came back for the winter.”"], &["Marta", "Tobias"]);
        assert!(matches!(got[0].1, Who::Character(_)));
    }

    #[test]
    fn a_person_is_not_their_own_addressee() {
        let got = scene(&["Tobias looked up. “You came back, Tobias.”"], &["Marta", "Tobias"]);
        assert_eq!(got[0].1, Who::Unknown);
    }

    #[test]
    fn vocatives_name_the_next_speaker() {
        let c = cast(&["Marta", "Tobias", "Reyes"]);
        let got = scene(&["“Is that so, Tobias?” said Reyes.", "“It is.”"], &["Marta", "Tobias", "Reyes"]);
        assert_eq!(got[1].1, Who::Character(c.find("Tobias").unwrap()));
    }

    #[test]
    fn first_person_narrator_speaks() {
        let got = scene(&["“Stay,” I said."], &["Marta"]);
        assert_eq!(got[0].1, Who::Narrator);
    }

    #[test]
    fn tag_names_are_collected() {
        let paras = parse(&["“A,” said Tobias.".to_string(), "“B,” Tobias said.".to_string(), "“C,” said Marta.".to_string(), "“D,” he said.".to_string()]);
        let names = tagged_names(&paras);
        assert_eq!(names.get("Tobias"), Some(&2));
        assert_eq!(names.get("Marta"), Some(&1));
        assert_eq!(names.len(), 2);
    }
}
