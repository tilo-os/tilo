//! How a view names the room's participants.
//!
//! The record holds ids, the same on every replica; the window knows the names: what the room's
//! roster calls each participant, and what this device calls its own person. An instance that
//! runs in the window's process asks the window directly. One that runs elsewhere, in a
//! tilolang host, names people by what its window told it last ([`Told`]).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::runtime::{NameHost, Principal};

/// The label of the participant `id` when nobody named them: a sample person a scenario named
/// (`person("Mia")`) by that name, software by the label of its kind (so an agent never reads
/// as a person), and anyone else by the first characters of their id.
#[must_use]
pub fn unnamed(id: &str) -> String {
    const KEPT: usize = 8;
    if let Some(name) = id.strip_prefix("scenario:") {
        return name.to_owned();
    }
    // A package updating its own records: "Board, updating to version 1a2b3c4d".
    if let Some((app, module)) = crate::authority::migration_of(id) {
        let app = app.rsplit('.').next().unwrap_or(app);
        let mut chars = app.chars();
        let app: String = chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
        let version: String = module.chars().take(KEPT).collect();
        return format!("{app}, updating to version {version}");
    }
    if let Some(kind) = id.strip_prefix("agent:") {
        let mut chars = kind.chars();
        let kind: String = chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default();
        return format!("{kind} (agent)");
    }
    if id.chars().count() <= KEPT * 2 {
        id.to_owned()
    } else {
        id.chars().take(KEPT).collect()
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

/// Names nobody told: each participant by [`unnamed`].
pub struct Unnamed;

impl NameHost for Unnamed {
    fn name(&self, who: &Principal) -> String {
        unnamed(&who.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_participant_is_named_as_the_window_said_and_shortened_when_nobody_did() {
        let id = "95522b97c3d1e0f4a7b8c9d0e1f2a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4";
        let told = Told::new(id, Rc::new(Unnamed));
        assert_eq!(told.name(&Principal::new(id.to_owned())), "95522b97");
        assert_eq!(
            told.name(&Principal::new("agent:planner".to_owned())),
            "Planner (agent)"
        );
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
