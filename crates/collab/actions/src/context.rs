//! Human context carried by an action into durable history.
//!
//! The action payload says how to reproduce a mutation. [`ActionContext`]
//! says how a person should find and understand it later. It
//! contains no generated prose: apps provide the stable action/resource
//! labels they already expose through the action door, while a person or
//! agent may add an explicit purpose and work-group label.

use serde::{Deserialize, Serialize};

/// Prefix used when context is embedded in an Automerge change message.
/// Ordinary application-authored commit messages remain untouched.
pub const CHANGE_MESSAGE_PREFIX: &str = "tilo.action/1 ";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionContext {
    /// Human action label, e.g. "Archive" or "Rename".
    pub action_label: String,
    /// Publishing app, when the host knows it.
    pub app: Option<ActionApp>,
    /// The resource the action addressed.
    pub resource: Option<ActionResource>,
    /// What the actor says they are trying to accomplish. Never inferred.
    pub purpose: Option<String>,
    /// Stable cross-action work unit: issue, agent run, task, or session.
    pub group: Option<ActionGroup>,
    /// Browseable dimensions that do not fit the resource hierarchy.
    pub facets: Vec<ActionFacet>,
    /// Who invoked the action and whose view it resolves against. `None`
    /// keeps the historical meaning: the local user acting in this window.
    /// Omitted from the encoded change message when absent, so existing
    /// history bytes are unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub invoker: Option<ActionInvoker>,
    /// What the change is to the people sharing the document, declared by
    /// the action ([`crate::Action::change_kind`]). Omitted for content, so
    /// existing history bytes are unchanged.
    #[serde(skip_serializing_if = "ChangeKind::is_content")]
    pub change: ChangeKind,
}

/// Whether a change is to a document's content or only housekeeping about
/// it. Housekeeping (a read marker, a reading position) is durable and
/// shared like any change, and stays in the document's own history, but a
/// feed of what people did to the workspace leaves it out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangeKind {
    #[default]
    Content,
    Housekeeping,
}

impl ChangeKind {
    pub const fn is_content(&self) -> bool {
        matches!(self, Self::Content)
    }
}

impl ActionContext {
    /// Fill missing provenance from a task/session context while preserving
    /// every value supplied by the concrete action/resource.
    #[must_use]
    pub fn enriched_by(mut self, ambient: &Self) -> Self {
        if self.action_label.trim().is_empty() {
            self.action_label.clone_from(&ambient.action_label);
        }
        if self.app.is_none() {
            self.app.clone_from(&ambient.app);
        }
        if self.resource.is_none() {
            self.resource.clone_from(&ambient.resource);
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
        for facet in &ambient.facets {
            if !self.facets.contains(facet) {
                self.facets.push(facet.clone());
            }
        }
        self
    }

    /// Build truthful baseline context from a serializable action enum.
    ///
    /// Serde's externally tagged representation exposes the stable variant
    /// name without requiring every app to duplicate a `match` merely to get a
    /// readable fallback. Apps should still replace the label and attach a
    /// resource for important domain actions.
    pub fn for_action<T: Serialize>(
        app_id: impl Into<String>,
        app_label: impl Into<String>,
        action: &T,
    ) -> Self {
        let action_label = serde_json::to_value(action)
            .ok()
            .as_ref()
            .and_then(action_variant)
            .map_or_else(|| "Update".to_string(), humanize_identifier);
        Self {
            action_label,
            ..Self::default()
        }
        .with_app(app_id, app_label)
    }

    pub fn for_resource(
        action_label: impl Into<String>,
        id: impl Into<String>,
        kind: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            action_label: action_label.into(),
            resource: Some(ActionResource {
                id: id.into(),
                kind: kind.into(),
                label: label.into(),
            }),
            ..Self::default()
        }
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
    /// exactly the authenticated `actor` and the host-derived `participant`,
    /// discarding whatever invoker the caller put in the envelope.
    #[must_use]
    pub fn with_host_invoker(
        mut self,
        actor: impl Into<String>,
        participant: Option<ParticipantId>,
    ) -> Self {
        self.invoker = Some(ActionInvoker {
            actor: actor.into(),
            participant,
        });
        self
    }

    #[must_use]
    pub fn with_facet(mut self, facet: ActionFacet) -> Self {
        self.facets.push(facet);
        self
    }

    #[must_use]
    pub fn with_change(mut self, change: ChangeKind) -> Self {
        self.change = change;
        self
    }

    /// Record what the action was given, summarised from its encoded payload
    /// ([`args_summary`]), as the [`ARGS_FACET`] facet: history then says
    /// what each step did, not only which action it was.
    #[must_use]
    pub fn with_args_from_payload(mut self, payload: &[u8]) -> Self {
        if self.facets.iter().any(|facet| facet.key == ARGS_FACET) {
            return self;
        }
        if let Some(summary) = args_summary(payload) {
            self.facets.push(ActionFacet::new(ARGS_FACET, summary));
        }
        self
    }

    /// The arguments summary [`Self::with_args_from_payload`] recorded.
    pub fn args(&self) -> Option<&str> {
        self.facets
            .iter()
            .find(|facet| facet.key == ARGS_FACET)
            .map(|facet| facet.value.as_str())
    }

    /// Whether the change this context describes is housekeeping, not content.
    pub fn is_housekeeping(&self) -> bool {
        self.change == ChangeKind::Housekeeping
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

    /// Read context from a change message. Unknown and malformed messages are
    /// ordinary legacy/app commits, never an error that hides their history.
    pub fn from_change_message(message: &str) -> Option<Self> {
        let json = message.strip_prefix(CHANGE_MESSAGE_PREFIX)?;
        serde_json::from_str(json).ok()
    }

    /// Best available truthful line for a collapsed activity group.
    pub fn summary(&self) -> Option<String> {
        if let Some(purpose) = self.purpose.as_deref().and_then(nonempty_ref) {
            return Some(purpose.to_string());
        }
        match (
            nonempty_ref(&self.action_label),
            self.resource
                .as_ref()
                .and_then(|resource| nonempty_ref(&resource.label)),
        ) {
            (Some(action), Some(resource)) => Some(format!("{action} · {resource}")),
            (Some(action), None) => Some(action.to_string()),
            (None, Some(resource)) => Some(resource.to_string()),
            (None, None) => None,
        }
    }
}

/// The facet key of an action's arguments summary.
pub const ARGS_FACET: &str = "args";

/// Longest arguments summary kept in a change message.
pub const MAX_ARGS_SUMMARY: usize = 160;

/// A short, readable summary of an encoded action payload: the value inside
/// a single-variant wrapper, strings cut short, nested arrays counted rather
/// than listed. `None` for a payload that is not JSON or says nothing.
pub fn args_summary(payload: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let value = arguments_of(&value);
    if value.is_null() || value.as_object().is_some_and(serde_json::Map::is_empty) {
        return None;
    }
    let mut out = String::new();
    brief(value, 0, &mut out);
    if out.chars().count() > MAX_ARGS_SUMMARY {
        out = out.chars().take(MAX_ARGS_SUMMARY).collect::<String>() + "…";
    }
    Some(out)
}

/// The arguments inside an encoded action's envelopes: a stamped
/// operation's `op` (beside its `at` time), then an adjacently tagged
/// operation's `args` (beside its `action` name) or an externally tagged
/// variant's fields.
fn arguments_of(value: &serde_json::Value) -> &serde_json::Value {
    let serde_json::Value::Object(fields) = value else {
        return value;
    };
    let mut keys: Vec<&str> = fields.keys().map(String::as_str).collect();
    keys.sort_unstable();
    match keys.as_slice() {
        ["at", "op"] => fields.get("op").map_or(value, arguments_of),
        ["action", "args"] => fields.get("args").unwrap_or(value),
        [_] => fields
            .values()
            .next()
            .filter(|fields| fields.is_object())
            .unwrap_or(value),
        _ => value,
    }
}

fn brief(value: &serde_json::Value, depth: usize, out: &mut String) {
    use std::fmt::Write as _;
    match value {
        serde_json::Value::String(text) => {
            let cut: String = text.chars().take(32).collect();
            let ellipsis = if cut.len() < text.len() { "…" } else { "" };
            let _ = write!(out, "{:?}", format!("{cut}{ellipsis}"));
        }
        serde_json::Value::Array(items) if depth > 0 || items.len() > 4 => {
            let _ = write!(out, "[{} items]", items.len());
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (at, item) in items.iter().enumerate() {
                if at > 0 {
                    out.push(',');
                }
                brief(item, depth.saturating_add(1), out);
            }
            out.push(']');
        }
        serde_json::Value::Object(fields) if depth > 1 => {
            let _ = write!(out, "{{{} fields}}", fields.len());
        }
        serde_json::Value::Object(fields) => {
            out.push('{');
            let mut first = true;
            for (key, field) in fields.iter().filter(|(_, field)| !field.is_null()) {
                if !first {
                    out.push(',');
                }
                first = false;
                let _ = write!(out, "{key}:");
                brief(field, depth.saturating_add(1), out);
            }
            out.push('}');
        }
        other => {
            let _ = write!(out, "{other}");
        }
    }
}

fn action_variant(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(variant) => Some(variant),
        serde_json::Value::Object(fields) if fields.len() == 1 => {
            fields.keys().next().map(String::as_str)
        }
        _ => None,
    }
}

fn humanize_identifier(value: &str) -> String {
    let mut result = String::with_capacity(value.len().saturating_add(4));
    let mut previous_lowercase_or_digit = false;
    for character in value.chars() {
        if character == '_' || character == '-' {
            if !result.ends_with(' ') {
                result.push(' ');
            }
            previous_lowercase_or_digit = false;
            continue;
        }
        if character.is_uppercase() && previous_lowercase_or_digit {
            result.push(' ');
        }
        previous_lowercase_or_digit = character.is_lowercase() || character.is_ascii_digit();
        result.push(character);
    }
    result
}

fn nonempty(value: &str) -> Option<String> {
    nonempty_ref(value).map(str::to_string)
}

fn nonempty_ref(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
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

/// The invoker of one action: the authenticated actor plus, optionally, the
/// participant whose view the invocation resolves against.
///
/// `participant: None` means the actor's single/default view (the local
/// window for a person; the only view of a single-view app).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionInvoker {
    /// Authenticated principal, the same string the host admitted
    /// (`PublishedInvocationRequest::actor` in ui-core).
    pub actor: String,
    pub participant: Option<ParticipantId>,
}

impl ActionInvoker {
    pub fn actor(actor: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            participant: None,
        }
    }

    #[must_use]
    pub fn with_participant(mut self, participant: ParticipantId) -> Self {
        self.participant = Some(participant);
        self
    }

    /// The invoker an application worker may act on for one delivered
    /// invocation. `actor` is the identity the host authenticated and put on
    /// the delivery; `context` is the host-built provenance of the same
    /// delivery. The participant is kept only when the context names this
    /// exact actor. A context the host did not rewrite for this actor
    /// therefore never routes to anybody's view, and the actor always comes
    /// from the delivery, never from the context.
    pub fn delivered(actor: &str, context: Option<&ActionContext>) -> Self {
        let participant = context
            .and_then(|context| context.invoker.as_ref())
            .filter(|invoker| invoker.actor == actor)
            .and_then(|invoker| invoker.participant.clone());
        Self {
            actor: actor.to_owned(),
            participant,
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

    #[test]
    fn args_summary_says_what_an_action_was_given_in_brief() {
        let payload = br#"{"Paint":{"layer":"sky","strokes":[{"points":[[1,2],[3,4]]},{"points":[]}],"note":null}}"#;
        assert_eq!(
            args_summary(payload).as_deref(),
            Some(r#"{layer:"sky",strokes:[2 items]}"#)
        );
        let long = format!(r#"{{"Rename":{{"title":"{}"}}}}"#, "x".repeat(400));
        let summary = args_summary(long.as_bytes()).unwrap();
        assert!(summary.chars().count() <= MAX_ARGS_SUMMARY + 1, "{summary}");
        assert_eq!(args_summary(b"\"Clear\""), Some("\"Clear\"".into()));
        assert_eq!(args_summary(b"{\"Clear\":{}}"), None);
        assert_eq!(args_summary(b"not json"), None);
        let stamped = br#"{"op":{"action":"add","args":{"text":"milk"}},"at":17}"#;
        assert_eq!(args_summary(stamped).as_deref(), Some(r#"{text:"milk"}"#));

        let context = ActionContext::for_resource("Paint", "r", "k", "l")
            .with_args_from_payload(payload)
            .with_args_from_payload(b"{\"Other\":1}");
        assert_eq!(context.args(), args_summary(payload).as_deref());
        let message = context.change_message().unwrap();
        assert_eq!(
            ActionContext::from_change_message(&message).unwrap().args(),
            context.args()
        );
    }

    #[test]
    fn absent_invoker_keeps_change_message_bytes() {
        let context = ActionContext::for_resource("Rename", "r", "k", "l");
        let message = context.change_message().unwrap();
        assert!(!message.contains("invoker"));
        assert_eq!(ActionContext::from_change_message(&message), Some(context));
    }

    #[test]
    fn invoker_round_trips_and_is_enriched_from_ambient() {
        let invoker = ActionInvoker::actor("agent:alpha")
            .with_participant(ParticipantId::new("task-7").unwrap());
        let context =
            ActionContext::for_resource("Type", "r", "k", "l").with_invoker(invoker.clone());
        let message = context.change_message().unwrap();
        assert_eq!(
            ActionContext::from_change_message(&message),
            Some(context.clone())
        );
        let enriched = ActionContext::default().enriched_by(&context);
        assert_eq!(enriched.invoker, Some(invoker));
    }

    #[test]
    fn host_invoker_overwrites_a_forged_envelope_invoker() {
        let forged = ActionContext::for_resource("Type", "r", "k", "l").with_invoker(
            ActionInvoker::actor("person:owner")
                .with_participant(ParticipantId::new("owner-window").unwrap()),
        );
        let task = ParticipantId::for_agent_task("11111111-1111-4111-8111-111111111111").unwrap();
        let rewritten = forged.with_host_invoker("agent:alpha", Some(task.clone()));
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
    }

    #[test]
    fn participant_ids_are_bounded() {
        assert!(ParticipantId::new("").is_err());
        assert!(ParticipantId::new("has space").is_err());
        assert!(ParticipantId::new("x".repeat(MAX_PARTICIPANT_ID_BYTES + 1)).is_err());
        assert!(ParticipantId::new("x".repeat(MAX_PARTICIPANT_ID_BYTES)).is_ok());
        assert!(serde_json::from_str::<ParticipantId>("\"\"").is_err());
    }

    /// A change message round-trips its context; unrelated or malformed messages read as legacy
    /// history.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn change_messages_carry_their_context_and_anything_else_is_legacy() {
        change_message_round_trips_context();
        unrelated_or_malformed_messages_are_legacy_history();
    }

    fn change_message_round_trips_context() {
        let context = ActionContext::for_resource(
            "Rename",
            "files.entry:brief.md",
            "files.entry",
            "brief.md",
        )
        .with_app("org.tilo.Files", "Files")
        .with_purpose("Prepare the launch brief")
        .with_group("issue:42", "Launch brief", "issue")
        .with_facet(ActionFacet::new("file-type", "Markdown"));
        let message = context.change_message().expect("non-empty context");
        assert_eq!(ActionContext::from_change_message(&message), Some(context));
    }

    #[test]
    fn housekeeping_is_carried_and_content_keeps_change_message_bytes() {
        let content = ActionContext::for_resource("Rename", "r", "k", "l");
        assert!(
            !content.change_message().unwrap().contains("change"),
            "content is the default and is not written"
        );
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

    fn unrelated_or_malformed_messages_are_legacy_history() {
        assert!(ActionContext::from_change_message("saved draft").is_none());
        assert!(ActionContext::from_change_message("tilo.action/1 {").is_none());
        assert!(ActionContext::default().change_message().is_none());
    }

    /// Summaries prefer a self-described purpose, serialisable actions get a readable truthful
    /// fallback, the generic palette infers stable app ownership, and ambient context fills intent
    /// without overwriting the action's own.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn action_context_describes_intent_truthfully_without_overwriting_it() {
        summary_prefers_self_described_purpose();
        serializable_action_provides_a_readable_truthful_fallback();
        generic_palette_context_infers_stable_app_ownership();
        ambient_context_fills_intent_without_overwriting_the_action();
    }

    fn summary_prefers_self_described_purpose() {
        let contextual = ActionContext::for_resource("Rename", "id", "kind", "brief.md")
            .with_purpose("Prepare the launch brief");
        assert_eq!(
            contextual.summary().as_deref(),
            Some("Prepare the launch brief")
        );

        let structural = ActionContext::for_resource("Rename", "id", "kind", "brief.md");
        assert_eq!(structural.summary().as_deref(), Some("Rename · brief.md"));
    }

    fn serializable_action_provides_a_readable_truthful_fallback() {
        #[derive(Serialize)]
        enum ExampleAction {
            RenameFile { id: u64 },
        }
        let context = ActionContext::for_action(
            "org.tilo.Code",
            "Code",
            &ExampleAction::RenameFile { id: 7 },
        );
        assert_eq!(context.action_label, "Rename File");
        assert_eq!(
            context.app.as_ref().map(|app| app.label.as_str()),
            Some("Code")
        );
    }

    fn generic_palette_context_infers_stable_app_ownership() {
        let context = ActionContext::for_resource("Export", "doc:1", "document", "Brief")
            .with_inferred_app("word-processor.action");
        assert_eq!(
            context.app,
            Some(ActionApp {
                id: "org.tilo.WordProcessor".into(),
                label: "Word Processor".into(),
            })
        );
    }

    fn ambient_context_fills_intent_without_overwriting_the_action() {
        let action = ActionContext::for_resource("Rename", "file:1", "file", "brief.md")
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
