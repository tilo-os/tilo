//! What a call that waits for a person's yes says about itself, and how the
//! questions that wait for a person are listed.
//!
//! A call the caller's grants leave to a person is refused by its provider
//! with [`super::ActionCallError::ConfirmationRequired`], carrying
//! [`AskedFacts`]: what the person is shown. The dispatch keeps the call as
//! its own `tilo.invocations` record in status asked, readable by the caller
//! and the person asked, and that record is the question: there is no store
//! of asks beside it. The person answers it with the record's `resolve`
//! action; on approve the dispatch admits the same call again, bound to that
//! record, and a provider reads that it was approved
//! ([`super::InvokeRequest::approval`]).
//!
//! [`WAITING`]`#`[`LIST`] lists the asked records a person may answer, each as
//! an [`Asked`] row.
//!
//! The person answers with `<record>#`[`RESOLVE`] ([`Resolve`]), which [`AskedStore`] admits:
//! only the person the record asks, only while it is asked, and at most once. An approve hands
//! the call back to the dispatch that keeps it ([`AskedDispatch`]), which admits it again under
//! its id for its original caller, checking that caller's grants as they stand then.

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::{ActionAddress, RemoteActionTarget};

mod store;
pub use store::{
    ANSWERED, ANSWERED_THERE, AskedStore, MOST_WAITING, NOT_YOURS, NOTHING_ASKED, TOO_MANY_WAIT,
    Waiting,
};

/// What a frame carrying an approve no resolve handed back is refused with
/// ([`AskedStore::admits_approval`]).
pub const FORGED: &str = "This call carries an approve its person did not give";

/// Admit `request`, a caller's frame, as its approve stands: refused where it carries an
/// approve `store` did not hand back ([`AskedStore::admits_approval`]), or any approve where no
/// store keeps asked records here.
///
/// # Errors
/// [`FORGED`], refused.
pub fn admit_frame(
    store: Option<&AskedStore>,
    request: &super::InvokeRequest,
) -> Result<(), super::ActionCallError> {
    if request.approval().is_none() || store.is_some_and(|store| store.admits_approval(request)) {
        Ok(())
    } else {
        Err(super::ActionCallError::Refused {
            reason: FORGED.to_owned(),
        })
    }
}

/// The provider the questions that wait for a person are listed under.
pub const PROVIDER: &str = "tilo.asking";
/// The resource the questions that wait for the person are read from: its
/// [`LIST`] answers [`AskedList`].
pub const WAITING: &str = "waiting";
/// Every question that waits for the person, as [`AskedList`].
pub const LIST: &str = "list";
/// Answer an asked record: `<record>#resolve` with a [`Resolve`], on the
/// [`super::workspace_invocations::PROVIDER`] provider.
pub const RESOLVE: &str = "resolve";
/// What an asked invocation the person declined ends with: cancelled, its message this.
pub const DECLINED: &str = "declined";
/// The phase of an invocation's progress while it waits in its asked record: its message
/// names the record (`<workspace>/<record>`).
pub const ASKED_PHASE: &str = "asked";

/// The target of action `action` of the resource `resource`.
#[must_use]
pub fn target(resource: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(resource, action),
    }
}

/// One fact the person reads before answering: a label and its value, as a
/// reviewer reads it, long values cut and secrets withheld.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    pub label: String,
    pub value: String,
}

impl Fact {
    #[must_use]
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// One argument an action shows the person before they answer its ask, as
/// its declaration names it: the parameter, optionally one field of it, and
/// how its value reads. Admission fills the ask's [`Fact`]s from the call's
/// arguments with these, so the card shows what the call would do without
/// asking the app.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shown {
    /// The parameter, by its declared name; it is also the fact's label.
    pub param: String,
    /// One field of the parameter's value, when only that field is shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(default)]
    pub render: ShownAs,
}

/// How a shown value reads on the card.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ShownAs {
    /// Plain text, cut when long.
    #[default]
    Text,
    /// A list of addresses (mail recipients, members), each its own line.
    Addresses,
    /// A long value (a message body) shown folded, opened on request.
    Long,
    /// A value the person must not read on the card: shown withheld.
    Secret,
}

/// Who a member is, by id and by the name the person knows them under.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Named {
    pub id: String,
    pub name: String,
}

/// Who asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AskedBy {
    /// An agent, working in a turn or on its own.
    Agent(Named),
    /// An app's code, by the app and its title, and who wrote it.
    App {
        app: String,
        title: String,
        #[serde(default)]
        author: Option<Named>,
    },
    /// A device of the person's, or the server that keeps their workspaces.
    Device(Named),
    /// A program outside Tilo on the device (a Linux program reaching the session through the
    /// desktop portal, polkit or `NetworkManager`), by its name and the app id it gives, if any.
    Program {
        name: String,
        #[serde(default)]
        app_id: Option<String>,
    },
}

impl Default for AskedBy {
    fn default() -> Self {
        Self::Device(Named::default())
    }
}

/// What a call that waits for a person's yes shows them: what the asked
/// record keeps beside the call itself, and what a [`Asked`] row lists.
/// Every part is written, empty or not: a refusal travels by position.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskedFacts {
    /// Who asks.
    pub by: AskedBy,
    /// What is asked, in the asker's words where it says something of its
    /// own (a turn's question); empty where the action's name says it.
    #[serde(default)]
    pub words: String,
    /// The record the call acts on, by its name, where it names one.
    #[serde(default)]
    pub on: Option<String>,
    /// What the person reads before answering, in order.
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// Whether the call takes away what Undo does not bring back.
    #[serde(default)]
    pub destructive: bool,
    /// A question's choices, where the person answers with one of them
    /// rather than yes or no.
    #[serde(default)]
    pub choices: Vec<String>,
    /// The action's verb alone (`set`), or the phrase it declares for always:
    /// "Always let {asker} {verb} {plural}".
    #[serde(default)]
    pub verb: String,
    /// The plural of the resource type the action is on (`alarms`), which
    /// "always" is offered for; `None` where it is not offered.
    #[serde(default)]
    pub plural: Option<String>,
    /// The conversation a turn's question belongs to, by its address.
    #[serde(default)]
    pub conversation: Option<String>,
    /// What the person types or records with their answer, in order: the approve carries one
    /// entry per take ([`Resolve::typed`]).
    #[serde(default)]
    pub takes: Vec<Take>,
    /// The action whose own call by the person answers this, `<provider>/<resource>#<action>`,
    /// where what they type is kept by that action (a password the vault keeps): the asking
    /// view opens its form, filled from [`Self::facts`], and nothing they type passes through
    /// the record.
    #[serde(default)]
    pub answered_by: Option<String>,
    /// Why the answer before this one did not do (a password that did not open it), shown at
    /// the takes. Each attempt is a record of its own.
    #[serde(default)]
    pub retry: Option<String>,
    /// The files or folder the person picks with their answer, where the asker wants some.
    #[serde(default)]
    pub files: Option<FilesAsked>,
    /// Whether the person picks any number of [`Self::choices`] ([`Resolve::chosen`]) rather
    /// than one ([`Resolve::choice`]).
    #[serde(default)]
    pub many: bool,
    /// The resource type the action is on, by its id (`clocks.alarm`): what a grant given
    /// with the answer is scoped to. Empty where the call is on no resource type.
    #[serde(default)]
    pub kind: String,
    /// What the action is able to do, as its contract declares it: the kind of action a grant
    /// given with the answer allows. `None` where nothing declares it, and then no grant is
    /// offered.
    #[serde(default)]
    pub effect: Option<crate::ActionKind>,
    /// Whether only the person's root signs the call, so it is answered and run at one of their
    /// devices that keeps it, not the device that asked ([`AT_ROOT`]).
    #[serde(default)]
    pub at_root: bool,
}

/// The dispatch a call answered at a device that keeps the person's root is kept under
/// ([`AskedFacts::at_root`]): the device that asked keeps no root, so its record waits in the
/// workspace for one of the person's devices that keeps it, which asks them once, runs it under
/// its own key and writes how it went into the record ([`KeptAsk::outcome`]).
pub const AT_ROOT: &str = "at-root";

/// What answering a call kept for a device that keeps the person's root is refused with on a
/// device that keeps none.
pub const ANSWERED_AT_ROOT: &str = "This is answered on a device of yours that keeps your identity";

/// One thing the person types, or records, with their answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Take {
    pub label: String,
    /// Whether what is typed is a secret (a password, a passphrase): never written to the
    /// record, its history or a log, handed only to the asker on the device that asked, and
    /// never filled in beforehand. It may be answered empty.
    #[serde(default)]
    pub secret: bool,
    /// Whether a secret is shown while it is typed.
    #[serde(default)]
    pub shown: bool,
    /// What the field holds before the person types; never for a secret.
    #[serde(default)]
    pub initial: Option<String>,
    /// Whether the person presses a key chord rather than typing (a shortcut).
    #[serde(default)]
    pub chord: bool,
}

/// The files or folder an ask has the person pick, in the system's file picker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilesAsked {
    pub mode: FilesMode,
    /// Whether more than one may be picked.
    #[serde(default)]
    pub many: bool,
    /// The types offered to pick among, each by its label and file name patterns.
    #[serde(default)]
    pub types: Vec<FileType>,
    /// The name a file being saved is suggested under.
    #[serde(default)]
    pub name: Option<String>,
    /// The folder the picker opens in.
    #[serde(default)]
    pub folder: Option<String>,
}

/// Whether the person picks files to open, a place to save one, or a folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesMode {
    Open,
    Save,
    Folder,
}

/// A type of file offered in the picker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileType {
    pub label: String,
    pub patterns: Vec<String>,
}

impl AskedFacts {
    /// Whether this ask is answered only on the device that asked it: it takes a secret, or
    /// files, which only that device's own picker and its asker may see.
    #[must_use]
    pub fn answered_where_asked(&self) -> bool {
        self.files.is_some() || self.takes.iter().any(|take| take.secret)
    }

    /// Facts about a call by `by` that show nothing beyond its action.
    #[must_use]
    pub fn by(by: AskedBy) -> Self {
        Self {
            by,
            ..Self::default()
        }
    }

    /// With `fact` added after the others.
    #[must_use]
    pub fn with_fact(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.facts.push(Fact::new(label, value));
        self
    }

    /// What a run's failure for want of the person's answer states in its
    /// `details`: these facts, as JSON.
    #[must_use]
    pub fn to_details(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// The facts a run's failure states in its `details`; none where it
    /// states none.
    #[must_use]
    pub fn from_details(details: Option<&serde_json::Value>) -> Self {
        details
            .cloned()
            .and_then(|details| serde_json::from_value(details).ok())
            .unwrap_or_default()
    }

    /// The value of the fact labelled `label`.
    #[must_use]
    pub fn fact(&self, label: &str) -> Option<&str> {
        self.facts
            .iter()
            .find(|fact| fact.label == label)
            .map(|fact| fact.value.as_str())
    }
}

/// A `tilo.invocations` record, by the workspace it is in and its address there.
///
/// Which record it is is its workspace and address alone; [`Self::device`] says where it was
/// asked, and plays no part in naming it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordRef {
    pub workspace: String,
    pub record: String,
    /// The device that asked, by its id, for an ask answered only there
    /// ([`AskedFacts::answered_where_asked`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

impl PartialEq for RecordRef {
    fn eq(&self, other: &Self) -> bool {
        self.workspace == other.workspace && self.record == other.record
    }
}

impl Eq for RecordRef {}

impl std::hash::Hash for RecordRef {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.workspace.hash(state);
        self.record.hash(state);
    }
}

impl RecordRef {
    /// How `invocation` stands while its person is asked about it in this record: under way,
    /// with nothing of it done, its phase saying so ([`ASKED_PHASE`]) and its message naming
    /// the record.
    #[must_use]
    pub fn waiting(&self, invocation: crate::InvocationId) -> crate::InvocationUpdate {
        crate::InvocationUpdate::Progress {
            invocation,
            sequence: 1,
            phase: ASKED_PHASE.to_owned(),
            completed: None,
            total: None,
            message: Some(format!("{}/{}", self.workspace, self.record)),
        }
    }
}

/// What writes a call that waits for a person's yes as its own invocation
/// record in status asked: the workspace's store, which the host hands
/// whoever answers callers.
pub trait AskedRecords: Send + Sync {
    /// Write the call `request` that `caller` made, which needs the yes of
    /// `facts.by`'s person, as its `tilo.invocations` record in status asked
    /// in `workspace`, read by the caller and the person asked: its address.
    ///
    /// # Errors
    /// Why it could not be written, in words its caller reads.
    fn write_asked(
        &self,
        workspace: &str,
        caller: &super::Caller,
        request: &super::InvokeRequest,
        facts: &AskedFacts,
    ) -> Result<RecordRef, String>;

    /// Every asked record `person` (their root key) may answer, across the
    /// workspaces here, oldest first.
    fn asked_of(&self, person: &[u8; 32]) -> Vec<Asked>;

    /// Answered once an asked record here changes: what a live listing of
    /// [`Self::asked_of`] waits on before it reads again.
    fn changed(&self) -> super::Pending<()>;
}

/// A person's approve of an asked record, as the call kept in it reads it
/// when the dispatch admits it again ([`super::InvokeRequest::approval`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// The asked record.
    pub record: RecordRef,
    /// The choice they picked, by its place among [`AskedFacts::choices`],
    /// for a question that offers choices.
    pub choice: Option<usize>,
    /// The choices they picked, for a question that offers many ([`AskedFacts::many`]).
    #[serde(default)]
    pub chosen: Vec<usize>,
    /// What they typed, one entry per take ([`AskedFacts::takes`]). Kept in memory only and
    /// wiped when dropped: it never travels in a frame, so it reaches the asker only on the
    /// device that asked.
    #[serde(skip)]
    pub typed: Vec<Zeroizing<String>>,
    /// The files they picked, as URIs ([`AskedFacts::files`]).
    #[serde(default)]
    pub files: Vec<String>,
}

impl Approval {
    /// The approve of `record`, picking `choice` where it offers choices, and nothing more.
    #[must_use]
    pub fn new(record: RecordRef, choice: Option<usize>) -> Self {
        Self {
            record,
            choice,
            chosen: Vec::new(),
            typed: Vec::new(),
            files: Vec::new(),
        }
    }
}

/// An answer a person may give an asked record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Offer {
    #[default]
    Approve,
    Decline,
    /// Approve, and grant the asker the action on every resource of its type.
    ApproveAndGrant,
}

impl Offer {
    /// What a question showing `facts` offers, in the order they are shown: approve, approve
    /// and grant where there is a resource type to name and an agent or an app to grant it
    /// to, and decline.
    #[must_use]
    pub fn of(facts: &AskedFacts) -> Vec<Self> {
        let mut offers = vec![Self::Approve];
        if facts.plural.is_some()
            && facts.choices.is_empty()
            && !matches!(facts.by, AskedBy::Device(_) | AskedBy::Program { .. })
        {
            offers.push(Self::ApproveAndGrant);
        }
        offers.push(Self::Decline);
        offers
    }
}

/// The arguments of [`RESOLVE`]: the person's answer, and for a question that offers choices
/// the one they picked, by its place among [`AskedFacts::choices`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resolve {
    pub resolution: Offer,
    #[serde(default)]
    pub choice: Option<usize>,
    /// The choices picked, for a question that offers many ([`AskedFacts::many`]).
    #[serde(default, deserialize_with = "none_is_empty")]
    pub chosen: Vec<usize>,
    /// What the person typed, one entry per take, in order ([`AskedFacts::takes`]). A secret
    /// one is never written anywhere: the record and its history keep the answer without it.
    #[serde(default, deserialize_with = "none_is_empty")]
    pub typed: Vec<String>,
    /// The files picked, as URIs ([`AskedFacts::files`]).
    #[serde(default, deserialize_with = "none_is_empty")]
    pub files: Vec<String>,
}

/// A list a caller left out as nothing (`null`, as a view passes an option it did not give)
/// read as an empty one.
fn none_is_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
}

impl Resolve {
    /// This answer as the record and its history may keep it: what was typed for a secret
    /// take of `facts` left out.
    #[must_use]
    pub fn kept(&self, facts: &AskedFacts) -> Self {
        let typed = self
            .typed
            .iter()
            .enumerate()
            .filter(|(at, _)| facts.takes.get(*at).is_none_or(|take| !take.secret))
            .map(|(_, typed)| typed.clone())
            .collect();
        Self {
            typed,
            ..self.clone()
        }
    }
}

/// Go on where the person approved `request`; else refuse it until they do, showing them
/// `facts`: what a provider whose action needs their yes does, and all it does. The dispatch
/// that carried the call keeps it as its asked record, and an approve brings it back once,
/// the same call, carrying [`super::InvokeRequest::approval`].
///
/// # Errors
/// [`super::ActionCallError::ConfirmationRequired`] with `facts`, for a call nobody approved.
pub fn approved_or_asks(
    request: &super::InvokeRequest,
    facts: impl FnOnce() -> AskedFacts,
) -> Result<(), super::ActionCallError> {
    if request.approval().is_some() {
        Ok(())
    } else {
        Err(super::ActionCallError::asks(facts()))
    }
}

/// Whether `target` answers an asked record ([`RESOLVE`]).
#[must_use]
pub fn is_resolve(target: &RemoteActionTarget) -> bool {
    target.provider == super::workspace_invocations::PROVIDER && target.address.action == RESOLVE
}

/// The person a call by `caller` asks when it waits for a yes: the person it acts for, else
/// the person calling. Never the person of the device that carried the connection.
#[must_use]
pub fn asked_person(caller: &super::Caller) -> Option<String> {
    caller
        .acting_for_person()
        .or_else(|| caller.acting().person_id())
        .filter(|person| !person.is_empty())
        .map(str::to_owned)
}

/// The person `caller` answers as: a person acting themselves, or a view acting for the
/// person it was opened for. An agent, a bot or an automation answers for nobody, whoever it
/// acts for.
#[must_use]
pub fn answering_person(caller: &super::Caller) -> Option<&str> {
    let acting = caller.acting();
    if acting.is_agent() || acting.is_bot() || acting.is_automation() {
        return None;
    }
    acting.person_id().filter(|person| !person.is_empty())
}

/// Whether `a` and `b` are the same call: the same target and the same arguments, byte for byte
/// as they travel.
#[must_use]
pub fn same_call(a: &super::InvokeRequest, b: &super::InvokeRequest) -> bool {
    let bytes = |request: &super::InvokeRequest| {
        serde_json::to_vec(&(&request.target, &request.argument)).ok()
    };
    matches!((bytes(a), bytes(b)), (Some(a), Some(b)) if a == b)
}

/// The dispatch that keeps a call waiting in its asked record, as the person's answer reaches
/// it: the only place a call carrying their approve is admitted again.
pub trait AskedDispatch: Send + Sync {
    /// What this dispatch needs, beside the call and its caller, to admit `invocation` again
    /// once it no longer remembers it (the host restarted): kept with its record.
    fn context(&self, invocation: super::InvocationId) -> serde_json::Value;

    /// Admit `request` again under its id for `caller`, who asked for it, carrying the person's
    /// approve ([`super::InvokeRequest::approval`]): only where it still waits in its asked
    /// record for the same call, or, where this dispatch no longer remembers it, as a new call
    /// under that id, with `context` ([`Self::context`]). It runs under `caller`'s grants as
    /// they stand now, and how it goes reaches `caller` as every invocation of theirs does.
    ///
    /// # Errors
    /// Why it was not admitted again.
    fn readmit(
        &self,
        caller: &super::Caller,
        request: super::InvokeRequest,
        context: &serde_json::Value,
    ) -> Result<(), super::ActionCallError>;

    /// End `invocation`, which `caller` asked for, declined by the person: `caller` is told so.
    /// One this dispatch no longer remembers ended already.
    ///
    /// # Errors
    /// Why it could not be ended.
    fn declined(
        &self,
        caller: &super::Caller,
        invocation: super::InvocationId,
    ) -> Result<(), super::ActionCallError>;

    /// End `invocation`, which `caller` asked for, withdrawn by whoever asked it before its
    /// person answered ([`AskedStore::withdraw`]): it ends cancelled, saying `message`.
    fn withdrawn(&self, caller: &super::Caller, invocation: super::InvocationId, message: &str) {
        let _ = (caller, invocation, message);
    }
}

/// An asked record as the host keeps it in its own state: the call that waits, who asked it,
/// what its person is shown, and what its dispatch needs to admit it again, so the call waits
/// across the host's restarts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KeptAsk {
    pub at: RecordRef,
    /// The person asked, their root key in hex.
    pub person: String,
    pub caller: super::KeptCaller,
    pub request: super::InvokeRequest,
    pub facts: AskedFacts,
    pub asked_at_ms: u64,
    /// The dispatch that keeps the call, by the name it gave the store.
    pub dispatch: String,
    /// What that dispatch needs to admit it again ([`AskedDispatch::context`]).
    pub context: serde_json::Value,
    /// How the call ended where it ran, once a device that keeps the person's root ran it
    /// ([`AT_ROOT`]): what the device that asked shows, after which it forgets the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<crate::InvocationUpdate>,
}

/// Where a host keeps the asked records that wait, across its restarts: each kept once it is
/// written, and forgotten once it is answered, in that order. The host reads what it kept back
/// when it starts ([`AskedStore::keep_through`]).
pub trait AskedKeeping: Send + Sync {
    /// Keep `ask`, in place of the one kept under its record.
    fn keep(&self, ask: &KeptAsk);

    /// Forget the record `at`: it waits no more.
    fn forget(&self, at: &RecordRef);
}

/// What writes the grant a person gives with an approve
/// ([`Offer::ApproveAndGrant`]): an ordinary grant of the workspace's policy, shown and taken
/// back as every grant is.
pub trait AskedGrants: Send + Sync {
    /// Write `grant` in `workspace`, given by `person` (their root key, hex).
    ///
    /// # Errors
    /// Why it was not written: refused where it reaches beyond what `person` may grant.
    fn grant(
        &self,
        workspace: &str,
        person: &str,
        grant: tilo_action_policy::Grant,
    ) -> super::Pending<Result<(), String>>;
}

/// Where the call was pressed, when a person's press asked it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pressed {
    /// The device the press was on, by its id.
    pub device: String,
    pub app: String,
    /// The window's tab key, as the session publishes it (`device.Window.key`).
    pub instance: String,
    pub resource: String,
}

/// One question that waits for the person, as [`WAITING`]`#`[`LIST`] lists it:
/// an asked invocation record they may answer with `<record>#resolve`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asked {
    /// The record: what `resolve` is called on.
    pub record: RecordRef,
    /// The workspace the asked call acts in.
    pub workspace: String,
    /// That workspace by its name.
    pub workspace_name: String,
    /// The app whose action is asked.
    pub app: String,
    /// The action, `<app>#<action>`.
    pub action: String,
    /// The action by its published label.
    pub name: String,
    /// Since when it waits, in milliseconds since the Unix epoch.
    pub asked_at_ms: u64,
    #[serde(default)]
    pub pressed: Option<Pressed>,
    pub offers: Vec<Offer>,
    #[serde(flatten)]
    pub facts: AskedFacts,
}

/// Every question that waits for the person, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskedList {
    pub questions: Vec<Asked>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    /// A row reads as a client binds it: camel case, its facts beside the
    /// record's own names.
    #[test]
    fn a_row_reads_as_a_client_binds_it() {
        let row = Asked {
            record: RecordRef {
                workspace: "acme".into(),
                record: "invocations/7".into(),
                device: None,
            },
            workspace: "acme".into(),
            workspace_name: "Acme".into(),
            app: "clocks".into(),
            action: "clocks#set-alarm".into(),
            name: "Set alarm".into(),
            asked_at_ms: 9,
            pressed: None,
            offers: Vec::new(),
            facts: AskedFacts {
                verb: "set".into(),
                plural: Some("alarms".into()),
                ..AskedFacts::by(AskedBy::Agent(Named {
                    id: "scout".into(),
                    name: "Scout".into(),
                }))
            }
            .with_fact("At", "07:00"),
        };
        let row = Asked {
            offers: Offer::of(&row.facts),
            ..row
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "record": {"workspace": "acme", "record": "invocations/7"},
                "workspace": "acme",
                "workspaceName": "Acme",
                "app": "clocks",
                "action": "clocks#set-alarm",
                "name": "Set alarm",
                "askedAtMs": 9,
                "pressed": null,
                "offers": ["approve", "approve_and_grant", "decline"],
                "by": {"agent": {"id": "scout", "name": "Scout"}},
                "words": "",
                "on": null,
                "facts": [{"label": "At", "value": "07:00"}],
                "destructive": false,
                "choices": [],
                "verb": "set",
                "plural": "alarms",
                "conversation": null,
                "takes": null,
                "kind": "",
                "effect": null,
            })
        );
        assert_eq!(serde_json::from_value::<Asked>(json).unwrap(), row);
    }

    /// The facts travel by position as a refusal does: every part is written
    /// whether or not it holds anything.
    #[test]
    fn facts_travel_by_position() {
        let facts = AskedFacts::by(AskedBy::Device(Named {
            id: "d".into(),
            name: "Laptop".into(),
        }))
        .with_fact("To", "https://visa.example");
        let wire = postcard::to_allocvec(&facts).unwrap();
        assert_eq!(postcard::from_bytes::<AskedFacts>(&wire).unwrap(), facts);
    }
}
