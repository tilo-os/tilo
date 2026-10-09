//! Deciding a received document op change by change, before it is merged.
//!
//! Every peer checks every incoming change ([`permissions.md`] §5.2). This
//! is the check that needs no app code and reads no content, so a relay runs
//! it on cleartext and sealed ops alike: each change's [`ChangeClaim`] names
//! an action the record's app publishes, the author is granted that action
//! on the record, and the change writes only the storage units the action
//! may write. What the app publishes (each verb's kind, which fields offer
//! edits, which units an action may write) comes from its signed package
//! surface, never from the claim; a receiver without that surface holds the
//! change rather than trusting it.
//!
//! A device that has the app also replays each change
//! ([`tilo_actions::verify_change`]), which catches an author who is granted
//! an action but wrote something else inside its units.
//!
//! [`permissions.md`]: ../../../../docs/architecture/permissions.md

use std::collections::BTreeSet;

use tilo_action_policy::{ActionKind, Level, TreePath};
use tilo_actions::{HistoryOp, PathStep};

use crate::claim::{ChangeClaim, ClaimedAction, FrameworkChange, UnitPath};

/// The units a received change writes, as the record's unit index maps its
/// operations, by the change's hash.
pub type WrittenUnits<'a> = &'a dyn Fn(&[u8; 32]) -> Vec<UnitPath>;

/// What a record's app publishes, as a receiver reads it from the app's
/// signed package surface.
pub trait ReceiptSurface {
    /// The resource kind of the record.
    fn kind(&self) -> &str;
    /// The kind of the verb `action` and the field it sits under in the
    /// tree (a comment verb under the comments it writes), or `None` when
    /// the app publishes no such verb.
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)>;
    /// Whether the field at `path` (from the record's root) offers edits: it
    /// is stored and not read-only.
    fn editable(&self, path: &[PathStep]) -> bool;
    /// The storage units `action` may write.
    fn footprint(&self, action: &ClaimedAction) -> Vec<UnitPath>;
}

/// The record a document op writes, as the tree names it.
#[derive(Clone, Copy, Debug)]
pub struct Record<'a> {
    pub room: &'a str,
    pub app: &'a str,
    pub record: &'a str,
}

/// What a receiver may do with an op whose claims it checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Receipt {
    /// Every change is granted to its author and writes only what its action
    /// may write: merge it.
    Admit,
    /// The app's package surface is not here yet: keep the op, neither merge
    /// nor forward it, and decide it once the surface arrives.
    Hold,
}

/// Why a received op is refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptRefusal {
    /// A change carries no claim, a claim names no change, or one change is
    /// claimed twice.
    #[error("the claims do not name exactly the op's changes")]
    Unclaimed,
    /// The claimed action is not one the record's app publishes.
    #[error("the record's app publishes no {0}")]
    NotPublished(String),
    /// The author is not granted the claimed action on the record.
    #[error("the author is not granted {kind:?} on {node}")]
    NotGranted { node: String, kind: ActionKind },
    /// The claim names a unit its action may not write.
    #[error("{action} may not write the unit {unit}")]
    OutsideFootprint { action: String, unit: String },
    /// The change writes a unit its claim does not name.
    #[error("the change writes the unit {0}, which its claim does not name")]
    UnclaimedUnit(String),
}

/// The node of the published tree `action` acts on in `record`.
#[must_use]
pub fn claim_node(
    record: &Record<'_>,
    surface: &dyn ReceiptSurface,
    action: &ClaimedAction,
) -> TreePath {
    let kind = surface.kind();
    let resource = TreePath::room(record.room)
        .app(record.app)
        .kind(kind)
        .resource(record.record);
    match action {
        ClaimedAction::Verb { action, item } => {
            let node = match item {
                Some(item) => resource.item(item.as_str()),
                None => resource,
            };
            match surface.verb(&action.action).and_then(|(_, field)| field) {
                Some(field) => node.field(&field),
                None => node,
            }
            .action(action.action.as_str())
        }
        ClaimedAction::Edit { path, .. } => edit_node(resource, path),
        ClaimedAction::History(op) => resource.action(match op {
            HistoryOp::Redo => "redo",
            HistoryOp::Restore => "restore",
            HistoryOp::Checkpoint { .. } => "checkpoint",
            HistoryOp::Undo | HistoryOp::Migrate { .. } => "undo",
        }),
        ClaimedAction::Framework(FrameworkChange::Genesis) => resource.action("create"),
        ClaimedAction::Framework(FrameworkChange::Migrate) => resource.action("migrate"),
        // Every member derives the same empty structure, so whoever may edit
        // the record may make it; a native edit is claimed as an edit of the
        // whole record, never as itself.
        ClaimedAction::Framework(FrameworkChange::Structure | FrameworkChange::Native) => {
            resource.action("field-edit")
        }
        ClaimedAction::Framework(FrameworkChange::Transfer) => resource.action("move"),
        // A group is decided member by member; its own node is the record.
        ClaimedAction::Group(_) => resource,
    }
}

/// `room › app › kind › record [› item] › field… › field-edit`, as the door
/// decides a field edit: the collection field holding an item is not a node.
fn edit_node(resource: TreePath, path: &[PathStep]) -> TreePath {
    let mut tree = resource;
    let mut item = false;
    let mut steps = path.iter().peekable();
    while let Some(step) = steps.next() {
        tree = match step {
            PathStep::Field(_) if !item && matches!(steps.peek(), Some(PathStep::Item(_))) => tree,
            PathStep::Item(id) if !item => {
                item = true;
                tree.item(id.as_str())
            }
            PathStep::Field(key) | PathStep::Key(key) | PathStep::Item(key) => {
                tree.push(Level::Field, key.as_str())
            }
            PathStep::Cell(row, column) => tree
                .push(Level::Field, row.as_str())
                .push(Level::Field, column.as_str()),
        };
    }
    tree.action("field-edit")
}

/// The kind of `action` as the app publishes it, or why it is not published.
fn published_kind(
    record: &Record<'_>,
    surface: &dyn ReceiptSurface,
    action: &ClaimedAction,
) -> Result<ActionKind, ReceiptRefusal> {
    match action {
        ClaimedAction::Verb { action, .. } => {
            if action.app != record.app || action.kind != surface.kind() {
                return Err(ReceiptRefusal::NotPublished(format!("{action}")));
            }
            surface
                .verb(&action.action)
                .map(|(kind, _)| kind)
                .ok_or_else(|| ReceiptRefusal::NotPublished(format!("{action}")))
        }
        ClaimedAction::Edit { kind, path } => {
            if (!kind.is_empty() && kind != surface.kind()) || !surface.editable(path) {
                return Err(ReceiptRefusal::NotPublished(format!(
                    "field edit at {}",
                    unit_label(path)
                )));
            }
            Ok(ActionKind::Undoable)
        }
        ClaimedAction::History(_) | ClaimedAction::Framework(_) | ClaimedAction::Group(_) => {
            Ok(ActionKind::Undoable)
        }
    }
}

fn unit_label(unit: &[PathStep]) -> String {
    if unit.is_empty() {
        return "(record)".into();
    }
    unit.iter()
        .map(|step| match step {
            PathStep::Field(key) | PathStep::Key(key) | PathStep::Item(key) => key.clone(),
            PathStep::Cell(row, column) => format!("{row}:{column}"),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn label(action: &ClaimedAction) -> String {
    match action {
        ClaimedAction::Verb { action, .. } => action.to_string(),
        ClaimedAction::Edit { path, .. } => format!("a field edit at {}", unit_label(path)),
        ClaimedAction::Group(_) => "a group".into(),
        ClaimedAction::History(op) => format!("{op:?}"),
        ClaimedAction::Framework(change) => format!("{change:?}"),
    }
}

/// Decide the claims of a received op on `record`.
///
/// - `changes`: the hashes of the changes the payload carries, when the
///   receiver can read them (cleartext); `None` for a sealed op, whose
///   changes devices check on opening it.
/// - `written`: the units each change writes, as the record's unit index
///   maps its operations, when the receiver holds the record.
/// - `surface`: the app's package surface; `None` holds the op.
/// - `allows`: whether the author (and their delegation) is granted an
///   action of `kind` at `node`. Ask is a refusal on the wire.
///
/// # Errors
/// The first [`ReceiptRefusal`] of any change.
pub fn check_claims(
    claims: &[ChangeClaim],
    record: &Record<'_>,
    changes: Option<&[[u8; 32]]>,
    written: Option<WrittenUnits<'_>>,
    surface: Option<&dyn ReceiptSurface>,
    allows: &dyn Fn(&TreePath, ActionKind) -> bool,
) -> Result<Receipt, ReceiptRefusal> {
    let claimed: BTreeSet<&[u8; 32]> = claims.iter().map(|claim| &claim.change).collect();
    if claimed.len() != claims.len()
        || changes.is_some_and(|changes| {
            changes.len() != claims.len() || changes.iter().any(|hash| !claimed.contains(hash))
        })
    {
        return Err(ReceiptRefusal::Unclaimed);
    }
    let Some(surface) = surface else {
        return Ok(Receipt::Hold);
    };
    for claim in claims {
        let mut footprint: Vec<UnitPath> = Vec::new();
        for member in claim.action.members() {
            let kind = published_kind(record, surface, member)?;
            let node = claim_node(record, surface, member);
            if !allows(&node, kind) {
                return Err(ReceiptRefusal::NotGranted {
                    node: node.to_string(),
                    kind,
                });
            }
            footprint.extend(surface.footprint(member));
        }
        if let Some(unit) = claim.units.iter().find(|unit| !footprint.contains(unit)) {
            return Err(ReceiptRefusal::OutsideFootprint {
                action: label(&claim.action),
                unit: unit_label(unit),
            });
        }
        if let Some(written) = written
            && let Some(unit) = written(&claim.change)
                .into_iter()
                .find(|unit| !claim.units.contains(unit))
        {
            return Err(ReceiptRefusal::UnclaimedUnit(unit_label(&unit)));
        }
    }
    Ok(Receipt::Admit)
}

#[cfg(test)]
mod tests;
