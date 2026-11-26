//! Resolving an automerge actor id to the person behind it.
//!
//! `tilo_resource::history::Version` hands back an `actor_hex` — a raw hex string,
//! unreadable on its own. This module is the join.
//!
//! Three shapes occur in documents: a raw 32-byte public key (the
//! only cryptographically meaningful one, joining to a person); a
//! self-declared handle
//! (`ActorId::from(app_id.as_bytes())`, the same key `tilo_ui_base::presence::user_color`
//! and the per-character `author` marks already use, but **unverified**); and
//! a random id (`ActorId::random()`, which resolves to nobody).
//!
//! For an unknown actor, use [`UNKNOWN_AUTHOR_NAME`] as the name, the first
//! eight hex characters as the label, and the same
//! [`tilo_ui_base::presence::user_color`] / [`tilo_ui_base::presence::PEER_PALETTE`] slot
//! presence uses for that identity, so history and presence cannot disagree.

use std::collections::HashMap;

use crate::widget::history::{AuthorRelation, HistoryAttribution, HistoryAuthor};
use collab_types::history::authors::handle_actor_hex;
/// The data half of naming authors, below the toolkit: decoding an actor
/// id, telling a device's actors apart and the colours history paints.
pub use collab_types::history::authors::{
    DeviceActors, MemberNames, ResolveAuthor, UNKNOWN_AUTHOR_NAME, actor_handle, app_name,
    app_words, author_coloured, device_actor, installed_app_name, name_words, set_installed_apps,
    short_id,
};
#[cfg(test)]
use collab_types::history::authors::{SHORT_LEN, rgb_to_hue};

/// Maps automerge actor ids to the people the chrome should name.
///
/// Registrations win over inference: the collaboration layer resolves peer
/// identity and the local process resolves the user, so both call
/// [`AuthorDirectory::register`]. Everything else falls through the
/// handle-decode and then the unknown-member path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuthorDirectory {
    by_actor: HashMap<String, HistoryAuthor>,
    /// A person's own colour (`Member::cursor_color`, local
    /// `avatar_color`). Wins over [`tilo_ui_base::presence::user_color`] for
    /// that identity, matching [`tilo_ui_base::presence::PresenceCtx::color_for`].
    color_overrides: HashMap<String, (u8, u8, u8)>,
    /// Who reads the history: their own changes read as theirs.
    reader: Option<tilo_actions::MemberId>,
    /// Delegated agents by the actor id they write as (their own device
    /// key): their changes read as theirs, on behalf of their person.
    agents: HashMap<String, HistoryAgent>,
    /// The people's devices by their actor id: a hover names the device whose
    /// session vouched for a writer, never a key.
    devices: HashMap<String, String>,
    /// The member each device actor writes for ([`Self::register_writers`]): a change one of
    /// the reader's own devices signed is the reader's.
    writers: HashMap<String, tilo_actions::MemberId>,
}

/// A delegated agent as history names it: its own actor, how it is
/// shown, and the person it acts for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryAgent {
    pub agent: tilo_actions::MemberId,
    /// Its display name.
    pub label: String,
    /// Its cursor colour: history paints it as presence does.
    pub rgb: (u8, u8, u8),
    /// The actor of the person it acts for, whose name the directory
    /// resolves ([`AuthorDirectory::register_member`]).
    pub person: String,
}

/// A delegated agent on a workspace's roster, defined beside the history ids in
/// the collaboration types so the session's services name it without the
/// toolkit.
pub use collab_types::history::RosterAgent;

/// The directory key of an actor: the hex of its UTF-8, the actor id a
/// process writing under that actor's handle has.
fn actor_key(actor: &str) -> String {
    hex::encode(actor.as_bytes())
}

impl AuthorDirectory {
    /// An empty directory. Every actor resolves to an unknown member
    /// until something registers it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind an actor id to a known person. Later registrations replace earlier
    /// ones for the same actor — a peer that re-announces with a corrected
    /// display name should win.
    pub fn register(&mut self, actor_hex: &str, author: HistoryAuthor) {
        self.by_actor.insert(actor_hex.to_lowercase(), author);
    }

    /// The directory presence names the workspace's members through: every
    /// device by what the workspace calls it, every agent as that agent for the
    /// person it works for, and every person by the root key they write as. What
    /// a folder file's version names — the person who signed it and the computer
    /// that vouched for it — resolves through this, so a row reads "Ada" and
    /// "Claude Code" on a page as it does on a computer.
    #[must_use]
    pub fn from_presence(presence: &tilo_ui_base::presence::PresenceCtx) -> Self {
        use tilo_ui_base::permissions::MemberId;
        let mut directory = Self::new();
        // Everyone presence names: a person by the root key their writes
        // carry — what a file version's actor names — and software by the
        // agent it is, for the person it acts for.
        for (id, name) in &presence.names {
            let name = name.trim();
            // A person the roster holds unnamed is named as the window names someone unnamed,
            // never left to read as their key; software nobody named keeps its own name.
            let shown = if name.is_empty() && id.is_person() {
                tilo_ui_base::presence::unnamed_person_label()
            } else {
                name.to_owned()
            };
            if shown.is_empty() {
                continue;
            }
            match id {
                MemberId::Person(key) => {
                    directory.register_member(id, author_for_identity(shown.clone(), key));
                    directory.register_device(key, shown);
                }
                MemberId::Agent { .. } | MemberId::Bot(_) => {
                    let (red, green, blue) = tilo_ui_base::presence::member_color(id);
                    let person = presence
                        .acting_for(id)
                        .and_then(MemberId::person_id)
                        .unwrap_or_default()
                        .to_owned();
                    directory.register_device(id.id(), shown.clone());
                    directory.register_agent(
                        id.id(),
                        HistoryAgent {
                            agent: id.clone(),
                            label: shown.clone(),
                            rgb: (red, green, blue),
                            person,
                        },
                    );
                }
            }
        }
        if let Some(me) = presence.self_id().filter(|me| me.is_person()) {
            directory.set_reader(me.clone());
        }
        directory
    }

    /// Bind `member` to how they are named, so the changes they make and the
    /// changes made on their behalf both name them.
    pub fn register_member(&mut self, member: &tilo_actions::MemberId, author: HistoryAuthor) {
        self.register(&hex::encode(member.actor_bytes()), author);
    }

    /// Bind a device's actor id to its name, so a hover that names the device
    /// whose session vouched for a writer names the person's own word for it.
    pub fn register_device(&mut self, actor_hex: &str, name: impl Into<String>) {
        let name = name.into();
        if !name.trim().is_empty() {
            self.devices.insert(actor_hex.to_lowercase(), name);
        }
    }

    /// What the directory calls the device `actor_hex`, when it knows it.
    #[must_use]
    pub fn device_name(&self, actor_hex: &str) -> Option<&str> {
        self.devices
            .get(&actor_hex.to_lowercase())
            .map(String::as_str)
    }

    /// Name the workspace's members as its roster names them (member
    /// id, name): a change written under a member's key, or under their
    /// handle, reads as theirs instead of as an unknown member.
    pub fn register_roster<'a>(
        &mut self,
        people: impl IntoIterator<Item = (&'a tilo_actions::MemberId, &'a str)>,
    ) {
        for (member, name) in people {
            let name = name.trim();
            // Someone the roster holds unnamed reads as the window names someone unnamed; software
            // nobody named keeps its own name.
            let name = if name.is_empty() {
                if !member.is_person() {
                    continue;
                }
                tilo_ui_base::presence::unnamed_person_label()
            } else {
                name.to_owned()
            };
            let rgb = match member.person_id() {
                Some(person) => self.colour_for(person),
                None => tilo_ui_base::presence::member_color(member),
            };
            let author = author_coloured(name.clone(), short_id(member.id()), rgb);
            if let Some(person) = member.person_id() {
                self.register(person, author.clone());
                // The handle string an action's context names the person by,
                // which an agent's change reads "for" ([`Self::resolve_handle`]).
                self.register(&hex::encode(person.as_bytes()), author.clone());
            }
            self.register(&hex::encode(member.actor_bytes()), author);
        }
    }

    /// Name the actors `member`'s devices write as (`writers`, hex) as the roster names the
    /// member ([`Self::register_roster`]): a change one of their devices signed, such as the
    /// workspace's own governance, reads as theirs. A member the roster does not name is left
    /// as it was.
    pub fn register_writers<'a>(
        &mut self,
        member: &tilo_actions::MemberId,
        writers: impl IntoIterator<Item = &'a str>,
    ) {
        let actor = hex::encode(member.actor_bytes());
        if !self.is_registered(&actor) {
            return;
        }
        let author = self.resolve(&actor);
        for writer in writers {
            self.register(writer, author.clone());
            self.writers.insert(writer.to_lowercase(), member.clone());
        }
    }

    /// The author of the member whose handle is `handle` (a member id, as an
    /// action's context names the person it acted for): what a delegated
    /// agent's change reads as having been made for.
    #[must_use]
    pub fn resolve_handle(&self, handle: &str) -> HistoryAuthor {
        self.resolve(&hex::encode(handle.as_bytes()))
    }

    /// Bind the actor id `actor_hex` a delegated agent writes as (its own
    /// device key) to that agent: its changes read as the agent's, on behalf
    /// of its person, wherever this directory names authors.
    pub fn register_agent(&mut self, actor_hex: &str, agent: HistoryAgent) {
        self.agents.insert(actor_hex.to_lowercase(), agent);
    }

    /// Name a verified roster agent: the changes its own device key signed
    /// read as the agent's, on behalf of its person, and the person reads by
    /// the name governance gives them unless the directory already names
    /// them.
    pub fn register_roster_agent(&mut self, agent: &RosterAgent) {
        let person = hex::encode(agent.person);
        if !self.by_actor.contains_key(&actor_key(&person)) {
            self.register_member(
                &tilo_actions::MemberId::person(person.clone()),
                author_for_identity(agent.person_name.clone(), &person),
            );
        }
        let [red, green, blue] = agent.color;
        self.register_agent(
            &hex::encode(agent.device),
            HistoryAgent {
                agent: tilo_actions::MemberId::agent(agent.agent_id.clone(), person.clone()),
                label: agent.label.clone(),
                rgb: (red, green, blue),
                person,
            },
        );
    }

    /// Bind `actor_hex` to the local user, read from the Tilo identity on disk.
    /// A `false` return is not an error: an app run outside a Tilo session has
    /// no identity, and the caller should leave the actor to resolve through
    /// the handle path.
    pub fn register_local_identity(&mut self, actor_hex: &str) -> bool {
        let Some(author) = local_identity_author() else {
            return false;
        };
        // The colour presence paints the person in (the roster's, else their slot), never the
        // identity file's, which nobody chose until they pick one: first boot writes grey.
        let rgb = self.resolve(actor_hex).rgb();
        self.register(actor_hex, author.with_rgb(rgb));
        true
    }

    /// Look up an actor. Never fails, never invents a name — see the module
    /// docs for the fallback ladder.
    pub fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        let key = actor_hex.to_lowercase();
        if let Some(agent) = self.agents.get(&key) {
            return self.agent_author(&key, agent);
        }
        self.resolve_identity(&key)
    }

    /// The author of a file version signed by `device`, certified by the
    /// root `person`: the agent that device is, on behalf of that person,
    /// or else the person by name. `None` when the directory knows neither.
    pub fn resolve_file_actor(
        &self,
        person: &[u8; 32],
        device: &[u8; 32],
    ) -> Option<HistoryAuthor> {
        let person = hex::encode(person);
        let device = hex::encode(device);
        if let Some(agent) = self
            .agents
            .get(&device)
            .filter(|agent| agent.person == person)
        {
            return Some(self.agent_author(&device, agent));
        }
        let key = actor_key(&person);
        self.by_actor
            .contains_key(&key)
            .then(|| self.resolve_identity(&key))
    }

    /// The author a registered agent is: the agent, on behalf of its person,
    /// in the agent's own colour.
    fn agent_author(&self, actor_hex: &str, agent: &HistoryAgent) -> HistoryAuthor {
        let person = self.resolve_identity(&actor_key(&agent.person)).name;
        let reader = self.reader_person();
        let mut author = author_coloured(agent.label.clone(), short_id(actor_hex), agent.rgb)
            .with_relation(AuthorRelation::Software)
            .with_attribution(
                HistoryAttribution::new(tilo_actions::Acting::member(agent.agent.clone()))
                    .acting_for(agent.person.clone(), person),
            );
        author.for_reader = reader.as_deref() == Some(agent.person.as_str());
        author
    }

    /// [`Self::resolve`] for anyone but a registered agent.
    fn resolve_identity(&self, key: &str) -> HistoryAuthor {
        let registered = self
            .by_actor
            .get(key)
            .or_else(|| handle_actor_hex(key).and_then(|handle| self.by_actor.get(&handle)));
        if let Some(a) = registered {
            let mut presented = a.clone();
            // Registered under a name that is none to show (blank, or the hex of a key, which is
            // what a person who has not named themselves goes by): someone without a name yet,
            // never the key.
            if tilo_ui_base::presence::shown_name(&presented.name).is_none() {
                presented.name = tilo_ui_base::presence::unnamed_person_label();
            }
            let override_key = actor_handle(key).unwrap_or_else(|| key.to_owned());
            if let Some(rgb) = self.color_overrides.get(&override_key) {
                presented = presented.with_rgb(*rgb);
            }
            return presented;
        }
        let short = short_id(key);
        let Some(handle) = actor_handle(key) else {
            return self.author_from_identity(UNKNOWN_AUTHOR_NAME, key, short);
        };
        match tilo_actions::Actor::of_actor_bytes(handle.as_bytes()) {
            // An app's change reads as the app: its own name, with no fragment of its actor id
            // beside it, which would say nothing a person reads.
            Some(actor @ tilo_actions::Actor::App(_)) => {
                self.author_from_identity(actor_label(&actor), &handle, "")
            }
            // An agent is named by its own name, and marked as the software it is.
            Some(actor @ tilo_actions::Actor::Member(tilo_actions::MemberId::Agent { .. })) => self
                .author_from_identity(actor_label(&actor), &handle, short)
                .with_relation(AuthorRelation::Software),
            Some(actor) => self.author_from_identity(actor_label(&actor), &handle, short),
            None => self.author_from_identity(handle.clone(), &handle, short),
        }
    }

    /// Name who reads the history: their own changes read as "You".
    /// Without one, the member this thread's app runs for ([`read_as`]).
    pub fn set_reader(&mut self, reader: tilo_actions::MemberId) {
        self.reader = Some(reader);
    }

    fn reader(&self) -> Option<tilo_actions::MemberId> {
        self.reader
            .clone()
            .or_else(|| THREAD_READER.with(|reader| reader.borrow().clone()))
    }

    /// The person who reads the history, when a person does.
    fn reader_person(&self) -> Option<String> {
        self.reader()
            .and_then(|reader| reader.person_id().map(str::to_owned))
    }

    /// Who wrote a change, by the authenticated invoker its entry records
    /// (the writing actor only when it names nobody): its name, the person
    /// it acted for, and who it is to the reader.
    pub fn resolve_entry(
        &self,
        actor_hex: &str,
        entry: Option<&tilo_actions::ActionContext>,
    ) -> HistoryAuthor {
        let reader = self.reader();
        let Some(invoker) = entry.and_then(|entry| entry.invoker.as_ref()) else {
            let author = self.resolve(actor_hex);
            let reads = reader.as_ref().is_some_and(|reader| {
                actor_handle(actor_hex)
                    .is_some_and(|handle| reader.actor_bytes() == handle.as_bytes())
                    || self.writers.get(&actor_hex.to_lowercase()) == Some(reader)
            });
            return if reads {
                author.with_relation(AuthorRelation::Reader)
            } else {
                author
            };
        };
        let actor = &invoker.acting.by;
        let software = !invoker.acting.is_person();
        let on_behalf_of = invoker.acting.acted_for_person().map(str::to_owned);
        // An agent session that acted for a person (a coding agent on the person's computer,
        // writing through their folder or their Tilo actions) reads as the agent, for the person.
        let agent = invoker
            .acting
            .member_id()
            .filter(|member| member.is_agent());
        if let (Some(agent), Some(session), Some(person)) = (
            agent,
            invoker.acting.agent_session.as_ref(),
            on_behalf_of.as_deref(),
        ) {
            return self.agent_session_author(invoker, (agent, Some(session.as_str())), person);
        }
        // A delegated agent writing as itself, under its own actor.
        if let Some(agent) = self.agents.get(&actor_hex.to_lowercase())
            && invoker.acting.member_id() == Some(&agent.agent)
        {
            return self.agent_author(actor_hex, agent);
        }
        // Any other agent that acted for a person, outside a session the change recorded, reads
        // as a session's change does. The name its connection gave it (often its session's
        // title) is detail, never its name.
        if let (Some(agent), Some(person)) = (agent, on_behalf_of.as_deref()) {
            return self.agent_session_author(invoker, (agent, None), person);
        }
        // The person's own writer names them as the directory knows them;
        // anyone else by their id.
        let author = if self.is_registered(actor_hex)
            && actor_handle(actor_hex)
                .is_some_and(|handle| handle.as_bytes() == actor.actor_bytes())
        {
            self.resolve(actor_hex)
        } else {
            let written_as = hex::encode(actor.actor_bytes());
            let author = self.resolve(&written_as);
            // Software is painted as presence paints it: by the one rule, from its identity.
            let author = if software {
                author.with_rgb(match invoker.acting.member_id() {
                    Some(member) => tilo_ui_base::presence::member_color(member),
                    None => tilo_ui_base::presence::user_color(actor.id()),
                })
            } else {
                author
            };
            match &invoker.name {
                // An agent goes by the name its admission certified for it.
                Some(name) => {
                    let mut named = author;
                    named.name.clone_from(name);
                    named
                }
                None if !self.is_registered(&written_as) => {
                    let mut author = author;
                    // An app goes by its own name in the reader's language.
                    author.name = match (actor, actor.id().trim()) {
                        (_, "") => UNKNOWN_AUTHOR_NAME.to_owned(),
                        (
                            tilo_actions::Actor::App(_)
                            | tilo_actions::Actor::Member(tilo_actions::MemberId::Person(_)),
                            _,
                        ) => actor_label(actor),
                        (_, id) => id.to_owned(),
                    };
                    author
                }
                None => author,
            }
        };
        let reads = reader
            .as_ref()
            .is_some_and(|reader| actor.member_id() == Some(reader));
        let relation = if reads {
            AuthorRelation::Reader
        } else if software {
            AuthorRelation::Software
        } else {
            AuthorRelation::Person
        };
        let for_person = on_behalf_of
            .as_deref()
            .map(|person| self.resolve_handle(person).name);
        let for_reader = on_behalf_of.is_some()
            && reader.as_ref().and_then(tilo_actions::MemberId::person_id)
                == on_behalf_of.as_deref();
        // The task the change was taken in names who did it: there are no
        // separate agents, only tasks, so a person whose window ran a task
        // reads as themselves through it, and an agent that ran one reads
        // as its person through it.
        let task = entry
            .and_then(|entry| entry.group.as_ref())
            .filter(|group| group.kind == crate::history_source::steps::AGENT_SESSION)
            .and_then(|task| task.label.clone())
            .filter(|label| !label.trim().is_empty());
        let attribution = software.then(|| {
            let mut attribution = HistoryAttribution::new(invoker.acting.clone());
            attribution.on_behalf_of_name = for_person;
            attribution
        });
        let mut author = author.with_relation(relation);
        author.attribution = attribution;
        author.for_reader = for_reader;
        author.task = task;
        author
    }

    /// The author of a change `agent` made for `person`, in its session `session` when the
    /// change recorded one: the agent, named as the workspace member it was admitted as and
    /// painted in that member's colour, acting for the person ([`Self::resolve_entry`]). The
    /// session's title stays detail for the hover; it never names the agent.
    fn agent_session_author(
        &self,
        invoker: &tilo_actions::ActionInvoker,
        (member, session): (&tilo_actions::MemberId, Option<&str>),
        person: &str,
    ) -> HistoryAuthor {
        let actor = hex::encode(member.actor_bytes());
        let known = self.is_registered(&actor);
        let named = self.resolve_identity(&actor);
        // The workspace's roster names the agent, which every viewer is lent
        // ([`Self::register_roster`]): the name a connection gave itself is its session's title,
        // never the agent's.
        let label = named.name.clone();
        // Painted as presence paints the member, unless the roster gave it its own colour.
        let rgb = if known {
            named.rgb()
        } else {
            tilo_ui_base::presence::member_color(member)
        };
        let mut author = self.agent_author(
            &actor,
            &HistoryAgent {
                agent: member.clone(),
                label,
                rgb,
                person: person.to_owned(),
            },
        );
        if let Some(attribution) = author.attribution.as_mut() {
            attribution.acting = invoker.acting.clone();
            attribution.writer = Some(collab_types::history::authors::AttributedWriter::Agent {
                agent: member.id().to_owned(),
                session: session.map(str::to_owned),
            });
            attribution.writer_label = invoker.name.clone().filter(|name| !name.trim().is_empty());
        }
        author
    }

    /// Seed or replace `member`'s colour so history agrees with
    /// [`tilo_ui_base::presence::PresenceCtx::color_for`].
    pub fn set_color_override(&mut self, member: &tilo_actions::MemberId, rgb: (u8, u8, u8)) {
        let identity = String::from_utf8_lossy(&member.actor_bytes()).into_owned();
        self.color_overrides.insert(identity, rgb);
    }

    /// The colour history paints `identity` in: the member's own colour
    /// when one was seeded, otherwise the one presence paints that identity in.
    pub fn colour_for(&self, identity: &str) -> (u8, u8, u8) {
        self.color_overrides
            .get(identity)
            .copied()
            .unwrap_or_else(|| tilo_ui_base::presence::identity_color(identity))
    }

    fn author_from_identity(
        &self,
        name: impl Into<String>,
        identity: &str,
        identity_short: impl Into<String>,
    ) -> HistoryAuthor {
        author_coloured(name, identity_short, self.colour_for(identity))
    }

    /// True when this actor has an explicitly registered identity, as opposed
    /// to a decoded handle or the unknown-member fallback. The chrome
    /// uses this to decide whether a row's author is trustworthy enough to,
    /// say, offer a "message them" affordance.
    pub fn is_registered(&self, actor_hex: &str) -> bool {
        let key = actor_hex.to_lowercase();
        self.by_actor.contains_key(&key) || self.agents.contains_key(&key)
    }

    pub fn len(&self) -> usize {
        self.by_actor.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_actor.is_empty()
    }
}

impl ResolveAuthor for AuthorDirectory {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        Self::resolve(self, actor_hex)
    }
}

thread_local! {
    static THREAD_READER: std::cell::RefCell<Option<tilo_actions::MemberId>> = const { std::cell::RefCell::new(None) };
}

/// The member this thread's app runs for: whose changes every history read
/// on this thread shows as theirs.
pub fn read_as(member: &tilo_actions::MemberId) {
    THREAD_READER.with(|reader| *reader.borrow_mut() = Some(member.clone()));
}

/// What a person goes by whom nobody named: their id where it is a name to show (a fixture's
/// `ada`), else the words every surface of the desktop uses for a person without a name yet
/// ([`tilo_ui_base::presence::unnamed_person_label`]), never their key.
fn person_label(person: &str) -> String {
    tilo_ui_base::presence::shown_name(person)
        .map_or_else(tilo_ui_base::presence::unnamed_person_label, str::to_owned)
}

/// What `actor` is called in history and audit: an agent, a bot or an automation by its name, an
/// app by its own name in the person's language, a person by their id where it is a name and
/// else as someone without a name yet ([`person_label`]), which a registered name replaces.
pub fn actor_label(actor: &tilo_actions::Actor) -> String {
    use tilo_actions::{Actor, MemberId};
    match actor {
        Actor::Member(MemberId::Agent { id, .. } | MemberId::Bot(id)) => name_words(id),
        Actor::Member(MemberId::Person(person)) => person_label(person.as_str()),
        Actor::Automation(id) => name_words(id.as_str()),
        Actor::App(app) => app_name(
            app.as_str(),
            tilo_ui_base::i18n::I18nManager::window_locale().as_str(),
        ),
        // Someone with no identity in the workspace has no name in it.
        Actor::Visitor(_) => {
            tilo_ui_base::i18n::I18nManager::lookup_plain("history-author-visitor")
        }
    }
}

/// Build a [`HistoryAuthor`] from the local identity
/// ([`tilo_ui_base::local_identity`]), if there is one.
/// Public because the shell wants the same answer for its own chrome without
/// having to own an [`AuthorDirectory`].
pub fn local_identity_author() -> Option<HistoryAuthor> {
    let id = tilo_ui_base::local_identity::load()?;
    let key_hex = hex::encode(id.group_public_key);
    let name = if id.display_name.is_empty() {
        // First boot writes an empty display name and the user names
        // themselves later. An empty string would render as a blank row, so
        // fall back to the stable label rather than to a guess.
        UNKNOWN_AUTHOR_NAME.to_string()
    } else {
        id.display_name
    };
    Some(author_coloured(
        name,
        short_id(&key_hex),
        (id.avatar_color[0], id.avatar_color[1], id.avatar_color[2]),
    ))
}

/// History row for `identity`: the colour presence paints that identity in
/// when no override is set.
pub fn author_for_identity(name: impl Into<String>, identity: &str) -> HistoryAuthor {
    author_coloured(
        name,
        short_id(identity),
        tilo_ui_base::presence::identity_color(identity),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn an_identity_wears_one_colour_wherever_it_is_shown;

        phase an_agents_change_wears_its_own_face_whatever_its_connection_called_it;
        // An agent's change made for a person wears the agent's own name and colour, as presence
        // paints the member: one agent reads alike whatever its connection called it.
        {
            let of = |name: &str| {
                let invoker = tilo_actions::ActionInvoker::admitted(&tilo_actions::Acting::agent(
                    "claude-code",
                    "ada",
                ))
                .acting_for("ada")
                .named(Some(name.into()));
                let entry = tilo_actions::ActionContext::default().with_invoker(invoker);
                AuthorDirectory::new().resolve_entry("00", Some(&entry))
            };
            let first = of("Claude Code #1");
            let second = of("Claude Code #2");
            assert_eq!(first.name, second.name, "one agent, one name");
            assert!(!first.name.contains("#1"), "{}", first.name);
            assert_eq!(second.rgb(), first.rgb(), "one agent, one colour");
            assert_eq!(
                first.rgb(),
                tilo_ui_base::presence::member_color(&tilo_actions::MemberId::agent("claude-code", "ada"))
            );
        }

        phase history_and_presence_pick_the_same_colour_for_the_same_identity;
        {
            use tilo_ui_base::permissions::MemberId;
            use tilo_ui_base::{presence::PresenceCtx, presence::user_color};

            for id in ["alice", "bob", "carol", "unknown-person-xyz"] {
                let hex = automerge::ActorId::from(id.as_bytes()).to_hex_string();
                let history = AuthorDirectory::new().resolve(&hex);
                let presence = PresenceCtx::default().color_for(&MemberId::person(id));
                assert_eq!(
                    history.rgb(),
                    presence,
                    "history and presence diverged for {id}"
                );
                assert_eq!(
                    history.rgb(),
                    user_color(id),
                    "unoverridden identity must land on the shared palette slot for {id}"
                );
            }

            // An agent's actor bytes name the agent and the person it acts for,
            // `agent:<person>/<id>`: an agent's change wears the colour of what it
            // is, as its face does, whichever person it acted for.
            let agent = "agent:ada/scribe";
            let member = MemberId::of_actor_bytes(agent.as_bytes()).expect("an agent's actor");
            assert_eq!(member, MemberId::agent("scribe", "ada"));
            let hex = automerge::ActorId::from(agent.as_bytes()).to_hex_string();
            let history = AuthorDirectory::new().resolve(&hex);
            assert_eq!(
                history.rgb(),
                PresenceCtx::default().color_for(&member),
                "an agent's change and its face show one colour"
            );
            assert_eq!(history.rgb(), tilo_ui_base::presence::member_color(&member));
            assert_eq!(
                history.rgb(),
                tilo_ui_base::presence::member_color(&MemberId::agent("scribe", "bob")),
                "an agent wears the colour of what it is, whichever person it acts for"
            );
            assert_eq!(
                MemberId::of_actor_bytes(b"agent:scribe"),
                None,
                "actor bytes name the person it acts for and the agent, never the agent alone"
            );

            let unknown = "ff00ff00ff00ff00";
            let history = AuthorDirectory::new().resolve(unknown);
            assert_eq!(
                history.rgb(),
                PresenceCtx::default().color_for(&MemberId::person(unknown)),
                "unknown people share the palette slot keyed by that identity"
            );

            let mut ctx = PresenceCtx::default();
            ctx.set_color_override(MemberId::person("carol"), "#112233");
            let mut dir = AuthorDirectory::new();
            dir.set_color_override(&MemberId::person("carol"), (0x11, 0x22, 0x33));
            let hex = automerge::ActorId::from("carol".as_bytes()).to_hex_string();
            assert_eq!(
                dir.resolve(&hex).rgb(),
                ctx.color_for(&MemberId::person("carol")),
                "a person's own colour must win on history and presence together"
            );
            assert_eq!(dir.resolve(&hex).rgb(), (0x11, 0x22, 0x33));
            assert_ne!(
                user_color("carol"),
                (0x11, 0x22, 0x33),
                "the override must differ from the palette or the win is invisible"
            );
        }
    }

    tilo_test_env::scenario! {
        fn actors_are_named_by_registration_handle_or_agent_and_never_misread;

        phase handle_round_trips_the_app_id_an_app_set;
        // Apps set their actor to the UTF-8 of `QUICK_APP_ID`; that has to come
        // back out, since it's the same key presence colours and author marks use.
        {
            let actor = automerge::ActorId::from("alice".as_bytes());
            let hex = actor.to_hex_string();
            assert_eq!(actor_handle(&hex).as_deref(), Some("alice"));

            let dir = AuthorDirectory::new();
            assert_eq!(dir.resolve(&hex).name, "alice");
        }

        phase agent_actors_are_presented_as_named_agents_not_wire_ids;
        {
            let scribe = tilo_actions::MemberId::agent("scribe", "carol");
            assert_eq!(actor_label(&scribe.clone().into()), "Scribe");

            let actor = automerge::ActorId::from(scribe.actor_bytes());
            let resolved = AuthorDirectory::new().resolve(&actor.to_hex_string());
            assert_eq!(resolved.name, "Scribe");
            assert!(!resolved.name.contains("agent:"));
        }

        phase an_apps_change_reads_as_the_app_never_as_its_id;
        // An app changing another app's record is named by its own name, and no fragment of its
        // actor id stands beside it: a person reads "Notes changed one note", not "app:so.t".
        {
            let app = tilo_actions::Actor::App("so.tilo.notes".into());
            assert_eq!(actor_label(&app), "Notes");

            let actor = automerge::ActorId::from(app.actor_bytes());
            let resolved = AuthorDirectory::new().resolve(&actor.to_hex_string());
            assert_eq!(resolved.name, "Notes");
            assert_eq!(
                resolved.identity_short, "",
                "an app's row shows no fragment of its actor id"
            );
        }

        phase a_person_nobody_named_reads_as_someone_without_a_name_never_their_key;
        // A person's id is the hex of their key until they name themselves: history shows the
        // words every surface uses for them, whether the change is signed by their key or
        // recorded as theirs by a writer nobody registered.
        {
            let key = hex::encode([5_u8; 32]);
            let person = tilo_actions::Actor::Member(tilo_actions::MemberId::person(&key));
            assert_eq!(actor_label(&person), tilo_ui_base::presence::unnamed_person_label());
            let ada = tilo_actions::Actor::Member(tilo_actions::MemberId::person("ada"));
            assert_eq!(actor_label(&ada), "ada");

            let invoker = tilo_actions::ActionInvoker::admitted(&tilo_actions::Acting::person(&key));
            let entry = tilo_actions::ActionContext::default().with_invoker(invoker);
            let author = AuthorDirectory::new().resolve_entry(&hex::encode([9_u8; 32]), Some(&entry));
            assert_eq!(author.name, tilo_ui_base::presence::unnamed_person_label());
            assert!(!author.name.contains(&key[..8]), "{}", author.name);

            // A page whose own person has no name yet registers them by their key.
            let member = tilo_actions::MemberId::person(&key);
            let mut directory = AuthorDirectory::new();
            directory.register_member(&member, author_for_identity(key.clone(), &key));
            let own = hex::encode(member.actor_bytes());
            assert_eq!(directory.resolve(&own).name, tilo_ui_base::presence::unnamed_person_label());
            let invoker = tilo_actions::ActionInvoker::admitted(&tilo_actions::Acting::person(&key));
            let entry = tilo_actions::ActionContext::default().with_invoker(invoker);
            assert_eq!(
                directory.resolve_entry(&own, Some(&entry)).name,
                tilo_ui_base::presence::unnamed_person_label()
            );
        }

        phase a_public_key_actor_is_not_mistaken_for_a_handle;
        // A 32-byte public key is not a handle. It must not be rendered as
        // mojibake in the name slot.
        {
            // A key with high bytes: not valid UTF-8, so the handle decode fails.
            let key: Vec<u8> = (0u8..32).map(|i| 0x80u8.wrapping_add(i)).collect();
            let hex = automerge::ActorId::from(key.as_slice()).to_hex_string();
            assert_eq!(
                actor_handle(&hex),
                None,
                "a raw key must not decode to a handle"
            );

            let dir = AuthorDirectory::new();
            let a = dir.resolve(&hex);
            assert_eq!(a.name, UNKNOWN_AUTHOR_NAME);
            assert_eq!(a.identity_short.len(), SHORT_LEN);
            assert!(
                hex.starts_with(&a.identity_short),
                "short id prefixes the actor"
            );
        }

        phase registration_wins_over_the_decoded_handle;
        // An explicit registration beats the decoded handle — the collaboration
        // layer knows the peer's registered name; the handle is self-declared.
        {
            let hex = automerge::ActorId::from("wp-1234".as_bytes()).to_hex_string();
            let mut dir = AuthorDirectory::new();
            assert_eq!(dir.resolve(&hex).name, "wp-1234");
            assert!(!dir.is_registered(&hex));

            dir.register(&hex, HistoryAuthor::new("Ada", 210.0, "abcdef01"));
            let a = dir.resolve(&hex);
            assert_eq!(a.name, "Ada");
            assert_eq!(a.avatar_hue, 210.0);
            assert_eq!(a.identity_short, "abcdef01");
            assert!(dir.is_registered(&hex));
        }

        phase lookup_is_case_insensitive;
        // Case-insensitive: `to_hex_string` is lowercase but callers that got the
        // id from somewhere else must still hit the same entry.
        {
            let mut dir = AuthorDirectory::new();
            dir.register("ABCDEF01", HistoryAuthor::new("Bob", 30.0, "abcdef01"));
            assert_eq!(dir.resolve("abcdef01").name, "Bob");
            assert_eq!(dir.resolve("AbCdEf01").name, "Bob");
        }

        phase malformed_actor_ids_fall_through_to_unknown;
        // Garbage in the actor slot must not produce a garbage name.
        {
            let dir = AuthorDirectory::new();
            for bad in ["", "zz", "abc", "00", "0a0b0c0d0e0f1011"] {
                let a = dir.resolve(bad);
                assert!(
                    a.name == UNKNOWN_AUTHOR_NAME || !a.name.is_empty(),
                    "{bad:?} produced an empty name"
                );
            }
            // A NUL-containing "handle" is not a handle.
            assert_eq!(actor_handle("610062"), None, "control bytes are rejected");
        }

        phase a_process_without_an_identity_is_not_an_author;
        // With no identity source installed and no identity service, the chrome gets
        // `None` and falls through to the handle/unknown ladder — an app run outside
        // a Tilo session is not an error.
        {
            let _lock = tilo_test_env::lock();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_SVC_IDENTITY_FD");
            let _env = env;
            tilo_ui_base::local_identity::install_source(None);
            assert!(local_identity_author().is_none());
        }
    }

    tilo_test_env::scenario! {
        fn colours_map_onto_the_expected_hue_sextants;

        phase rgb_maps_onto_the_expected_hue_sextants;
        // The identity file's avatar colour drives the dot, so the timeline
        // agrees with the colour the user picked at first boot.
        {
            assert!((rgb_to_hue([255, 0, 0]) - 0.0).abs() < 0.5, "red");
            assert!((rgb_to_hue([0, 255, 0]) - 120.0).abs() < 0.5, "green");
            assert!((rgb_to_hue([0, 0, 255]) - 240.0).abs() < 0.5, "blue");
            // First boot writes neutral grey; it has no hue and must not blow up.
            assert_eq!(rgb_to_hue([128, 128, 128]), 0.0);
        }
    }

    /// An agent's session that acted for its person reads as the person with the agent
    /// beside them, also when the agent is on the roster under its own key: as its writes to
    /// the person's files read.
    #[test]
    fn an_agent_session_for_a_person_reads_as_the_agent() {
        let device = hex::encode([7_u8; 32]);
        let person = "ada";
        let mut authors = AuthorDirectory::new();
        authors.register_member(
            &tilo_actions::MemberId::person(person),
            HistoryAuthor::new("Ada", 0.0, "ada00000"),
        );
        authors.register_agent(
            &device,
            HistoryAgent {
                agent: tilo_actions::MemberId::agent("scout", person),
                label: "Workspace folder".into(),
                rgb: (10, 20, 30),
                person: person.into(),
            },
        );
        let invoker = tilo_actions::ActionInvoker::admitted(
            &tilo_actions::Acting::agent("scout", person)
                .in_session("s1")
                .acting_for(tilo_actions::Acting::person(person)),
        )
        .named(Some("Workspace folder".into()));
        let entry = tilo_actions::ActionContext::default()
            .with_group(
                "t1",
                "Workspace folder",
                crate::history_source::steps::AGENT_SESSION,
            )
            .with_invoker(invoker);
        let author = authors.resolve_entry(&device, Some(&entry));
        assert_eq!(author.name, "Scout");
        assert_eq!(author.relation, AuthorRelation::Software);
        // The agent is the author itself, so no badge names a writer beside it.
        assert_eq!(crate::history_source::sentence::writer_badge(&author), None);
        // The session's title is detail on the hover, never part of who did it.
        assert_eq!(author.task, None);
        let hover = crate::history_source::sentence::attribution_hover(&author).unwrap_or_default();
        assert!(hover.contains("Workspace folder"), "{hover}");
    }

    /// An agent that acted for a person, with or without a session, reads as the agent: by its
    /// own name when nothing names it, and its session's title is detail on the hover, never
    /// its name.
    #[test]
    fn an_agent_for_a_person_reads_as_the_agent_and_never_by_its_session_title() {
        let person = "ada";
        let mut authors = AuthorDirectory::new();
        authors.register_member(
            &tilo_actions::MemberId::person(person),
            HistoryAuthor::new("Ada", 0.0, "ada00000"),
        );
        let invoker = tilo_actions::ActionInvoker::admitted(
            &tilo_actions::Acting::agent("scout", person)
                .acting_for(tilo_actions::Acting::person(person)),
        )
        .named(Some("Workspace folder, sixth round".into()));
        let entry = tilo_actions::ActionContext::default().with_invoker(invoker);
        let author = authors.resolve_entry(&hex::encode([9_u8; 32]), Some(&entry));
        assert_eq!(author.name, "Scout");
        assert_eq!(author.relation, AuthorRelation::Software);
        let parts = crate::history_source::sentence::author_parts(&author);
        assert_eq!(parts.person, "Ada");
        assert_eq!(parts.writer.as_deref(), Some("Scout"));
        let hover = crate::history_source::sentence::attribution_hover(&author).unwrap_or_default();
        assert!(hover.contains("Workspace folder, sixth round"), "{hover}");
        assert!(hover.contains("for Ada"), "{hover}");
    }

    /// An agent session for a person reads as the workspace member the agent was admitted as:
    /// its name and colour, for the person. Its session's title names neither.
    #[test]
    fn an_agent_session_reads_as_the_member_it_was_admitted_as() {
        let person = "ben";
        let agent = tilo_actions::MemberId::agent("agent-key-1234abcd", person);
        let mut authors = AuthorDirectory::new();
        authors.set_reader(tilo_actions::MemberId::person(person));
        authors.register_member(
            &tilo_actions::MemberId::person(person),
            HistoryAuthor::new("Ben", 0.0, "ben00000"),
        );
        authors.register_member(
            &agent,
            author_coloured("Planner", short_id(agent.id()), (1, 2, 3)),
        );
        let invoker = tilo_actions::ActionInvoker::admitted(
            &tilo_actions::Acting::agent(agent.id(), person)
                .in_session("s1")
                .acting_for(tilo_actions::Acting::person(person)),
        )
        .named(Some("Tidy the intro".into()));
        let entry = tilo_actions::ActionContext::default().with_invoker(invoker);
        let author = authors.resolve_entry(&hex::encode([9_u8; 32]), Some(&entry));
        assert_eq!(author.name, "Planner");
        assert_eq!(author.rgb(), (1, 2, 3));
        assert_eq!(author.relation, AuthorRelation::Software);
        assert!(author.for_reader, "it acted for the reader");
        let hover = crate::history_source::sentence::attribution_hover(&author).unwrap_or_default();
        assert!(
            hover.contains("Planner") && hover.contains("Tidy the intro"),
            "{hover}"
        );
        assert_eq!(crate::history_source::sentence::writer_badge(&author), None);
        assert_eq!(
            crate::history_source::sentence::acted_for(&author).as_deref(),
            Some("for you")
        );
        let parts = crate::history_source::sentence::author_parts(&author);
        assert_eq!(parts.person, "Ben");
        assert_eq!(parts.writer.as_deref(), Some("Planner"));
        // Unnamed, it goes by its own id in words, never by what it was asked.
        let author = AuthorDirectory::new().resolve_entry(&hex::encode([9_u8; 32]), Some(&entry));
        assert_ne!(author.name, "Tidy the intro");
        assert!(!author.name.contains("Tidy"), "{}", author.name);
    }

    /// A change an agent made for a person reads as the workspace's roster names both of them:
    /// the person it acted for and the agent that wrote, which is what a person's page and an
    /// agent's view of the same row show when both are lent the workspace's members.
    #[test]
    fn a_change_an_agent_made_for_a_person_reads_as_the_roster_names_both() {
        let person = "ada-root";
        let agent = tilo_actions::MemberId::agent("a4b05b334073f7dc3c31652f1b0143be", person);
        let mut authors = AuthorDirectory::new();
        authors.register_roster([
            (&tilo_actions::MemberId::person(person), "Ada Stack"),
            (&agent, "Local stack"),
        ]);
        let invoker = tilo_actions::ActionInvoker::admitted(&tilo_actions::Acting::handed_agent(
            agent.id(),
            person,
        ));
        let entry = tilo_actions::ActionContext::default().with_invoker(invoker);
        let author = authors.resolve_entry(&hex::encode([9_u8; 32]), Some(&entry));
        let parts = crate::history_source::sentence::author_parts(&author);
        assert_eq!(parts.person, "Ada Stack", "the roster names the person");
        assert_eq!(
            parts.writer.as_deref(),
            Some("Local stack"),
            "and the agent that wrote for them"
        );

        // A roster that names only the person: the agent it does not list reads as nobody the
        // workspace names, never as what its connection called itself.
        let mut only_people = AuthorDirectory::new();
        only_people.register_roster([(&tilo_actions::MemberId::person(person), "Ada Stack")]);
        let entry = tilo_actions::ActionContext::default().with_invoker(
            tilo_actions::ActionInvoker::admitted(&tilo_actions::Acting::handed_agent(
                agent.id(),
                person,
            ))
            .named(Some("Local stack".into())),
        );
        let author = only_people.resolve_entry(&hex::encode([9_u8; 32]), Some(&entry));
        let parts = crate::history_source::sentence::author_parts(&author);
        assert_eq!(parts.person, "Ada Stack");
        assert_ne!(
            parts.writer.as_deref(),
            Some("Local stack"),
            "an agent nobody on the roster names is not named by its connection"
        );
    }
}
