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

    tilo_test_env::scenario! {
        fn the_window_names_each_participant_by_the_roster_and_then_by_what_they_are;

        phase a_participant_is_named_by_what_they_are_never_by_part_of_their_id;
        // What a window with no roster and no identity bound, as under test, calls each id: the id,
        // the label, and why.
        {
            let names = Names::new("me-id");
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
                (
                    "me-id",
                    "You",
                    "the window's own participant, no identity bound",
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
                assert_eq!(names.label(id), *expected, "{id:?}: {why}");
            }
        }

        phase the_roster_names_a_participant_first_and_follow_says_when_a_name_changed;
        // The roster's name for a participant comes before what their id says they are, the window's
        // own person reads as the workspace calls them once it says, and `follow` says whether a
        // name changed: the same roster said again changes nothing, and a person the roster no
        // longer names is unnamed again, never shown by the name they had.
        {
            use ui::permissions::UserId;

            let ben = || UserId("ben-id".to_owned());
            let names = Names::new("me-id");
            let mut presence = PresenceCtx::default();
            assert!(
                !names.follow(&presence),
                "no roster yet and none before: nothing changed"
            );

            presence.names.insert(ben(), "Ben".to_owned());
            assert!(names.follow(&presence), "the roster named a person");
            assert_eq!(names.label("ben-id"), "Ben");
            assert_eq!(
                names.label("tomas"),
                "Unknown participant",
                "a person the roster does not name stays unnamed"
            );
            assert_eq!(
                names.label("me-id"),
                "You",
                "the workspace has not said what it calls this window's person"
            );
            assert!(
                !names.follow(&presence),
                "the same roster said again changes nothing"
            );

            presence.set_self_participant(UserId("me-id".to_owned()), "Ada");
            assert!(
                names.follow(&presence),
                "the workspace named this window's person"
            );
            assert_eq!(names.label("me-id"), "Ada");
            assert_eq!(
                said(&presence),
                (
                    "Ada".to_owned(),
                    vec![("ben-id".to_owned(), "Ben".to_owned())]
                ),
                "what the window tells an instance out of its process: its person, and the others"
            );

            presence.names.remove(&ben());
            assert!(
                names.follow(&presence),
                "the roster no longer names a person"
            );
            assert_eq!(
                names.label("ben-id"),
                "Unknown participant",
                "a person the roster dropped is not shown by the name they had"
            );
            assert_eq!(
                names.label("me-id"),
                "Ada",
                "and the window's person keeps theirs"
            );
        }
    }
}
