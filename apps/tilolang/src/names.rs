//! How views show the people and agents of the workspace.
//!
//! The record holds ids, the same on every replica; the window knows the names. A view that
//! shows `author.name()` shows what the workspace's roster calls that participant, the name this
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

/// The names of the workspace, shared between the window and the runtime that asks for them.
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
        let mut names: HashMap<String, String> = others(presence).into_iter().collect();
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
        if &*self.me == id {
            return own_name();
        }
        tilolang_tilo::names::unnamed(id)
    }
}

/// What this device calls its person, when presence does not say.
fn own_name() -> String {
    ui::local_identity::bound()
        .map(|identity| identity.display_name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "You".to_owned())
}

/// The workspace's other participants as presence names them, by id.
fn others(presence: &PresenceCtx) -> Vec<(String, String)> {
    let mut others: Vec<(String, String)> = presence
        .names
        .keys()
        .map(|user| (user.0.clone(), presence.display_name(user)))
        .collect();
    others.sort();
    others
}

/// What a window tells an instance running out of its process it calls people
/// (`ToApp::Names`): its own person, and the others by id.
#[must_use]
pub fn said(presence: &PresenceCtx) -> (String, Vec<(String, String)>) {
    let me = presence.self_label().unwrap_or_else(own_name);
    (me, others(presence))
}

impl NameHost for Names {
    fn name(&self, who: &Principal) -> String {
        self.label(&who.0)
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
        assert_eq!(names.label("scenario:Mia"), "Mia");
    }

    #[test]
    fn the_window_s_own_participant_reads_as_you_without_a_known_name() {
        // No identity is bound under test.
        assert_eq!(Names::new("me-id").label("me-id"), "You");
    }
}
