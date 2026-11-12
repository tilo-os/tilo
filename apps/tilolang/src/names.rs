//! How views show the people, agents and apps of the workspace.
//!
//! The record holds ids, the same on every replica; the window knows the names. A view that
//! shows `author.name()` shows what the workspace's roster calls that participant, the name this
//! device knows its person by for the participant at this window, and for software the name it
//! goes by — an app in the person's own language, an agent with the label of its kind, so an
//! agent never reads as a person.

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

    /// The label to show for the participant `id`: a person by the name the roster calls them, an
    /// app by its own name in the person's language, an agent by its name.
    #[must_use]
    pub fn label(&self, id: &str) -> String {
        if let Some(name) = self.roster.borrow().names.get(id) {
            return name.clone();
        }
        if &*self.me == id {
            return own_name();
        }
        tilolang_tilo::names::named(id, self)
    }
}

impl ui::history_source::ParticipantNames for Names {
    fn person(&self, id: &str) -> Option<String> {
        self.roster.borrow().names.get(id).cloned()
    }

    /// An app by its own name in the language this window speaks, from the names the session
    /// sends with the workspace's people: an app installed nowhere near this device reads as the
    /// words its id makes, never as its id.
    fn app(&self, app_id: &str) -> String {
        ui::history_source::app_name(app_id, ui::i18n::I18nManager::window_locale().as_str())
    }

    /// An agent by its name, marked as one: the window's rows say which hands are software.
    fn agent(&self, name: &str) -> String {
        format!("{} (agent)", ui::history_source::name_words(name))
    }
}

/// What this device calls its person, when presence does not say.
fn own_name() -> String {
    ui::local_identity::own_name().unwrap_or_else(|| "You".to_owned())
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
    fn a_participant_is_named_by_what_they_are_never_by_part_of_their_id() {
        let names = Names::new("me-id");
        assert_eq!(names.label("agent:planner"), "Planner (agent)");
        assert_eq!(
            names.label("0123456789abcdef0123456789abcdef"),
            "Unknown participant",
            "a key nobody named is never shown, in full or cut short"
        );
        assert_eq!(names.label("tomas"), "Unknown participant");
        assert_eq!(names.label("scenario:Mia"), "Mia");
        assert_eq!(names.label("trigger:so.tilo.lang.today"), "Today");
    }

    #[test]
    fn an_app_reads_as_the_name_the_session_sent_for_it() {
        let names = Names::new("me-id");
        assert_eq!(names.label("app:so.tilo.lang.today"), "Today");
        assert_eq!(names.label("app:so.tilo.lang.notes"), "Notes");
        assert_eq!(
            names.label("app:org.example.Editor"),
            "Editor",
            "an app installed nowhere here reads as the words its id makes"
        );
    }

    #[test]
    fn the_window_s_own_participant_reads_as_you_without_a_known_name() {
        // No identity is bound under test.
        assert_eq!(Names::new("me-id").label("me-id"), "You");
    }
}
