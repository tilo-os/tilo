//! **The door**: how a participant that is not the app enumerates what
//! it has open and performs one of its exported operations.
//!
//! [`Resource`] trees already describe an app's surface and
//! [`dispatch`](crate::executor::dispatch) already performs one
//! operation on it; the missing step was naming a thing from outside the
//! process that built it — without which collaboration, agency and
//! scripting stay per-app features.
//!
//! [`survey`] and [`invoke`] stay separate so enumeration can never
//! cause an effect: a descriptor carries no callback, token or handle,
//! so read-only is the shape of the type, not a promise in a comment.
//!
//! Authority stays with the host, which resolves the caller against the
//! workspace's capability model before offering an address: an
//! operation the caller may not perform is *absent* from the survey,
//! never a flag to ignore.

use crate::action::{EncodedAction, ExecutionLocus};
use crate::context::ActionContext;
use crate::executor::{ActionTransport, Dispatched, dispatch_by_with_context};
use crate::registry::ActionRegistry;
use crate::resource::{ActionInput, ActionSpec, Collects, KEY_SEP, Resource};

/// The stable address of one exported operation: which resource, which
/// action. Both halves are the app's own ids — no screen position, no
/// list index — so the same address names the same operation across
/// layouts, themes, resolutions, and a caller with no display can work
/// from ids and labels alone.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ActionAddress {
    pub resource: String,
    pub action: String,
}

impl ActionAddress {
    pub fn new(resource: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            resource: resource.into(),
            action: action.into(),
        }
    }

    /// The single-string form, for transports that carry one id. [`KEY_SEP`]
    /// never appears in either half, so [`parse`](Self::parse) is unambiguous.
    pub fn encode(&self) -> String {
        format!("{}{}{}", self.resource, KEY_SEP, self.action)
    }

    /// Recover an address from [`encode`](Self::encode). `None` when the string
    /// is not one, so a caller that made an address up is refused here rather
    /// than looked up against a half-parsed id.
    pub fn parse(s: &str) -> Option<Self> {
        let (resource, action) = s.split_once(KEY_SEP)?;
        if resource.is_empty() || action.is_empty() {
            return None;
        }
        Some(Self::new(resource, action))
    }
}

/// What an invocation of one action must carry, advertised on the
/// descriptor rather than discovered by trying — enumeration is the only
/// thing a caller that can't see the screen has to work from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputSpec {
    /// Takes nothing. The majority.
    None,
    /// Takes one text argument. `prompt` and `placeholder` are the app's own
    /// words for it — the ones the palette would show a person — and `collects`
    /// says what kind of thing it names.
    Text {
        prompt: String,
        placeholder: String,
        collects: Collects,
    },
}

impl InputSpec {
    fn of(input: &ActionInput) -> Self {
        match input {
            ActionInput::Immediate(_) => InputSpec::None,
            ActionInput::Prompted {
                prompt,
                placeholder,
                collects,
                ..
            } => InputSpec::Text {
                prompt: prompt.clone(),
                placeholder: placeholder.clone(),
                collects: *collects,
            },
        }
    }

    /// Whether an invocation must supply text.
    pub fn wants_text(&self) -> bool {
        matches!(self, InputSpec::Text { .. })
    }

    /// Whether the text it wants is the address of content rather than
    /// words — words can be quoted out of the request, a content hash cannot.
    pub fn wants_content(&self) -> bool {
        matches!(
            self,
            InputSpec::Text {
                collects: Collects::Content,
                ..
            }
        )
    }
}

/// One exported operation, as inert data: everything a caller needs to
/// decide whether to perform it, nothing that could perform it — an
/// [`ActionSpec`]'s closure never appears here or crosses a boundary,
/// only the address does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionDescriptor {
    pub address: ActionAddress,
    /// The publishing resource's stable kind tag (`word_processor.section`,
    /// `files.entry`), shared with the OS-wide type registry so the same kind
    /// means the same thing across apps.
    pub resource_kind: String,
    /// The resource's human label, as the app words it.
    pub resource_label: String,
    pub action_label: String,
    pub description: Option<String>,
    pub locus: ExecutionLocus,
    pub input: InputSpec,
}

/// Why an address could not be performed. `Unknown` deliberately covers
/// both "no such address" and "not offered to you", so a caller can't
/// map what it may not reach by probing for a different error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoorError {
    /// Nothing in the tree carries this address.
    Unknown(ActionAddress),
    /// The action takes text and none was supplied.
    MissingArgument(ActionAddress),
    /// The action takes nothing and an argument was supplied. Refused, not
    /// dropped — a silently ignored argument leaves the caller believing it
    /// asked for something it didn't get.
    UnexpectedArgument(ActionAddress),
    /// The app never registered a decoder for the action's type, so the door
    /// can't prove what it would run. Fail-closed: a refusal, not a
    /// best-effort apply.
    UnregisteredType(String),
    /// The action ran and failed.
    Failed(String),
}

impl DoorError {
    /// Stable machine-readable code, distinguishing "I addressed this
    /// wrongly" from "it ran and could not do its job".
    pub fn code(&self) -> &'static str {
        match self {
            DoorError::Unknown(_) | DoorError::UnregisteredType(_) => "unknown_action",
            DoorError::MissingArgument(_) | DoorError::UnexpectedArgument(_) => "invalid_arguments",
            DoorError::Failed(_) => "action_failed",
        }
    }
}

impl std::fmt::Display for DoorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DoorError::Unknown(a) => {
                write!(f, "no action addressed by ({}, {})", a.resource, a.action)
            }
            DoorError::MissingArgument(a) => write!(
                f,
                "action ({}, {}) needs a text argument and none was given",
                a.resource, a.action
            ),
            DoorError::UnexpectedArgument(a) => {
                write!(f, "action ({}, {}) takes no argument", a.resource, a.action)
            }
            DoorError::UnregisteredType(t) => {
                write!(f, "no decoder registered for action type `{t}`")
            }
            DoorError::Failed(why) => write!(f, "{why}"),
        }
    }
}

/// Walk a resource forest and describe every operation it exports.
/// Depth-first, parents before children, matching
/// [`flatten`](crate::resource::flatten)'s order. Borrows the tree,
/// returns owned data — nothing handed back can act on what it described.
pub fn survey(resources: &[Resource]) -> Vec<ActionDescriptor> {
    let mut out = Vec::new();
    for r in resources {
        survey_into(r, &mut out);
    }
    out
}

fn survey_into(resource: &Resource, out: &mut Vec<ActionDescriptor>) {
    for action in &resource.actions {
        out.push(ActionDescriptor {
            address: ActionAddress::new(resource.id.as_str(), &action.id),
            resource_kind: resource.kind.clone(),
            resource_label: resource.label.clone(),
            action_label: action.label.clone(),
            description: action.description.clone(),
            locus: action.locus,
            input: InputSpec::of(&action.input),
        });
    }
    for child in &resource.children {
        survey_into(child, out);
    }
}

/// Resolve `address` against `resources` without running anything, so a caller
/// can check what an address currently names before committing to it.
pub fn resolve<'a>(resources: &'a [Resource], address: &ActionAddress) -> Option<&'a ActionSpec> {
    fn walk<'a>(r: &'a Resource, address: &ActionAddress) -> Option<&'a ActionSpec> {
        if r.id.as_str() == address.resource
            && let Some(spec) = r.actions.iter().find(|a| a.id == address.action)
        {
            return Some(spec);
        }
        r.children.iter().find_map(|c| walk(c, address))
    }
    resources.iter().find_map(|r| walk(r, address))
}

/// Turn an address plus its argument into the replayable action it
/// names. Split out from [`invoke`] so a host can build/inspect/route
/// the encoded action itself, and so argument rules are checked in one
/// place. Binding happens *before* anything runs, so a bad call is a
/// refusal with the document untouched, never a half-applied edit.
///
/// # Errors
///
/// Returns [`DoorError::Unknown`] if `address` names no resource/action
/// in `resources`; [`DoorError::UnexpectedArgument`] if `argument` is
/// `Some` for an [`ActionInput::Immediate`] action; or
/// [`DoorError::MissingArgument`] if it's `None` for an
/// [`ActionInput::Prompted`] one.
pub fn encode(
    resources: &[Resource],
    address: &ActionAddress,
    argument: Option<&str>,
) -> Result<EncodedAction, DoorError> {
    let spec = resolve(resources, address).ok_or_else(|| DoorError::Unknown(address.clone()))?;
    match (&spec.input, argument) {
        (ActionInput::Immediate(encoded), None) => Ok(encoded.clone()),
        (ActionInput::Immediate(_), Some(_)) => Err(DoorError::UnexpectedArgument(address.clone())),
        (ActionInput::Prompted { build, .. }, Some(text)) => Ok(build(text)),
        (ActionInput::Prompted { .. }, None) => Err(DoorError::MissingArgument(address.clone())),
    }
}

/// Perform the operation `address` names, attributed to `actor`.
/// Resolve, bind, decode through the app's own registry, then
/// [`dispatch_by`] honouring the action's locus. `actor` is the caller's
/// Automerge actor bytes, so the room's history names who asked.
///
/// `resources` must be a tree built *now*, not the one an earlier
/// [`survey`] walked — re-resolving turns an address that has since
/// stopped naming anything into a refusal rather than an edit to
/// whatever took its place. Decoding through `registry` (rather than
/// trusting the tree) makes this fail-closed: an unregistered action
/// type can't run, and the decoder used here is the one a replaying peer
/// would use too.
///
/// # Errors
///
/// Returns any error [`encode`] documents; [`DoorError::UnregisteredType`]
/// if `registry` has no decoder for the encoded action's type; or
/// [`DoorError::Failed`] if [`dispatch_by`] fails once decoded.
pub fn invoke(
    doc: &mut crate::action::Doc,
    resources: &[Resource],
    address: &ActionAddress,
    argument: Option<&str>,
    actor: &[u8],
    registry: &ActionRegistry,
    transport: &mut dyn ActionTransport,
) -> Result<Dispatched, DoorError> {
    let resource = find_resource(resources, &address.resource)
        .ok_or_else(|| DoorError::Unknown(address.clone()))?;
    let spec = resource
        .actions
        .iter()
        .find(|candidate| candidate.id == address.action)
        .ok_or_else(|| DoorError::Unknown(address.clone()))?;
    let context = ActionContext::for_resource(
        &spec.label,
        resource.id.as_str(),
        &resource.kind,
        &resource.label,
    );
    let encoded = encode(resources, address, argument)?;
    let action = registry
        .decode(&encoded)
        .map_err(|_| DoorError::UnregisteredType(encoded.type_id.clone()))?;
    dispatch_by_with_context(
        doc,
        action.as_ref(),
        encoded,
        transport,
        Some(actor),
        context,
    )
    .map_err(|e| DoorError::Failed(e.to_string()))
}

fn find_resource<'a>(resources: &'a [Resource], id: &str) -> Option<&'a Resource> {
    fn walk<'a>(resource: &'a Resource, id: &str) -> Option<&'a Resource> {
        if resource.id.as_str() == id {
            return Some(resource);
        }
        resource.children.iter().find_map(|child| walk(child, id))
    }
    resources.iter().find_map(|resource| walk(resource, id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Action, ActionError, Doc};
    use crate::executor::{LocalTransport, replay};
    use crate::resource::ResourceProvider;
    use automerge::transaction::Transactable;
    use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue};
    use serde::{Deserialize, Serialize};

    const NOTE_TYPE: &str = "test.note";

    #[derive(Debug, Clone, Serialize, Deserialize)]
    enum Note {
        Star,
        Retitle(String),
    }

    impl Action for Note {
        fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
            match self {
                Note::Star => {
                    doc.put(&ROOT, "starred", ScalarValue::Boolean(true))?;
                }
                Note::Retitle(title) => {
                    doc.put(&ROOT, "title", ScalarValue::Str(title.as_str().into()))?;
                }
            }
            Ok(())
        }
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.register::<Note>(NOTE_TYPE);
        r
    }

    fn encoded(action: &Note) -> EncodedAction {
        EncodedAction::encode(NOTE_TYPE, action).unwrap()
    }

    /// One note publishing an immediate action and a prompted one, as an app
    /// would. `star_does` / `retitle_does` decide what each *performs* while
    /// leaving every id and label identical.
    fn note(star_does: &Note, retitle_does: fn(&str) -> Note) -> Vec<Resource> {
        vec![
            Resource::new("notes.note:1", "notes.note", "Harbour")
                .with_action(ActionSpec::immediate("star", "Star", NOTE_TYPE, star_does).unwrap())
                .with_action(ActionSpec::prompted(
                    "retitle",
                    "Rename",
                    "New title",
                    "title",
                    ExecutionLocus::ClientLocal,
                    move |text| encoded(&retitle_does(text)),
                )),
        ]
    }

    fn open() -> Vec<Resource> {
        note(&Note::Star, |text| Note::Retitle(text.to_string()))
    }

    fn star() -> ActionAddress {
        ActionAddress::new("notes.note:1", "star")
    }

    fn retitle() -> ActionAddress {
        ActionAddress::new("notes.note:1", "retitle")
    }

    fn perform(
        doc: &mut Doc,
        tree: &[Resource],
        address: &ActionAddress,
        argument: Option<&str>,
    ) -> Result<Dispatched, DoorError> {
        invoke(
            doc,
            tree,
            address,
            argument,
            b"caller-key",
            &registry(),
            &mut LocalTransport::new(),
        )
    }

    /// A descriptor carries no live handle: two trees whose actions *do
    /// different things* survey to equal descriptors — only the address
    /// reaches the closure, and only against a tree handed in again.
    #[test]
    fn a_descriptor_carries_nothing_that_could_act() {
        let honest = open();
        let hijacked = note(&Note::Retitle("hijacked".into()), |_| Note::Star);
        assert_eq!(
            survey(&honest),
            survey(&hijacked),
            "descriptors that differ only in what they would run must not differ"
        );

        let mut a = AutoCommit::new();
        perform(&mut a, &honest, &star(), None).unwrap();
        let mut b = AutoCommit::new();
        perform(&mut b, &hijacked, &star(), None).unwrap();

        assert_eq!(
            a.get(&ROOT, "starred").unwrap().unwrap().0.to_bool(),
            Some(true)
        );
        assert!(b.get(&ROOT, "starred").unwrap().is_none());
        assert_eq!(
            b.get(&ROOT, "title")
                .unwrap()
                .unwrap()
                .0
                .into_string()
                .unwrap(),
            "hijacked",
            "the same descriptor performed a different operation — so it holds none of it"
        );

        // Owned data: the descriptions outlive the tree they came from.
        let described = survey(&honest);
        drop(honest);
        assert_eq!(described.len(), 2);
    }

    /// A tree is per-frame presentation, so an address is re-resolved against a
    /// freshly built one. An address whose resource has gone is refused, not
    /// applied to whatever is open in its place.
    #[test]
    fn an_address_that_no_longer_resolves_is_refused_not_redirected() {
        let address = survey(&open())[0].address.clone();
        assert_eq!(address, star());

        // The note closed; a different one is open, exporting the same action id.
        let now = vec![
            Resource::new("notes.note:2", "notes.note", "Ledger").with_action(
                ActionSpec::immediate("star", "Star", NOTE_TYPE, &Note::Star).unwrap(),
            ),
        ];
        assert!(resolve(&now, &address).is_none());

        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        let err = perform(&mut doc, &now, &address, None).unwrap_err();

        assert_eq!(err, DoorError::Unknown(address));
        assert_eq!(err.code(), "unknown_action");
        assert_eq!(doc.get_heads(), before, "a refused address left no change");
        assert!(doc.get(&ROOT, "starred").unwrap().is_none());
    }

    /// Argument rules are checked both directions: a missing declared
    /// argument, and one supplied where none was declared. Neither is
    /// quietly tolerated.
    #[test]
    fn an_argument_that_does_not_match_the_declaration_is_refused() {
        let tree = open();
        let described = survey(&tree);
        let spec = described.iter().find(|d| d.address == retitle()).unwrap();
        assert!(spec.input.wants_text());
        assert!(!spec.input.wants_content());

        let mut doc = AutoCommit::new();
        let before = doc.get_heads();

        let err = perform(&mut doc, &tree, &retitle(), None).unwrap_err();
        assert_eq!(err, DoorError::MissingArgument(retitle()));
        assert_eq!(err.code(), "invalid_arguments");

        let err = perform(&mut doc, &tree, &star(), Some("Ledger")).unwrap_err();
        assert_eq!(err, DoorError::UnexpectedArgument(star()));
        assert_eq!(err.code(), "invalid_arguments");

        assert_eq!(doc.get_heads(), before, "a refused call left no change");
        assert!(doc.get(&ROOT, "title").unwrap().is_none());
        assert!(doc.get(&ROOT, "starred").unwrap().is_none());
    }

    /// An action the app never registered a decoder for cannot be run, even
    /// though its own tree offers it: the door will not dispatch what it cannot
    /// reconstruct the way a replaying peer would.
    #[test]
    fn an_unregistered_action_type_is_refused_rather_than_applied() {
        let tree = open();
        let mut doc = AutoCommit::new();
        let before = doc.get_heads();

        let err = invoke(
            &mut doc,
            &tree,
            &star(),
            None,
            b"caller-key",
            &ActionRegistry::new(),
            &mut LocalTransport::new(),
        )
        .unwrap_err();

        assert_eq!(err, DoorError::UnregisteredType(NOTE_TYPE.into()));
        assert_eq!(doc.get_heads(), before);
        assert!(doc.get(&ROOT, "starred").unwrap().is_none());
    }

    /// A provider that reads the document to build its tree, so enumeration
    /// exercises the path an app actually takes.
    struct Notes;

    impl ResourceProvider for Notes {
        fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
            let label = doc
                .get(&ROOT, "title")
                .ok()
                .flatten()
                .and_then(|(v, _)| v.into_string().ok())
                .unwrap_or_else(|| "Untitled".into());
            vec![
                Resource::new("notes.note:1", "notes.note", label).with_action(
                    ActionSpec::immediate("star", "Star", NOTE_TYPE, &Note::Star).unwrap(),
                ),
            ]
        }
    }

    /// Asking what can be done here is not doing it: building the tree and
    /// surveying it twice leaves the document byte-identical, so a caller may
    /// explore an unfamiliar app without changing it.
    #[test]
    fn enumeration_leaves_the_document_untouched() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "title", ScalarValue::Str("Harbour".into()))
            .unwrap();
        doc.commit();
        let heads = doc.get_heads();
        let saved = doc.save();

        let first = survey(&Notes.resources(&doc));
        let second = survey(&Notes.resources(&doc));

        assert_eq!(first, second);
        assert_eq!(
            first.len(),
            1,
            "the survey found something to be side-effect free about"
        );
        assert_eq!(
            first[0].resource_label, "Harbour",
            "it really read the document"
        );
        assert_eq!(doc.get_heads(), heads);
        assert_eq!(doc.save(), saved, "surveying rewrote the document");
    }

    /// A change made through the door commits under the *caller's* actor, so
    /// history names who asked rather than the app — and since the actor
    /// rides the envelope, a peer still replays to the identical document.
    #[test]
    fn a_change_through_the_door_is_attributed_to_the_caller() {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, "seed", ScalarValue::Str("s".into()))
            .unwrap();
        doc.commit();
        let app_actor = doc.get_actor().clone();
        let mut peer = AutoCommit::load(&doc.save()).unwrap();

        let caller: &[u8] = b"maren-device-key";
        let mut transport = LocalTransport::new();
        let dispatched = invoke(
            &mut doc,
            &open(),
            &star(),
            None,
            caller,
            &registry(),
            &mut transport,
        )
        .unwrap();

        assert!(dispatched.mutated_local());
        let env = dispatched.envelope().clone();
        assert_eq!(env.actor.as_deref(), Some(caller));
        let context = env.context.as_ref().expect("door supplies provenance");
        assert_eq!(context.action_label, "Star");
        assert_eq!(
            context
                .resource
                .as_ref()
                .map(|resource| resource.id.as_str()),
            Some("notes.note:1")
        );

        let heads = doc.get_heads();
        let change = doc.get_change_by_hash(&heads[0]).unwrap();
        assert_eq!(change.actor_id().to_bytes(), caller);
        assert_eq!(
            change
                .message()
                .and_then(|message| ActionContext::from_change_message(message)),
            env.context,
            "semantic provenance survives as part of the durable change"
        );
        assert_ne!(change.actor_id(), &app_actor, "not the app's own actor");
        assert_ne!(
            change.actor_id().to_bytes(),
            &env.id.actor_bytes()[..],
            "not the action id's derived actor"
        );

        replay(&mut peer, &env, &registry()).unwrap();
        assert_eq!(
            peer.get_heads(),
            heads,
            "attribution must not cost the identical replay"
        );
        assert_eq!(transport.replicated.len(), 1);
    }

    /// Resource ids carry colons, so the address separator has to be something
    /// they cannot contain; and a string that is not an address is refused
    /// rather than half-parsed into a lookup.
    #[test]
    fn an_address_survives_its_string_form() {
        let address = star();
        assert_eq!(ActionAddress::parse(&address.encode()), Some(address));
        assert_eq!(ActionAddress::parse("notes.note:1"), None);
        assert_eq!(ActionAddress::parse(&format!("{KEY_SEP}star")), None);
    }
}
