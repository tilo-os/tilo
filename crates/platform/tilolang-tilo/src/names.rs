//! How a view names the workspace's participants.
//!
//! The record holds ids, the same on every replica; the window knows the names: what the workspace's
//! roster calls each participant, and what this device calls its own person. An instance that
//! runs in the window's process asks the window directly. One that runs elsewhere, in a
//! tilolang host, names people by what its window told it last ([`Told`]).
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
    if let Some(name) = id.strip_prefix("scenario:") {
        return name.to_owned();
    }
    // A package working on its own records, running one of its own actions or updating them to a
    // later build: the app itself, named as the app it is.
    if let Some(app) = crate::authority::trigger_of(id) {
        return Unnamed.app(app);
    }
    if let Some((app, _)) = crate::authority::migration_of(id) {
        return Unnamed.app(app);
    }
    actor_name(id, &Unnamed)
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

/// What the window said it calls the participants.
#[derive(Default, PartialEq, Eq)]
struct Said {
    me: Option<String>,
    others: HashMap<String, String>,
}

/// The names an instance shows people by: what its window told it ([`Told::tell`]) first, and
/// what its embedder knows otherwise.
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

    /// The window calls its person `me` and each of `others` by id now. Whether any name
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
