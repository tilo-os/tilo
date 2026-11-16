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
use ui_core::presence::PresenceCtx;
use ui_core::tokens::PresenceHue;
use ui_core::widget::mention_picker::{MentionCandidate, merged_roster};
use ui_core::widget::{Button, ButtonRole, Element, Stack, Tone};

use super::ViewCx;
use tilo_resource::MemberId;

/// The names the host knows for the participants of the window's workspace.
/// Cloneable; clones read the same roster.
#[derive(Clone)]
pub struct People {
    roster: Rc<RefCell<Roster>>,
    revision: LocalState<u64>,
    /// The participant this window is for, when someone edits it.
    me: Option<MemberId>,
    /// The name this device knows its person by, for `me` until the workspace
    /// names them.
    local: Option<Rc<str>>,
}

#[derive(Default, PartialEq)]
struct Roster {
    /// Each participant the workspace names, as presence labels them.
    names: HashMap<MemberId, String>,
    /// The colour each of them wears, as presence paints them: the one they
    /// chose, or the one their identity derives.
    hues: HashMap<MemberId, PresenceHue>,
    /// The label this session's participant goes by, when the workspace said.
    own: Option<String>,
    /// Everyone the workspace names, by name, as `@` offers them.
    members: Vec<MentionCandidate>,
    /// The same participants as a history directory reads them: what a file
    /// version's person and device keys resolve through.
    registry: ui_core::history_source::AuthorDirectory,
}

/// A participant as a view shows them: what the workspace calls them, the id
/// a face takes its photograph from, the colour it wears, and whether the
/// workspace offers them as an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Actor {
    /// The member, or `None` for someone outside the workspace.
    pub member: Option<MemberId>,
    /// What the workspace calls them, or their id where it names them not.
    pub name: String,
    pub hue: PresenceHue,
}

impl Actor {
    /// The id a face takes its photograph from.
    #[must_use]
    pub fn identity(&self) -> &str {
        self.member.as_ref().map_or(&self.name, MemberId::id)
    }

    /// Whether they are an agent of the workspace, not a person in it.
    #[must_use]
    pub fn agent(&self) -> bool {
        self.member.as_ref().is_some_and(MemberId::is_agent)
    }
}

impl People {
    pub(crate) fn new(me: Option<&MemberId>) -> Self {
        Self {
            roster: Rc::default(),
            revision: LocalState::new(0),
            local: me
                .and_then(|_| ui_core::local_identity::own_name())
                .map(Rc::from),
            me: me.cloned(),
        }
    }

    /// The name to show for `member`: the roster's label; for the window's
    /// own participant before the workspace names them, the name this device
    /// knows its person by; else their id, which is what a capture's fixture
    /// people are called.
    pub fn name(&self, member: &MemberId) -> String {
        let roster = self.roster.borrow();
        if let Some(name) = roster.names.get(member) {
            return name.clone();
        }
        if self.me.as_ref() == Some(member)
            && let Some(own) = roster.own.as_deref().or(self.local.as_deref())
        {
            return own.to_owned();
        }
        member.id().to_owned()
    }

    /// The hue `member` wears: the one the workspace announced for them, else
    /// the one their identity derives, so a view paints a face, a card or a
    /// chip in the same colour presence paints their caret and their byline.
    pub fn hue(&self, member: &MemberId) -> PresenceHue {
        self.roster
            .borrow()
            .hues
            .get(member)
            .copied()
            .unwrap_or_else(|| ui_core::presence::member_hue(member, None))
    }

    /// The workspace's people and agents, this window's own participant among
    /// them, sorted by name: whom a picker of members offers
    /// ([`ViewCx::person_picker`]).
    pub fn members(&self) -> Vec<MentionCandidate> {
        self.roster.borrow().members.clone()
    }

    /// The workspace's `member`, when it names them: their name, their hue
    /// and whether they are this window's own.
    pub fn member(&self, member: &MemberId) -> Option<MentionCandidate> {
        self.roster
            .borrow()
            .members
            .iter()
            .find(|candidate| candidate.id == *member)
            .cloned()
    }

    /// The participants as a history directory reads them: what a file
    /// version's person and device keys resolve through, and the hover that
    /// names the computer whose session vouched for the writer.
    pub fn registry(&self) -> ui_core::history_source::AuthorDirectory {
        self.roster.borrow().registry.clone()
    }

    /// `member` as a view shows them.
    pub fn actor_of(&self, member: &MemberId) -> Actor {
        Actor {
            name: self.name(member),
            hue: self.hue(member),
            member: Some(member.clone()),
        }
    }

    /// Who the id an action recorded names: a person by their bare root key (as
    /// an assignee or a mention holds them), an agent as the action API names
    /// it (`agent:<id>`), or someone outside the workspace by their name
    /// (`outside:<name>`). A person is never spelled with a prefix, so an id
    /// that has one names software or nobody, and reads as what it says.
    pub fn actor(&self, id: &str) -> Actor {
        // Someone outside the workspace, named where an item came from
        // until they join: their name is all the record knows of them.
        if let Some(name) = id.strip_prefix("outside:") {
            return Actor {
                member: None,
                name: name.trim().to_owned(),
                hue: PresenceHue::of_identity(id),
            };
        }
        match MemberId::parse(id) {
            Some(member) => self.actor_of(&member),
            None => Actor {
                member: None,
                name: id.to_owned(),
                hue: PresenceHue::of_identity(id),
            },
        }
    }

    /// `member` as a view shows them, named by `kept` (the name the record
    /// kept beside them: what an agent acted under) where the workspace names
    /// them not, so an agent that left the roster reads by its name, never
    /// its id.
    pub fn actor_kept(&self, member: Option<&MemberId>, kept: &str) -> Actor {
        let mut actor = member.map_or_else(
            || Actor {
                member: None,
                name: String::new(),
                hue: PresenceHue::of_identity(kept),
            },
            |member| self.actor_of(member),
        );
        let named = actor.member.as_ref().is_some_and(|member| {
            self.roster.borrow().names.contains_key(member) || self.is_me(member)
        });
        if !named && !kept.trim().is_empty() {
            kept.trim().clone_into(&mut actor.name);
        }
        actor
    }

    /// Whether another person is in the workspace with this window now.
    pub fn others_here(&self) -> bool {
        self.roster
            .borrow()
            .members
            .iter()
            .any(|member| !member.is_self && member.id.is_person())
    }

    /// Whether `id` is the participant this window is for.
    pub fn is_me(&self, member: &MemberId) -> bool {
        self.me.as_ref() == Some(member)
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
                .map(|user| (user.clone(), presence.display_name(user)))
                .collect(),
            hues: presence
                .names
                .keys()
                .map(|user| (user.clone(), presence.hue_for(user)))
                .collect(),
            own: presence.self_label(),
            members: merged_roster(presence, &[]),
            registry: {
                let mut registry =
                    ui_core::history_source::AuthorDirectory::from_presence(presence);
                // This window's own person: presence names their devices, and
                // the device this page is knows its person's name. That is the
                // key their writes carry, so a page never shows it raw.
                if let Some(me) = self.me.as_ref().and_then(MemberId::person_id) {
                    let name = self
                        .local
                        .as_deref()
                        .filter(|name| !name.trim().is_empty())
                        .unwrap_or(me);
                    registry.register_member(
                        &MemberId::person(me),
                        ui_core::history_source::author_for_identity(name.to_owned(), me),
                    );
                    registry.set_reader(MemberId::person(me));
                }
                if let Some(me) = presence.self_id().filter(|me| me.is_person()) {
                    registry.set_reader(me.clone());
                }
                registry
            },
        };
        if *self.roster.borrow() == next {
            return false;
        }
        *self.roster.borrow_mut() = next;
        self.revision.set(self.revision.get().wrapping_add(1));
        true
    }

    /// People named as `names` gives them, for a capture whose fixture
    /// places members, or a test.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn named(me: Option<&MemberId>, names: &[(MemberId, &str)]) -> Self {
        let people = Self::new(me);
        let mut roster = people.roster.borrow_mut();
        roster.names = names
            .iter()
            .map(|(id, name)| (id.clone(), (*name).to_owned()))
            .collect();
        roster.members = names
            .iter()
            .map(|(id, name)| MentionCandidate {
                id: id.clone(),
                name: (*name).to_owned(),
                hue: ui_core::presence::member_hue(id, None),
                is_self: me == Some(id),
            })
            .collect();
        drop(roster);
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
                        &format!("{id}-{}", member.id.id()),
                        &member.name,
                        Some(value),
                        Some((&member.name, member.id.id(), member.hue)),
                        member.id.is_agent(),
                        held_action.clone(),
                    ));
                }
                let (name, agent, chip) = match &who {
                    Some(who) => (
                        who.name.clone(),
                        who.agent(),
                        Button::new(id, &who.name)
                            .with_avatar(&who.name, who.identity(), who.hue)
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
        face: Option<(&str, &str, PresenceHue)>,
        agent: bool,
        action: B,
    ) -> Element {
        let mut button = Button::new(id, label).role(ButtonRole::Plain);
        button = match face {
            Some((name, identity, hue)) => button.with_avatar(name, identity, hue),
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

    tilo_test_env::scenario! {
        fn a_participant_reads_as_the_name_the_workspace_gives;

        phase an_id_reads_as_the_rosters_name_or_itself;
        {
            let me = MemberId::person("me-id");
            let peer = MemberId::person("peer-id");
            let people = People::new(Some(&me));
            assert_eq!(
                people.name(&me),
                "me-id",
                "nobody is called \"You\": an unnamed self reads as its id"
            );

            let mut presence = PresenceCtx::default();
            presence.set_display_name(peer.clone(), "Bob");
            presence.set_self_participant(me.clone(), "Ada");
            assert!(people.follow(&presence));
            assert!(
                !people.follow(&presence),
                "an unchanged roster changes nothing"
            );
            assert_eq!(people.name(&peer), "Bob");
            assert_eq!(people.name(&me), "Ada");
            assert!(people.is_me(&me));
            assert_eq!(people.revision().get(), 1);

            presence.set_display_name(peer.clone(), "ada");
            assert!(people.follow(&presence));
            assert_eq!(people.name(&me), "Ada (1)");
            assert_eq!(people.name(&peer), "ada (2)");
        }

        phase a_persons_bare_root_reads_as_the_name_the_workspace_gives;
        {
            let people = People::new(None);
            let mut presence = PresenceCtx::default();
            presence.set_display_name(MemberId::person("bob"), "Bob");
            presence.set_display_name(MemberId::agent("scribe"), "Scribe");
            assert!(people.follow(&presence));

            let bob = people.actor("bob");
            assert_eq!(bob.name, "Bob", "an assignee's bare root reads by its name");
            assert_eq!(bob.member, Some(MemberId::person("bob")));
            let scribe = people.actor("agent:scribe");
            assert_eq!(scribe.name, "Scribe · Agent");
            assert!(scribe.agent());
            let old = people.actor("person:bob");
            assert_eq!(
                old.member, None,
                "the old prefixed spelling names nobody the workspace knows"
            );
            assert_eq!(old.name, "person:bob", "and reads as what it says");
        }
    }
}
