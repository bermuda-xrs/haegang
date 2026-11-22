//! Who is in a text, under which names.

use std::cmp::Reverse;
use std::collections::HashMap;

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Member {
    pub name: String,
    pub aliases: Vec<String>,
    pub description: String,
}

impl Member {
    pub fn surfaces(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.name.as_str()).chain(self.aliases.iter().map(String::as_str))
    }
}

/// Titles tell people apart: Mr. Bennet is not Mrs. Bennet.
pub(super) const TITLES: &[&str] = &[
    "mr", "mrs", "ms", "miss", "mx", "dr", "sir", "lady", "lord", "madam", "madame", "master", "mistress", "mister",
    "captain", "capt", "colonel", "col", "major", "lieutenant", "lt", "sergeant", "sgt", "general", "gen", "doctor",
    "professor", "prof", "judge", "king", "queen", "prince", "princess", "duke", "duchess", "count", "countess",
    "uncle", "aunt", "father", "mother", "brother", "sister", "saint", "st",
];

pub(super) fn is_title(word: &str) -> bool {
    TITLES.contains(&word.trim_end_matches('.').to_lowercase().as_str())
}

fn split_title(name: &str) -> (Option<String>, Vec<String>) {
    let mut words: Vec<String> = name.split_whitespace().map(|w| w.to_lowercase()).collect();
    if words.first().is_some_and(|w| w == "the") {
        words.remove(0);
    }
    let title = (words.len() > 1 && is_title(&words[0])).then(|| words.remove(0).trim_end_matches('.').to_string());
    (title, words)
}

fn is_hangul(s: &str) -> bool {
    s.chars().all(|c| matches!(c as u32, 0xAC00..=0xD7A3))
}

/// Could these be one person? "Marta" and "Miss Marta", "Darcy" and "Fitzwilliam Darcy", "민수" and "김민수".
fn names_match(a: &str, b: &str) -> bool {
    let ((ta, ca), (tb, cb)) = (split_title(a), split_title(b));
    if ta.is_some() && tb.is_some() && ta != tb {
        return false;
    }
    if ca == cb {
        return true;
    }
    let (short, title, long) = if ca.len() < cb.len() { (&ca, &ta, &cb) } else { (&cb, &tb, &ca) };
    if short.len() == 1 && long.len() > 1 && title.is_none() {
        return long.first() == short.first() || long.last() == short.first();
    }
    let (x, y) = if a.chars().count() < b.chars().count() { (a, b) } else { (b, a) };
    x.chars().count() >= 2 && is_hangul(x) && is_hangul(y) && y.ends_with(x) && y.chars().count() == x.chars().count() + 1
}

/// Merge names into distinct people. A partial match merges only when unambiguous: "Bennet" stays
/// apart when both Mr. and Mrs. Bennet exist.
pub(super) fn merge(mut list: Vec<Member>) -> Vec<Member> {
    list.sort_by_key(|m| Reverse(m.name.chars().count()));
    let mut out: Vec<Member> = Vec::new();
    for m in list {
        let exact = out.iter().position(|o| o.surfaces().any(|a| m.surfaces().any(|b| a.eq_ignore_ascii_case(b))));
        let partial: Vec<usize> = (0..out.len())
            .filter(|&i| out[i].surfaces().any(|a| m.surfaces().any(|b| names_match(a, b))))
            .collect();
        let target = exact.or(if partial.len() == 1 { Some(partial[0]) } else { None });
        let Some(i) = target else {
            out.push(m);
            continue;
        };
        for s in std::iter::once(m.name).chain(m.aliases) {
            if !out[i].surfaces().any(|a| a.eq_ignore_ascii_case(&s)) {
                out[i].aliases.push(s);
            }
        }
        if out[i].description.is_empty() {
            out[i].description = m.description;
        }
    }
    for m in &mut out {
        let mut seen = vec![m.name.to_lowercase()];
        m.aliases.retain(|a| {
            let key = a.to_lowercase();
            let new = !seen.contains(&key);
            seen.push(key);
            new
        });
    }
    out
}

pub(super) fn find_member(members: &[Member], name: &str) -> Option<usize> {
    let exact = members.iter().position(|m| m.surfaces().any(|s| s.eq_ignore_ascii_case(name)));
    let partial: Vec<usize> = (0..members.len()).filter(|&i| members[i].surfaces().any(|s| names_match(s, name))).collect();
    exact.or(if partial.len() == 1 { Some(partial[0]) } else { None })
}

pub(super) struct Cast {
    pub members: Vec<Member>,
    /// `None` when several members share the name.
    lookup: HashMap<String, Option<usize>>,
    /// Longest first, so "Mrs. Halloran" wins over "Halloran".
    surfaces: Vec<(String, usize)>,
}

impl Cast {
    pub fn new(members: Vec<Member>) -> Self {
        let mut lookup: HashMap<String, Option<usize>> = HashMap::new();
        for (i, m) in members.iter().enumerate() {
            for s in m.surfaces() {
                lookup.entry(s.to_lowercase()).and_modify(|e| *e = e.filter(|o| *o == i)).or_insert(Some(i));
            }
        }
        let mut surfaces: Vec<(String, usize)> = Vec::new();
        for (i, m) in members.iter().enumerate() {
            for s in m.surfaces() {
                if lookup.get(&s.to_lowercase()) == Some(&Some(i)) {
                    surfaces.push((s.to_string(), i));
                }
            }
        }
        surfaces.sort_by_key(|s| Reverse(s.0.chars().count()));
        Cast { members, lookup, surfaces }
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        if let Some(found) = self.lookup.get(&name.to_lowercase()) {
            return *found;
        }
        let core = split_title(name).1.join(" ");
        self.lookup.get(&core).copied().flatten()
    }

    fn occurrences<'a>(&'a self, text: &'a str) -> impl Iterator<Item = (usize, usize, usize)> + 'a {
        self.surfaces.iter().flat_map(move |(s, member)| {
            text.match_indices(s.as_str()).filter(move |(at, _)| at_word_start(text, *at)).filter_map(move |(at, _)| {
                let end = at + s.len();
                ends_word(s, &text[end..]).then_some((*member, at, end))
            })
        })
    }

    pub fn mentions(&self, text: &str) -> Vec<usize> {
        let mut found: Vec<usize> = self.occurrences(text).map(|o| o.0).collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    /// Who a line of speech is addressed to: "Well now, Marta," or "지수야".
    pub fn addressed(&self, quote: &str) -> Option<usize> {
        self.occurrences(quote).find_map(|(member, at, end)| {
            let mut before = quote[..at].trim_end();
            if let Some((rest, last)) = before.rsplit_once(char::is_whitespace) {
                if is_title(last) {
                    before = rest.trim_end();
                }
            } else if is_title(before) {
                before = "";
            }
            let after = &quote[end..];
            let after_ok = after.is_empty() || after.starts_with([',', '.', '!', '?', ';', ':', '—', '…']);
            let opened = before.is_empty() || before.ends_with([',', '—', '…', '.', '!', '?']);
            let korean = after.starts_with(['야', '아', '씨', '님']);
            ((opened && after_ok) || korean).then_some(member)
        })
    }
}

fn at_word_start(text: &str, at: usize) -> bool {
    !text[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric())
}

/// Korean names take particles, so only Latin names must end at a word boundary.
fn ends_word(name: &str, rest: &str) -> bool {
    let joined = name.chars().next_back().is_some_and(|c| !c.is_ascii() && c.is_alphabetic() && !c.is_lowercase() && !c.is_uppercase());
    joined || !rest.chars().next().is_some_and(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str) -> Member {
        Member { name: name.into(), ..Default::default() }
    }

    fn names(list: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = merge(list.iter().map(|n| member(n)).collect()).into_iter().map(|m| m.name).collect();
        out.sort();
        out
    }

    #[test]
    fn merging_keeps_people_apart() {
        assert_eq!(names(&["Marta", "Miss Marta"]), ["Miss Marta"]);
        assert_eq!(names(&["Darcy", "Fitzwilliam Darcy"]), ["Fitzwilliam Darcy"]);
        assert_eq!(names(&["Mr. Bennet", "Mrs. Bennet"]), ["Mr. Bennet", "Mrs. Bennet"]);
        assert_eq!(names(&["Bennet", "Mr. Bennet", "Mrs. Bennet"]).len(), 3);
        assert_eq!(names(&["Mr. Bennet", "Elizabeth Bennet"]).len(), 2);
        assert_eq!(names(&["민수", "김민수"]), ["김민수"]);
    }

    #[test]
    fn aliases_join_and_descriptions_survive() {
        let a = Member { name: "Elizabeth Bennet".into(), aliases: vec!["Lizzy".into()], description: String::new() };
        let b = Member { name: "Lizzy".into(), aliases: vec!["Eliza".into()], description: "the heroine".into() };
        let m = merge(vec![a, b]);
        assert_eq!(m.len(), 1);
        assert!(m[0].aliases.contains(&"Eliza".to_string()));
        assert_eq!(m[0].description, "the heroine");
    }

    #[test]
    fn aliases_stay_unique() {
        let a = Member { name: "Tomas".into(), aliases: vec!["Tobias".into(), "Tomas".into(), "tobias".into()], description: String::new() };
        assert_eq!(merge(vec![a])[0].aliases, ["Tobias"]);
    }

    #[test]
    fn lookups_and_mentions() {
        let cast = Cast::new(merge(vec![member("Marta"), member("Tobias"), member("Mrs. Halloran"), member("지수")]));
        assert_eq!(cast.find("marta"), cast.find("Miss Marta"));
        assert!(cast.find("Nobody").is_none());
        assert_eq!(cast.mentions("Martan went out").len(), 0);
        assert_eq!(cast.mentions("Marta saw Tobias, and Marta left.").len(), 2);
        assert_eq!(cast.mentions("지수가 물었다.").len(), 1);
    }

    #[test]
    fn vocatives() {
        let cast = Cast::new(merge(vec![member("Marta"), member("Tobias"), member("지수")]));
        let t = cast.find("Tobias");
        let m = cast.find("Marta");
        assert_eq!(cast.addressed("Doesn't she look thin, Tobias?"), t);
        assert_eq!(cast.addressed("Well now, Marta, you look thin"), m);
        assert_eq!(cast.addressed("I had hoped you would see reason, Miss Marta."), m);
        assert_eq!(cast.addressed("Tobias, come here."), t);
        assert_eq!(cast.addressed("I saw Tobias at the shop"), None);
        assert_eq!(cast.addressed("지수야, 어디 가?"), cast.find("지수"));
    }
}
