//! Who the participants are, by name: what a view or surface shows for an
//! id an action recorded ([`ActionCx::author`](tilo_resource::ActionCx::author))
//! or the participant reading ([`RecordEnv::me`](super::RecordEnv::me)).
//!
//! The document records ids — the same on every replica, whatever anyone is
//! called — and the host knows the names: the room's roster announces them
//! with the people who are present. A view resolves an id when it shows it,
//! so a renamed person, or one whose name arrived after their edit, reads
//! correctly everywhere at once. Names are the room's labels: people who
//! share a name carry the number presence tells them apart by
//! ([`ui_core::presence::ParticipantLabels`]), and the participant reading
//! is shown their own name like anyone else's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_resource::{Binds, Resource};
use ui_core::LocalState;
use ui_core::presence::PresenceCtx;
use ui_core::widget::mention_picker::{MentionCandidate, merged_roster};
use ui_core::widget::{Dropdown, Element};

use super::ViewCx;

/// The names the host knows for the participants of the window's room.
/// Cloneable; clones read the same roster.
#[derive(Clone)]
pub struct People {
    roster: Rc<RefCell<Roster>>,
    revision: LocalState<u64>,
    /// The participant this window is for, when someone edits it.
    me: Option<Rc<str>>,
    /// The name this device knows its person by, for `me` until the room
    /// names them.
    local: Option<Rc<str>>,
}

#[derive(Default, PartialEq)]
struct Roster {
    /// Each participant the room names, as presence labels them.
    names: HashMap<String, String>,
    /// The label this session's participant goes by, when the room said.
    own: Option<String>,
    /// Everyone the room names, by name, as `@` offers them.
    members: Vec<MentionCandidate>,
}

impl People {
    pub(crate) fn new(me: Option<&str>) -> Self {
        Self {
            roster: Rc::default(),
            revision: LocalState::new(0),
            local: me.and_then(|_| {
                ui_core::local_identity::bound()
                    .map(|identity| identity.display_name.trim().to_owned())
                    .filter(|name| !name.is_empty())
                    .map(Rc::from)
            }),
            me: me.map(Rc::from),
        }
    }

    /// The name to show for `id`: the roster's label; for the window's own
    /// participant before the room names them, the name this device knows
    /// its person by; else the id itself — what a record written before ids
    /// were names holds, and what a capture's fixture people are called.
    pub fn name(&self, id: &str) -> String {
        let roster = self.roster.borrow();
        if let Some(name) = roster.names.get(id) {
            return name.clone();
        }
        if self.me.as_deref() == Some(id)
            && let Some(own) = roster.own.as_deref().or(self.local.as_deref())
        {
            return own.to_owned();
        }
        id.to_owned()
    }

    /// The room's people and agents, this window's own participant among
    /// them, sorted by name: whom a picker of members offers
    /// ([`ViewCx::member_picker`]).
    pub fn members(&self) -> Vec<MentionCandidate> {
        self.roster.borrow().members.clone()
    }

    /// Whether another person is in the room with this window now.
    pub fn others_here(&self) -> bool {
        self.roster.borrow().members.iter().any(|member| {
            !member.is_self && member.kind == ui_core::presence::ParticipantKind::Person
        })
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
                .keys()
                .map(|user| (user.0.clone(), presence.display_name(user)))
                .collect(),
            own: presence.self_label(),
            members: merged_roster(presence, &[]),
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

impl<R: Resource, T: 'static> ViewCx<R, T> {
    /// A dropdown of the room's people and agents by name, bound to a verb
    /// as [`Self::control_with`] binds a control: it shows `read` of the
    /// resource and invokes `action` with the value of the one picked.
    /// `first` are the options before the members (nobody, say); `value_of`
    /// is how the record names a member, `None` leaving one out; `make`
    /// dresses the bound dropdown (its id, its label). Built again as the roster
    /// changes, so it offers whom the room names now.
    pub fn member_picker<B: Binds<R, String> + Clone>(
        &self,
        first: Vec<(String, String)>,
        value_of: impl Fn(&MentionCandidate) -> Option<String> + 'static,
        make: impl Fn(Dropdown) -> Element + 'static,
        read: impl Fn(&T) -> String + 'static,
        action: B,
    ) -> Element {
        let people = self.people();
        let read = Rc::new(read);
        self.switch(&people.revision(), move |cx, _| {
            let options = first
                .iter()
                .cloned()
                .chain(
                    people
                        .members()
                        .iter()
                        .filter_map(|member| Some((value_of(member)?, member.name.clone()))),
                )
                .collect();
            let read = Rc::clone(&read);
            make(cx.control_with(
                |state| {
                    let mut picker = Dropdown::from_state(Vec::new(), state);
                    picker.set_value_options(options);
                    picker
                },
                move |state| read(state),
                action.clone(),
            ))
        })
    }
}

impl std::fmt::Debug for People {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("People")
            .field("me", &self.me)
            .field("local", &self.local)
            .field("known", &self.roster.borrow().names.len())
            .field("revision", &self.revision.get())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_core::permissions::UserId;

    #[test]
    fn an_id_reads_as_the_rosters_name_or_itself() {
        let people = People::new(Some("me-id"));
        assert_eq!(
            people.name("me-id"),
            "me-id",
            "nobody is called \"You\": an unnamed self reads as its id"
        );
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

        presence.set_display_name(UserId("peer-id".into()), "ada");
        assert!(people.follow(&presence));
        assert_eq!(people.name("me-id"), "Ada (1)");
        assert_eq!(people.name("peer-id"), "ada (2)");
    }
}
