//! The history entry an action carries into its committed change.
//!
//! The action payload says how to reproduce a mutation. [`ActionContext`]
//! is the entry history keeps of it: who acted and for whom, in which task or
//! run, on which resource and item (with the item's label at the time), what
//! was done ([`Did`]), and what kind of change it was. It contains no
//! sentence: readers render one in their own language, and agents, scripts
//! and logs read the data. The caller supplies who and why (invoker,
//! purpose, group); the executor fills in what, from the action itself.

use serde::{Deserialize, Serialize};

use crate::InvocationId;
use crate::entry::{ActionItem, Described, Did};
use crate::interaction::ActorKind;
use crate::key::ActionKind;

/// Prefix used when an entry is embedded in an Automerge change message.
/// Ordinary application-authored commit messages remain untouched.
pub const CHANGE_MESSAGE_PREFIX: &str = "tilo.entry/1 ";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionContext {
    /// Publishing app, when the host knows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<ActionApp>,
    /// The resource the action addressed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<ActionResource>,
    /// The item inside the resource the action addressed, with its label at
    /// the time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<ActionItem>,
    /// What the action did, described by the action before it applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub what: Option<Did>,
    /// What the action is able to do (read, change the document, reach
    /// outside it). Written by the executor from the action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ActionKind>,
    /// What the actor says they are trying to accomplish. Never inferred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Stable cross-action work unit: issue, agent run, task, or session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<ActionGroup>,
    /// Browseable dimensions that do not fit the resource hierarchy.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub facets: Vec<ActionFacet>,
    /// Who invoked the action, for whom, and whose view it resolves
    /// against. `None` is the local person acting in this window.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoker: Option<ActionInvoker>,
    /// The invocation admitted by the action door, when this change came
    /// through one. This is correlation, not authority: executors replace
    /// this field from their retained admission before applying an action.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_invocation: Option<InvocationId>,
    /// Replayable document compensation computed by the canonical action
    /// executor. This describes a change; it carries no caller authority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compensation: Option<ActionCompensation>,
    /// The gesture this change belongs to: an id every change of one
    /// actor's gesture shares (the keystrokes of a word, the values one drag
    /// of a slider passes through), which undo takes back as one step.
    /// Absent for a change that is a step of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gesture: Option<crate::ActionId>,
    /// Exact executor action that produced this change. This is inert
    /// correlation, never caller authority: fresh semantic envelopes replace
    /// it with their own identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executed_action: Option<crate::ActionId>,
    /// What the change is to the people sharing the document, declared by
    /// the action ([`crate::Action::change_kind`]) or, for a change that is
    /// nobody's step whatever it does (what a brand-new record starts with),
    /// by the context it is applied with.
    #[serde(skip_serializing_if = "ChangeKind::is_content")]
    pub change: ChangeKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionCompensation {
    pub undo: crate::EncodedAction,
    pub redo: crate::EncodedAction,
}

/// What a change is to the people sharing a document. Every kind is durable,
/// shared and kept in the document's own history; they differ in whether a
/// person takes the change back and whether a feed of what people did to the
/// workspace shows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangeKind {
    /// Something a person did to the content: an undo step of its author's,
    /// shown in workspace activity.
    #[default]
    Content,
    /// Housekeeping about the content (a read marker, a reading position):
    /// no undo step, and workspace activity leaves it out.
    Housekeeping,
    /// Content that came from outside rather than from anyone's edit:
    /// articles a refresh fetched, a process's output, what a brand-new
    /// record starts with from state that already existed. Collaborators
    /// see it like any content, but it is nobody's undo step, and workspace
    /// activity leaves it out.
    External,
}

impl ChangeKind {
    pub const fn is_content(&self) -> bool {
        matches!(self, Self::Content)
    }

    /// Whether a change of this kind is an undo step of its author's. Only
    /// a person's own edits of the content are.
    pub const fn is_step(&self) -> bool {
        self.is_content()
    }
}

impl ActionContext {
    /// Fill missing provenance from a task/session context while preserving
    /// every value supplied by the concrete action/resource.
    #[must_use]
    pub fn enriched_by(mut self, ambient: &Self) -> Self {
        if self.app.is_none() {
            self.app.clone_from(&ambient.app);
        }
        if self.resource.is_none() {
            self.resource.clone_from(&ambient.resource);
        }
        if self.item.is_none() {
            self.item.clone_from(&ambient.item);
        }
        if self.purpose.is_none() {
            self.purpose.clone_from(&ambient.purpose);
        }
        if self.group.is_none() {
            self.group.clone_from(&ambient.group);
        }
        if self.invoker.is_none() {
            self.invoker.clone_from(&ambient.invoker);
        }
        if self.source_invocation.is_none() {
            self.source_invocation = ambient.source_invocation;
        }
        for facet in &ambient.facets {
            if !self.facets.contains(facet) {
                self.facets.push(facet.clone());
            }
        }
        self
    }

    /// Pin the invocation retained by the admitted dispatch boundary.
    ///
    /// This replaces rather than fills an ambient value. Only the executor
    /// that owns admission should call it.
    #[must_use]
    pub fn with_source_invocation(mut self, invocation: Option<InvocationId>) -> Self {
        self.source_invocation = invocation;
        self
    }

    /// The entry of an action on one resource.
    pub fn for_resource(
        id: impl Into<String>,
        kind: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            resource: Some(ActionResource {
                id: id.into(),
                kind: kind.into(),
                label: label.into(),
            }),
            ..Self::default()
        }
    }

    /// The entry of an action published by one app.
    pub fn for_app(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::default().with_app(id, label)
    }

    #[must_use]
    pub fn with_app(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.app = Some(ActionApp {
            id: id.into(),
            label: label.into(),
        });
        self
    }

    /// Fill app ownership from the registered action type when a generic
    /// palette/door caller has only resource metadata. Explicit app context
    /// always wins. Action types conventionally start with the package stem
    /// (`word-processor.action`, `api_tester.action`).
    #[must_use]
    pub fn with_inferred_app(mut self, type_id: &str) -> Self {
        if self.app.is_some() {
            return self;
        }
        let stem = type_id.split('.').next().unwrap_or("").trim();
        let words: Vec<&str> = stem
            .split(['-', '_'])
            .filter(|word| !word.is_empty())
            .collect();
        if words.is_empty() {
            return self;
        }
        let title_word = |word: &str| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        };
        let component = words
            .iter()
            .map(|word| title_word(word))
            .collect::<String>();
        let label = words
            .iter()
            .map(|word| title_word(word))
            .collect::<Vec<_>>()
            .join(" ");
        self.app = Some(ActionApp {
            id: format!("org.tilo.{component}"),
            label,
        });
        self
    }

    #[must_use]
    pub fn with_purpose(mut self, purpose: impl Into<String>) -> Self {
        let purpose = purpose.into();
        self.purpose = nonempty(&purpose);
        self
    }

    #[must_use]
    pub fn with_group(
        mut self,
        id: impl Into<String>,
        label: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        self.group = Some(ActionGroup {
            id: id.into(),
            label: label.into(),
            kind: kind.into(),
        });
        self
    }

    #[must_use]
    pub fn with_invoker(mut self, invoker: ActionInvoker) -> Self {
        self.invoker = Some(invoker);
        self
    }

    /// Host-side rewrite of a caller-supplied context: the invoker becomes
    /// exactly the authenticated person `actor` and the host-derived
    /// `participant`, discarding whatever invoker the caller put in the
    /// envelope.
    #[must_use]
    pub fn with_host_invoker(
        self,
        actor: impl Into<String>,
        participant: Option<ParticipantId>,
    ) -> Self {
        self.with_host_actor(
            &crate::InteractionActor::new(actor, ActorKind::Person),
            participant,
        )
    }

    /// [`Self::with_host_invoker`] for any admitted actor: an agent, a
    /// Shortcut or an app, with the person it acts for.
    #[must_use]
    pub fn with_host_actor(
        mut self,
        actor: &crate::InteractionActor,
        participant: Option<ParticipantId>,
    ) -> Self {
        let mut invoker = ActionInvoker::admitted(actor);
        invoker.participant = participant;
        self.invoker = Some(invoker);
        self
    }

    #[must_use]
    pub fn with_facet(mut self, facet: ActionFacet) -> Self {
        self.facets.push(facet);
        self
    }

    /// Continue the gesture `gesture` (see [`Self::gesture`]).
    #[must_use]
    pub fn in_gesture(mut self, gesture: Option<crate::ActionId>) -> Self {
        self.gesture = gesture;
        self
    }

    #[must_use]
    pub fn with_change(mut self, change: ChangeKind) -> Self {
        self.change = change;
        self
    }

    /// Record what the action did and the item it addressed.
    #[must_use]
    pub fn with_described(mut self, described: Described) -> Self {
        self.what = Some(described.what);
        if described.item.is_some() {
            self.item = described.item;
        }
        self
    }

    /// What the action did and to which item, as one value.
    pub fn described(&self) -> Option<Described> {
        Some(Described {
            what: self.what.clone()?,
            item: self.item.clone(),
        })
    }

    /// Whether the change this context describes is housekeeping, not content.
    pub fn is_housekeeping(&self) -> bool {
        self.change == ChangeKind::Housekeeping
    }

    /// Whether workspace activity shows this change: content, not
    /// housekeeping and not what came from outside.
    pub fn is_activity(&self) -> bool {
        self.change.is_content()
    }

    /// The actor's kind; the local person when nobody else is recorded.
    pub fn actor_kind(&self) -> ActorKind {
        self.invoker
            .as_ref()
            .map_or(ActorKind::Person, |invoker| invoker.kind)
    }

    /// Encode into the Automerge change message. Returning `None` keeps an
    /// empty/default context from displacing an app's ordinary history path.
    pub fn change_message(&self) -> Option<String> {
        if self == &Self::default() {
            return None;
        }
        serde_json::to_string(self)
            .ok()
            .map(|json| format!("{CHANGE_MESSAGE_PREFIX}{json}"))
    }

    /// Read an entry from a change message. Unknown and malformed messages
    /// are ordinary app commits, never an error that hides their history.
    pub fn from_change_message(message: &str) -> Option<Self> {
        let json = message.strip_prefix(CHANGE_MESSAGE_PREFIX)?;
        serde_json::from_str(json).ok()
    }
}

fn nonempty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Upper bound on a [`ParticipantId`] in bytes. Participants key bounded
/// per-view maps in app workers; they are never free text.
pub const MAX_PARTICIPANT_ID_BYTES: usize = 128;

/// One participant in a shared record: a person's window or an agent task.
/// Each participant owns its own view state (caret, selection, Find), and the
/// app builds that participant's resource tree from it
/// ([`crate::ResourceProvider::resources_for`]).
///
/// Opaque, non-empty, ASCII-printable and at most
/// [`MAX_PARTICIPANT_ID_BYTES`] bytes. It is a routing key for view state,
/// never authority: the authenticated principal is [`ActionInvoker::actor`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ParticipantId(String);

impl ParticipantId {
    /// Validate and wrap a participant id.
    ///
    /// # Errors
    ///
    /// Returns a description when `id` is empty, longer than
    /// `MAX_PARTICIPANT_ID_BYTES`, or not printable ASCII.
    pub fn new(id: impl Into<String>) -> Result<Self, String> {
        let id = id.into();
        if id.is_empty()
            || id.len() > MAX_PARTICIPANT_ID_BYTES
            || !id.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(format!(
                "participant id must be 1..={MAX_PARTICIPANT_ID_BYTES} printable ASCII bytes"
            ));
        }
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The participant of one agent task. Only the host derives it, from
    /// the task it admitted (never from anything the agent sends), so an
    /// agent can address only the view of the task it runs.
    ///
    /// # Errors
    ///
    /// Returns a description when the derived id is not a valid participant
    /// id (see [`ParticipantId::new`]).
    pub fn for_agent_task(task_id: &str) -> Result<Self, String> {
        Self::new(format!("{AGENT_TASK_PARTICIPANT_PREFIX}{task_id}"))
    }

    /// One host-minted execution's private view. This key is subordinate to the
    /// admitted principal and confers no authority. Hosts derive it from their
    /// execution lease, never a client-supplied run label or context.
    pub fn for_execution(execution: uuid::Uuid) -> Self {
        Self(format!("execution:{execution}"))
    }

    /// The task id of a host-derived agent task participant.
    pub fn agent_task(&self) -> Option<&str> {
        self.0.strip_prefix(AGENT_TASK_PARTICIPANT_PREFIX)
    }
}

/// Reserved namespace of [`ParticipantId::for_agent_task`].
pub const AGENT_TASK_PARTICIPANT_PREFIX: &str = "agent-task:";

impl TryFrom<String> for ParticipantId {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ParticipantId> for String {
    fn from(value: ParticipantId) -> Self {
        value.0
    }
}

impl std::fmt::Display for ParticipantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The invoker of one action: the authenticated actor, its kind, the person
/// it acts for, and optionally the participant whose view the invocation
/// resolves against.
///
/// `participant: None` means the actor's single/default view (the local
/// window for a person; the only view of a single-view app).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionInvoker {
    /// Authenticated principal, the same string the host admitted
    /// (`PublishedInvocationRequest::actor` in ui-core).
    pub actor: String,
    #[serde(skip_serializing_if = "is_person")]
    pub kind: ActorKind,
    /// The person an agent, Shortcut or app acted for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_behalf_of: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant: Option<ParticipantId>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_person(kind: &ActorKind) -> bool {
    matches!(kind, ActorKind::Person)
}

impl ActionInvoker {
    /// A person the host authenticated, acting for themselves.
    pub fn actor(actor: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            kind: ActorKind::Person,
            on_behalf_of: None,
            participant: None,
        }
    }

    /// The actor the host admitted: its id, its kind, and the person it acts
    /// for.
    pub fn admitted(actor: &crate::InteractionActor) -> Self {
        Self {
            actor: actor.id.clone(),
            kind: actor.kind,
            on_behalf_of: actor
                .for_person
                .clone()
                .filter(|person| *person != actor.id),
            participant: None,
        }
    }

    #[must_use]
    pub fn with_participant(mut self, participant: ParticipantId) -> Self {
        self.participant = Some(participant);
        self
    }

    #[must_use]
    pub fn with_kind(mut self, kind: ActorKind) -> Self {
        self.kind = kind;
        self
    }

    /// The person this actor acts for. A person acting for themselves
    /// records nobody.
    #[must_use]
    pub fn acting_for(mut self, person: impl Into<String>) -> Self {
        let person = person.into();
        self.on_behalf_of = (person != self.actor && !person.trim().is_empty()).then_some(person);
        self
    }

    /// The invoker an application worker may act on for one delivered
    /// invocation. `actor` is the identity the host authenticated and put on
    /// the delivery; `context` is the host-built provenance of the same
    /// delivery. The participant and the person it acts for are kept only
    /// when the context names this exact actor, or a Shortcut acting for
    /// exactly that actor. A context the host did not rewrite for this actor
    /// therefore never routes to anybody's view.
    pub fn delivered(actor: &str, context: Option<&ActionContext>) -> Self {
        // A Shortcut step is delivered as the person it runs for; the host
        // names the Shortcut acting for exactly that person.
        if let Some(shortcut) =
            context
                .and_then(|context| context.invoker.as_ref())
                .filter(|invoker| {
                    invoker.kind == ActorKind::Shortcut
                        && invoker.on_behalf_of.as_deref() == Some(actor)
                })
        {
            return shortcut.clone();
        }
        let named = context
            .and_then(|context| context.invoker.as_ref())
            .filter(|invoker| invoker.actor == actor);
        Self {
            actor: actor.to_owned(),
            kind: named.map_or(ActorKind::Person, |invoker| invoker.kind),
            on_behalf_of: named.and_then(|invoker| invoker.on_behalf_of.clone()),
            participant: named.and_then(|invoker| invoker.participant.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionApp {
    /// Stable package/application id.
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionResource {
    pub id: String,
    pub kind: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionGroup {
    /// Stable key shared by every action in the work unit.
    pub id: String,
    pub label: String,
    /// Open vocabulary: `issue`, `task`, `agent-run`, `session`, …
    pub kind: String,
}

/// A filterable activity dimension. Open key/value vocabulary lets future
/// apps add facets without changing the wire enum.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionFacet {
    /// e.g. `path`, `directory`, `file-type`, `mime`, `repository`.
    pub key: String,
    pub value: String,
}

impl ActionFacet {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{EditOp, FieldEdit, PathStep, Scalar};
    use crate::key::ActionKey;

    fn starred() -> Described {
        Described::new(Did::Edit(FieldEdit {
            ty: "snippet".into(),
            field: "favorite".into(),
            path: vec![
                PathStep::Field("snippets".into()),
                PathStep::Item("s1".into()),
                PathStep::Field("favorite".into()),
            ],
            op: EditOp::Set,
            old: Some(false.into()),
            new: Some(true.into()),
            args: std::collections::BTreeMap::new(),
        }))
        .on("s1", "Deploy script")
    }

    #[test]
    fn entries_round_trip_through_the_change_message() {
        let entry = ActionContext::for_resource("lib", "snippets.library", "Snippets")
            .with_app("org.tilo.Snippets", "Snippets")
            .with_described(starred())
            .with_group("run:7", "Tidy snippets", "agent-run")
            .with_invoker(
                ActionInvoker::actor("agent:claude")
                    .with_kind(ActorKind::Agent)
                    .acting_for("person:quinten"),
            )
            .with_source_invocation(Some(InvocationId(uuid::Uuid::from_u128(42))));
        let message = entry.change_message().unwrap();
        let read = ActionContext::from_change_message(&message).unwrap();
        assert_eq!(read, entry);
        assert_eq!(read.actor_kind(), ActorKind::Agent);
        assert_eq!(
            read.invoker.as_ref().unwrap().on_behalf_of.as_deref(),
            Some("person:quinten")
        );
        assert_eq!(read.described(), Some(starred()));

        let verb = ActionContext::default().with_described(Described::new(Did::verb(
            ActionKey::new("snippets", "snippets.library", "new-snippet"),
            [("title".to_string(), Scalar::from("Deploy"))],
        )));
        let read = ActionContext::from_change_message(&verb.change_message().unwrap()).unwrap();
        assert_eq!(read, verb);
    }

    #[test]
    fn defaults_are_not_written() {
        let context = ActionContext::for_resource("r", "k", "l");
        let message = context.change_message().unwrap();
        let json: serde_json::Value =
            serde_json::from_str(message.strip_prefix(CHANGE_MESSAGE_PREFIX).unwrap()).unwrap();
        let written: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(written, ["resource"], "{message}");
        assert_eq!(ActionContext::from_change_message(&message), Some(context));
        let outside = ActionContext::default().with_change(ChangeKind::External);
        let mut outside = outside;
        outside.kind = Some(ActionKind::Outside);
        let read = ActionContext::from_change_message(&outside.change_message().unwrap()).unwrap();
        assert_eq!(read.kind, Some(ActionKind::Outside));
        assert!(
            !read.is_activity(),
            "what came from outside is not activity"
        );
    }

    #[test]
    fn invoker_round_trips_and_is_enriched_from_ambient() {
        let invoker = ActionInvoker::actor("agent:alpha")
            .with_participant(ParticipantId::new("task-7").unwrap());
        let context = ActionContext::for_resource("r", "k", "l").with_invoker(invoker.clone());
        let message = context.change_message().unwrap();
        assert_eq!(
            ActionContext::from_change_message(&message),
            Some(context.clone())
        );
        let enriched = ActionContext::default().enriched_by(&context);
        assert_eq!(enriched.invoker, Some(invoker));
    }

    #[test]
    fn a_person_acting_for_themselves_records_nobody() {
        assert_eq!(
            ActionInvoker::actor("quinten")
                .acting_for("quinten")
                .on_behalf_of,
            None
        );
        let agent = ActionInvoker::admitted(
            &crate::InteractionActor::new("agent:claude", ActorKind::Agent).acting_for("quinten"),
        );
        assert_eq!(agent.kind, ActorKind::Agent);
        assert_eq!(agent.on_behalf_of.as_deref(), Some("quinten"));
    }

    #[test]
    fn host_invoker_overwrites_a_forged_envelope_invoker() {
        let forged = ActionContext::for_resource("r", "k", "l").with_invoker(
            ActionInvoker::actor("person:owner")
                .acting_for("person:other")
                .with_participant(ParticipantId::new("owner-window").unwrap()),
        );
        let task = ParticipantId::for_agent_task("11111111-1111-4111-8111-111111111111").unwrap();
        let rewritten = forged
            .clone()
            .with_host_invoker("agent:alpha", Some(task.clone()));
        assert_eq!(
            rewritten.invoker,
            Some(ActionInvoker::actor("agent:alpha").with_participant(task.clone()))
        );
        assert_eq!(
            task.agent_task(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(
            ParticipantId::new("owner-window").unwrap().agent_task(),
            None
        );
        let delegated = forged.with_host_actor(
            &crate::InteractionActor::new("agent:alpha", ActorKind::Agent)
                .acting_for("person:owner"),
            None,
        );
        let invoker = delegated.invoker.unwrap();
        assert_eq!(invoker.kind, ActorKind::Agent);
        assert_eq!(
            invoker.on_behalf_of.as_deref(),
            Some("person:owner"),
            "the host records whom the admitted actor acts for"
        );
    }

    #[test]
    fn delivered_invoker_keeps_the_participant_only_for_the_delivered_actor() {
        let participant = ParticipantId::for_agent_task("t").unwrap();
        let context =
            ActionContext::default().with_host_invoker("agent:a", Some(participant.clone()));
        assert_eq!(
            ActionInvoker::delivered("agent:a", Some(&context)),
            ActionInvoker::actor("agent:a").with_participant(participant)
        );
        // A context naming another actor never routes to a view, and the
        // actor always comes from the delivery.
        assert_eq!(
            ActionInvoker::delivered("person:b", Some(&context)),
            ActionInvoker::actor("person:b")
        );
        assert_eq!(
            ActionInvoker::delivered("person:b", None),
            ActionInvoker::actor("person:b")
        );
        // A Shortcut step is delivered as its person; the Shortcut acting for
        // exactly that person is kept, for anybody else it is not.
        let shortcut = ActionInvoker::admitted(
            &crate::InteractionActor::new("shortcut:morning", ActorKind::Shortcut)
                .acting_for("person:b"),
        );
        let run = ActionContext::default().with_invoker(shortcut.clone());
        assert_eq!(ActionInvoker::delivered("person:b", Some(&run)), shortcut);
        assert_eq!(
            ActionInvoker::delivered("person:c", Some(&run)),
            ActionInvoker::actor("person:c")
        );
    }

    #[test]
    fn participant_ids_are_bounded() {
        assert!(ParticipantId::new("").is_err());
        assert!(ParticipantId::new("has space").is_err());
        assert!(ParticipantId::new("x".repeat(MAX_PARTICIPANT_ID_BYTES + 1)).is_err());
        assert!(ParticipantId::new("x".repeat(MAX_PARTICIPANT_ID_BYTES)).is_ok());
        assert!(serde_json::from_str::<ParticipantId>("\"\"").is_err());
    }

    #[test]
    fn housekeeping_is_carried() {
        let content = ActionContext::for_resource("r", "k", "l");
        let marker = content.clone().with_change(ChangeKind::Housekeeping);
        let message = marker.change_message().unwrap();
        let read = ActionContext::from_change_message(&message).unwrap();
        assert!(read.is_housekeeping());
        assert_eq!(read, marker);
        let bare = ActionContext::default().with_change(ChangeKind::Housekeeping);
        assert!(
            bare.change_message().is_some(),
            "housekeeping alone is still written"
        );
        assert!(
            !ActionContext::from_change_message(&content.change_message().unwrap())
                .unwrap()
                .is_housekeeping()
        );
    }

    #[test]
    fn unrelated_or_malformed_messages_are_not_entries() {
        assert!(ActionContext::from_change_message("saved draft").is_none());
        assert!(ActionContext::from_change_message("tilo.entry/1 {").is_none());
        assert!(ActionContext::default().change_message().is_none());
    }

    #[test]
    fn generic_palette_context_infers_stable_app_ownership() {
        let context = ActionContext::for_resource("doc:1", "document", "Brief")
            .with_inferred_app("word-processor.action");
        assert_eq!(
            context.app,
            Some(ActionApp {
                id: "org.tilo.WordProcessor".into(),
                label: "Word Processor".into(),
            })
        );
    }

    #[test]
    fn ambient_context_fills_intent_without_overwriting_the_action() {
        let action = ActionContext::for_resource("file:1", "file", "brief.md")
            .with_app("org.tilo.Code", "Code");
        let ambient = ActionContext::default()
            .with_purpose("Prepare launch")
            .with_group("issue:42", "Launch", "issue")
            .with_app("wrong", "Wrong");
        let merged = action.enriched_by(&ambient);
        assert_eq!(
            merged.app.as_ref().map(|app| app.id.as_str()),
            Some("org.tilo.Code")
        );
        assert_eq!(merged.purpose.as_deref(), Some("Prepare launch"));
        assert_eq!(
            merged.group.as_ref().map(|group| group.id.as_str()),
            Some("issue:42")
        );
    }
}
