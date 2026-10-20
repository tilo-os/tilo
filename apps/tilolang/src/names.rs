//! How views show the people and agents of the room.
//!
//! The record holds ids, the same on every replica; the window knows the names. A view that
//! shows `author.name()` shows what the room's roster calls that participant, the name this
//! device knows its person by for the participant at this window, and for software the label
//! of its kind, so an agent never reads as a person.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilolang_tilo::runtime::{NameHost, Principal};
use ui::presence::PresenceCtx;

/// The roster as the last frame's presence reported it.
#[derive(Clone, Default, PartialEq, Eq)]
struct Roster {
    names: HashMap<String, String>,
}

/// The names of the room, shared between the window and the runtime that asks for them.
#[derive(Clone)]
pub struct Names {
    me: Rc<str>,
    roster: Rc<RefCell<Roster>>,
}

impl Names {
    #[must_use]
    pub fn new(me: &str) -> Self {
        Self {
            me: Rc::from(me),
            roster: Rc::default(),
        }
    }

    /// Take the names presence reports now. Whether any changed.
    pub fn follow(&self, presence: &PresenceCtx) -> bool {
        let mut names: HashMap<String, String> = presence
            .names
            .keys()
            .map(|user| (user.0.clone(), presence.display_name(user)))
            .collect();
        if let Some(own) = presence.self_label() {
            names.insert(self.me.to_string(), own);
        }
        let next = Roster { names };
        if *self.roster.borrow() == next {
            return false;
        }
        *self.roster.borrow_mut() = next;
        true
    }

    /// The label to show for the participant `id`.
    #[must_use]
    pub fn label(&self, id: &str) -> String {
        if let Some(name) = self.roster.borrow().names.get(id) {
            return name.clone();
        }
        if let Some(kind) = id.strip_prefix("agent:") {
            return format!("{} (agent)", sentence(kind));
        }
        if &*self.me == id {
            return ui::local_identity::bound()
                .map(|identity| identity.display_name.trim().to_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "You".to_owned());
        }
        short(id)
    }
}

impl NameHost for Names {
    fn name(&self, who: &Principal) -> String {
        self.label(&who.0)
    }
}

/// `planner` as `Planner`.
fn sentence(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// An id no roster named, short enough to read: a long id keeps its first characters.
fn short(id: &str) -> String {
    const KEPT: usize = 8;
    if id.chars().count() <= KEPT * 2 {
        id.to_owned()
    } else {
        id.chars().take(KEPT).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_is_marked_as_one_and_an_unnamed_person_gets_a_short_label() {
        let names = Names::new("me-id");
        assert_eq!(names.label("agent:planner"), "Planner (agent)");
        assert_eq!(names.label("0123456789abcdef0123456789abcdef"), "01234567");
        assert_eq!(names.label("tomas"), "tomas");
    }

    #[test]
    fn the_window_s_own_participant_reads_as_you_without_a_known_name() {
        // No identity is bound under test.
        assert_eq!(Names::new("me-id").label("me-id"), "You");
    }
}
