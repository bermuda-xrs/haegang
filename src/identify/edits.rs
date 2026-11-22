//! Changing the cast list the way the user asked.
//!
//! A small model rewriting a whole list quietly drops parts of the request; asked only what the
//! request *means* it does better, but its labels still need a firm hand: it files merged names under
//! `remove` as well, reverses renames, and reports additions as removals. So the edits here are read
//! by what they can mean, and only names the user actually mentioned are touched.

use super::cast::{find_member, Member};

#[derive(Debug, Default)]
pub(super) struct Edits {
    pub remove: Vec<String>,
    /// (name to keep, names that are the same person)
    pub merge: Vec<(String, Vec<String>)>,
    pub rename: Vec<(String, String)>,
    pub add: Vec<Member>,
    /// (listed name, its other names)
    pub aliases: Vec<(String, Vec<String>)>,
}

impl Edits {
    /// The list after the user's correction, or `None` if nothing about it changes.
    pub fn apply(self, members: Vec<Member>, said: &str) -> Option<Vec<Member>> {
        let before = members.clone();
        let mut list = List { members, said: said.to_lowercase() };
        let settled = self.settled();

        for (keep, others) in self.merge {
            list.merge(&keep, &others);
        }
        for (from, to) in self.rename {
            list.rename(from, to);
        }
        for name in self.remove {
            if !settled.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
                list.remove(name);
            }
        }
        for (name, aliases) in self.aliases {
            list.add_aliases(&name, aliases);
        }
        for member in self.add {
            list.add(member);
        }
        (list.members != before).then_some(list.members)
    }

    /// Names a real merge or rename already accounts for; a later "remove" of one of them is the
    /// model double-reporting the same change. No-ops, such as merging a name with itself, do not count.
    fn settled(&self) -> Vec<String> {
        let differs = |a: &String, b: &String| !a.eq_ignore_ascii_case(b);
        self.merge
            .iter()
            .filter(|(keep, others)| others.iter().any(|o| differs(o, keep)))
            .flat_map(|(keep, others)| std::iter::once(keep).chain(others))
            .chain(self.rename.iter().filter(|(from, to)| differs(from, to)).flat_map(|(from, to)| [from, to]))
            .cloned()
            .collect()
    }
}

struct List {
    members: Vec<Member>,
    /// What the user said, lowercased.
    said: String,
}

impl List {
    fn mentioned(&self, name: &str) -> bool {
        self.said.contains(&name.to_lowercase())
    }

    /// The member called `name`, but only if the user's sentence points at them. They can also be
    /// pointed at by what they are: "the shopkeeper".
    fn listed(&self, name: &str) -> Option<usize> {
        let i = find_member(&self.members, name)?;
        let m = &self.members[i];
        let by_name = m.surfaces().any(|s| self.mentioned(s));
        let by_role = m.description.split_whitespace().any(|w| w.chars().count() >= 4 && self.mentioned(w));
        (by_name || by_role).then_some(i)
    }

    fn holds(&self, name: &str) -> bool {
        find_member(&self.members, name).is_some()
    }

    fn add_alias(&mut self, i: usize, alias: String) {
        if !self.members[i].surfaces().any(|s| s.eq_ignore_ascii_case(&alias)) {
            self.members[i].aliases.push(alias);
        }
    }

    fn rename_to(&mut self, i: usize, name: String) {
        self.members[i].aliases.retain(|a| !a.eq_ignore_ascii_case(&name));
        let old = std::mem::replace(&mut self.members[i].name, name);
        self.members[i].aliases.push(old);
    }

    fn absorb(&mut self, from: usize, into: usize) {
        let gone = self.members.remove(from);
        let target = &mut self.members[into - usize::from(from < into)];
        target.aliases.extend(std::iter::once(gone.name).chain(gone.aliases));
        if target.description.is_empty() {
            target.description = gone.description;
        }
    }

    /// Fold every name onto the first of them that is listed. A `keep` that is not yet a listed name
    /// means the person is really called that, so it becomes their name.
    fn merge(&mut self, keep: &str, others: &[String]) {
        let names: Vec<&str> = std::iter::once(keep).chain(others.iter().map(String::as_str)).collect();
        let Some(first) = names.iter().find(|n| self.listed(n).is_some()).map(|n| n.to_string()) else { return };
        for name in names {
            let target = find_member(&self.members, &first).expect("found just above");
            match self.listed(name) {
                Some(i) if i != target => self.absorb(i, target),
                _ if self.mentioned(name) => self.add_alias(target, name.to_string()),
                _ => {}
            }
        }
        if !keep.eq_ignore_ascii_case(&first) && self.mentioned(keep) {
            if let Some(i) = find_member(&self.members, &first) {
                self.rename_to(i, keep.to_string());
            }
        }
    }

    /// A rename of an unlisted name onto a listed one is the model reporting it backwards.
    fn rename(&mut self, from: String, to: String) {
        let (from, to) = if !self.holds(&from) && self.holds(&to) { (to, from) } else { (from, to) };
        if from.eq_ignore_ascii_case(&to) {
            return;
        }
        match self.listed(&from) {
            Some(i) => self.rename_to(i, to),
            None => self.add(Member { name: to, ..Default::default() }),
        }
    }

    /// Removing a name that is not listed is the model's way of saying it is missing.
    fn remove(&mut self, name: String) {
        match self.listed(&name) {
            Some(i) => {
                self.members.remove(i);
            }
            None => self.add(Member { name, ..Default::default() }),
        }
    }

    fn add_aliases(&mut self, name: &str, aliases: Vec<String>) {
        let Some(i) = self.listed(name) else { return };
        let wanted: Vec<String> = aliases.into_iter().filter(|a| self.mentioned(a)).collect();
        for alias in wanted {
            self.add_alias(i, alias);
        }
    }

    fn add(&mut self, mut member: Member) {
        if self.mentioned(&member.name) && !self.holds(&member.name) {
            member.aliases.retain(|a| self.mentioned(a));
            self.members.push(member);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::parse_edits;
    use super::*;

    fn member(name: &str) -> Member {
        Member { name: name.into(), ..Default::default() }
    }

    fn cast() -> Vec<Member> {
        let mut list: Vec<Member> = ["Marta", "Tobias", "Mrs. Halloran", "민수"].map(member).to_vec();
        list[2].description = "The shopkeeper of the village".into();
        list
    }

    /// Every `reply` below is what the real model answered for that sentence.
    fn apply(reply: &str, said: &str) -> Option<Vec<Member>> {
        parse_edits(reply).unwrap().apply(cast(), said)
    }

    fn names(m: &[Member]) -> Vec<&str> {
        m.iter().map(|x| x.name.as_str()).collect()
    }

    #[test]
    fn plain_edits() {
        let m = apply(
            r#"{"remove":["Mrs. Halloran"],"merge":[{"keep":"Tobias","names":["the keeper"]}],"rename":[],"add":[],"aliases":[]}"#,
            "Mrs. Halloran is not a real character. Tobias is also called the keeper.",
        )
        .unwrap();
        assert_eq!(names(&m), ["Marta", "Tobias", "민수"]);
        assert_eq!(m[1].aliases, ["the keeper"]);
    }

    #[test]
    fn merged_names_are_not_removed() {
        let m = apply(
            r#"{"remove":["Marta","Mrs. Halloran"],"merge":[{"keep":"Marta","names":["Mrs. Halloran"]}],"rename":[],"add":[],"aliases":[]}"#,
            "Marta and Mrs. Halloran are the same person.",
        )
        .unwrap();
        assert_eq!(names(&m), ["Marta", "Tobias", "민수"]);
        assert_eq!(m[0].aliases, ["Mrs. Halloran"]);
        assert_eq!(m[0].description, "The shopkeeper of the village");

        let m = apply(
            r#"{"remove":["민수"],"merge":[{"keep":"민수","names":["지수"]}],"rename":[],"add":[],"aliases":[]}"#,
            "민수와 지수는 같은 사람이야",
        )
        .unwrap();
        assert_eq!(names(&m), ["Marta", "Tobias", "Mrs. Halloran", "민수"]);
        assert_eq!(m[3].aliases, ["지수"]);
    }

    #[test]
    fn a_no_op_merge_does_not_cancel_a_removal() {
        let m = apply(
            r#"{"remove":["Mrs. Halloran"],"merge":[{"keep":"Mrs. Halloran","names":["Mrs. Halloran"]},{"keep":"Marta","names":[]}],"rename":[{"from":"Marta","to":"Marta"}],"add":[],"aliases":[]}"#,
            "Mrs. Halloran is not a real character, remove her.",
        )
        .unwrap();
        assert_eq!(names(&m), ["Marta", "Tobias", "민수"]);
    }

    #[test]
    fn a_variant_of_a_listed_name_becomes_an_alias() {
        let m = apply(
            r#"{"remove":["Marta"],"merge":[{"keep":"Marta","names":["Miss Marta"]}],"rename":[],"add":[],"aliases":[]}"#,
            "Marta is also called Miss Marta.",
        )
        .unwrap();
        assert_eq!(names(&m), ["Marta", "Tobias", "Mrs. Halloran", "민수"]);
        assert_eq!(m[0].aliases, ["Miss Marta"]);
    }

    #[test]
    fn reversed_renames_are_turned_around() {
        let m = apply(
            r#"{"remove":["Tobias"],"merge":[],"rename":[{"from":"Tomas","to":"Tobias"}],"add":[],"aliases":[]}"#,
            "Tobias is actually named Tomas.",
        )
        .unwrap();
        assert_eq!(names(&m), ["Marta", "Tomas", "Mrs. Halloran", "민수"]);
        assert_eq!(m[1].aliases, ["Tobias"]);
    }

    #[test]
    fn a_merge_into_a_new_name_is_a_rename() {
        let m = apply(
            r#"{"remove":[],"merge":[{"keep":"Tomas","names":["Tobias"]},{"keep":"Marta","names":["Marta"]}],"rename":[],"add":[],"aliases":[]}"#,
            "Tobias is actually named Tomas.",
        )
        .unwrap();
        assert_eq!(names(&m), ["Marta", "Tomas", "Mrs. Halloran", "민수"]);
    }

    #[test]
    fn removing_an_unlisted_name_means_adding_it() {
        let m = apply(
            r#"{"remove":["Captain Reyes"],"merge":[],"rename":[],"add":[],"aliases":[]}"#,
            "There is also a character called Captain Reyes.",
        )
        .unwrap();
        assert_eq!(names(&m), ["Marta", "Tobias", "Mrs. Halloran", "민수", "Captain Reyes"]);
    }

    #[test]
    fn names_the_user_never_mentioned_are_left_alone() {
        // the model wanted to remove Tobias and merge him into the new character
        let m = apply(
            r#"{"remove":["Tobias"],"merge":[{"keep":"Tobias","names":["Captain Reyes"]}],"rename":[],"add":[{"name":"Captain Reyes","aliases":["Tobias"],"role":"x"}],"aliases":[]}"#,
            "There is also a character called Captain Reyes.",
        )
        .unwrap();
        assert_eq!(names(&m), ["Marta", "Tobias", "Mrs. Halloran", "민수", "Captain Reyes"]);
        assert!(m[4].aliases.is_empty());
    }

    #[test]
    fn people_can_be_pointed_at_by_description() {
        let m = apply(r#"{"remove":["Mrs. Halloran"],"merge":[],"rename":[],"add":[],"aliases":[]}"#, "the shopkeeper is not a character").unwrap();
        assert_eq!(names(&m), ["Marta", "Tobias", "민수"]);
    }

    #[test]
    fn nothing_usable_changes_nothing() {
        assert!(apply(r#"{"remove":[],"merge":[],"rename":[],"add":[],"aliases":[]}"#, "hmm").is_none());
        assert!(apply(r#"{"remove":["Tobias"],"merge":[],"rename":[{"from":"Marta","to":"Marta"}],"add":[],"aliases":[]}"#, "hmm").is_none());
    }
}
