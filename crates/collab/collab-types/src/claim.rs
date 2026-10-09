//! What each change of a signed document op says it is.
//!
//! A receiver decides a change before merging it ([`permissions.md`] §5.2):
//! the relay checks, without app code and without reading content, that the
//! action a change claims is granted to its author and that the change
//! writes only the storage units that action may write; a device that has
//! the app replays the action and checks that it produces exactly the change.
//! The claim is what both read, one per change, derived from the change
//! itself on the author's device: the
//! history entry the executor writes into every change names the action
//! ([`tilo_actions::Did`]), and the units are listed from the record.
//!
//! [`permissions.md`]: ../../../../docs/architecture/permissions.md

use serde::{Deserialize, Serialize};
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// Changes made by the framework rather than by an app's action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FrameworkChange {
    /// What a brand-new record starts with: its genesis and first content.
    Genesis,
    /// A storage migration to the model's current version.
    Migrate,
}

/// Why a change cannot be claimed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClaimError {
    /// The change carries no history entry naming what it does: something
    /// other than the executor wrote it.
    #[error("the change names no action")]
    Unnamed,
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
            None if !context.change.is_step() && context.compensation.is_none() => {
                Ok(Self::Framework(FrameworkChange::Genesis))
            }
            None => Err(ClaimError::Unnamed),
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
        let context = message
            .and_then(ActionContext::from_change_message)
            .ok_or(ClaimError::Unnamed)?;
        Ok(Self {
            change: hash,
            action: ClaimedAction::of(&context)?,
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
        .map(|change| ChangeClaim::of_change(change.hash().0, change.message(), units(change)))
        .collect()
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
        let group = Did::Group(GroupOf {
            members: members.clone(),
            count: 3,
        });
        let ClaimedAction::Group(listed) = ClaimedAction::of(&context(group)).unwrap() else {
            panic!("a group");
        };
        assert_eq!(listed.len(), 2, "each distinct member once");
        let truncated = Did::Group(GroupOf { members, count: 40 });
        assert!(matches!(
            ClaimedAction::of(&context(truncated)),
            Err(ClaimError::GroupTooLarge { .. })
        ));
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
}
