//! Telling people apart by name: the one rule every surface that names a
//! participant — chat lines, cursor pills, people lists, the top bar,
//! mentions — follows.
//!
//! Two people in a room may go by the same name. Each is then shown with a
//! number, "Ada (1)" and "Ada (2)", and everyone in the room must see the
//! same numbers, so they come from an order every replica shares: when each
//! joined the room, as its membership records it, so a newcomer never takes
//! an earlier member's number; then the participants' ids, which the room
//! names identically on every machine — never the order this machine
//! happened to hear of them. A
//! name compares case- and whitespace-insensitively ("ada  lovelace" and
//! "Ada Lovelace" collide). Once the names no longer collide, the numbers
//! go away.
//!
//! Only people are numbered: an agent, a Shortcut or an app is already
//! told apart by its kind label.

use std::collections::{BTreeMap, HashMap};

/// The numbers a room's participants are told apart by, resolved once from
/// its roster and read for each label shown. Cheap to rebuild whenever the
/// roster changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParticipantLabels {
    /// Each participant whose name collides: the name's comparison key and
    /// their 1-based number among those sharing it.
    numbered: HashMap<String, (String, usize)>,
}

impl ParticipantLabels {
    /// Resolve the numbers for `people`, each an `(id, name)` pair. Order
    /// does not matter, and a repeated id counts once (its last name). A
    /// blank name collides with nothing: it is shown as whatever stands in
    /// for it.
    #[must_use]
    pub fn new<'a>(people: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self::by_joining(people.into_iter().map(|(id, name)| (id, name, None)))
    }

    /// [`Self::new`] for `people` as `(id, name, joined)`, `joined` being
    /// when the room admitted them (any unit every replica shares, such as
    /// the admission's Unix time). Whoever joined first is numbered first;
    /// those the room has no time for follow, by id.
    #[must_use]
    pub fn by_joining<'a>(
        people: impl IntoIterator<Item = (&'a str, &'a str, Option<u64>)>,
    ) -> Self {
        let mut by_id: BTreeMap<&str, (String, Option<u64>)> = BTreeMap::new();
        for (id, name, joined) in people {
            if id.trim().is_empty() {
                continue;
            }
            by_id.insert(id, (name_key(name), joined));
        }
        let mut ordered: Vec<(&str, &String, Option<u64>)> = by_id
            .iter()
            .map(|(id, (key, joined))| (*id, key, *joined))
            .collect();
        ordered.sort_by_key(|(id, _, joined)| (joined.is_none(), *joined, *id));
        let mut sharing: HashMap<&str, Vec<&str>> = HashMap::new();
        // In their shared order, so each name's list is too.
        for (id, key, _) in ordered {
            if !key.is_empty() {
                sharing.entry(key.as_str()).or_default().push(id);
            }
        }
        let mut numbered = HashMap::new();
        for (key, ids) in sharing {
            if ids.len() < 2 {
                continue;
            }
            for (index, id) in ids.into_iter().enumerate() {
                numbered.insert(id.to_owned(), (key.to_owned(), index + 1));
            }
        }
        Self { numbered }
    }

    /// The number `id` is told apart by, when their name collides.
    #[must_use]
    pub fn number(&self, id: &str) -> Option<usize> {
        self.numbered.get(id).map(|(_, number)| *number)
    }

    /// `name` as `id` is shown: "Ada (2)" when the room has more than one
    /// Ada, `name` itself otherwise. The number is only added while `name`
    /// is still the one that collided, so a caller holding a newer name than
    /// the roster's never shows a number that belongs to the old one.
    #[must_use]
    pub fn label(&self, id: &str, name: &str) -> String {
        match self.numbered.get(id) {
            Some((key, number)) if *key == name_key(name) => crate::i18n::widget_format(
                "participant-numbered",
                &[
                    ("name", name.trim().into()),
                    ("number", number.to_string().into()),
                ],
            ),
            _ => name.to_owned(),
        }
    }

    /// Whether any participant needs a number.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.numbered.is_empty()
    }
}

/// What two names compare as: case-folded, with runs of whitespace as one
/// space and none at the ends.
#[must_use]
pub fn name_key(name: &str) -> String {
    name.split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_names_are_shown_as_they_are() {
        let labels = ParticipantLabels::new([("a", "Ada"), ("b", "Bea")]);
        assert!(labels.is_empty());
        assert_eq!(labels.label("a", "Ada"), "Ada");
        assert_eq!(labels.number("b"), None);
    }

    #[test]
    fn colliding_names_are_numbered_case_and_space_insensitively() {
        let labels = ParticipantLabels::new([
            ("peer-b", "ada  lovelace"),
            ("peer-a", " Ada Lovelace"),
            ("peer-c", "Bea"),
        ]);
        assert_eq!(labels.label("peer-a", " Ada Lovelace"), "Ada Lovelace (1)");
        assert_eq!(labels.label("peer-b", "ada  lovelace"), "ada  lovelace (2)");
        assert_eq!(labels.label("peer-c", "Bea"), "Bea");
    }

    #[test]
    fn every_replica_numbers_alike_whatever_order_it_heard_people_in() {
        let people = [("k3", "Sam"), ("k1", "Sam"), ("k2", "sam"), ("k4", "Kim")];
        let first = ParticipantLabels::new(people);
        let mut reversed = people;
        reversed.reverse();
        assert_eq!(first, ParticipantLabels::new(reversed));
        let rotated = [people[2], people[3], people[0], people[1]];
        assert_eq!(first, ParticipantLabels::new(rotated));
        assert_eq!(first.number("k1"), Some(1));
        assert_eq!(first.number("k2"), Some(2));
        assert_eq!(first.number("k3"), Some(3));
        assert_eq!(first.number("k4"), None);
    }

    #[test]
    fn whoever_joined_first_keeps_their_number_when_a_namesake_arrives() {
        let before =
            ParticipantLabels::by_joining([("zz", "Alex", Some(10)), ("mm", "Alex", Some(20))]);
        assert_eq!(before.number("zz"), Some(1));
        assert_eq!(before.number("mm"), Some(2));
        let after = ParticipantLabels::by_joining([
            ("zz", "Alex", Some(10)),
            ("mm", "Alex", Some(20)),
            ("aa", "Alex", Some(30)),
            ("bb", "Alex", None),
        ]);
        assert_eq!(after.number("zz"), Some(1), "an earlier member keeps (1)");
        assert_eq!(after.number("mm"), Some(2));
        assert_eq!(after.number("aa"), Some(3), "the newcomer takes the next");
        assert_eq!(after.number("bb"), Some(4), "no join time sorts last");
    }

    #[test]
    fn numbers_go_away_once_the_names_no_longer_collide() {
        let colliding = ParticipantLabels::new([("a", "Ada"), ("b", "Ada")]);
        assert_eq!(colliding.label("b", "Ada"), "Ada (2)");
        let renamed = ParticipantLabels::new([("a", "Ada"), ("b", "Ada B")]);
        assert!(renamed.is_empty());
        assert_eq!(renamed.label("a", "Ada"), "Ada");
        let departed = ParticipantLabels::new([("a", "Ada")]);
        assert_eq!(departed.label("a", "Ada"), "Ada");
    }

    #[test]
    fn a_newer_name_than_the_rosters_carries_no_stale_number() {
        let labels = ParticipantLabels::new([("a", "Ada"), ("b", "Ada")]);
        assert_eq!(labels.label("b", "Bea"), "Bea");
    }

    #[test]
    fn blank_names_and_ids_collide_with_nothing() {
        let labels = ParticipantLabels::new([("a", " "), ("b", ""), ("", "Ada"), ("c", "Ada")]);
        assert!(labels.is_empty());
    }

    #[test]
    fn a_repeated_id_counts_once() {
        let labels = ParticipantLabels::new([("a", "Ada"), ("a", "Ada")]);
        assert!(labels.is_empty());
    }
}
