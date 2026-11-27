//! Who the members are, by name: what a view or surface shows for an
//! id an action recorded ([`ActionCx::author`](tilo_resource::ActionCx::author))
//! or the member reading ([`ResourceEnv::me`](super::ResourceEnv::me)).
//!
//! The document records ids — the same on every replica, whatever anyone is
//! called — and the workspace's `workspace/members` resource knows the names:
//! its rows name each member, and presence says where they are. A view resolves
//! an id when it shows it, so a renamed person, or one whose name arrived after
//! their edit, reads correctly everywhere at once. Names are the workspace's
//! labels: people who share a name both go by it, and the member reading is
//! shown their own name like anyone else's. A view no window draws (an
//! agent's) reads the members lent to the app's code
//! ([`tilo_resource::members`]) where the resource named none of them.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_resource::{ActionKey, Binds, Resource};
use tilo_ui_base::LocalState;
use tilo_ui_base::icons::Icon;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::tokens::PresenceHue;
use tilo_ui_base::{widget::Element, widget::Tone};
use ui_core::peniko::Color;
use ui_core::widget::arrange::Stack as Arranged;
use ui_core::widget::mention_picker::MentionCandidate;
use ui_core::{widget::Button, widget::ButtonRole, widget::Chip};

use super::ViewCx;
use tilo_resource::MemberId;

/// The names the workspace gives the members of the window's workspace: its members' resource,
/// read through the store and lent here ([`tilo_resource::members`]). Cloneable; clones read the
/// same names.
#[derive(Clone)]
pub struct People {
    revision: LocalState<u64>,
    /// The member this window is for, when someone edits it.
    me: Option<MemberId>,
    /// The name this device knows its person by, for `me` until the workspace
    /// names them.
    local: Option<Rc<str>>,
    /// The devices presence knows, as a history directory reads them: what names a version's
    /// device keys and the hover that names the computer whose session vouched for the writer.
    /// The members themselves are read from the workspace's members' resource each time a
    /// surface asks for them.
    devices: Rc<RefCell<ui_core::history_source::AuthorDirectory>>,
    /// The members the workspace named when `revision` last moved.
    seen: Rc<RefCell<Vec<(MemberId, String)>>>,
}

/// A member as a view shows them: what the workspace calls them, the id
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
    /// Someone outside the workspace, by `name`: what an item moved from elsewhere names
    /// until they join.
    #[must_use]
    pub fn outside(name: &str) -> Self {
        Self {
            member: None,
            name: name.trim().to_owned(),
            hue: PresenceHue::of_identity(name),
        }
    }

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

    /// The chip a view shows `self` as: their face, as every face is drawn, beside the name
    /// they go by.
    #[must_use]
    pub fn chip(&self) -> Chip {
        let (red, green, blue) = tilo_ui_base::presence::identity_rgb(self.hue);
        Chip::new(self.name.clone()).with_face(self.person(), Color::from_rgb8(red, green, blue))
    }

    /// Them as a face shows them ([`tilo_ui_base::widget::AvatarFace::of`]); someone outside the
    /// workspace as a person by their name.
    #[must_use]
    pub fn person(&self) -> tilo_ui_base::component::Person {
        let member = self
            .member
            .clone()
            .unwrap_or_else(|| MemberId::person(self.name.clone()));
        tilo_ui_base::component::Person {
            identity: tilo_actions::Actor::Member(member),
            name: self.name.clone(),
            hue: self.hue,
            picture: None,
        }
    }
}

impl People {
    pub(crate) fn new(me: Option<&MemberId>) -> Self {
        Self {
            revision: LocalState::new(0),
            local: me
                .and_then(|_| ::collaboration::local_identity::own_name())
                .map(Rc::from),
            me: me.cloned(),
            devices: Rc::default(),
            seen: Rc::default(),
        }
    }

    /// The name to show for `member`: what the workspace's members' resource calls them; for the
    /// window's own member before the workspace names them, the name this device knows its
    /// person by; else their id, which is what a capture's fixture people are called.
    pub fn name(&self, member: &MemberId) -> String {
        if let Some(name) = Self::lent(member) {
            return name;
        }
        if self.me.as_ref() == Some(member)
            && let Some(own) = self.local.as_deref()
        {
            return own.to_owned();
        }
        member.id().to_owned()
    }

    /// What the workspace's members' resource calls `member`
    /// ([`tilo_resource::members::name_of`]): the names the app's code reads, which every surface
    /// shows. A person the list holds unnamed reads as the page says someone unnamed, never as
    /// their key; software it holds unnamed goes by its own name where one names it, else its id
    /// ([`tilo_ui_base::presence::unnamed_label`]).
    fn lent(member: &MemberId) -> Option<String> {
        let name = tilo_resource::members::name_of(member)?;
        if !name.trim().is_empty() {
            return Some(name);
        }
        member
            .is_person()
            .then(tilo_ui_base::presence::unnamed_person_label)
    }

    /// The hue `member` wears: the one their identity derives, so a view paints a face, a card
    /// or a chip in the same colour presence paints their caret and their byline.
    #[allow(
        clippy::unused_self,
        reason = "a window's colour rule is its own, though it needs no state to answer"
    )]
    pub fn hue(&self, member: &MemberId) -> PresenceHue {
        tilo_ui_base::tokens::PresenceHue::of_member(member)
    }

    /// The workspace's people and agents, this window's own member among
    /// them, sorted by name: whom a picker of members offers
    /// ([`ViewCx::person_picker`]).
    pub fn members(&self) -> Vec<MentionCandidate> {
        let mut members: Vec<MentionCandidate> = tilo_resource::members::named()
            .into_iter()
            .map(|(member, name)| MentionCandidate {
                is_self: self.is_me(&member),
                hue: tilo_ui_base::tokens::PresenceHue::of_member(&member),
                id: member,
                name,
            })
            .collect();
        members.sort_by(|a, b| a.name.cmp(&b.name));
        members
    }

    /// The workspace's `member`, when it names them: their name, their hue
    /// and whether they are this window's own.
    pub fn member(&self, member: &MemberId) -> Option<MentionCandidate> {
        Some(MentionCandidate {
            is_self: self.is_me(member),
            id: member.clone(),
            name: Self::lent(member)?,
            hue: tilo_ui_base::tokens::PresenceHue::of_member(member),
        })
    }

    /// The members as a history directory reads them: what a file
    /// version's person and device keys resolve through, and the hover that
    /// names the computer whose session vouched for the writer.
    pub fn registry(&self) -> ui_core::history_source::AuthorDirectory {
        let mut registry = self.devices.borrow().clone();
        let listed = tilo_resource::members::listed();
        if !listed.is_empty() {
            registry.register_roster(listed.iter().map(|(member, name)| (member, name.as_str())));
        }
        registry
    }

    /// `member` as a view shows them.
    pub fn actor_of(&self, member: &MemberId) -> Actor {
        Actor {
            name: self.name(member),
            hue: self.hue(member),
            member: Some(member.clone()),
        }
    }

    /// `member` as a view shows them, named by `kept` (the name the record
    /// kept beside them: what an agent acted under) where the workspace names
    /// them not, so an agent that left the member list reads by its name, never
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
        let named = actor
            .member
            .as_ref()
            .is_some_and(|member| self.is_me(member) || Self::lent(member).is_some());
        if !named && !kept.trim().is_empty() {
            kept.trim().clone_into(&mut actor.name);
        }
        actor
    }

    /// Whether the workspace has a person besides this window's, here now or not: what it keeps
    /// is shared with them.
    pub fn others_here(&self) -> bool {
        tilo_resource::members::named()
            .iter()
            .any(|(member, _)| member.is_person() && !self.is_me(member))
    }

    /// Whether `id` is the member this window is for.
    pub fn is_me(&self, member: &MemberId) -> bool {
        self.me.as_ref() == Some(member)
    }

    /// Increases whenever the workspace's members' resource names a different list, for a view
    /// to show names that follow it (`Text::from_state(&people.revision(), ..)`).
    pub fn revision(&self) -> LocalState<u64> {
        self.revision.clone()
    }

    /// Take the devices presence knows, as the history directory reads them, and move `revision`
    /// when the workspace's members' resource names a different list than the last frame's.
    /// Whether the names changed.
    pub(crate) fn follow(&self, presence: &PresenceCtx) -> bool {
        let next = {
            let mut registry = ui_core::history_source::AuthorDirectory::from_presence(presence);
            // This window's own person: presence names their devices, and the device this page
            // is knows its person's name. That is the key their writes carry, so a page never
            // shows it raw.
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
        };
        *self.devices.borrow_mut() = next;
        let listed = tilo_resource::members::listed();
        if *self.seen.borrow() == listed {
            return false;
        }
        *self.seen.borrow_mut() = listed;
        self.revision.set(self.revision.get().wrapping_add(1));
        true
    }

    /// People named as `names` gives them, for a capture whose fixture
    /// places members, or a test.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn named(me: Option<&MemberId>, names: &[(MemberId, &str)]) -> Self {
        let people = Self::new(me);
        tilo_resource::members::know(
            names
                .iter()
                .map(|(id, name)| (id.clone(), (*name).to_owned())),
        );
        people
    }
}

impl<R: Resource, T: 'static> ViewCx<R, T> {
    /// A chip showing whom the resource assigns (`read`, as the view shows them) — their avatar and
    /// name, or `nobody` while it names no one — that opens the workspace's
    /// people and agents, each with their avatar, and invokes `action` with
    /// the value the record stores for the one picked (`None` for nobody).
    /// `value_of` is how the record names a member, `None` leaving one out;
    /// `label` names the control for assistive technology. Built again as the
    /// member list changes, so it offers whom the workspace names now, and as the
    /// record's value changes, so the chip follows an assignment made
    /// anywhere.
    pub fn person_picker<B: Binds<R, Option<String>> + Clone>(
        &self,
        id: &'static str,
        label: String,
        nobody: String,
        value_of: impl Fn(&MentionCandidate) -> Option<String> + 'static,
        read: impl Fn(&T) -> Option<Actor> + 'static,
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
            let (label, nobody, value_of) =
                (Rc::clone(&label), Rc::clone(&nobody), Rc::clone(&value_of));
            let held_action = action.clone();
            cx.switch(&held, move |cx, who| {
                let mut choices = Arranged::stack();
                choices = choices.push(cx.person_choice(
                    &format!("{id}-none"),
                    &nobody,
                    None,
                    None,
                    held_action.clone(),
                ));
                for member in &members {
                    let Some(value) = value_of(member) else {
                        continue;
                    };
                    let kind = match member.id {
                        MemberId::Person(_) => "person",
                        MemberId::Agent { .. } => "agent",
                        MemberId::Bot(_) => "bot",
                    };
                    choices = choices.push(cx.person_choice(
                        &format!("{id}-{kind}-{}", member.id.id()),
                        &member.name,
                        Some(value),
                        Some(tilo_ui_base::component::Person {
                            identity: tilo_actions::Actor::Member(member.id.clone()),
                            name: member.name.clone(),
                            hue: member.hue,
                            picture: None,
                        }),
                        held_action.clone(),
                    ));
                }
                let (name, agent, chip) = match &who {
                    Some(who) => (
                        who.name.clone(),
                        who.agent(),
                        Button::new(id, &who.name)
                            .with_avatar(who.person())
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
                // What a reader hears keeps the kind in words: the agent mark beside the name is
                // not something a screen reader can see.
                let spoken = if agent {
                    ui_core::history_source::agent_spoken_label(&name)
                } else {
                    name
                };
                let chip = chip.with_accessible_name(&format!("{label}: {spoken}"));
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
        face: Option<tilo_ui_base::component::Person>,
        action: B,
    ) -> Element {
        let agent = face
            .as_ref()
            .and_then(|who| who.identity.member_id())
            .is_some_and(MemberId::is_agent);
        let mut button = Button::new(id, label).role(ButtonRole::Plain);
        button = match face {
            Some(who) => button.with_avatar(who),
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
            .field("known", &tilo_resource::members::listed().len())
            .field("revision", &self.revision.get())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_member_reads_as_the_name_the_workspace_gives;

        phase an_id_reads_as_the_members_name_or_itself;
        {
            let me = MemberId::person("me-id");
            let peer = MemberId::person("peer-id");
            let people = People::new(Some(&me));
            assert_eq!(
                people.name(&me),
                "me-id",
                "nobody is called \"You\": an unnamed self reads as its id"
            );

            // The workspace names them in its members' resource, which the window read.
            tilo_resource::members::know([
                (me.clone(), "Ada".to_owned()),
                (peer.clone(), "Bob".to_owned()),
            ]);
            let presence = PresenceCtx::default();
            assert!(people.follow(&presence));
            assert!(
                !people.follow(&presence),
                "an unchanged member list changes nothing"
            );
            assert_eq!(people.name(&peer), "Bob");
            assert_eq!(people.name(&me), "Ada");
            assert!(people.is_me(&me));
            assert_eq!(people.revision().get(), 1);

            tilo_resource::members::know([
                (me.clone(), "Ada".to_owned()),
                (peer.clone(), "ada".to_owned()),
            ]);
            assert!(people.follow(&presence));
            assert_eq!(people.name(&me), "Ada", "a namesake is no reason to number a name");
            assert_eq!(people.name(&peer), "ada");
            tilo_resource::members::know([]);
        }

        // What someone is, a chip says with their face or an agent's own mark, never by joining
        // the kind into the name. A reader hears the kind in words.
        phase an_agents_chip_reads_its_name_and_its_mark_says_agent;
        {
            let me = MemberId::person("me-id");
            let agent = MemberId::agent("planner", "me-id");
            let people = People::new(Some(&me));
            tilo_resource::members::know([
                (me.clone(), "Ada".to_owned()),
                (agent.clone(), "Planner".to_owned()),
            ]);
            let mut presence = PresenceCtx::default();
            presence.set_self_member(me, "Ada");
            assert!(people.follow(&presence));
            assert_eq!(
                people.name(&agent),
                "Planner",
                "the chip's name is the agent's own: its mark already says agent"
            );
            assert!(people.actor_of(&agent).agent(), "the chip carries the mark");
            // Presence names the agent as the members' resource does; a surface shows that name
            // and does not join what a member is into it.
            presence.set_display_name(agent.clone(), "Planner");
            assert_eq!(
                presence.display_name(&agent),
                "Planner",
                "no surface joins what a member is into their name"
            );
            assert_eq!(
                ui_core::history_source::agent_spoken_label("Planner"),
                "Planner, agent",
                "a reader hears the kind the mark shows"
            );
            tilo_resource::members::know([]);
        }

        // A view no window draws (an agent's) reads the members lent to the app's code: the
        // names the workspace's governance gives.
        phase a_views_lent_members_are_named;
        {
            let me = MemberId::person("me-id");
            let ada = MemberId::person("ada-key");
            let planner = MemberId::agent("planner", "ada-key");
            let people = People::new(Some(&me));
            tilo_resource::members::know([
                (ada.clone(), "Ada".to_owned()),
                (planner.clone(), "Planner".to_owned()),
            ]);
            assert_eq!(
                people.name(&ada),
                "Ada",
                "a lent member reads by the name lent to the app"
            );
            assert_eq!(people.name(&planner), "Planner");
            assert!(
                people
                    .members()
                    .iter()
                    .any(|candidate| candidate.id == ada && candidate.name == "Ada"),
                "a picker offers the lent members"
            );
            assert_eq!(
                people.registry().resolve_entry(ada.id(), None).name,
                "Ada",
                "a change a lent member wrote reads as theirs"
            );
            assert_eq!(
                people.registry().resolve_handle(ada.id()).name,
                "Ada",
                "the person an agent acted for reads by name"
            );
            tilo_resource::members::know([]);
            assert_eq!(
                people.name(&ada),
                "ada-key",
                "a view lent nothing leaves the id"
            );
        }

        // Someone the workspace lists but nobody named reads as the window names someone unnamed,
        // the same in a lent view as on the page: never as their key.
        phase an_unnamed_lent_member_reads_as_the_unnamed_label;
        {
            let me = MemberId::person("me-id");
            let nobody = MemberId::person("b".repeat(64).as_str());
            let people = People::new(Some(&me));
            tilo_resource::members::know([(nobody.clone(), "  ".to_owned())]);
            assert_eq!(
                people.name(&nobody),
                tilo_ui_base::presence::unnamed_person_label()
            );
            assert_eq!(
                people.registry().resolve_entry(nobody.id(), None).name,
                tilo_ui_base::presence::unnamed_person_label(),
                "a row an unnamed member wrote reads as the unnamed, not as their key"
            );
            assert!(
                people.members().iter().all(|candidate| candidate.id != nobody),
                "a picker offers the named alone"
            );
            tilo_resource::members::know([]);
        }
    }
}
