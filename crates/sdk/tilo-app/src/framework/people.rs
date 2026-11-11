//! Who the participants are, by name: what a view or surface shows for an
//! id an action recorded ([`ActionCx::author`](tilo_resource::ActionCx::author))
//! or the participant reading ([`RecordEnv::me`](super::RecordEnv::me)).
//!
//! The document records ids — the same on every replica, whatever anyone is
//! called — and the host knows the names: the workspace's roster announces them
//! with the people who are present. A view resolves an id when it shows it,
//! so a renamed person, or one whose name arrived after their edit, reads
//! correctly everywhere at once. Names are the workspace's labels: people who
//! share a name carry the number presence tells them apart by
//! ([`ui_core::presence::ParticipantLabels`]), and the participant reading
//! is shown their own name like anyone else's.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_resource::{ActionKey, Binds, Resource};
use ui_core::LocalState;
use ui_core::icons::Icon;
use ui_core::presence::{ParticipantKind, PresenceCtx};
use ui_core::tokens::PresenceHue;
use ui_core::widget::mention_picker::{MentionCandidate, merged_roster};
use ui_core::widget::{Button, ButtonRole, Element, Stack, Tone};

use super::ViewCx;

/// The names the host knows for the participants of the window's workspace.
/// Cloneable; clones read the same roster.
#[derive(Clone)]
pub struct People {
    roster: Rc<RefCell<Roster>>,
    revision: LocalState<u64>,
    /// The participant this window is for, when someone edits it.
    me: Option<Rc<str>>,
    /// The name this device knows its person by, for `me` until the workspace
    /// names them.
    local: Option<Rc<str>>,
}

#[derive(Default, PartialEq)]
struct Roster {
    /// Each participant the workspace names, as presence labels them.
    names: HashMap<String, String>,
    /// The colour each of them wears, as presence paints them: the one they
    /// chose, or the one their identity derives.
    hues: HashMap<String, PresenceHue>,
    /// The label this session's participant goes by, when the workspace said.
    own: Option<String>,
    /// Everyone the workspace names, by name, as `@` offers them.
    members: Vec<MentionCandidate>,
}

/// A participant as a view shows them: what the workspace calls the id an
/// action recorded, the id presence knows them by (which a face takes its
/// colour and photograph from), and whether the workspace offers them as an
/// agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    /// The id presence names them by: an agent's `agent:<id>`, a person's
    /// bare id.
    pub id: String,
    /// What the workspace calls them, or their id where it names them not.
    pub name: String,
    /// Whether they are an agent of the workspace, not a person in it.
    pub agent: bool,
}

impl People {
    pub(crate) fn new(me: Option<&str>) -> Self {
        Self {
            roster: Rc::default(),
            revision: LocalState::new(0),
            local: me
                .and_then(|_| ui_core::local_identity::own_name())
                .map(Rc::from),
            me: me.map(Rc::from),
        }
    }

    /// The name to show for `id`: the roster's label; for the window's own
    /// participant before the workspace names them, the name this device knows
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

    /// The hue `id` wears: the one the workspace announced for them, else the
    /// one their identity derives, so a view paints a face, a card or a chip
    /// in the same colour presence paints their caret and their byline.
    pub fn hue(&self, id: &str) -> PresenceHue {
        self.roster
            .borrow()
            .hues
            .get(id)
            .copied()
            .unwrap_or_else(|| PresenceHue::of_identity(id))
    }

    /// The workspace's people and agents, this window's own participant among
    /// them, sorted by name: whom a picker of members offers
    /// ([`ViewCx::person_picker`]).
    pub fn members(&self) -> Vec<MentionCandidate> {
        self.roster.borrow().members.clone()
    }

    /// Who the id an action recorded names, however the record spells it: a
    /// person as an assignee holds them (`person:<id>`), an agent as the
    /// action door names it (`agent:<id>`), or the bare id. The name is the
    /// workspace's, else the id without its prefix: a view never shows a
    /// record's spelling of a participant.
    pub fn actor(&self, id: &str) -> Actor {
        let (identity, bare, agent) = if let Some(bare) = id.strip_prefix("person:") {
            (bare, bare, false)
        } else if let Some(bare) = id.strip_prefix("agent:") {
            (id, bare, true)
        } else {
            (id, id, false)
        };
        let name = [identity, bare]
            .into_iter()
            .map(|key| self.name(key))
            .find(|name| name != identity && name != bare)
            .unwrap_or_else(|| bare.to_owned());
        Actor {
            id: identity.to_owned(),
            name,
            agent,
        }
    }

    /// Whether another person is in the workspace with this window now.
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
            hues: presence
                .names
                .keys()
                .map(|user| (user.0.clone(), presence.hue_of(&user.0)))
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
    /// A chip showing whom the resource assigns `read` to — their avatar and
    /// name, or `nobody` while it names no one — that opens the workspace's
    /// people and agents, each with their avatar, and invokes `action` with
    /// the value the record stores for the one picked (`None` for nobody).
    /// `value_of` is how the record names a member, `None` leaving one out;
    /// `label` names the control for assistive technology. Built again as the
    /// roster changes, so it offers whom the workspace names now, and as the
    /// record's value changes, so the chip follows an assignment made
    /// anywhere.
    pub fn person_picker<B: Binds<R, Option<String>> + Clone>(
        &self,
        id: &'static str,
        label: String,
        nobody: String,
        value_of: impl Fn(&MentionCandidate) -> Option<String> + 'static,
        read: impl Fn(&T) -> Option<String> + 'static,
        action: B,
    ) -> Element {
        let people = self.people();
        let revision = people.revision();
        let label = Rc::new(label);
        let nobody = Rc::new(nobody);
        let value_of = Rc::new(value_of);
        let read = Rc::new(read);
        self.switch(&revision, move |cx, _| {
            let members = people.members();
            let held = {
                let read = Rc::clone(&read);
                cx.value(move |state: &T| read(state))
            };
            let people = people.clone();
            let (label, nobody, value_of) =
                (Rc::clone(&label), Rc::clone(&nobody), Rc::clone(&value_of));
            let held_action = action.clone();
            cx.switch(&held, move |cx, held| {
                let who = held.as_deref().map(|value| people.actor(value));
                let mut choices = Stack::new();
                choices = choices.push(cx.person_choice(
                    &format!("{id}-none"),
                    &nobody,
                    None,
                    None,
                    false,
                    held_action.clone(),
                ));
                for member in &members {
                    let Some(value) = value_of(member) else {
                        continue;
                    };
                    choices = choices.push(cx.person_choice(
                        &format!("{id}-{}", member.id),
                        &member.name,
                        Some(value),
                        Some((&member.name, &member.id)),
                        member.kind == ParticipantKind::Agent,
                        held_action.clone(),
                    ));
                }
                let (name, agent, chip) = match &who {
                    Some(who) => (
                        who.name.clone(),
                        who.agent,
                        Button::new(id, &who.name)
                            .with_avatar(&who.name, &who.id)
                            .showing_choice(),
                    ),
                    None => (
                        nobody.to_string(),
                        false,
                        Button::new(id, nobody.as_str())
                            .with_icon(Icon::User)
                            .with_placeholder()
                            .showing_choice(),
                    ),
                };
                let chip = chip.with_accessible_name(&format!("{label}: {name}"));
                let chip = if agent { chip.tone(Tone::Info) } else { chip };
                Box::new(chip.with_popover(choices)) as Element
            })
        })
    }

    /// One person the picker offers: their face and name, invoking `action`
    /// with `value` when they are pressed.
    fn person_choice<B: Binds<R, Option<String>>>(
        &self,
        id: &str,
        label: &str,
        value: Option<String>,
        face: Option<(&str, &str)>,
        agent: bool,
        action: B,
    ) -> Element {
        let mut button = Button::new(id, label).role(ButtonRole::Plain);
        button = match face {
            Some((name, identity)) => button.with_avatar(name, identity),
            None => button.with_icon(Icon::User).with_placeholder(),
        };
        if agent {
            button = button.tone(Tone::Info);
        }
        let binding = self.actions().binding(Chosen {
            binds: action,
            value,
        });
        match binding {
            Some(binding) => Box::new(button.on_action(binding)),
            None => Box::new(button.with_enabled(false)),
        }
    }
}

/// [`Binds`] for a button that names one value of an action taking an optional
/// one: the picker's choices.
struct Chosen<B> {
    binds: B,
    value: Option<String>,
}

impl<R: Resource, B: Binds<R, Option<String>>> Binds<R, ()> for Chosen<B> {
    fn action_id(&self) -> &'static str {
        self.binds.action_id()
    }

    fn action_key(&self) -> ActionKey {
        self.binds.action_key()
    }

    fn arguments(&self, (): ()) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        self.binds.arguments(self.value.clone())
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

    #[test]
    fn an_actors_record_spelling_reads_as_the_name_the_workspace_gives() {
        let people = People::new(None);
        let mut presence = PresenceCtx::default();
        presence.set_display_name(UserId("bob".into()), "Bob");
        presence.set_display_name(UserId("agent:scribe".into()), "Scribe");
        presence.set_participant_kind(UserId("agent:scribe".into()), ParticipantKind::Agent);
        assert!(people.follow(&presence));

        assert_eq!(
            people.actor("person:bob"),
            Actor {
                id: "bob".into(),
                name: "Bob".into(),
                agent: false,
            },
            "an assignee's spelling is not a name"
        );
        assert_eq!(
            people.actor("agent:scribe"),
            Actor {
                id: "agent:scribe".into(),
                name: "Scribe · Agent".into(),
                agent: true,
            },
            "an agent keeps the id presence colours its face by"
        );
        assert_eq!(
            people.actor("person:stranger"),
            Actor {
                id: "stranger".into(),
                name: "stranger".into(),
                agent: false,
            },
            "an id the workspace names not reads without its prefix"
        );
        assert_eq!(people.actor("bob").name, "Bob", "a bare id reads the same");
    }
}
