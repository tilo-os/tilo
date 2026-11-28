//! Who sends a request: the acting tree a transport authenticated, and what
//! it admitted them to.

use tilo_member_types::Acting;

use super::{ExecutionLease, within};

/// The agent a person's own computer runs under the name `name`: one agent
/// on every computer of theirs and in every workspace, whichever session it
/// comes in on. A workspace's record of its agents keeps it under this id
/// (`agent:<id>`), so its name, colour, permissions and history follow it from
/// session to session; another name is another agent. The name is compared
/// without case or surrounding space.
#[must_use]
pub fn device_agent_id(person: &[u8; 32], name: &str) -> String {
    let name = name.trim().to_lowercase();
    let mut stem: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while stem.contains("--") {
        stem = stem.replace("--", "-");
    }
    let stem: String = stem.trim_matches('-').chars().take(40).collect();
    let stem = if stem.is_empty() {
        "agent".to_owned()
    } else {
        stem
    };
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"tilo device agent\0");
    hasher.update(person);
    hasher.update(name.as_bytes());
    let hash = hasher.finalize();
    let tag = hex::encode(hash.as_bytes().get(..5).unwrap_or_default());
    format!("{stem}-{tag}")
}

/// How a call came to the service that answers it ([`Caller`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Came {
    /// Over a link.
    OverALink,
    /// On the person's plain connection to their own device's daemon.
    OwnConnection,
    /// From the device's own code, in process.
    InProcess,
    /// From a turn this device's agent engine runs here, as the agent.
    EngineTurnHere,
    /// From a program on this device that the shell of seat `0` handed over after the person
    /// said yes to it there.
    HandedBySeat(u64),
}

/// The authenticated actor behind a request.
///
/// Built only by the transport that authenticated the connection — from the
/// certificate chain a peer presented, or from the admitted identity of a
/// local connection. It is deliberately not `Deserialize`: a payload cannot
/// assert who sent it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Caller {
    pub(super) acting: Acting,
    pub(super) user_public_key: Option<[u8; 32]>,
    pub(super) device_public_key: Option<[u8; 32]>,
    pub(super) workspace: Option<String>,
    // Transport-issued execution scope, never accepted from request context.
    // It narrows invocation ownership without changing the acting tree.
    pub(super) execution: Option<ExecutionLease>,
    // The person's own device handed this connection to the agent
    // (`Self::delegated_to`), rather than the agent being admitted as a member
    // in its own right and acting for them (`Self::acting_for`).
    pub(super) session_delegated: bool,
    // How far the invocation this caller's requests continue was admitted
    // to reach, set by the provider that matched them to it.
    pub(super) within: within::Within,
    // The turn an agent acts inside: the person's message in a conversation
    // that caused it (`Self::agent_turn`). Every op carries it, so the
    // receiving device checks it against that message and presence can show
    // what the agent would click.
    pub(super) turn: Option<tilo_action_policy::turn::Turn>,
    roster_created_at: Option<i64>,
    // The roster agent whose approval this caller works under, where that is another agent
    // than the one acting: a sub-agent its starter started (`Self::working_under`).
    approved_as: Option<String>,
    // The person whose message the server's turn answers, as the server read
    // its cause (`Self::on_roster_turn`).
    turn_person: Option<[u8; 32]>,
    // The name an agent goes by, from the key its person certified for it
    // (`Self::named`): presentation only, what History names it by.
    pub(super) agent_name: Option<String>,
    // The key one of the person's own calls came in on (`Self::own_key`), beside the person it
    // acts as: what listings name the call by. It grants nothing the person does not hold.
    key: Option<KeyCall>,
    // What the person asked the agent to do, in a line (`Self::asked`): what
    // its session is labelled by, where it was asked.
    pub(super) asked: Option<String>,
    // The device's operating system (`Self::local_system`), which acts for nobody.
    system: bool,
    // How the call came: over a link, on the person's plain connection to their own device's
    // daemon, which the kernel says is their own account (`Self::own_person`), where their CLI's
    // own prompt may confirm what the workspace asks them (the call never does), or from the
    // device's own code in process (`Self::in_process`).
    came: Came,
    // The invitation a visitor redeems (`Self::invited`): it acts by its issuer's authority,
    // which the action checks against the issuer's grant when it runs.
    invited: Option<Invited>,
    // The device whose link brought this caller's request, signed by the caller, where it is
    // another device than the caller's own (`Self::forwarded_by`): a host handing a page action to
    // the window it placed.
    forwarded_by: Option<[u8; 32]>,
    // The grants the transport that admitted this connection holds for it besides its
    // certificate's: what a device hands an app it started, from its signed manifest.
    granted: Vec<tilo_action_policy::Grant>,
    // The app this device's daemon launched and attests this connection's process to be
    // (`Self::launched`): the one caller that acts as that app itself. No payload, kept caller or
    // shell link carries it.
    launched_app: Option<tilo_member_types::AppId>,
    // The person and records a placed device's certificate names (`Self::placed`): set only by
    // the transport that read them from the verified certificate.
    placed: Option<PlacedCaller>,
    // The step of a Shortcut run whose action the Shortcut's saved rules consent to in advance
    // (`Self::consented_in_advance`): set only by the runner, in process, never from a frame.
    consented: bool,
    // The rules the person saved with the Shortcut this caller's run is (`Self::within_shortcut`):
    // set only by the runner, in process, never from a frame.
    shortcut_grant: Option<Vec<tilo_action_policy::Rule>>,
    // The program a seat's shell handed over, by what the kernel said it runs
    // (`Self::handed_by_seat`): what History and the person's questions name its calls by.
    // Display only: it grants nothing.
    program: Option<String>,
}

/// The key one of a person's calls came in on, beside the person it acts as
/// ([`Caller::own_key`]): the member the key is known by everywhere a key is named
/// ([`tilo_member_types::key_agent_id`]) and the name the person gave it when they certified it.
/// Display only: the call is the person's, decided by their grants and narrowed by the key's
/// certificate, and it is never more.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyCall {
    id: String,
    label: String,
}

impl KeyCall {
    /// The member the key is: `grant-` and its device key, hex, as the workspace and History
    /// name what a key does.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The name the person gave the key, as the certificate they certified it with says.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// What a placed device's certificate says of the caller ([`Caller::placed`]): the person whose
/// host placed it, whom history names, and the records of its workspace it reaches. The
/// person is no authority: a placed caller acts as its device's key alone, decided by the
/// records it reaches.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PlacedCaller {
    person: [u8; 32],
    channels: Vec<[u8; 32]>,
}

impl PlacedCaller {
    /// The root key of the person whose host placed the device: whom history names for
    /// what it writes. Never whom it acts as.
    #[must_use]
    pub const fn person(&self) -> &[u8; 32] {
        &self.person
    }

    /// The channel of each record it reaches, in ascending order.
    #[must_use]
    pub fn channels(&self) -> &[[u8; 32]] {
        &self.channels
    }

    /// Whether the record told by `channel` is one it reaches.
    #[must_use]
    pub fn reaches(&self, channel: &[u8; 32]) -> bool {
        self.channels.binary_search(channel).is_ok()
    }
}

/// The authority an invitation carries ([`Caller::invited`]): the signed invitation, and the
/// device key of whoever issued it, whose grant decides what it admits.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Invited {
    /// The signed invitation, as its issuer handed it over.
    pub invitation: Vec<u8>,
    /// The device key that signed it.
    pub issuer: [u8; 32],
}

impl Caller {
    /// `acting`, as the provider admitted it (`keys`: the user and device keys a transport
    /// verified, if any), with nothing more said of it yet.
    fn admitted(acting: Acting, keys: Option<([u8; 32], [u8; 32])>) -> Self {
        Self {
            acting,
            user_public_key: keys.map(|(user, _)| user),
            device_public_key: keys.map(|(_, device)| device),
            workspace: None,
            execution: None,
            session_delegated: false,
            within: within::Within::NONE,
            turn: None,
            roster_created_at: None,
            approved_as: None,
            turn_person: None,
            agent_name: None,
            key: None,
            asked: None,
            system: false,
            came: Came::OverALink,
            invited: None,
            forwarded_by: None,
            granted: Vec::new(),
            launched_app: None,
            placed: None,
            consented: false,
            shortcut_grant: None,
            program: None,
        }
    }

    /// A remote device whose certificate the transport verified to chain to
    /// `user_public_key`.
    #[must_use]
    pub fn authenticated(user_public_key: [u8; 32], device_public_key: [u8; 32]) -> Self {
        Self::admitted(
            Acting::person(hex::encode(user_public_key)),
            Some((user_public_key, device_public_key)),
        )
    }

    /// One of the person's own keys, the certificate the transport verified said it is one of
    /// theirs (`device_label` is the name they gave it): a server key made for their programs,
    /// whose calls are their own — their grants decide every call, narrowed by the key's
    /// certificate, and it is never anything more. Beside the person it acts as, the caller
    /// carries the key ([`KeyCall`]), so a listing says the key's call is the key's.
    #[must_use]
    pub fn own_key(user_public_key: [u8; 32], device_public_key: [u8; 32], label: &str) -> Self {
        let mut caller = Self::authenticated(user_public_key, device_public_key);
        caller.key = Some(KeyCall {
            id: tilo_member_types::key_agent_id(&device_public_key),
            label: label.trim().to_owned(),
        });
        caller
    }

    /// The key this caller's call came in on ([`Self::own_key`]), where it did: whose name and
    /// member it is, never a second person's.
    #[must_use]
    pub const fn key(&self) -> Option<&KeyCall> {
        self.key.as_ref()
    }

    /// A visitor: a key no certificate in the workspace names, known by the key its
    /// connection proved alone. A page at a workspace's public entry is one, and so is a
    /// computer being added to a person's devices while it asks to be certified. It acts for
    /// nobody, so it holds only what the workspace opens to any key
    /// ([`tilo_action_policy::Grantee::AnyKey`]).
    #[must_use]
    pub fn visitor(device_public_key: [u8; 32]) -> Self {
        let mut caller = Self::admitted(Acting::visitor(&device_public_key), None);
        caller.device_public_key = Some(device_public_key);
        caller
    }

    /// A device placed for records of `workspace`, the `channels` its certificate names, by
    /// a host for `person`: the transport read them from that verified certificate. It acts as
    /// its device's key alone, never as the person and never as a member's device: it has no
    /// person, no user key and nobody it acts for, so nothing that decides by those reaches it,
    /// and what it may do is decided by the records it reaches ([`Self::placed_records`]).
    #[must_use]
    pub fn placed(
        device_public_key: [u8; 32],
        person: [u8; 32],
        workspace: impl Into<String>,
        channels: impl IntoIterator<Item = [u8; 32]>,
    ) -> Self {
        let mut channels: Vec<[u8; 32]> = channels.into_iter().collect();
        channels.sort_unstable();
        channels.dedup();
        let mut caller = Self::visitor(device_public_key).in_workspace(workspace);
        caller.placed = Some(PlacedCaller { person, channels });
        caller
    }

    /// What a placed device's certificate says of this caller ([`Self::placed`]).
    #[must_use]
    pub const fn placed_records(&self) -> Option<&PlacedCaller> {
        self.placed.as_ref()
    }

    /// A visitor redeeming `invited`: the device `device_public_key` its connection proved,
    /// acting by the authority of the invitation's issuer and nothing more. The action that
    /// admits it checks the invitation, and the issuer's grant, when it runs.
    #[must_use]
    pub fn invited(device_public_key: [u8; 32], invited: Invited) -> Self {
        let mut caller = Self::visitor(device_public_key);
        caller.invited = Some(invited);
        caller
    }

    /// The invitation this caller redeems ([`Self::invited`]).
    #[must_use]
    pub const fn invitation(&self) -> Option<&Invited> {
        self.invited.as_ref()
    }

    /// A person on a connection the provider admitted itself, by the id that
    /// admission established.
    #[must_use]
    pub fn local_person(person: impl Into<tilo_member_types::PersonId>) -> Self {
        Self::admitted(Acting::person(person.into()), None)
    }

    /// The person on a plain connection to their own device's daemon, which the kernel says is
    /// their own account ([`crate::remote::Caller`]): their calls are their own act, and the
    /// answer their CLI's own prompt gives on it to what the workspace asks them is their
    /// confirmation ([`crate::remote::asked`]). Making the call is never that answer.
    #[must_use]
    pub fn own_person(person: impl Into<tilo_member_types::PersonId>) -> Self {
        Self {
            came: Came::OwnConnection,
            ..Self::admitted(Acting::person(person.into()), None)
        }
    }

    /// Whether this is the person on their own device's daemon connection
    /// ([`Self::own_person`]).
    #[must_use]
    pub const fn is_own_connection(&self) -> bool {
        matches!(self.came, Came::OwnConnection)
    }

    /// The same caller, as the person on their own device's daemon connection.
    #[must_use]
    pub const fn on_own_connection(mut self) -> Self {
        self.came = Came::OwnConnection;
        self
    }

    /// The same caller, asked by the device's own code in process rather than over a link:
    /// only the service that answers it sets this.
    #[must_use]
    pub const fn in_process(mut self) -> Self {
        self.came = Came::InProcess;
        self
    }

    /// Whether the device's own code asked this in process ([`Self::in_process`]).
    #[must_use]
    pub const fn is_in_process(&self) -> bool {
        matches!(self.came, Came::InProcess)
    }

    /// The same caller, calling from a turn this device's agent engine runs here: where the
    /// call comes from, never what it may do. Its identity stays the agent's, and its calls are
    /// decided on the agent's grants as any other's. Only the engine that runs the turn sets
    /// this.
    #[must_use]
    pub const fn in_engine_turn_here(mut self) -> Self {
        self.came = Came::EngineTurnHere;
        self
    }

    /// Whether this call comes from a turn this device's agent engine runs here
    /// ([`Self::in_engine_turn_here`]).
    #[must_use]
    pub const fn is_engine_turn_here(&self) -> bool {
        matches!(self.came, Came::EngineTurnHere)
    }

    /// The same caller, calling from `program`, a program on this device that the shell of
    /// `seat` handed over once the person said yes to it there: where the call comes from, never
    /// what it may do. It is no connection of the person's own, so what a workspace asks of them
    /// is still put to them, and the program never answers it. Only the daemon that took the
    /// hand-over sets this.
    #[must_use]
    pub fn handed_by_seat(mut self, seat: u64, program: impl Into<String>) -> Self {
        self.came = Came::HandedBySeat(seat);
        self.program = Some(program.into());
        self
    }

    /// The same caller, calling from `program`, which dialled the daemon's socket itself: it acts
    /// as the person and is never their own act. Only the daemon that took the connection sets it.
    #[must_use]
    pub fn from_program(mut self, program: impl Into<String>) -> Self {
        self.program = Some(program.into());
        self
    }

    /// The program this caller's calls come from ([`Self::handed_by_seat`],
    /// [`Self::from_program`]), by what the kernel said it runs.
    #[must_use]
    pub fn handed_program(&self) -> Option<&str> {
        self.program.as_deref()
    }

    /// The seat whose shell handed this caller's program over ([`Self::handed_by_seat`]), where
    /// one did.
    #[must_use]
    pub const fn handing_seat(&self) -> Option<u64> {
        match self.came {
            Came::HandedBySeat(seat) => Some(seat),
            _ => None,
        }
    }

    /// The device's operating system on a local connection the provider admitted as its
    /// superuser: what signs people in at the device and out of it. It acts for nobody, so it
    /// has nothing in any workspace; only the device's own actions answer it.
    #[must_use]
    pub fn local_system() -> Self {
        Self {
            system: true,
            ..Self::admitted(Acting::person(""), None)
        }
    }

    /// Whether this is the device's operating system ([`Self::local_system`]).
    #[must_use]
    pub const fn is_system(&self) -> bool {
        self.system
    }

    /// A delegated agent of `person` on a connection the provider admitted itself, acting for
    /// them.
    #[must_use]
    #[track_caller]
    pub fn local_agent(
        agent_id: impl Into<String>,
        person: impl Into<tilo_member_types::PersonId>,
    ) -> Self {
        let person = person.into();
        Self::admitted(
            Acting::agent(agent_id, person.clone()).acting_for(Acting::person(person)),
            None,
        )
    }

    /// An installed bot on a connection the provider admitted itself, by its
    /// installation: software, never a person, whatever its id spells.
    #[must_use]
    pub fn local_bot(installation: impl Into<String>) -> Self {
        Self::admitted(Acting::bot(installation.into()), None)
    }

    /// An application on a connection the provider admitted itself, by the
    /// package id that admission established. It acts as the person whose
    /// session runs it ([`Self::acting_for`]), in that app; with nobody
    /// behind it, it acts by itself, with only the grants that name it.
    #[must_use]
    pub fn local_app(app_id: impl Into<String>) -> Self {
        let app: String = app_id.into();
        Self::admitted(Acting::by_app(app.clone()).in_app(app), None)
    }

    /// App `app_id`, in the process this device's daemon launched it as and attests to be it
    /// ([`Self::local_app`]): the one caller that may act as the app itself, with the grants that
    /// name it. Set only where the daemon attests the process; nothing restored or carried is it.
    #[must_use]
    pub fn launched(app_id: impl Into<String>) -> Self {
        let app: String = app_id.into();
        Self {
            launched_app: Some(tilo_member_types::AppId::new(app.clone())),
            ..Self::local_app(app)
        }
    }

    /// The app this device's daemon launched this caller's process as ([`Self::launched`]).
    #[must_use]
    pub const fn launched_app(&self) -> Option<&tilo_member_types::AppId> {
        self.launched_app.as_ref()
    }

    /// A remote agent device whose certificate the transport verified to
    /// chain to `user_public_key` and whose root-signed delegation names it
    /// agent `agent_id`. It is its own member, never the person who
    /// delegated it.
    #[must_use]
    pub fn delegated_agent(
        agent_id: impl Into<String>,
        user_public_key: [u8; 32],
        device_public_key: [u8; 32],
    ) -> Self {
        Self::admitted(
            Acting::agent(agent_id, hex::encode(user_public_key))
                .acting_for(Acting::person(hex::encode(user_public_key))),
            Some((user_public_key, device_public_key)),
        )
    }

    /// An agent's own key: `user_public_key`'s root certified `device_public_key` as its agent
    /// `agent_id` (an agent's grant), and the transport verified that certificate. Outside any
    /// turn this server runs, it works for that person as a turn's agent works for the person
    /// who asked it ([`Self::turn_person`]), decided on its own grants and theirs.
    #[must_use]
    pub fn agent_key(
        agent_id: impl Into<String>,
        user_public_key: [u8; 32],
        device_public_key: [u8; 32],
    ) -> Self {
        Self {
            turn_person: Some(user_public_key),
            ..Self::delegated_agent(agent_id, user_public_key, device_public_key)
        }
    }

    /// Whether this is an agent's own key ([`Self::agent_key`]): an agent working for its own
    /// person on no roster turn.
    #[must_use]
    pub fn is_agent_key(&self) -> bool {
        self.roster_created_at.is_none()
            && !self.session_delegated
            && self.acting.is_agent()
            && self.device_public_key.is_some()
            && self.turn_person.is_some()
            && self.turn_person == self.user_public_key
    }

    /// An agent a device handed its session to: `user_public_key`'s root certified the key
    /// `device_public_key` for `agent_id`, which is the device of `owner`, not the agent's
    /// person's. The change names the agent acting for its person, and `owner`'s device as the
    /// one that ran it.
    #[must_use]
    pub fn handed_agent(
        agent_id: impl Into<String>,
        user_public_key: [u8; 32],
        device_public_key: [u8; 32],
        owner: [u8; 32],
    ) -> Self {
        Self::admitted(
            Acting::handed_agent(agent_id.into(), hex::encode(user_public_key))
                .agent_of(hex::encode(owner)),
            Some((user_public_key, device_public_key)),
        )
    }

    /// The agent `agent_id` of `own` acting inside a turn: `person`'s message in `turn`'s
    /// conversation caused it, and the agent takes part there. It may do exactly what that person
    /// may, held to the bound its own record carries, and its reads are decided inside the turn,
    /// so a conversation's own rules apply to them. Set only by the host that admitted the turn.
    #[must_use]
    pub fn agent_turn(
        (agent_id, own): (impl Into<String>, impl Into<tilo_member_types::PersonId>),
        person: impl Into<tilo_member_types::PersonId>,
        turn: tilo_action_policy::turn::Turn,
    ) -> Self {
        let acting = Acting::agent(agent_id, own).acting_for(Acting::person(person));
        Self {
            turn: Some(turn),
            ..Self::admitted(acting, None)
        }
    }

    /// A bot device whose certificate chains to `user_public_key` (its
    /// installer) and whose root-signed delegation names it installation
    /// `installation`. It acts for its installer and is never that person.
    #[must_use]
    pub fn bot(
        installation: impl Into<String>,
        user_public_key: [u8; 32],
        device_public_key: [u8; 32],
    ) -> Self {
        Self::admitted(
            Acting::bot(installation.into())
                .acting_for(Acting::person(hex::encode(user_public_key))),
            Some((user_public_key, device_public_key)),
        )
    }

    /// Give an already admitted connection its own execution scope. The provider
    /// mints the identifier; no payload, display name or history run selects it.
    /// This changes invocation ownership, never the caller's permissions.
    #[must_use]
    pub fn in_new_execution(mut self) -> Self {
        self.execution = Some(ExecutionLease::mint());
        self
    }

    /// Retain a provider-issued execution when projecting the same transport into
    /// another verified action API. A lease conveys ownership and lifetime,
    /// never certificate ownership or additional permissions.
    #[must_use]
    pub fn in_execution(mut self, execution: ExecutionLease) -> Self {
        self.execution = Some(execution);
        self
    }

    /// Provider-issued execution identity. An identifier alone conveys no authority.
    #[must_use]
    pub fn execution_id(&self) -> Option<uuid::Uuid> {
        self.execution.as_ref().map(|lease| lease.id)
    }

    #[must_use]
    pub fn execution_lease(&self) -> Option<ExecutionLease> {
        self.execution.clone()
    }

    #[must_use]
    pub fn execution_is_live(&self) -> bool {
        self.execution.as_ref().is_some_and(ExecutionLease::is_live)
    }

    /// Whether this caller may follow, ask again or cancel an invocation `owner` started: it is
    /// `owner`, or `owner`'s connection ended and this is the same caller on a connection admitted
    /// after that. A device or key whose connection dropped in the middle of a long import (one does,
    /// now and then) keeps the invocations it started; a second live connection of the same device still
    /// borrows none of the first's.
    #[must_use]
    pub fn continues(&self, owner: &Self) -> bool {
        // The name an agent goes by is presentation only ([`Self::named`]): a client that
        // names its session names the calls it makes, and the caller following them names
        // nothing, so ownership reads the callers without their names. The name is display
        // only and grants nothing either way.
        let (mine, theirs) = (self.unnamed(), owner.unnamed());
        if mine == theirs {
            return true;
        }
        let replaced = match (&self.execution, &owner.execution) {
            (Some(own), Some(ended)) => own.replaced(ended),
            _ => false,
        };

        replaced && {
            let mut same = mine;
            same.execution.clone_from(&owner.execution);
            same == theirs
        }
    }

    /// Whether this caller may follow or cancel an invocation `owner` started: as
    /// [`Self::continues`], or it is the device that brought `owner`'s call, in the run it brought
    /// it in, which hands the caller what became of the call as it handed them its first receipt.
    /// Never whether it may ask the invocation again as its own.
    #[must_use]
    pub fn follows(&self, owner: &Self) -> bool {
        self.continues(owner)
            || (self.forwarded_by.is_none()
                && owner
                    .forwarded_by
                    .is_some_and(|by| self.device_public_key == Some(by))
                && self.execution == owner.execution)
    }

    /// Whether this caller may follow a call `owner` made by who it is, on whatever connection
    /// or link it asks: it is `owner` itself (the same member, person and device keys), or the
    /// device that brought `owner`'s call, asking as the person it belongs to. Never an app, an
    /// agent or an automation on that device, and never whether it may ask the call again.
    /// What following a call that waits in its asked record is decided by, as reading that
    /// record is.
    #[must_use]
    pub fn follows_by_identity(&self, owner: &Self) -> bool {
        let same_member = self.acting == owner.acting
            && self.user_public_key == owner.user_public_key
            && self.device_public_key == owner.device_public_key;
        let brought_it = self.forwarded_by.is_none()
            && owner
                .forwarded_by
                .is_some_and(|by| self.device_public_key == Some(by))
            && self.acting.is_person()
            && self.acting.app().is_none()
            && !self.acting.is_automation();
        same_member || brought_it
    }

    /// This caller without the name it goes by, which is presentation only
    /// ([`Self::named`]): what ownership compares.
    #[must_use]
    fn unnamed(&self) -> Self {
        let mut bare = self.clone();
        bare.agent_name = None;
        bare
    }

    /// A transport ends its own run; retained caller clones cannot reopen it.
    pub fn retire_execution(&self) {
        if let Some(lease) = &self.execution {
            lease.close();
        }
    }

    /// Mark whether the caller acts from a foreign device: a Tilo runtime on
    /// an operating system Tilo does not control, as its certificate says.
    /// Workspaces it does not govern cap such a caller's role.
    #[must_use]
    pub fn from_foreign_device(mut self, foreign: bool) -> Self {
        self.acting = self.acting.on_foreign_device(foreign);
        self
    }

    /// The caller acts for `person`: an agent or an app the person started.
    /// Set only by the provider that admitted the delegation, never from a
    /// payload. It names who the actor works for in presence and history; it
    /// widens nothing.
    #[must_use]
    pub fn acting_for(mut self, person: impl Into<tilo_member_types::PersonId>) -> Self {
        let person = person.into();
        self.acting = if let Some(app) = self.acting.app.clone() {
            Acting::person(person).in_app(app)
        } else {
            let mut acting = self.acting;
            acting.acted_for = vec![Acting::person(person)];
            acting
        };
        self
    }

    /// The caller, acting through the app `app` the provider admitted its connection for: an app
    /// process acting as its person. Set only by that provider, never from a payload. The one
    /// reader bounds what such a caller keeps open of a record by what the app declares.
    #[must_use]
    pub fn through_app(mut self, app: impl Into<String>) -> Self {
        self.acting = self.acting.in_app(app.into());
        self
    }

    /// This person's own device hands the connection to agent `agent_id`, acting for them: a
    /// program in the workspace's machine its person let in, which has no key of its own yet.
    /// The agent is the actor, and it may do only what both the person and the delegation a
    /// device gives its agents allow. A caller that is not a person is returned as it is.
    #[must_use]
    pub fn delegated_to(mut self, agent_id: impl Into<String>) -> Self {
        let Some(person) = self.acting.person_id().map(str::to_owned) else {
            return self;
        };
        self.session_delegated = true;
        self.acting = Acting::handed_agent(agent_id, person);
        self
    }

    /// Mark this agent caller as one a device handed its session to: the mark
    /// [`Self::delegated_to`] sets, for a caller whose certificate the transport verified and
    /// whose admission said a member's own session runs it. Set only by that admission, never
    /// from a payload; changes nothing for a caller that is not an agent.
    #[must_use]
    pub fn session_handed(mut self) -> Self {
        if self.acting.is_agent() && self.device_public_key.is_some() {
            self.session_delegated = true;
            // What it writes says so, as [`Self::delegated_to`]'s does: every device that admits
            // the write decides it with the delegation the person's device handed it, the one
            // this device decided it with, never as an agent nobody delegated.
            self.acting = self.acting.handed();
        }
        self
    }

    /// An agent caller named `name`, as the certificate the transport
    /// verified for its key says (`device_label`): what History names its
    /// changes by. Names nothing for a caller that is not an agent.
    #[must_use]
    pub fn named(mut self, name: &str) -> Self {
        let name = name.trim();
        if self.acting.is_agent() && !name.is_empty() {
            self.agent_name = Some(name.to_owned());
        }
        self
    }

    /// An agent caller as `message` says the session it works in is called: by that name
    /// ([`super::InvokeRequest::said_session_name`]), where its client named one, and by the name it
    /// went by else. One name for everything the agent does here: the window it opens, the
    /// message it writes, its History rows and the questions it puts to its person. Display
    /// only, like the session's name itself; it grants nothing and changes no member id.
    #[must_use]
    pub fn in_named_session(self, message: &super::RemoteActionMessage) -> Self {
        let super::RemoteActionMessage::Invoke(request) = message else {
            return self;
        };
        self.in_session_of(request)
    }

    /// [`Self::in_named_session`] for the call `request`.
    #[must_use]
    pub fn in_session_of(self, request: &super::InvokeRequest) -> Self {
        match request.said_session_name() {
            Some(session) => self.named(&session),
            None => self,
        }
    }

    /// An agent caller working in the agent session `session`: what its
    /// changes are grouped and taken back by. Set only by the provider that
    /// admitted the session; names nothing for a caller that is not an agent.
    #[must_use]
    pub fn in_agent_session(mut self, session: impl Into<String>) -> Self {
        if self.acting.is_agent() {
            self.acting = self.acting.in_session(session);
        }
        self
    }

    /// An agent caller working on `request`, the words of the person that
    /// started its session (their message, what they sent it): its first line,
    /// trimmed and bounded, labels the session ([`Self::agent_session`]). A
    /// blank request leaves the session labelled by the agent's name.
    #[must_use]
    pub fn asked(mut self, request: &str) -> Self {
        self.asked = crate::context::one_line(request, crate::context::MAX_LABEL_CHARS);
        self
    }

    /// What an agent caller is called: its name ([`Self::named`]), else its id.
    #[must_use]
    pub fn agent_called(&self) -> Option<&str> {
        self.agent_name().or_else(|| self.acting.agent_id())
    }

    /// The name an agent caller goes by ([`Self::named`]).
    #[must_use]
    pub fn agent_name(&self) -> Option<&str> {
        self.agent_name.as_deref()
    }

    /// Bind a certified agent caller to the hosted roster entry it was
    /// approved as, answering `person`: the author of the message whose turn
    /// it answers, as its provider read it. The agent's own key still signs:
    /// every call is decided for `person`, bounded by the agent and by the
    /// person it belongs to ([`Acting::agent_of`],
    /// [`tilo_member_types::Acting::signed_by`]), so the turn does only what
    /// both the agent and `person` may, whoever the agent belongs to. The
    /// workspace service must verify this binding.
    #[must_use]
    pub fn on_roster(mut self, created_at: i64, person: [u8; 32]) -> Self {
        if !self.session_delegated && self.acting.is_agent() && self.device_public_key.is_some() {
            self.roster_created_at = Some(created_at);
            self.turn_person = Some(person);
            self.acting.acted_for = vec![Acting::person(hex::encode(person))];
            if let Some(owner) = self.user_public_key.map(hex::encode) {
                self.acting = self.acting.clone().agent_of(owner);
            }
        }
        self
    }

    /// This agent caller as the agent of `owner`, the person it belongs to, answering whoever
    /// it acts for ([`Acting::agent_of`]): where that is someone else, `owner` bounds it too.
    /// Set only by the provider that runs the agent, from the agent's own record. Changes
    /// nothing for a caller that is not an agent.
    #[must_use]
    pub fn agent_of(mut self, owner: impl Into<tilo_member_types::PersonId>) -> Self {
        if self.acting.is_agent() {
            self.acting = self.acting.agent_of(owner);
        }
        self
    }

    /// A caller bound to its roster entry ([`Self::on_roster`]), inside `turn`. Changes nothing
    /// for a caller that is not so bound.
    #[must_use]
    pub fn on_roster_turn(mut self, turn: tilo_action_policy::turn::Turn) -> Self {
        if self.roster_created_at.is_some() {
            self.turn = Some(turn);
        }
        self
    }

    /// The agent `agent_id`, named `name`, working under this roster agent's approval: a
    /// sub-agent its starter started. It acts as itself, so what it writes and says is its
    /// own, for the same person, as one more actor in its starter's chain
    /// ([`Acting::handed_on`]): the starter, the person its turn answers and the person the
    /// starter belongs to all bound it. Every change it makes is signed by the starter's key,
    /// whose approval bounds that tree too ([`Acting::signed_by`]). It holds no approval of
    /// its own. Changes nothing for a caller not bound to its roster entry
    /// ([`Self::on_roster`]).
    #[must_use]
    pub fn working_under(mut self, agent_id: impl Into<String>, name: &str) -> Self {
        let Some(approved) = self.approved_agent().map(str::to_owned) else {
            return self;
        };
        if self.acting.acted_for_person().is_none() {
            return self;
        }
        let Some(acting) = self.acting.clone().handed_on(agent_id) else {
            return self;
        };
        self.acting = acting;
        self.approved_as = Some(approved);
        self.agent_name = None;
        self.named(name)
    }

    /// The roster agent whose approval a caller bound to its roster entry acts under
    /// ([`Self::on_roster`]): the acting agent itself, or the starter of a sub-agent
    /// ([`Self::working_under`]). `None` for any other caller.
    #[must_use]
    pub fn approved_agent(&self) -> Option<&str> {
        self.roster_created_at?;
        self.approved_as
            .as_deref()
            .or_else(|| self.acting.agent_id())
    }

    /// The person a roster agent works for ([`Self::on_roster`]).
    #[must_use]
    pub const fn turn_person(&self) -> Option<&[u8; 32]> {
        self.turn_person.as_ref()
    }

    /// The creation time of the roster entry the provider bound this turn to.
    #[must_use]
    pub const fn roster_created_at(&self) -> Option<i64> {
        self.roster_created_at
    }

    /// Whether this is an agent a person's own device runs with a key of its own, outside a
    /// roster turn: an MCP client, a local assistant. Its key's certificate names its person
    /// ([`Self::user_public_key`]) and every invocation it makes is signed by that key, so this
    /// reads nothing a payload says.
    #[must_use]
    pub fn is_device_agent(&self) -> bool {
        self.acting.is_agent()
            && self.device_public_key.is_some()
            && self.roster_created_at.is_none()
            && self.acting.acted_for_person().is_some()
    }

    /// Whether this agent is one the person's own device handed its session to
    /// ([`Self::delegated_to`]): it carries the delegation a device gives its
    /// agents. An agent admitted as a member that acts for a person
    /// ([`Self::acting_for`]) carries only what the workspace grants it.
    #[must_use]
    pub fn is_session_delegated(&self) -> bool {
        self.session_delegated
    }

    /// The turn this agent acts inside ([`Self::agent_turn`]), when it was
    /// admitted as one: the message whose conversation caused its ops, which
    /// every API it reaches decides it against.
    #[must_use]
    pub fn turn(&self) -> Option<&tilo_action_policy::turn::Turn> {
        self.turn.as_ref()
    }

    /// The person this caller acts for, when it is not that person.
    #[must_use]
    pub fn acting_for_person(&self) -> Option<&str> {
        self.acting.acted_for_person().or_else(|| {
            self.acting
                .app()
                .and_then(|_| self.acting.person_id())
                .filter(|person| !person.is_empty())
        })
    }

    /// The caller's invocations are the steps of Shortcut `id`'s run: still
    /// the person's authority (bounded by the Shortcut's own grants, which
    /// the runner applies), but the Shortcut is who acts, for the person, and
    /// through the agent that saved it when one did, whose grants bound it too.
    /// Set only by the provider's Shortcut runner.
    #[must_use]
    pub fn running_shortcut(mut self, id: impl Into<String>) -> Self {
        let person = self
            .acting
            .person_id()
            .or_else(|| self.acting.acted_for_person())
            .unwrap_or_default()
            .to_owned();
        // A Shortcut an agent saved runs through that agent, for its person: the agent's own
        // grants bound the run wherever it is decided, as they bound the agent's own calls.
        let through = if self.acting.is_agent() {
            let mut agent = self.acting.clone();
            agent.app = None;
            agent
        } else {
            Acting::person(person)
        };
        let mut acting = Acting::automation(id).acting_for(through);
        acting.app.clone_from(&self.acting.app);
        acting.foreign_device = self.acting.foreign_device;
        self.acting = acting;
        self
    }

    /// The step of a Shortcut run ([`Self::running_shortcut`]) whose action the rules the person
    /// saved with the Shortcut consent to in advance: the yes they gave when they saved it, which
    /// answers this step's ask. Set only by the runner, after it checked those rules name this
    /// very action; a caller that runs no Shortcut is left as it was.
    #[must_use]
    pub fn consented_in_advance(mut self) -> Self {
        self.consented = matches!(self.acting.by, tilo_member_types::Actor::Automation(_));
        self
    }

    /// The same Shortcut run ([`Self::running_shortcut`]), bounded by `grant`, the rules its
    /// person saved with it: every call the run makes, its steps and whatever their code calls at
    /// any depth, is decided within them as well as by whom it runs for. Set only by the
    /// provider's Shortcut runner; a caller that runs no Shortcut is left as it was.
    #[must_use]
    pub fn within_shortcut(mut self, grant: Vec<tilo_action_policy::Rule>) -> Self {
        if self.acting.is_automation() {
            self.shortcut_grant = Some(grant);
        }
        self
    }

    /// The rules bounding this caller's Shortcut run ([`Self::within_shortcut`]).
    #[must_use]
    pub fn shortcut_grant(&self) -> Option<&[tilo_action_policy::Rule]> {
        self.shortcut_grant.as_deref()
    }

    /// `access` as this caller holds it: with the grants made for its software
    /// ([`Self::granted`]) and, for a Shortcut's run, within the rules its person saved with it
    /// ([`Self::within_shortcut`]). What every gate deciding this caller by a member access
    /// decides it with.
    #[must_use]
    pub fn held(
        &self,
        access: tilo_action_policy::MemberAccess,
    ) -> tilo_action_policy::MemberAccess {
        access
            .granted(self.granted())
            .within_shortcut(self.shortcut_grant())
    }

    /// This caller's access in the workspace `policy` is, as it acted ([`Self::held`]).
    #[must_use]
    pub fn access(
        &self,
        policy: impl Into<std::sync::Arc<tilo_action_policy::WorkspacePolicy>>,
    ) -> tilo_action_policy::MemberAccess {
        self.held(tilo_action_policy::MemberAccess::new(
            policy,
            self.acting.clone(),
        ))
    }

    /// Whether the person's saved Shortcut consents to this step in advance
    /// ([`Self::consented_in_advance`]).
    #[must_use]
    pub const fn is_consented_in_advance(&self) -> bool {
        self.consented
    }

    /// Who an action this caller invokes is decided for, as the transport
    /// admitted it ([`tilo_member_types::Acting`]).
    #[must_use]
    pub const fn acting(&self) -> &Acting {
        &self.acting
    }

    /// The grants this caller's certificate names for the agent it is in `workspace`
    /// ([`tilo_action_policy::roles::certified_agent`]): its person's default delegation, which
    /// the workspace's own grants naming the agent then narrow. Nothing for a caller no
    /// certificate names as an agent: it holds only what the workspace grants it.
    ///
    /// The certificate delegates to an agent a device **handed** its session
    /// ([`Self::delegated_to`]) or a workspace's **roster** bound ([`Self::on_roster`]): a caller
    /// that only says it is an agent acting for a person, on that person's key, is named by no
    /// session and no roster, and is offered nothing.
    #[must_use]
    pub fn certified(&self, workspace: &str) -> Vec<tilo_action_policy::Grant> {
        let (Some((agent, own)), Some(person)) = (
            self.acting
                .member_id()
                .and_then(tilo_member_types::MemberId::as_agent),
            self.acting.acted_for_person(),
        ) else {
            return Vec::new();
        };
        if self.device_public_key.is_none()
            || (self.roster_created_at.is_none() && !self.session_delegated)
        {
            return Vec::new();
        }
        tilo_action_policy::roles::certified_agent(workspace, (agent, own.as_str()), person)
    }

    /// The grants made for this caller's software: its certificate's ([`Self::certified`]),
    /// the one place an agent's rights become grants, so every host and every receiver decides
    /// an agent alike. Asked in no workspace (its person's list of them), the certificate gives
    /// the delegation at the root.
    #[must_use]
    pub fn granted(&self) -> Vec<tilo_action_policy::Grant> {
        let mut held = self.granted.clone();
        held.extend(self.certified(self.workspace.as_deref().unwrap_or_default()));
        held
    }

    /// This caller with `grants` too, which the transport that admitted it holds for it: what a
    /// device hands an app it started, from its signed manifest. Never accepted from a request.
    #[must_use]
    pub fn granting(mut self, grants: Vec<tilo_action_policy::Grant>) -> Self {
        self.granted.extend(grants);
        self
    }

    /// The group a session named `id` is, as this caller's provider admitted
    /// it: called by what the session was asked ([`Self::asked`]), else by the
    /// name the provider knows the agent by ([`Self::named`]). History groups
    /// its changes, Activity lists it and undo takes it back whole.
    #[must_use]
    pub fn session_group(&self, id: impl Into<String>) -> crate::WorkGroup {
        crate::WorkGroup {
            id: id.into(),
            label: self
                .asked
                .clone()
                .or_else(|| self.agent_name().map(str::to_owned)),
            kind: crate::context::AGENT_SESSION_GROUP.to_owned(),
            declared_id: None,
            session: None,
        }
    }

    /// The session this caller's actions are part of, as the provider that
    /// admitted it says: an agent (or a key it holds) in an execution the
    /// provider minted for its connection works in one session
    /// ([`Self::session_group`]), whatever the client says of itself. `None`
    /// for anyone else.
    #[must_use]
    pub fn agent_session(&self) -> Option<crate::WorkGroup> {
        self.acting.agent_id()?;
        let id = match &self.acting.agent_session {
            Some(session) => session.as_str().to_owned(),
            None => format!(
                "{}:{}",
                crate::context::AGENT_SESSION_GROUP,
                self.execution_id()?
            ),
        };
        Some(self.session_group(id))
    }

    /// The request reached here over the link of `device`, which forwarded it for this caller,
    /// who signed it. Set only by the transport that verified both, never from a payload.
    #[must_use]
    pub const fn forwarded_by(mut self, device: [u8; 32]) -> Self {
        self.forwarded_by = Some(device);
        self
    }

    /// The device whose link brought this caller's request, when another device than its own
    /// did ([`Self::forwarded_by`]).
    #[must_use]
    pub const fn forwarder(&self) -> Option<&[u8; 32]> {
        self.forwarded_by.as_ref()
    }

    /// Confine the caller to one workspace: the connection was admitted to that
    /// workspace only, so nothing outside it is offered or reachable.
    #[must_use]
    pub fn in_workspace(mut self, workspace: impl Into<String>) -> Self {
        self.workspace = Some(workspace.into());
        self
    }

    /// The workspace this caller is confined to, if any.
    #[must_use]
    pub fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    /// The user root a remote device chains to.
    #[must_use]
    pub const fn user_public_key(&self) -> Option<&[u8; 32]> {
        self.user_public_key.as_ref()
    }

    #[must_use]
    pub const fn is_foreign_device(&self) -> bool {
        self.acting.foreign_device
    }

    /// The remote device that sent the request.
    #[must_use]
    pub const fn device_public_key(&self) -> Option<&[u8; 32]> {
        self.device_public_key.as_ref()
    }

    /// This caller as the host keeps it beside a call that waits for its person's yes, to admit
    /// the call again for it after a restart ([`KeptCaller`]). The execution it ran in is not
    /// kept: it ended with the process.
    #[must_use]
    pub fn kept(&self) -> KeptCaller {
        // A placed caller is kept as the key it is and nothing more: restored, it is a visitor,
        // never its person.
        if self.placed.is_some() {
            return KeptCaller {
                acting: Acting::visitor(&self.device_public_key.unwrap_or_default()),
                user_public_key: None,
                device_public_key: self.device_public_key,
                workspace: self.workspace.clone(),
                session_delegated: false,
                within: None,
                turn: None,
                roster_created_at: None,
                approved_as: None,
                turn_person: None,
                agent_name: None,
                asked: None,
                system: false,
                shortcut_grant: None,
            };
        }
        KeptCaller {
            acting: self.acting.clone(),
            user_public_key: self.user_public_key,
            device_public_key: self.device_public_key,
            workspace: self.workspace.clone(),
            session_delegated: self.session_delegated,
            within: self.ceiling().map(|reach| (reach.kind, reach.privileged)),
            turn: self.turn.clone(),
            roster_created_at: self.roster_created_at,
            approved_as: self.approved_as.clone(),
            turn_person: self.turn_person,
            agent_name: self.agent_name.clone(),
            asked: self.asked.clone(),
            system: self.system,
            shortcut_grant: self.shortcut_grant.clone(),
        }
    }
}

/// A [`Caller`] as a host keeps it in its own state, beside a call that waits for its
/// person's yes: everything the transport admitted of it but the execution it ran in. A caller
/// is made of it again only for the call it was kept with ([`KeptCaller::restored`]), or by the
/// person's shell for a call its own device's daemon hands it on the link the daemon made for it
/// ([`KeptCaller::carried`]), so a payload from anyone else still cannot say who sent it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeptCaller {
    acting: Acting,
    user_public_key: Option<[u8; 32]>,
    device_public_key: Option<[u8; 32]>,
    workspace: Option<String>,
    session_delegated: bool,
    within: Option<(tilo_action_types::ActionKind, bool)>,
    turn: Option<tilo_action_policy::turn::Turn>,
    roster_created_at: Option<i64>,
    approved_as: Option<String>,
    turn_person: Option<[u8; 32]>,
    agent_name: Option<String>,
    asked: Option<String>,
    system: bool,
    /// The rules a Shortcut's run is held to ([`Caller::within_shortcut`]). A record kept before
    /// they were kept with it holds none. Always written: the shell's link carries a kept caller
    /// as postcard, which names no fields, so one left out would leave its reader a field short.
    #[serde(default)]
    shortcut_grant: Option<Vec<tilo_action_policy::Rule>>,
}

impl KeptCaller {
    /// The caller the device's daemon admitted a call of a provider the shell serves for, as the
    /// daemon says it on the shell's link: the one place a shell reads who called it.
    #[must_use]
    pub fn carried(self) -> Caller {
        self.restored()
    }

    /// The caller this was kept from, with no execution of its own.
    #[must_use]
    pub fn restored(self) -> Caller {
        // A run is held to the rules kept with it, and one kept with none (a record from before
        // they were kept) to no rules at all: it reaches nothing, never all its person may.
        let shortcut_grant = self
            .shortcut_grant
            .or_else(|| self.acting.is_automation().then(Vec::new));
        Caller {
            acting: self.acting,
            user_public_key: self.user_public_key,
            device_public_key: self.device_public_key,
            workspace: self.workspace,
            execution: None,
            session_delegated: self.session_delegated,
            within: within::Within::of(
                self.within
                    .map(|(kind, privileged)| tilo_action_policy::Reach { kind, privileged }),
            ),
            turn: self.turn,
            roster_created_at: self.roster_created_at,
            approved_as: self.approved_as,
            turn_person: self.turn_person,
            agent_name: self.agent_name,
            // The key a call came in on is not kept: a kept caller is admitted again for the
            // call it waits with, by the person's authority, and the ask it waits in names the
            // key it came on ([`super::asked::AskedFacts::by`]).
            key: None,
            asked: self.asked,
            system: self.system,
            // A kept call is admitted again for what it was made for, never as a live
            // connection's own act.
            came: Came::OverALink,
            // A redemption never waits for a person's yes, so no kept call carries one.
            invited: None,
            forwarded_by: None,
            granted: Vec::new(),
            // Only the daemon's own attestation of the process it launched makes a caller the
            // app itself; a kept or carried caller is never that.
            launched_app: None,
            placed: None,
            // A kept call is asked again for what it was made for.
            consented: false,
            shortcut_grant,
            // The ask it waits in names the program it came from
            // ([`super::asked::AskedFacts::by`]).
            program: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Caller, device_agent_id};
    use crate::context::{AGENT_SESSION_GROUP, ActionContext, WorkGroup};
    use crate::remote::{ActionAddress, InvokeArgument, InvokeRequest, RemoteActionTarget};
    use tilo_action_policy::ActingPolicy as _;

    /// A call made in the session `session` names, in a work group of `kind`.
    fn call_in(kind: &str, session: Option<&str>) -> InvokeRequest {
        InvokeRequest {
            asks: false,
            invocation: crate::InvocationId::new(),
            target: RemoteActionTarget {
                provider: "chat".into(),
                address: ActionAddress::new("chat.messages", "post"),
            },
            argument: InvokeArgument::Text(String::new()),
            context: Some(ActionContext {
                group: Some(WorkGroup {
                    id: "visit-1".into(),
                    label: None,
                    kind: kind.into(),
                    declared_id: None,
                    session: session.map(str::to_owned),
                }),
                ..ActionContext::default()
            }),
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        }
    }

    /// A kept caller reads back whole from the postcard the shell's link carries it in, with
    /// and without a Shortcut's rules: a field one side leaves out would end the link.
    #[test]
    fn a_kept_caller_reads_back_from_the_links_postcard() {
        let ada = hex::encode([1; 32]);
        for caller in [
            Caller::own_person(ada.as_str()),
            Caller::own_person(ada.as_str()).running_shortcut("reminder"),
        ] {
            let kept = caller.kept();
            let wire = postcard::to_allocvec(&kept).unwrap();
            assert_eq!(
                postcard::from_bytes::<super::KeptCaller>(&wire).unwrap(),
                kept
            );
        }
    }

    /// A Shortcut's unattended run, saved by its person or by their agent, is the Shortcut acting
    /// for that person: nobody is at a screen, so no gate that asks for the person themselves
    /// passes it, while an app reads the person as whom it writes for.
    #[test]
    fn a_shortcuts_run_is_no_person_at_a_screen_and_writes_for_its_person() {
        let ada = hex::encode([1; 32]);
        for (saved_by, saver) in [
            ("her device", Caller::own_person(ada.as_str())),
            ("her agent", Caller::local_agent("scout", ada.as_str())),
        ] {
            let run = saver.running_shortcut("reminder");
            let acting = run.acting();
            assert!(
                !acting.is_person() && !acting.is_agent() && acting.person_id().is_none(),
                "{saved_by}: {acting:?}"
            );
            assert!(acting.is_automation(), "{saved_by}: {acting:?}");
            assert_eq!(
                crate::remote::asked::answering_person(&run),
                None,
                "{saved_by}: a run answers none of its person's asks"
            );
            assert_eq!(
                crate::remote::asked::asked_person(&run).as_deref(),
                Some(ada.as_str()),
                "{saved_by}: what a run asks is put to its person"
            );
            assert_eq!(acting.acted_for_person(), Some(ada.as_str()), "{saved_by}");
            assert_eq!(
                acting.acting_agent().and_then(|agent| agent.agent_id()),
                (saved_by == "her agent").then_some("scout"),
                "{saved_by}: a run its agent saved runs through that agent"
            );
            assert_eq!(
                acting.principal().member_id().and_then(|m| m.person_id()),
                Some(ada.as_str()),
                "{saved_by}: the run writes for her"
            );
        }
        let own = Caller::own_person(ada.as_str());
        assert_eq!(own.acting().principal(), &own.acting().by);
        let agent = Caller::local_agent("scout", ada.as_str());
        assert_eq!(
            agent.acting().principal(),
            &agent.acting().by,
            "an agent writes as itself"
        );
    }

    /// Ada's approved agent answers Bo: it acts for Bo, bounded by Ada. A sub-agent it starts
    /// on that turn works for Bo under Ada's agent's approval, and records the whole chain, so
    /// whatever decides its changes is bounded by Ada's agent and Ada as well.
    #[test]
    fn a_sub_agent_on_another_members_turn_carries_its_starters_whole_chain() {
        let (ada, bo) = ([1; 32], [2; 32]);
        let (ada_id, bo_id) = (hex::encode(ada), hex::encode(bo));
        let scout = Caller::delegated_agent("scout", ada, [3; 32]).on_roster(7, bo);
        assert_eq!(scout.acting().acted_for_person(), Some(bo_id.as_str()));
        assert_eq!(
            scout.acting().people(),
            vec![bo_id.as_str(), ada_id.as_str()]
        );
        assert_eq!(scout.turn_person(), Some(&bo));

        let helper = scout.clone().working_under("helper", "Helper");
        let acting = helper.acting();
        assert_eq!(acting.agent_id(), Some("helper"));
        assert_eq!(
            acting.acted_for_person(),
            Some(bo_id.as_str()),
            "it works for Bo"
        );
        assert!(acting.people().contains(&ada_id.as_str()), "{acting:?}");
        assert!(
            acting.nodes().iter().any(|node| *node == scout.acting()),
            "its starter's whole tree: {acting:?}"
        );
        assert_eq!(helper.approved_agent(), Some("scout"));

        // Ada's own turn records her once.
        let hers = Caller::delegated_agent("scout", ada, [3; 32]).on_roster(7, ada);
        assert_eq!(hers.acting().people(), vec![ada_id.as_str()]);
    }

    /// An agent's certificate gives it its person's default delegation in the workspace, which
    /// the workspace's own grants naming it then narrow. The certificate reaches an agent a
    /// device handed its session to or a roster bound: a caller that only says it is an agent
    /// for a person is named by neither and holds nothing, and neither does a person.
    #[test]
    fn an_agents_certificate_gives_it_its_default_delegation() {
        use tilo_action_policy::Grantee;
        let (ada, device) = ([1; 32], [3; 32]);
        let person = hex::encode(ada);
        let expected = tilo_action_policy::roles::certified_agent("w", ("scout", &person), &person);
        assert!(!expected.is_empty());
        // An agent its workspace's roster bound carries the certificate's delegation.
        let bound = Caller::delegated_agent("scout", ada, device)
            .in_workspace("w")
            .on_roster(7, ada);
        assert_eq!(bound.certified("w"), expected, "the certificate's own");
        assert_eq!(bound.granted(), expected, "nothing else narrows it");
        assert!(
            expected
                .iter()
                .all(|grant| grant.grantee == Grantee::agent("scout", person.as_str()))
        );
        assert!(
            expected
                .iter()
                .all(|grant| grant.granted_by == Grantee::person(person.clone()))
        );
        // A handed agent of the person's own device carries the same delegation, and so does a
        // listed agent a member's own session admitted.
        let handed = Caller::authenticated(ada, [4; 32])
            .delegated_to("scout")
            .in_workspace("w");
        assert_eq!(handed.certified("w"), expected);
        let joined = Caller::delegated_agent("scout", ada, device)
            .session_handed()
            .in_workspace("w");
        assert_eq!(joined.certified("w"), expected);
        // What either writes names that delegation, so every device admitting the write decides
        // it as this one did.
        let preset = handed.acting().handed_preset("w");
        assert!(!preset.is_empty());
        assert_eq!(joined.acting().handed_preset("w"), preset);
        // An agent that only says it is one for a person is named by no session and no roster.
        let bare = Caller::delegated_agent("scout", ada, device).in_workspace("w");
        assert!(bare.acting().handed_preset("w").is_empty());
        assert!(
            bare.certified("w").is_empty(),
            "no session and no roster: {:?}",
            bare.certified("w")
        );
        // A person, and an agent no certificate names, hold nothing of their own.
        assert!(Caller::local_person("ada").certified("w").is_empty());
        assert!(
            Caller::local_agent("scout", "ada")
                .certified("w")
                .is_empty()
        );
    }

    /// An agent goes by the name of the session its client named for the call, and by its own
    /// name where none was named; a person is named by no session.
    #[test]
    fn an_agent_goes_by_its_sessions_name() {
        let claude = || Caller::local_agent("claude-code", "ada").named("Claude Code");
        let named = |caller: Caller, request| {
            caller
                .in_session_of(&request)
                .agent_called()
                .map(str::to_owned)
        };
        assert_eq!(
            named(claude(), call_in(AGENT_SESSION_GROUP, Some("Release prep"))).as_deref(),
            Some("Release prep")
        );
        assert_eq!(
            named(claude(), call_in(AGENT_SESSION_GROUP, Some("   "))).as_deref(),
            Some("Claude Code")
        );
        assert_eq!(
            named(claude(), call_in("issue", Some("Release prep"))).as_deref(),
            Some("Claude Code")
        );
        let ada = Caller::local_person("ada")
            .in_session_of(&call_in(AGENT_SESSION_GROUP, Some("Release prep")));
        assert_eq!(ada.agent_called(), None);
    }

    /// A call that names its session names the caller, and the connection following the
    /// invocation it started names nothing of it: what an agent is called is presentation
    /// only, so both continue the same invocations. Another session borrows none of them.
    #[test]
    fn an_agents_session_name_does_not_change_who_continues_its_invocations() {
        let caller = || Caller::local_agent("claude-code", "ada").named("Claude Code");
        let named = caller().in_session_of(&call_in(AGENT_SESSION_GROUP, Some("Release prep")));
        assert_eq!(named.agent_called(), Some("Release prep"));
        assert!(
            caller().continues(&named),
            "the connection follows what it started"
        );
        assert!(named.continues(&caller()));
        let another = Caller::local_agent("claude-code", "ada")
            .in_new_execution()
            .named("Claude Code");
        assert!(
            !another.continues(&named),
            "another live connection of the same agent borrows nothing"
        );
    }

    /// An agent session names itself by what it was asked, and by the agent's name where it was
    /// asked nothing.
    #[test]
    fn an_agent_sessions_label_is_what_it_was_asked_else_the_agents_name() {
        let session = Caller::local_agent("claude-code", "ada")
            .in_new_execution()
            .named("Claude Code")
            .in_agent_session("visit-1");
        let label = |caller: &Caller| caller.agent_session().and_then(|session| session.label);
        assert_eq!(label(&session).as_deref(), Some("Claude Code"));
        let asked = session.asked("Fill Jane's application");
        assert_eq!(label(&asked).as_deref(), Some("Fill Jane's application"));
    }

    /// A placed device's caller is its key and nothing more: no person it acts as, no user key,
    /// nobody it acts for, and nothing whose decisions read the person reaches it. Its person is
    /// only named, for history. Kept and restored, it is a plain visitor.
    #[test]
    fn a_placed_device_is_never_its_person() {
        let (device, person) = ([7; 32], [1; 32]);
        let placed = Caller::placed(device, person, "w", [[3; 32], [2; 32], [3; 32]]);
        assert_eq!(placed.user_public_key(), None);
        assert_eq!(placed.acting().person_id(), None);
        assert_eq!(placed.acting_for_person(), None);
        assert_eq!(placed.acting().visitor_key(), Some(device));
        assert_eq!(placed.device_public_key(), Some(&device));
        assert_eq!(placed.workspace(), Some("w"));
        assert!(!placed.is_system());
        assert_eq!(placed.approved_agent(), None);
        assert_eq!(placed.turn_person(), None);
        let records = placed
            .placed_records()
            .expect("the placed records are said");
        assert_eq!(records.person(), &person);
        assert_eq!(records.channels(), &[[2; 32], [3; 32]]);
        assert!(records.reaches(&[2; 32]) && records.reaches(&[3; 32]));
        assert!(!records.reaches(&[4; 32]));
        assert_ne!(
            placed,
            Caller::authenticated(person, device).in_workspace("w")
        );
        let restored = placed.kept().restored();
        assert_eq!(restored.placed_records(), None);
        assert_eq!(restored.user_public_key(), None);
        assert_eq!(restored.acting().person_id(), None);
        assert_eq!(restored.acting().visitor_key(), Some(device));
    }

    /// One person's agent under one name is one agent, however the name is typed; another name
    /// or another person's agent is another.
    #[test]
    fn a_persons_agent_is_one_agent_by_its_name() {
        let ada = [1; 32];
        let claude = device_agent_id(&ada, "Claude Code");
        assert!(claude.starts_with("claude-code-"), "{claude}");
        assert_eq!(claude, device_agent_id(&ada, "  claude code "));
        assert_ne!(claude, device_agent_id(&ada, "Reviewer"));
        assert_ne!(claude, device_agent_id(&[2; 32], "Claude Code"));
        assert!(device_agent_id(&ada, "…").starts_with("agent-"));
    }

    /// The person's own connection is made only where the daemon admits them from its own
    /// socket: no kept call and no frame carries it. A call admitted again from its asked record
    /// asks as any other, and a frame that names the flag is refused rather than read.
    #[test]
    fn a_kept_caller_is_never_the_persons_own_connection() {
        let person = "a".repeat(64);
        let own = Caller::own_person(person);
        assert!(own.is_own_connection());
        let kept = own.kept();
        assert!(!kept.clone().restored().is_own_connection());
        assert!(!kept.clone().carried().is_own_connection());
        let carried = serde_json::to_value(&kept).expect("a kept caller is written");
        assert!(
            carried.get("own_connection").is_none(),
            "nothing kept says it: {carried}"
        );
        let mut named = carried;
        named["own_connection"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<super::KeptCaller>(named).is_err(),
            "a frame that names the flag is refused"
        );
    }
}
