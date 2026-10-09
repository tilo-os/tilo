//! What each change of a signed document op says it is.
//!
//! A receiver decides a change before merging it ([`permissions.md`] §5.2):
//! the relay checks, without app code and without reading content, that the
//! action a change claims is granted to its author and that the change
//! writes only the storage units that action may write; a device that has
//! the app replays the action and checks that it produces exactly the change.
//! The claim is what both read. It is signed with the op, one per change,
//! and it is derived from the change itself on the author's device: the
//! history entry the executor writes into every change names the action
//! ([`tilo_actions::Did`]), and the units are listed from the record.
//!
//! [`permissions.md`]: ../../../../docs/architecture/permissions.md

use serde::{Deserialize, Serialize};
pub use tilo_actions::FrameworkChange;
use tilo_actions::{ActionContext, ActionKey, Did, HistoryOp, PathStep};

/// A storage unit, as the path from the record's root to the unit's root
/// (a `#[model(unit)]` field, or an item of one). The empty path is the
/// record itself.
pub type UnitPath = Vec<PathStep>;

/// What one Automerge change of a signed op claims to be.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeClaim {
    /// The change's hash.
    pub change: [u8; 32],
    /// What it does.
    pub action: ClaimedAction,
    /// The storage units it writes. A receiver refuses a change that writes
    /// a unit this list leaves out, and a list naming a unit its action may
    /// not write.
    pub units: Vec<UnitPath>,
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
            None => Ok(Self::whole_record_edit(kind)),
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

    /// An edit of the whole record of kind `kind`; an empty kind is the
    /// record's own.
    #[must_use]
    pub fn whole_record_edit(kind: String) -> Self {
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
        let action = match FrameworkChange::from_change_message(message) {
            Some(FrameworkChange::Native) => ClaimedAction::whole_record_edit(String::new()),
            Some(change) => ClaimedAction::Framework(change),
            None => ClaimedAction::of(
                &ActionContext::from_change_message(message).ok_or(ClaimError::Unnamed)?,
            )?,
        };
        Ok(Self {
            change: hash,
            action,
            units,
        })
    }
}

/// The changes an incremental document mutation carries, parsed but merged
/// into nothing.
///
/// # Errors
/// [`ClaimError::Unparsable`] for bytes that are not change chunks.
pub fn parse_changes(bytes: &[u8]) -> Result<Vec<automerge::Change>, ClaimError> {
    automerge::Automerge::parse_changes(bytes)
        .map_err(|error| ClaimError::Unparsable(error.to_string()))
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

/// The units a change writes in a record that keeps no unit index: the
/// record's own unit.
#[must_use]
pub fn record_unit(_change: &automerge::Change) -> Vec<UnitPath> {
    vec![Vec::new()]
}

/// The claims of `mutation`'s changes, each writing the units `units` lists
/// for it. A snapshot carries none: it is a checkpoint, which only the
/// room's pinned governor may publish.
///
/// # Errors
/// The first change that cannot be claimed, or bytes that do not parse.
pub fn mutation_claims(
    mutation: &crate::op::DocumentMutation,
    units: impl FnMut(&automerge::Change) -> Vec<UnitPath>,
) -> Result<Vec<ChangeClaim>, ClaimError> {
    match mutation {
        crate::op::DocumentMutation::Snapshot(_) => Ok(Vec::new()),
        crate::op::DocumentMutation::Incremental(bytes) => claims_of(&parse_changes(bytes)?, units),
    }
}

/// Committing changes that name what they do, where no app's executor makes
/// them: a test standing in for an app, writing a record directly.
pub trait CommitNamed {
    /// Commit the pending operations as a field edit of `field` on a record
    /// of kind `kind`.
    fn commit_edit(&mut self, kind: &str, field: &str) -> Option<automerge::ChangeHash>;
}

impl CommitNamed for automerge::AutoCommit {
    fn commit_edit(&mut self, kind: &str, field: &str) -> Option<automerge::ChangeHash> {
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
        };
        let mut options = automerge::transaction::CommitOptions::default();
        if let Some(message) = context.change_message() {
            options.set_message(message);
        }
        self.commit_with(options)
    }
}

/// A fresh record of kind `kind` whose one change sets `field` to `value`,
/// as an incremental mutation with its claims: a stand-in for an app's write.
///
/// # Errors
/// [`ClaimError::Unparsable`] when the field cannot be written.
pub fn field_edit_mutation(
    kind: &str,
    field: &str,
    value: &str,
) -> Result<(crate::op::DocumentMutation, Vec<ChangeClaim>), ClaimError> {
    use automerge::transaction::Transactable as _;
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, field, value)
        .map_err(|error| ClaimError::Unparsable(error.to_string()))?;
    doc.commit_edit(kind, field);
    let mutation = crate::op::DocumentMutation::Incremental(doc.save_after(&[]));
    let claims = mutation_claims(&mutation, record_unit)?;
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

/// The changes of an incremental payload, each naming what it does.
///
/// # Errors
/// Bytes that do not parse, or the first change that names nothing,
/// described with what it writes so its producer can be found.
pub fn named_changes(bytes: &[u8]) -> Result<Vec<automerge::Change>, String> {
    let changes = parse_changes(bytes).map_err(|error| error.to_string())?;
    for change in &changes {
        if let Err(error) = ChangeClaim::of_change(change.hash().0, change.message(), Vec::new()) {
            return Err(format!("{}: {error}", describe(change)));
        }
    }
    Ok(changes)
}

/// Whether `op` claims every change it carries, as its author must before
/// signing: a cleartext incremental document op names one claim per change,
/// in order. Ops that carry no document changes (snapshots, segments of a
/// larger mutation, other artifacts) and sealed ops, whose changes the
/// signer cannot read here, have nothing to check.
///
/// # Errors
/// The first change that names no action or has no claim, described.
pub fn check_claimed(op: &crate::op::SignedOp) -> Result<(), String> {
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
    if parse_changes(&bytes).is_err() {
        return Ok(());
    }
    let changes = named_changes(&bytes)?;
    let carried: Vec<[u8; 32]> = op.claims.iter().map(|claim| claim.change).collect();
    let made: Vec<[u8; 32]> = changes.iter().map(|change| change.hash().0).collect();
    if carried == made {
        Ok(())
    } else {
        Err(format!(
            "the op claims {} changes for the {} it carries",
            carried.len(),
            made.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn a_claim_names_the_verb_or_field_its_entry_names() {
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
            })))
            .unwrap(),
            ClaimedAction::History(HistoryOp::Undo)
        );
    }

    #[test]
    fn a_group_lists_each_distinct_member_and_refuses_what_it_cannot_list() {
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

    #[test]
    fn a_mutation_claims_each_change_and_a_snapshot_none() {
        use automerge::transaction::{CommitOptions, Transactable};
        let mut doc = automerge::AutoCommit::new();
        let before = doc.get_heads();
        for field in ["favorite", "title"] {
            doc.put(automerge::ROOT, field, true).unwrap();
            doc.commit_with(
                CommitOptions::default()
                    .with_message(context(edit(field)).change_message().unwrap()),
            );
        }
        let mutation = crate::op::DocumentMutation::Incremental(doc.save_after(&before));
        let claims = mutation_claims(&mutation, record_unit).unwrap();
        assert_eq!(claims.len(), 2);
        let made: Vec<[u8; 32]> = doc
            .get_changes(&before)
            .iter()
            .map(|change| change.hash().0)
            .collect();
        assert_eq!(
            claims.iter().map(|claim| claim.change).collect::<Vec<_>>(),
            made,
            "one claim per change, in order"
        );
        assert!(claims.iter().all(|claim| claim.units == [Vec::new()]));
        assert!(
            mutation_claims(
                &crate::op::DocumentMutation::Snapshot(doc.save()),
                record_unit
            )
            .unwrap()
            .is_empty()
        );
    }

    #[test]
    fn changes_parse_without_their_dependencies() {
        use automerge::transaction::Transactable;
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "title", "a").unwrap();
        doc.commit();
        let before = doc.get_heads();
        doc.put(automerge::ROOT, "title", "b").unwrap();
        doc.commit();
        let changes = parse_changes(&doc.save_after(&before)).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].deps(), before.as_slice());
    }

    #[test]
    fn a_change_without_an_entry_cannot_be_claimed() {
        assert_eq!(
            ChangeClaim::of_change([0; 32], None, Vec::new()),
            Err(ClaimError::Unnamed)
        );
        assert_eq!(
            ChangeClaim::of_change([0; 32], Some("free text"), Vec::new()),
            Err(ClaimError::Unnamed)
        );
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
            ClaimedAction::whole_record_edit(String::new())
        );
        let unspecific = ActionContext {
            invoker: Some(tilo_actions::ActionInvoker::actor("alice")),
            ..ActionContext::for_resource("notes/1", "note", "Note")
        };
        assert_eq!(
            claim(&unspecific.change_message().unwrap()).unwrap().action,
            ClaimedAction::whole_record_edit("note".into())
        );
    }

    fn op_of(mutation: &crate::op::DocumentMutation) -> crate::op::SignedOp {
        crate::op::SignedOp::new_unsigned(
            mutation.encode().unwrap(),
            crate::op::document_artifact_id("app", "room", "record"),
            crate::Ed25519PublicKey::from_bytes([7; 32]),
            crate::op::document_attestation_id("app", "room", "record"),
            crate::OpKind::Change,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        )
    }

    /// The origin check every signer runs: each change of a cleartext
    /// incremental op names its action and is claimed, in order.
    #[test]
    fn an_op_must_claim_every_change_it_carries() {
        use automerge::transaction::Transactable;
        let (mutation, claims) = field_edit_mutation("note", "title", "a").unwrap();
        assert_eq!(check_claimed(&op_of(&mutation).with_claims(claims)), Ok(()));
        assert!(
            check_claimed(&op_of(&mutation))
                .unwrap_err()
                .contains("claims 0 changes for the 1")
        );
        let mut raw = automerge::AutoCommit::new();
        raw.put(automerge::ROOT, "title", "a").unwrap();
        raw.commit();
        let unnamed = crate::op::DocumentMutation::Incremental(raw.save_after(&[]));
        let error = check_claimed(&op_of(&unnamed)).unwrap_err();
        assert!(error.contains("names no action"), "{error}");
        assert!(
            error.contains("\"title\""),
            "the producer is findable: {error}"
        );
        // Nothing to check: a snapshot, and bytes that are not changes.
        let snapshot = crate::op::DocumentMutation::Snapshot(raw.save());
        assert_eq!(check_claimed(&op_of(&snapshot)), Ok(()));
        let junk = crate::op::DocumentMutation::Incremental(vec![1, 2, 3]);
        assert_eq!(check_claimed(&op_of(&junk)), Ok(()));
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "not all claimed")]
    fn a_debug_build_refuses_to_sign_an_unclaimed_change() {
        let (mutation, _) = field_edit_mutation("note", "title", "a").unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let _ = crate::crypto::sign_op(&mut op_of(&mutation), &key);
    }
}
