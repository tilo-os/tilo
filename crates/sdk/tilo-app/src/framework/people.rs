//! Who the participants are, by name: what a view or surface shows for an
//! id an action recorded ([`ActionCx::author`](tilo_resource::ActionCx::author))
//! or the participant reading ([`RecordEnv::me`](super::RecordEnv::me)).
//!
//! The document records ids — the same on every replica, whatever anyone is
//! called — and the host knows the names: the room's roster announces them
//! with the people who are present. A view resolves an id when it shows it,
//! so a renamed person, or one whose name arrived after their edit, reads
//! correctly everywhere at once.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use ui_core::LocalState;
use ui_core::permissions::UserId;
use ui_core::presence::PresenceCtx;

/// The names the host knows for the participants of the window's room.
/// Cloneable; clones read the same roster.
#[derive(Clone)]
pub struct People {
    roster: Rc<RefCell<Roster>>,
    revision: LocalState<u64>,
    /// The participant this window is for, when someone edits it.
    me: Option<Rc<str>>,
}

#[derive(Default, PartialEq)]
struct Roster {
    names: HashMap<String, String>,
    agents: Vec<String>,
    /// The name this session's participant goes by, when the room said.
    own: Option<String>,
}

/// What an id reads as when the participant is this window's own and the
/// host has not said their name.
const YOU: &str = "You";

impl People {
    pub(crate) fn new(me: Option<&str>) -> Self {
        Self {
            roster: Rc::default(),
            revision: LocalState::new(0),
            me: me.map(Rc::from),
        }
    }

    /// The name to show for `id`: the roster's, the window's own participant
    /// as "You" until the room names them, else the id itself — what a
    /// record written before ids were names holds, and what a capture's
    /// fixture people are called.
    pub fn name(&self, id: &str) -> String {
        let roster = self.roster.borrow();
        if let Some(name) = roster.names.get(id) {
            return if roster.agents.iter().any(|agent| agent == id) {
                ui_core::history_source::agent_display_label(name)
            } else {
                name.clone()
            };
        }
        if self.me.as_deref() == Some(id) {
            return roster.own.clone().unwrap_or_else(|| YOU.to_owned());
        }
        id.to_owned()
    }

    /// Whether `id` is the participant this window is for.
    pub fn is_me(&self, id: &str) -> bool {
        self.me.as_deref() == Some(id)
    }

    /// Increases whenever a name changes, for a view to show names that
    /// follow the roster (`Text::from_state(&people.revision(), ..)`).
    pub fn revision(&self) -> LocalState<u64> {
        self.revision.clone()
    }

    /// Take the names the host's roster holds now. Whether any changed.
    pub(crate) fn follow(&self, presence: &PresenceCtx) -> bool {
        let next = Roster {
            names: presence
                .names
                .iter()
                .map(|(UserId(id), name)| (id.clone(), name.clone()))
                .collect(),
            agents: presence
                .names
                .keys()
                .filter(|user| presence.is_agent(user))
                .map(|UserId(id)| id.clone())
                .collect(),
            own: presence.self_name().map(str::to_owned),
        };
        if *self.roster.borrow() == next {
            return false;
        }
        *self.roster.borrow_mut() = next;
        self.revision.set(self.revision.get().wrapping_add(1));
        true
    }

    /// People named as `names` gives them, for a capture whose fixture
    /// places collaborators, or a test.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn named(me: Option<&str>, names: &[(&str, &str)]) -> Self {
        let people = Self::new(me);
        people.roster.borrow_mut().names = names
            .iter()
            .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
            .collect();
        people
    }
}

impl std::fmt::Debug for People {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("People")
            .field("me", &self.me)
            .field("known", &self.roster.borrow().names.len())
            .field("revision", &self.revision.get())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_reads_as_the_rosters_name_you_or_itself() {
        let people = People::new(Some("me-id"));
        assert_eq!(people.name("me-id"), "You");
        assert_eq!(
            people.name("Alice"),
            "Alice",
            "an unknown id reads as itself"
        );

        let mut presence = PresenceCtx::default();
        presence.set_display_name(UserId("peer-id".into()), "Bob");
        presence.set_self_participant(UserId("me-id".into()), "Ada");
        assert!(people.follow(&presence));
        assert!(
            !people.follow(&presence),
            "an unchanged roster changes nothing"
        );
        assert_eq!(people.name("peer-id"), "Bob");
        assert_eq!(people.name("me-id"), "Ada");
        assert!(people.is_me("me-id"));
        assert_eq!(people.revision().get(), 1);
    }
}
