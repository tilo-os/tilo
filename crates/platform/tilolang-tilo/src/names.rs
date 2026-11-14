//! How a view names the workspace's participants.
//!
//! The record holds ids, the same on every replica; the workspace's member list knows the names:
//! what it calls each participant, the instance's own person among them. A tilolang host names
//! people by the list its instance's connection heard last ([`Told`]).
//!
//! Naming is by what a participant is ([`actor_name`]): a person by their name, an app by its own
//! name, an agent by the name it goes by. A history row, a presence mark and a catch-up line say
//! who acted, never an internal id and never part of one.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use collaboration::history_source::authors::{ParticipantNames, actor_name, name_words};

use crate::runtime::{NameHost, Principal};

/// The name of the participant `id` when nobody named them, by what they are: a sample person a
/// scenario named (`person("Mia")`) by that name, an app by its own name, an agent by its name.
/// A person the reader has no name for is
/// [`UNKNOWN_AUTHOR_NAME`](collaboration::history_source::authors::UNKNOWN_AUTHOR_NAME), never
/// their key.
#[must_use]
pub fn unnamed(id: &str) -> String {
    named(id, &Unnamed)
}

/// The name of the participant `id` to a reader who calls people, apps and agents as `names`
/// does: a sample person a scenario named by that name, a package acting on its own records (one
/// of its own actions, or an update to a later build) as the app itself, and anyone else by
/// [`actor_name`].
#[must_use]
pub fn named(id: &str, names: &dyn ParticipantNames) -> String {
    if let Some(name) = id
        .strip_prefix("scenario:")
        .or_else(|| id.strip_prefix("agent:scenario:"))
    {
        return name.to_owned();
    }
    if let Some(app) = crate::authority::trigger_of(id) {
        return names.app(app);
    }
    if let Some((app, _)) = crate::authority::migration_of(id) {
        return names.app(app);
    }
    actor_name(id, names)
}

/// What a participant reads as when the reader's own process knows no name for them.
pub struct Unnamed;

impl ParticipantNames for Unnamed {
    /// An agent by its name, and that it is one: a row of history, a face and a mark say so
    /// beside a person's changes.
    fn agent(&self, name: &str) -> String {
        format!("{} (agent)", name_words(name))
    }
}

impl NameHost for Unnamed {
    fn name(&self, who: &Principal) -> String {
        unnamed(&who.0)
    }
}

/// What the member list calls the participants.
#[derive(Default, PartialEq, Eq)]
struct Said {
    me: Option<String>,
    others: HashMap<String, String>,
}

/// The names an instance shows people by: what its host heard the member list call them
/// ([`Told::tell`]) first, and what its embedder knows otherwise.
pub struct Told {
    me: String,
    said: RefCell<Said>,
    otherwise: Rc<dyn NameHost>,
}

impl Told {
    /// The names for the person `me`, falling back to `otherwise`.
    #[must_use]
    pub fn new(me: &str, otherwise: Rc<dyn NameHost>) -> Self {
        Self {
            me: me.to_owned(),
            said: RefCell::default(),
            otherwise,
        }
    }

    /// The member list calls the person `me` and each of `others` by id now. Whether any name
    /// changed.
    pub fn tell(&self, me: String, others: Vec<(String, String)>) -> bool {
        let next = Said {
            me: Some(me).filter(|me| !me.trim().is_empty()),
            others: others
                .into_iter()
                .filter(|(_, name)| !name.trim().is_empty())
                .collect(),
        };
        if *self.said.borrow() == next {
            return false;
        }
        *self.said.borrow_mut() = next;
        true
    }
}

impl NameHost for Told {
    fn name(&self, who: &Principal) -> String {
        let said = self.said.borrow();
        if who.0 == self.me
            && let Some(me) = &said.me
        {
            return me.clone();
        }
        if let Some(name) = said.others.get(&who.0) {
            return name.clone();
        }
        drop(said);
        self.otherwise.name(who)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a window with nobody named yet calls each id: by what the id says it is, never by
    /// part of a key.
    #[test]
    fn a_participant_nobody_named_is_named_by_what_they_are_never_by_part_of_their_id() {
        let told = Told::new("me-id", Rc::new(Unnamed));
        let rows: &[(&str, &str, &str)] = &[
            ("agent:planner", "Planner (agent)", "an agent by its name"),
            (
                "0123456789abcdef0123456789abcdef",
                "Unknown participant",
                "a key nobody named is never shown, in full or cut short",
            ),
            ("tomas", "Unknown participant", "a person nobody named"),
            ("scenario:Mia", "Mia", "a scenario's sample person"),
            (
                "trigger:so.tilo.lang.today",
                "Today",
                "an app acting by itself",
            ),
            ("app:so.tilo.lang.today", "Today", "an app by its own name"),
            ("app:so.tilo.lang.notes", "Notes", "an app by its own name"),
            (
                "app:org.example.Editor",
                "Editor",
                "an app installed nowhere here reads as the words its id makes",
            ),
            ("", "Unknown participant", "no id at all"),
            (
                "agent:",
                "Unknown participant (agent)",
                "an agent that gave no name still reads as an agent",
            ),
            (
                "  agent:planner  ",
                "Planner (agent)",
                "the spaces around an id are not part of it",
            ),
            ("app:", "Unknown participant", "an app id with no words"),
            (
                "app:org.gnome.TextEditor",
                "Text Editor",
                "a camel-case hump is a space",
            ),
            (
                "app:so.tilo.lang.read-later",
                "Read Later",
                "a dash is a space, and each word is capitalised",
            ),
            (
                "bot:helper",
                "Helper",
                "a bot nobody named, by its own name",
            ),
            ("shortcut:morning", "Morning", "a shortcut by its name"),
            (
                "migration:so.tilo.lang.today@1f2e",
                "Today",
                "an update to a later build is the app's own act",
            ),
        ];
        for (id, expected, why) in rows {
            assert_eq!(
                told.name(&Principal::new((*id).to_owned())),
                *expected,
                "{id:?}: {why}"
            );
        }
    }

    #[test]
    fn a_participant_is_named_as_the_window_said_and_by_what_they_are_when_nobody_did() {
        let id = "95522b97c3d1e0f4a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4";
        let told = Told::new(id, Rc::new(Unnamed));
        assert_eq!(
            told.name(&Principal::new(id.to_owned())),
            "Unknown participant",
            "a key nobody named is never shown, in full or cut short"
        );
        assert_eq!(
            told.name(&Principal::new("agent:planner".to_owned())),
            "Planner (agent)"
        );
        assert_eq!(
            told.name(&Principal::new("app:so.tilo.lang.today".to_owned())),
            "Today",
            "an app is its own name"
        );
        assert_eq!(unnamed("trigger:t.board"), "Board");
        assert!(told.tell(
            "Ada".to_owned(),
            vec![("ben-id".to_owned(), "Ben".to_owned())]
        ));
        assert!(!told.tell(
            "Ada".to_owned(),
            vec![("ben-id".to_owned(), "Ben".to_owned())]
        ));
        assert_eq!(told.name(&Principal::new(id.to_owned())), "Ada");
        assert_eq!(told.name(&Principal::new("ben-id".to_owned())), "Ben");
    }
}
