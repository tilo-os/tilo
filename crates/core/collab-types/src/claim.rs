//! What each change of a signed document op says it is.
//!
//! A receiver decides a change before merging it:
//! the server checks, without app code and without reading content, that the
//! action a change claims is granted to its author and that the change
//! writes only the storage units that action may write; a device that has
//! the app replays the action and checks that it produces exactly the change.
//! The claim is what both read. It is signed with the op, one per change,
//! and it is derived from the change itself on the author's device: the
//! history entry the executor writes into every change names the action
//! ([`tilo_actions::Did`]), and the units are listed from the record.

use serde::{Deserialize, Serialize};
use tilo_action_policy::PersonId;
pub use tilo_actions::FrameworkChange;
use tilo_actions::context::ContinuedAction;
use tilo_actions::{ActionContext, ActionKey, Did, HistoryOp, PathStep};

/// A storage unit, as the path from the record's root to the unit's root
/// (a `#[model(unit)]` field, or an item of one). The empty path is the
/// record itself.
pub type UnitPath = Vec<PathStep>;

/// What one Automerge change of a signed op claims to be.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeClaim {
    /// The change's hash.
    pub change: [u8; 32],
    /// What it does.
    pub action: ClaimedAction,
    /// The storage units it writes. A receiver refuses a change that writes
    /// a unit this list leaves out, and a list naming a unit its action may
    /// not write.
    pub units: Vec<UnitPath>,
    /// Who made it, as its history entry names them, when not the person
    /// whose device signed it: the change is decided as this invoker, bounded
    /// by the signer. A receiver that cannot read a sealed change decides it
    /// by this; the devices that open it check that the change names the same.
    #[serde(with = "invoker_text")]
    pub invoker: Option<Invoker>,
    /// The invocation as its caller signed it, when the device that wrote the
    /// change ran the action for a caller on another device: the caller's own
    /// statement — its address, the hash of its arguments, its signature and
    /// certificate — and never the arguments themselves. It is the caller's
    /// bytes, kept as read and never re-signed; the op's signature covers its
    /// presence. A receiver that keeps the op sealed checks it as the caller's
    /// call; one that opens the change checks that the call the change keeps
    /// is this one (`collab_admission::check_opened_mutation`).
    #[serde(default)]
    pub call: Option<Box<tilo_actions::remote::SignedInvocation>>,
}

/// Who made a change, as a claim carries them: the tree, and the text the claim's signer wrote it
/// as. A claim read keeps that text exactly as it came, so the signature is checked over the bytes
/// that were signed, never over this build's writing of the tree.
#[derive(Clone, Debug)]
pub struct Invoker {
    acting: tilo_action_policy::Acting,
    /// The text as read; `None` for an invoker made here, written as its JSON when it is sent.
    text: Option<String>,
}

impl Invoker {
    /// `acting`, as this device names it in a claim it makes.
    #[must_use]
    pub const fn new(acting: tilo_action_policy::Acting) -> Self {
        Self { acting, text: None }
    }

    /// The text the claim carries: as read, or this device's writing of the tree.
    ///
    /// # Errors
    /// A tree that does not write.
    pub fn text(&self) -> Result<String, serde_json::Error> {
        match &self.text {
            Some(text) => Ok(text.clone()),
            None => serde_json::to_string(&self.acting),
        }
    }

    /// The invoker `text` names, keeping `text` as it is.
    ///
    /// # Errors
    /// Text that is no tree.
    pub fn read(text: String) -> Result<Self, serde_json::Error> {
        Ok(Self {
            acting: serde_json::from_str(&text)?,
            text: Some(text),
        })
    }
}

impl From<tilo_action_policy::Acting> for Invoker {
    fn from(acting: tilo_action_policy::Acting) -> Self {
        Self::new(acting)
    }
}

impl std::ops::Deref for Invoker {
    type Target = tilo_action_policy::Acting;

    fn deref(&self) -> &Self::Target {
        &self.acting
    }
}

/// Two invokers are the same who: the text each was written as decides nothing.
impl PartialEq for Invoker {
    fn eq(&self, other: &Self) -> bool {
        self.acting == other.acting
    }
}

impl Eq for Invoker {}

/// An invoker as a claim carries it: the text its signer wrote, as [`Invoker::text`] keeps it.
pub(crate) mod invoker_text {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use super::Invoker;

    /// The text `invoker` is written as.
    ///
    /// # Errors
    /// A tree that does not write.
    pub fn text(invoker: Option<&Invoker>) -> Result<Option<String>, serde_json::Error> {
        invoker.map(Invoker::text).transpose()
    }

    /// Writes `invoker` as its text.
    ///
    /// # Errors
    /// A tree that does not write.
    #[expect(
        clippy::ref_option,
        reason = "serde's `with` hands the field by reference"
    )]
    pub fn serialize<S: Serializer>(
        invoker: &Option<Invoker>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        text(invoker.as_ref())
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    /// Reads an invoker [`serialize`] wrote.
    ///
    /// # Errors
    /// Text that is no tree.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Invoker>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(Invoker::read)
            .transpose()
            .map_err(serde::de::Error::custom)
    }
}

/// What a change does, in the terms grants are written in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClaimedAction {
    /// One of the resource's verbs, on the item it names, if any.
    Verb {
        action: ActionKey,
        item: Option<String>,
    },
    /// A built-in field edit of the resource kind `kind` at `path`.
    Edit { kind: String, path: Vec<PathStep> },
    /// Several of those as one step. Every distinct action of the group is
    /// listed; a group too large to list is refused at the origin.
    Group(Vec<ClaimedAction>),
    /// Taking back or bringing back a step of the record's history.
    History(HistoryOp),
    /// A change the framework makes, which no app action produces.
    Framework(FrameworkChange),
}

/// Why a change cannot be claimed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClaimError {
    /// The change carries no history entry naming what it does: something
    /// other than the executor wrote it.
    #[error("the change names no action")]
    Unnamed,
    /// A change of a payload names no action, described so its producer
    /// can be found.
    #[error("{0}: the change names no action")]
    UnnamedChange(String),
    /// The payload's changes do not parse.
    #[error("the changes do not parse: {0}")]
    Unparsable(String),
    /// A group with more distinct members than its entry records.
    #[error("the group records {recorded} of its {count} members")]
    GroupTooLarge { recorded: usize, count: u32 },
    /// A resource store's stamp names no invoker: no person or agent it was
    /// made as.
    #[error("the change's stamp names no invoker")]
    NoInvoker,
}

impl ClaimedAction {
    /// What the change with history entry `context` does, read from the
    /// entry.
    ///
    /// # Errors
    /// Refuses an entry that names nothing, and a group whose entry does not
    /// record every member.
    pub fn of(context: &ActionContext) -> Result<Self, ClaimError> {
        let kind = context
            .resource
            .as_ref()
            .map(|resource| resource.kind.clone())
            .unwrap_or_default();
        let item = context.item.as_ref().map(|item| item.id.clone());
        match &context.what {
            Some(what) => Self::of_did(what, &kind, item),
            // An entry that names no finer action (a window's own commit of
            // what its widgets wrote) is an edit of the whole record.
            None => Ok(Self::whole_resource_edit(kind)),
        }
    }

    fn of_did(what: &Did, kind: &str, item: Option<String>) -> Result<Self, ClaimError> {
        Ok(match what {
            Did::Edit(edit) => Self::Edit {
                kind: kind.to_owned(),
                path: edit.path.clone(),
            },
            Did::Verb(verb) => Self::Verb {
                action: verb.action.clone(),
                item,
            },
            Did::Group(group) if !group.distinct.is_empty() => {
                let mut members = Vec::new();
                for member in &group.distinct {
                    let member = Self::of_did(member, kind, None)?;
                    if !members.contains(&member) {
                        members.push(member);
                    }
                }
                Self::Group(members)
            }
            Did::Group(group) => {
                let mut members = Vec::new();
                for member in &group.members {
                    let member = Self::of_did(member, kind, None)?;
                    if !members.contains(&member) {
                        members.push(member);
                    }
                }
                let recorded = group.members.len();
                if usize::try_from(group.count).unwrap_or(usize::MAX) > recorded
                    && group.shared().is_none()
                {
                    return Err(ClaimError::GroupTooLarge {
                        recorded,
                        count: group.count,
                    });
                }
                Self::Group(members)
            }
            Did::History(step) if matches!(step.op, HistoryOp::Migrate { .. }) => {
                Self::Framework(FrameworkChange::Migrate)
            }
            Did::History(step) => Self::History(step.op.clone()),
        })
    }

    /// What a continuation message (the next keystroke of a gesture) says
    /// its change is: what the entry it continues claims.
    #[must_use]
    pub fn of_continuation(message: &str) -> Option<Self> {
        Some(match ContinuedAction::of_message(message)? {
            ContinuedAction::Edit { kind, path } => Self::Edit { kind, path },
            ContinuedAction::Verb { action, item } => Self::Verb { action, item },
        })
    }

    /// An edit of the whole record of kind `kind`. An empty kind is the record's own: a native
    /// window's edit of the record, or an entry that names no resource, is decided against the
    /// app's own resource. A resource store's change is left unnamed here and the store that
    /// wrote it names the kinds of the records it wrote ([`name_record_kinds`]); one still
    /// unnamed is refused ([`crate::check_claims_by_kind`]).
    #[must_use]
    pub fn whole_resource_edit(kind: String) -> Self {
        Self::Edit {
            kind,
            path: Vec::new(),
        }
    }

    /// Every verb or field edit the action is made of: itself, or the
    /// members of a group.
    pub fn members(&self) -> Vec<&Self> {
        match self {
            Self::Group(members) => members.iter().flat_map(Self::members).collect(),
            other => vec![other],
        }
    }

    /// What the app that made it calls each member, with the item it acted
    /// on: a verb's key, an edit's field path (`title`, `rows.cells`), a
    /// history step's name. A change the framework makes is no app's
    /// action and names nothing.
    #[must_use]
    pub fn named(&self) -> Vec<(String, Option<String>)> {
        self.members()
            .into_iter()
            .filter_map(|member| match member {
                Self::Verb { action, item } => Some((action.action.clone(), item.clone())),
                Self::Edit { path, .. } => {
                    let mut fields = Vec::new();
                    let mut item = None;
                    for step in path {
                        match step {
                            PathStep::Field(field) => fields.push(field.as_str()),
                            PathStep::Item(id) | PathStep::Key(id) | PathStep::Cell(id, _) => {
                                item.get_or_insert_with(|| id.clone());
                            }
                        }
                    }
                    Some((fields.join("."), item))
                }
                Self::History(op) => Some((
                    match op {
                        HistoryOp::Undo | HistoryOp::Migrate { .. } => "undo",
                        HistoryOp::Redo => "redo",
                        HistoryOp::Restore => "restore",
                        HistoryOp::Checkpoint { .. } => "checkpoint",
                    }
                    .to_owned(),
                    None,
                )),
                Self::Group(_) | Self::Framework(_) => None,
            })
            .collect()
    }
}

impl ChangeClaim {
    /// The claim of the change `hash` whose commit message is `message`,
    /// writing `units`.
    ///
    /// # Errors
    /// As [`ClaimedAction::of`]; a change with no history entry is
    /// [`ClaimError::Unnamed`].
    pub fn of_change(
        hash: [u8; 32],
        message: Option<&str>,
        units: Vec<UnitPath>,
    ) -> Result<Self, ClaimError> {
        let message = message.ok_or(ClaimError::Unnamed)?;
        let mut stamp: Option<crate::Stamp> = None;
        // The call the change keeps, when a host ran it for a caller elsewhere:
        // the caller's own signed invocation, never its re-signing.
        let mut call: Option<Box<tilo_actions::remote::SignedInvocation>> = None;
        let action = match FrameworkChange::from_change_message(message) {
            // A window's own commit names no finer action than the record whole, and no kind:
            // the record is the app's own, which only its own document says, so the claim
            // names none and is decided against that resource, never a record's first.
            Some(FrameworkChange::Native) => ClaimedAction::whole_resource_edit(String::new()),
            Some(change) => ClaimedAction::Framework(change),
            None if message.starts_with(tilo_actions::context::CONTINUATION_PREFIX) => {
                ClaimedAction::of_continuation(message).ok_or(ClaimError::Unnamed)?
            }
            None => {
                // A resource store's change names its verb in its stamp, and the store
                // that takes it judges that verb's write-set against it: the claim is an
                // edit of the records it writes, whose kinds only the store that keeps
                // them reads ([`name_record_kinds`]). The stamp is read first: it reads as
                // an action context too, and one that names no invoker is refused as every
                // change that names no one, which reading it as an entry would lose.
                if let Some(parsed) = crate::Stamp::of_message(message) {
                    stamp_invoker(&parsed)?;
                    call = parsed
                        .signed
                        .as_deref()
                        .map(|signed| Box::new(signed.signed.clone()));
                    stamp = Some(parsed);
                    ClaimedAction::whole_resource_edit(String::new())
                } else if let Some(context) = ActionContext::from_change_message(message) {
                    call = context
                        .signed
                        .as_deref()
                        .map(|signed| Box::new(signed.signed.clone()));
                    ClaimedAction::of(&context)?
                } else {
                    return Err(ClaimError::Unnamed);
                }
            }
        };
        Ok(Self {
            change: hash,
            action,
            units,
            invoker: match &stamp {
                Some(stamp) => Some(stamp_invoker(stamp)?),
                None => invoker_of_message(message),
            }
            .map(Invoker::new),
            call,
        })
    }

    /// [`Self::of_change`], for a change of a history a device of `imported`'s imported, when
    /// it is one: a change that names no invoker is that person's, as the import step said
    /// (`collab_admission::decide_opened`). Without one it is refused as any is.
    ///
    /// # Errors
    /// As [`Self::of_change`].
    pub fn of_imported_change(
        hash: [u8; 32],
        message: Option<&str>,
        units: Vec<UnitPath>,
        imported: Option<&tilo_action_policy::Acting>,
    ) -> Result<Self, ClaimError> {
        match (Self::of_change(hash, message, units.clone()), imported) {
            (Err(ClaimError::NoInvoker), Some(person)) => Ok(Self {
                change: hash,
                action: ClaimedAction::whole_resource_edit(String::new()),
                units,
                invoker: Some(Invoker::new(person.clone())),
                call: None,
            }),
            (claim, _) => claim,
        }
    }
}

/// The invoker a resource store's [`crate::Stamp`] names: a person, or an
/// agent or automation acting for one (as its `acted_for` says). A stamp names
/// none: the change says who made it or it is refused.
///
/// # Errors
/// [`ClaimError::NoInvoker`] for a stamp that names no invoker.
pub fn stamp_invoker(stamp: &crate::Stamp) -> Result<tilo_action_policy::Acting, ClaimError> {
    let acting = stamp
        .invoked_by()
        .map(tilo_action_policy::Acting::from)
        .ok_or(ClaimError::NoInvoker)?;
    Ok(match &stamp.acted_for {
        // The agent works for the person its stamp names, so every receiver
        // decides it as that person's agent, never as a bare name.
        Some(person) => acting.acting_for(tilo_action_policy::Acting::person(person.clone())),
        None => acting,
    })
}

/// Who made the change whose commit message is `message`, as its history
/// entry names them, when not the person whose device signed it.
fn invoker_of_message(message: &str) -> Option<tilo_action_policy::Acting> {
    // A continuation names its gesture's invoker in its claim token, as its
    // first entry names it ([`crate::receipt::check_continuations`]).
    if message.starts_with(tilo_actions::context::CONTINUATION_PREFIX) {
        return ContinuedAction::invoker_of_message(message);
    }
    if let Some(context) = ActionContext::from_change_message(message) {
        return context.invoker.map(|invoker| invoker.acting);
    }
    crate::Stamp::of_message(message).and_then(|stamp| stamp_invoker(&stamp).ok())
}

/// Who made `change` ([`invoker_of_message`]).
#[must_use]
pub fn invoker_of(change: &automerge::Change) -> Option<tilo_action_policy::Acting> {
    change.message().and_then(invoker_of_message)
}

/// The claim of every change in `changes`, each writing the units `units`
/// lists for it: what the author's device signs with its op.
///
/// # Errors
/// The first change that cannot be claimed.
pub fn claims_of(
    changes: &[automerge::Change],
    mut units: impl FnMut(&automerge::Change) -> Vec<UnitPath>,
) -> Result<Vec<ChangeClaim>, ClaimError> {
    changes
        .iter()
        .map(|change| {
            ChangeClaim::of_change(change.hash().0, change.message(), units(change)).map_err(
                |error| match error {
                    ClaimError::Unnamed => ClaimError::UnnamedChange(describe(change)),
                    other => other,
                },
            )
        })
        .collect()
}

/// What each record of a workspace's resource store is kept under at its document's root: a key
/// that starts with this, holding the record's kind under [`STORE_RECORD_KIND`].
pub const STORE_RECORD: &str = "r:";
/// The key of a stored record's kind, inside its key at the document's root.
pub const STORE_RECORD_KIND: &str = "k";

/// The kinds of the records each change writes, by the change's hash: what a change of a
/// workspace's resource store claims, an edit of its records whole of these kinds.
pub type RecordKinds = std::collections::BTreeMap<[u8; 32], std::collections::BTreeSet<String>>;

/// The kinds of the records each of `changes` writes in `record`, a document of a workspace's
/// resource store, held as `held` before them (`None` for one held nowhere yet): read from the
/// records themselves as they stand at the change's own earlier changes and after it, so a change
/// of a record's kind names both, and every holder reads the same whatever else it holds. A
/// content document holds one resource, whose kind its id names. A change whose records name no
/// kind, or whose earlier changes are not here, is left out.
#[must_use]
pub fn store_record_kinds(
    record: &str,
    held: Option<&automerge::Automerge>,
    changes: &[automerge::Change],
) -> RecordKinds {
    use automerge::ReadDoc as _;
    let mut kinds = RecordKinds::new();
    if !crate::is_store_document(record) {
        return kinds;
    }
    if let Some(kind) = content_kind(record) {
        for change in changes {
            kinds.insert(change.hash().0, std::iter::once(kind.to_owned()).collect());
        }
        return kinds;
    }
    let kind_at = |doc: &automerge::Automerge, key: &str| {
        let (_, map) = doc.get(automerge::ROOT, key).ok().flatten()?;
        let (value, _) = doc.get(&map, STORE_RECORD_KIND).ok().flatten()?;
        value.as_str().map(str::to_owned)
    };
    let mut held = held.cloned().unwrap_or_else(automerge::Automerge::new);
    for change in changes {
        let Ok(mut doc) = held.fork_at(change.deps()) else {
            continue;
        };
        let before = doc.clone();
        let heads = doc.get_heads();
        if doc.apply_changes([change.clone()]).is_err() {
            continue;
        }
        let records: std::collections::BTreeSet<String> = doc
            .diff(&heads, &doc.get_heads())
            .into_iter()
            .filter_map(|patch| match (patch.path.first(), patch.action) {
                (Some((_, automerge::Prop::Map(key))), _) => Some(key.clone()),
                (
                    None,
                    automerge::PatchAction::PutMap { key, .. }
                    | automerge::PatchAction::DeleteMap { key },
                ) => Some(key),
                _ => None,
            })
            .filter(|key| key.starts_with(STORE_RECORD))
            .collect();
        let written: std::collections::BTreeSet<String> = records
            .iter()
            .flat_map(|key| [kind_at(&before, key), kind_at(&doc, key)])
            .flatten()
            .collect();
        if !written.is_empty() {
            kinds.insert(change.hash().0, written);
        }
        // Later changes may build on this one.
        tilo_failure::ignored!(
            "a change that does not apply here names nothing",
            held.apply_changes([change.clone()]).ok()
        );
    }
    kinds
}

/// The kinds `action`, a change's claim, says it edits whole: one for an edit of a record whole
/// of a kind, each of a group of them. `None` for any other claim.
#[must_use]
pub fn whole_edit_kinds(action: &ClaimedAction) -> Option<std::collections::BTreeSet<String>> {
    let whole = |action: &ClaimedAction| match action {
        ClaimedAction::Edit { kind, path } if path.is_empty() => Some(kind.clone()),
        _ => None,
    };
    match action {
        ClaimedAction::Group(members) => members.iter().map(whole).collect(),
        single => whole(single).map(|kind| std::iter::once(kind).collect()),
    }
    .map(|kinds: std::collections::BTreeSet<String>| {
        kinds.into_iter().filter(|kind| !kind.is_empty()).collect()
    })
}

/// The kinds of the records `change` makes in a resource store's document, read from the change
/// alone: each record it starts at the document's root, with the kind it gives it. What it writes
/// in a record made before is read only by whoever holds the record ([`store_record_kinds`]).
#[must_use]
pub fn kinds_made_in(change: &automerge::Change) -> std::collections::BTreeSet<String> {
    use automerge::legacy::{Key, ObjectId, OpId, OpType};
    let expanded = change.decode();
    let made: Vec<OpId> = expanded
        .operations
        .iter()
        .zip(expanded.start_op.get()..)
        .filter(|(op, _)| {
            op.obj == ObjectId::Root
                && matches!(op.action, OpType::Make(automerge::ObjType::Map))
                && matches!(&op.key, Key::Map(key) if key.starts_with(STORE_RECORD))
        })
        .map(|(_, counter)| OpId(counter, expanded.actor_id.clone()))
        .collect();
    expanded
        .operations
        .iter()
        .filter_map(|op| match (&op.obj, &op.key, &op.action) {
            (
                ObjectId::Id(record),
                Key::Map(key),
                OpType::Put(automerge::ScalarValue::Str(kind)),
            ) if key == STORE_RECORD_KIND && made.contains(record) => Some(kind.to_string()),
            _ => None,
        })
        .collect()
}

/// The kind of the one resource a content document of a resource store holds, as its id names it
/// (`<readers>/<kind>/<id>~<n>`); `None` for a document of resources.
fn content_kind(record: &str) -> Option<&str> {
    let (rest, number) = record.rsplit_once('~')?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let (_, rest) = rest.split_once('/')?;
    // A kind names no `/`; an id may.
    rest.split_once('/').map(|(kind, _)| kind)
}

/// Name in each claim of `claims` that edits its record whole the kinds `kinds` holds for its
/// change ([`store_record_kinds`]).
pub fn name_record_kinds(claims: &mut [ChangeClaim], kinds: &RecordKinds) {
    let whole = ClaimedAction::whole_resource_edit(String::new());
    for claim in claims {
        let Some(written) = kinds.get(&claim.change).filter(|_| claim.action == whole) else {
            continue;
        };
        let mut edits: Vec<ClaimedAction> = written
            .iter()
            .map(|kind| ClaimedAction::whole_resource_edit(kind.clone()))
            .collect();
        claim.action = if edits.len() == 1 {
            edits.remove(0)
        } else {
            ClaimedAction::Group(edits)
        };
    }
}

/// The units a change writes in a record that keeps no unit index: the
/// record's own unit.
#[must_use]
pub fn record_unit(_change: &automerge::Change) -> Vec<UnitPath> {
    vec![Vec::new()]
}

/// Committing changes that name what they do, where no app's executor makes
/// them: a test in place of an app, writing a record directly.
pub trait CommitNamed {
    /// Commit the pending operations as a field edit of `field` on a record
    /// of kind `kind`, made by the person whose root key is `person`: every
    /// change names who made it, and a receiver refuses one that names nobody.
    fn commit_edit(
        &mut self,
        kind: &str,
        field: &str,
        person: &[u8; 32],
    ) -> Option<automerge::ChangeHash>;
}

impl CommitNamed for automerge::AutoCommit {
    fn commit_edit(
        &mut self,
        kind: &str,
        field: &str,
        person: &[u8; 32],
    ) -> Option<automerge::ChangeHash> {
        let context = ActionContext {
            what: Some(Did::Edit(tilo_actions::FieldEdit {
                ty: kind.to_owned(),
                field: field.to_owned(),
                path: vec![PathStep::Field(field.to_owned())],
                op: tilo_actions::EditOp::Set,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            })),
            ..ActionContext::for_resource(kind, kind, kind)
        }
        .with_invoker(tilo_actions::ActionInvoker::person(PersonId::from_root(
            *person,
        )));
        let mut options = automerge::transaction::CommitOptions::default();
        if let Some(message) = context.change_message() {
            options.set_message(message);
        }
        self.commit_with(options)
    }
}

/// A fresh record of kind `kind` whose one change sets `field` to `value`, made by the person
/// whose root key is `person`, as an incremental mutation with its claims: a stand-in for an
/// app's write.
///
/// # Errors
/// [`ClaimError::Unparsable`] when the field cannot be written.
pub fn field_edit_mutation(
    kind: &str,
    field: &str,
    value: &str,
    person: &[u8; 32],
) -> Result<(crate::op::DocumentMutation, Vec<ChangeClaim>), ClaimError> {
    use automerge::transaction::Transactable as _;
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, field, value)
        .map_err(|error| ClaimError::Unparsable(error.to_string()))?;
    doc.commit_edit(kind, field, person);
    let mutation = crate::op::DocumentMutation::Incremental(doc.save_after(&[]));
    let claims = claims_of(&doc.get_changes(&[]), record_unit)?;
    Ok((mutation, claims))
}

/// A change as its producer would recognise it: who wrote it, after what,
/// its first operations and its message.
fn describe(change: &automerge::Change) -> String {
    let written: Vec<String> = change
        .decode()
        .operations
        .iter()
        .take(4)
        .map(|op| format!("{:?} {:?}", op.key, op.action))
        .collect();
    format!(
        "change {} by {} after {} deps ({} ops: {}), message {:?}",
        change.hash(),
        change.actor_id(),
        change.deps().len(),
        change.len(),
        written.join(", "),
        change.message()
    )
}

/// Whether each of `changes` names what it does.
///
/// # Errors
/// The first change that names nothing, described with what it writes so its
/// producer can be found.
pub fn check_named(changes: &[automerge::Change]) -> Result<(), String> {
    for change in changes {
        if let Err(error) = ChangeClaim::of_change(change.hash().0, change.message(), Vec::new()) {
            return Err(format!("{}: {error}", describe(change)));
        }
    }
    Ok(())
}

/// Whether every change `op` carries names its action, as its author must
/// ensure before signing: each change of a cleartext incremental op names
/// it in its message, and the envelope carries no claims (only a sealed op
/// does, which the signer cannot read here and the SDK checks before
/// sealing). Snapshots, segments and other artifacts have nothing to check.
/// `parse` reads the payload's changes: where the bytes came from decides
/// whether this process may read them itself.
///
/// # Errors
/// The first change that names no action or has no claim, described.
pub fn check_claimed<E>(
    op: &crate::op::SignedOp,
    parse: impl FnOnce(&[u8]) -> Result<Vec<automerge::Change>, E>,
) -> Result<(), String> {
    if op.encryption.is_some() {
        return Ok(());
    }
    let Ok(crate::op::DocumentMutation::Incremental(bytes)) =
        crate::op::DocumentMutation::decode(&op.payload)
    else {
        return Ok(());
    };
    // Bytes that are not changes are refused by every receiver; this check
    // is about changes that do not say what they are.
    let Ok(changes) = parse(&bytes) else {
        return Ok(());
    };
    check_named(&changes)?;
    // A cleartext change is its own claim; a second copy in the envelope
    // would be a second source of truth, and receivers refuse it.
    if op.claims.is_empty() {
        Ok(())
    } else {
        Err("a cleartext op carries claims; its changes name their actions".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resource_store_change_is_claimed_by_its_stamp_and_one_naming_no_invoker_is_refused() {
        let person = "ab".repeat(32);
        let stamp = crate::Stamp {
            verb: "$origin".into(),
            invoker: crate::Stamp::by(&tilo_action_policy::Actor::Member(
                tilo_action_policy::MemberId::Person(
                    tilo_action_policy::PersonId::parse(&person).unwrap(),
                ),
            )),
            invocation: "i1".into(),
            ..crate::Stamp::default()
        };
        let claim = ChangeClaim::of_change([7; 32], Some(&stamp.message()), Vec::new()).unwrap();
        assert_eq!(
            claim.action,
            ClaimedAction::whole_resource_edit(String::new())
        );
        assert_eq!(
            claim
                .invoker
                .and_then(|invoker| invoker.person_id().map(str::to_owned)),
            Some(person),
            "the stamp's invoker is the claim's"
        );
        let unnamed = crate::Stamp {
            verb: "$origin".into(),
            invocation: "i1".into(),
            ..crate::Stamp::default()
        };
        assert_eq!(
            ChangeClaim::of_change([7; 32], Some(&unnamed.message()), Vec::new()),
            Err(ClaimError::NoInvoker),
            "a change that names no invoker is refused"
        );
        assert_eq!(
            ChangeClaim::of_change([7; 32], Some(r#"{"verb":"add"}"#), Vec::new()),
            Err(ClaimError::Unnamed),
            "a message that names a verb and nothing else is no stamp"
        );
    }
    use tilo_actions::{EditOp, FieldEdit, GroupOf, HistoryStep, VerbCall};

    fn edit(field: &str) -> Did {
        Did::Edit(FieldEdit {
            ty: "snippet".into(),
            field: field.into(),
            path: vec![
                PathStep::Field("snippets".into()),
                PathStep::Item("s1".into()),
                PathStep::Field(field.into()),
            ],
            op: EditOp::Set,
            old: None,
            new: None,
            args: std::collections::BTreeMap::new(),
        })
    }

    fn context(what: Did) -> ActionContext {
        ActionContext {
            what: Some(what),
            ..ActionContext::for_resource("snippets.library/1", "snippets.library", "Snippets")
        }
    }

    tilo_test_env::scenario! {
        /// How the changes an op carries are claimed from the entries written into them, each case on changes of its own.
        fn a_change_is_claimed_for_the_action_its_entry_names;

        phase a_claim_names_the_verb_or_field_its_entry_names;
        {
            let delete = ActionKey::new("so.tilo.snippets", "snippets.library", "delete");
            let verb = Did::Verb(VerbCall::new(delete.clone(), []));
            let mut entry = context(verb);
            entry.item = Some(tilo_actions::ActionItem {
                id: "s1".into(),
                label: "Deploy".into(),
            });
            assert_eq!(
                ClaimedAction::of(&entry).unwrap(),
                ClaimedAction::Verb {
                    action: delete,
                    item: Some("s1".into())
                }
            );
            let ClaimedAction::Edit { kind, path } =
                ClaimedAction::of(&context(edit("favorite"))).unwrap()
            else {
                panic!("a field edit");
            };
            assert_eq!(kind, "snippets.library");
            assert_eq!(path.last(), Some(&PathStep::Field("favorite".into())));
            assert_eq!(
                ClaimedAction::of(&context(Did::History(HistoryStep {
                    op: HistoryOp::Undo,
                    steps: 1,
                    of: None,
                    takes_back: Vec::new(),
                })))
                .unwrap(),
                ClaimedAction::History(HistoryOp::Undo)
            );
        }

        phase a_group_lists_each_distinct_member_and_refuses_what_it_cannot_list;
        {
            let members = vec![edit("favorite"), edit("favorite"), edit("title")];
            // Forty members, more than the entry keeps by value: every distinct
            // action is still listed.
            let group = Did::group(members.iter().cycle().take(40).cloned());
            let ClaimedAction::Group(listed) = ClaimedAction::of(&context(group)).unwrap() else {
                panic!("a group");
            };
            assert_eq!(listed.len(), 2, "each distinct member once");
            let truncated = Did::Group(GroupOf {
                members,
                count: 40,
                distinct: Vec::new(),
            });
            assert!(matches!(
                ClaimedAction::of(&context(truncated)),
                Err(ClaimError::GroupTooLarge { .. })
            ));
        }

        phase a_change_without_an_entry_cannot_be_claimed;
        {
            assert_eq!(
                ChangeClaim::of_change([0; 32], None, Vec::new()),
                Err(ClaimError::Unnamed)
            );
            assert_eq!(
                ChangeClaim::of_change([0; 32], Some("free text"), Vec::new()),
                Err(ClaimError::Unnamed)
            );
        }

        phase every_change_an_op_carries_must_name_its_action;
        // The origin check every signer runs: each change of a cleartext
        // incremental op names its action and is claimed, in order.
        {
            use automerge::transaction::Transactable;
            let (mutation, claims) = field_edit_mutation("note", "title", "a", &[1; 32]).unwrap();
            assert_eq!(check_claimed(&op_of(&mutation), automerge::Automerge::parse_changes), Ok(()));
            assert!(
                check_claimed(&op_of(&mutation).with_claims(claims), automerge::Automerge::parse_changes)
                    .unwrap_err()
                    .contains("carries claims"),
                "a cleartext op names its actions in its changes only"
            );
            let mut raw = automerge::AutoCommit::new();
            raw.put(automerge::ROOT, "title", "a").unwrap();
            raw.commit();
            let unnamed = crate::op::DocumentMutation::Incremental(raw.save_after(&[]));
            let error = check_claimed(&op_of(&unnamed), automerge::Automerge::parse_changes).unwrap_err();
            assert!(error.contains("names no action"), "{error}");
            assert!(
                error.contains("\"title\""),
                "the producer is findable: {error}"
            );
            // Nothing to check: a snapshot, and bytes that are not changes.
            let snapshot = crate::op::DocumentMutation::Snapshot(raw.save());
            assert_eq!(check_claimed(&op_of(&snapshot), automerge::Automerge::parse_changes), Ok(()));
            let junk = crate::op::DocumentMutation::Incremental(vec![1, 2, 3]);
            assert_eq!(check_claimed(&op_of(&junk), automerge::Automerge::parse_changes), Ok(()));
        }

        phase a_compact_group_entry_claims_each_member_at_its_path;
        // A grid gesture's entry, written as compactly as the history keeps it
        // (a group action stating its members' shared path once, the members'
        // paths left to that action), claims each member at its full path.
        {
            let path = vec![
                PathStep::Field("sheets".into()),
                PathStep::Item("s1".into()),
                PathStep::Field("cells".into()),
            ];
            let member = |op| {
                Did::Edit(FieldEdit {
                    ty: "sheet".into(),
                    field: "cells".into(),
                    path: path.clone(),
                    op,
                    old: None,
                    new: None,
                    args: std::collections::BTreeMap::new(),
                })
            };
            let mut entry =
                ActionContext::for_resource("sheets.workbook", "sheets.workbook", "Workbook");
            entry.what = Some(Did::group([
                member(EditOp::InsertRow),
                member(EditOp::SetCell),
            ]));
            entry.action = Some(tilo_actions::EncodedAction {
                type_id: "sheets.workbook+group".into(),
                payload: br#"{"at":5,"ops":[{"action":"field-edit","args":{"at":2,"id":"r2","op":"insert-row"}},{"action":"field-edit","args":{"column":"c0","op":"set-cell","row":"r2","value":{"raw":"2"}}}],"path":[{"field":"sheets"},{"item":"s1"},{"field":"cells"}]}"#.to_vec(),
            });
            let message = entry.change_message().unwrap();
            assert_eq!(
                tilo_actions::compact::decode(
                    message
                        .strip_prefix(tilo_actions::CHANGE_MESSAGE_PREFIX)
                        .unwrap()
                )
                .unwrap()
                .matches("\"s1\"")
                .count(),
                1,
                "the path once: {message}"
            );
            let claim = ChangeClaim::of_change([1; 32], Some(&message), Vec::new()).unwrap();
            assert_eq!(
                claim.action,
                ClaimedAction::Group(vec![ClaimedAction::Edit {
                    kind: "sheets.workbook".into(),
                    path,
                }])
            );
        }

        phase a_continuation_claims_what_its_gestures_first_entry_claims;
        {
            let claim = |message: &str| {
                ChangeClaim::of_change([1; 32], Some(message), Vec::new()).map(|claim| claim.action)
            };
            let gesture = tilo_actions::ActionId(uuid::Uuid::from_u128(7));
            let typed = |what: Option<Did>, payload: &str| ActionContext {
                what,
                action: Some(tilo_actions::EncodedAction {
                    type_id: "sheets.type".into(),
                    payload: payload.as_bytes().to_vec(),
                }),
                ..ActionContext::for_resource("r1", "sheets.workbook", "Plan").in_gesture(Some(gesture))
            };
            let verb = ActionKey::new("sheets", "sheets.workbook", "type-text");
            for what in [None, Some(Did::Verb(tilo_actions::VerbCall::new(verb, [])))] {
                let first = typed(what.clone(), r#""a""#);
                let next = typed(what, r#""ab""#).continuation_of(&first).unwrap();
                assert_eq!(claim(&next), claim(&first.change_message().unwrap()), "{next}");
            }
            assert_eq!(claim("tilo.cont/3 g nonsense 0 1 x"), Err(ClaimError::Unnamed));
        }
    }

    #[test]
    fn framework_changes_and_unspecific_entries_are_claimed_for_what_they_are() {
        let claim = |message: &str| ChangeClaim::of_change([0; 32], Some(message), Vec::new());
        assert_eq!(
            claim(&FrameworkChange::Genesis.change_message())
                .unwrap()
                .action,
            ClaimedAction::Framework(FrameworkChange::Genesis)
        );
        assert_eq!(
            claim(&FrameworkChange::Transfer.change_message())
                .unwrap()
                .action,
            ClaimedAction::Framework(FrameworkChange::Transfer)
        );
        // A window's own commit names no finer action than the whole record.
        assert_eq!(
            claim(&FrameworkChange::Native.change_message())
                .unwrap()
                .action,
            ClaimedAction::whole_resource_edit(String::new())
        );
        let unspecific = ActionContext {
            invoker: Some(tilo_actions::ActionInvoker::person("alice")),
            ..ActionContext::for_resource("notes/1", "note", "Note")
        };
        assert_eq!(
            claim(&unspecific.change_message().unwrap()).unwrap().action,
            ClaimedAction::whole_resource_edit("note".into())
        );
    }

    fn op_of(mutation: &crate::op::DocumentMutation) -> crate::op::SignedOp {
        crate::op::SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            crate::op::document_artifact_id("app", "workspace", "record"),
            crate::Ed25519PublicKey::from_bytes([7; 32]),
            crate::op::document_attestation_id("app", "workspace", "record"),
            crate::OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        )
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "not all claimed")]
    fn a_debug_build_refuses_to_sign_an_unnamed_change() {
        use automerge::transaction::Transactable;
        let mut raw = automerge::AutoCommit::new();
        raw.put(automerge::ROOT, "title", "a").unwrap();
        raw.commit();
        let unnamed = crate::op::DocumentMutation::Incremental(raw.save_after(&[]));
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let _ = crate::crypto::sign_op(&mut op_of(&unnamed), &key);
    }
}
