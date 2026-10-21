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

/// What a received change writes, as the record's unit index maps its
/// operations, by the change's hash.
pub type WrittenUnits<'a> = &'a dyn Fn(&[u8; 32]) -> crate::units::Written;

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
    /// The storage units of the record's outboxes: what a change queues
    /// there is sent outside the document once it is delivered. None for a
    /// record that queues nothing.
    fn outboxes(&self) -> Vec<UnitPath> {
        Vec::new()
    }
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
    /// A change shows another name than its author's, and its author is not
    /// the bot allowed to, or the name is not valid.
    #[error("{actor} may not show the appearance it carries: {reason}")]
    Appearance {
        /// The invoker the change names.
        actor: String,
        /// Why the appearance is refused.
        reason: String,
    },
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
    /// The op names another app surface than the one its record's identity
    /// names, and is not the change that names a new one.
    #[error("the op names another app surface than its record's")]
    OtherSurface,
    /// A change names no action.
    #[error("a change names no action: {0}")]
    Unnamed(String),
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
        ClaimedAction::Framework(FrameworkChange::Upgrade) => resource.action("upgrade"),
        // A run marker only names steps already decided one by one.
        ClaimedAction::Framework(FrameworkChange::Run) => resource.action("checkpoint"),
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
            // A continuation names its verb alone: the record's app and kind.
            if (!action.app.is_empty() && action.app != record.app)
                || (!action.kind.is_empty() && action.kind != surface.kind())
            {
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
    let outboxes = surface.outboxes();
    for claim in claims {
        // What reaches outside a record that queues is what a change queued
        // in its outboxes: one that queued something reaches outside
        // whatever its action, and one that queued nothing did not, though
        // its action may queue. A receiver that read the change goes by the
        // entries it put into an outbox: an outbox made empty, as a new
        // record makes it, or an entry settled by whoever ran it, queues
        // nothing. One deciding a sealed op goes by its claim, and the
        // devices that open it check what it really queued.
        let in_outbox = |units: &[UnitPath]| {
            units.iter().any(|unit| {
                outboxes
                    .iter()
                    .any(|outbox| crate::surface::covers(outbox, unit))
            })
        };
        let read = written.map(|written| written(&claim.change));
        let queues = match &read {
            Some(read) => in_outbox(&read.added),
            None => in_outbox(&claim.units),
        };
        let queued_nothing = !queues && !outboxes.is_empty();
        let mut footprint: Vec<UnitPath> = Vec::new();
        if queues || in_outbox(&claim.units) {
            footprint.extend(outboxes.iter().cloned());
        }
        for member in claim.action.members() {
            let kind = match published_kind(record, surface, member)? {
                _ if queues => ActionKind::Outside,
                ActionKind::Outside if queued_nothing => ActionKind::Undoable,
                kind => kind,
            };
            let node = claim_node(record, surface, member);
            if !allows(&node, kind) {
                return Err(ReceiptRefusal::NotGranted {
                    node: node.to_string(),
                    kind,
                });
            }
            footprint.extend(surface.footprint(member));
        }
        if let Some(unit) = claim.units.iter().find(|unit| {
            !footprint
                .iter()
                .any(|allowed| crate::surface::covers(allowed, unit))
        }) {
            return Err(ReceiptRefusal::OutsideFootprint {
                action: label(&claim.action),
                unit: unit_label(unit),
            });
        }
        // An outbox is no unit a claim must name: what a change queued there
        // is judged above, as reaching outside, by what it wrote.
        if let Some(read) = read
            && let Some(unit) = read
                .units
                .into_iter()
                .find(|unit| !claim.units.contains(unit) && !in_outbox(std::slice::from_ref(unit)))
        {
            return Err(ReceiptRefusal::UnclaimedUnit(unit_label(&unit)));
        }
    }
    Ok(Receipt::Admit)
}

/// Where a receiver finds the package surface of a record's app.
pub trait SurfaceSource: Send + Sync {
    /// The surface `named` (the content hash an op names) for records of
    /// kind `kind` (the app's own resource when `None`), or `None` while it
    /// is not here: the op is held until it is.
    fn surface(
        &self,
        record: &Record<'_>,
        kind: Option<&str>,
        named: Option<&[u8; 32]>,
    ) -> Option<std::sync::Arc<dyn ReceiptSurface>>;
}

/// The surfaces a receiver decides claims with, or none: before a receiver
/// has a surface source it decides a change by the record alone (an edit of
/// the record), as it did before changes named their actions.
#[derive(Clone, Default)]
pub struct Surfaces(Option<std::sync::Arc<dyn SurfaceSource>>);

fn process_surfaces() -> &'static std::sync::Mutex<Surfaces> {
    static PROCESS: std::sync::OnceLock<std::sync::Mutex<Surfaces>> = std::sync::OnceLock::new();
    PROCESS.get_or_init(std::sync::Mutex::default)
}

impl Surfaces {
    /// The surfaces every receiver of this process decides with, as its host
    /// set them once at startup ([`Self::set_process`]); none before.
    #[must_use]
    pub fn process() -> Self {
        process_surfaces()
            .lock()
            .map(|surfaces| surfaces.clone())
            .unwrap_or_default()
    }

    /// Decide every change this process receives with `surfaces`.
    pub fn set_process(surfaces: Self) {
        if let Ok(mut current) = process_surfaces().lock() {
            *current = surfaces;
        }
    }

    /// Decide claims with `source`.
    #[must_use]
    pub fn from_source(source: std::sync::Arc<dyn SurfaceSource>) -> Self {
        Self(Some(source))
    }

    /// Whether claims are decided against app surfaces.
    #[must_use]
    pub fn enforced(&self) -> bool {
        self.0.is_some()
    }

    /// `claims` naming the units each action may write by `record`'s app
    /// surface (`named`, for records of kind `kind`), as a receiver that
    /// cannot read a sealed change checks them: a claim left on the whole
    /// record is refused wherever the action may only write some of its
    /// units (an entry settled in an outbox, a comment). Unchanged while the
    /// surface is not here.
    #[must_use]
    pub fn with_footprints(
        &self,
        record: &Record<'_>,
        kind: Option<&str>,
        named: Option<&[u8; 32]>,
        mut claims: Vec<ChangeClaim>,
    ) -> Vec<ChangeClaim> {
        let Some(source) = &self.0 else {
            return claims;
        };
        let kind = kind.or_else(|| claimed_kind(&claims)).map(str::to_owned);
        let Some(surface) = source.surface(record, kind.as_deref(), named) else {
            return claims;
        };
        for claim in &mut claims {
            claim.units = surface.footprint(&claim.action);
        }
        claims
    }
}

impl std::fmt::Debug for Surfaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.enforced() {
            "Surfaces(enforced)"
        } else {
            "Surfaces(record only)"
        })
    }
}

/// The node of a record, `room › app › record`, as a change is decided
/// without its app's surface.
#[must_use]
pub fn record_node(record: &Record<'_>) -> TreePath {
    TreePath::room(record.room)
        .app(record.app)
        .resource(record.record)
}

/// Decide a received op on `record`: a compaction or redaction as the
/// room's governance action, a change by its claims ([`check_claims`])
/// against the app surface `surfaces` finds, or, with no surface source, as
/// an edit of the record.
///
/// # Errors
/// The first [`ReceiptRefusal`].
pub fn decide_op(
    op: &crate::op::SignedOp,
    record: &Record<'_>,
    kind: Option<&str>,
    held: Option<automerge::AutoCommit>,
    surfaces: &Surfaces,
    allows: &dyn Fn(&TreePath, ActionKind) -> bool,
) -> Result<Receipt, ReceiptRefusal> {
    decide_op_pinned(op, record, kind, held, None, surfaces, allows)
}

/// Refuse changes whose invoker shows an appearance unless they come from
/// the one bot actor their author device may show other names as
/// (`persona_author`, `bot:<installation>` for a bot device whose
/// installation allows `Personas`), and the appearance is valid as written.
/// The relay cannot read a sealed change, so devices run this once they hold
/// the plaintext, like every other replay check.
///
/// # Errors
/// [`ReceiptRefusal::Appearance`] for the first change that breaks the rule.
pub fn check_appearances(
    changes: &[automerge::Change],
    persona_author: Option<&str>,
) -> Result<(), ReceiptRefusal> {
    for change in changes {
        let Some(invoker) = change
            .message()
            .and_then(tilo_actions::ActionContext::from_change_message)
            .and_then(|context| context.invoker)
        else {
            continue;
        };
        let Some(appearance) = &invoker.appearance else {
            continue;
        };
        if invoker.kind != tilo_actions::ActorKind::Bot
            || persona_author != Some(invoker.actor.as_str())
        {
            return Err(ReceiptRefusal::Appearance {
                actor: invoker.actor.clone(),
                reason: "its author is not a bot allowed to post under other names".into(),
            });
        }
        appearance
            .validate()
            .map_err(|error| ReceiptRefusal::Appearance {
                actor: invoker.actor.clone(),
                reason: error.to_string(),
            })?;
    }
    Ok(())
}

/// [`decide_op`] for a record whose identity names `pinned`, the surface
/// its changes are decided against: an op naming another surface is
/// refused unless it is the change that names a new one for the record
/// ([`FrameworkChange::Upgrade`]).
///
/// # Errors
/// The first [`ReceiptRefusal`].
pub fn decide_op_pinned(
    op: &crate::op::SignedOp,
    record: &Record<'_>,
    kind: Option<&str>,
    held: Option<automerge::AutoCommit>,
    pinned: Option<[u8; 32]>,
    surfaces: &Surfaces,
    allows: &dyn Fn(&TreePath, ActionKind) -> bool,
) -> Result<Receipt, ReceiptRefusal> {
    use tilo_action_policy::roles;
    let governed = |action: &str| {
        let node = roles::governance_action(record.room, action);
        if allows(&node, roles::GOVERNANCE_KIND) {
            Ok(Receipt::Admit)
        } else {
            Err(ReceiptRefusal::NotGranted {
                node: node.to_string(),
                kind: roles::GOVERNANCE_KIND,
            })
        }
    };
    match op.kind {
        crate::role::OpKind::Compaction => governed(roles::governance::COMPACT),
        crate::role::OpKind::Redaction => governed(roles::governance::REDACT),
        crate::role::OpKind::Change => {
            // An op of an app that publishes no surface (and of a record that
            // names none) is decided as an edit of the record; one naming a
            // surface is decided against it, and held until it is here.
            let Some(source) = surfaces
                .0
                .as_ref()
                .filter(|_| op.surface.is_some() || pinned.is_some())
            else {
                let node = record_node(record);
                return if allows(&node, ActionKind::Undoable) {
                    Ok(Receipt::Admit)
                } else {
                    Err(ReceiptRefusal::NotGranted {
                        node: node.to_string(),
                        kind: ActionKind::Undoable,
                    })
                };
            };
            let surface_for = |claims: &[ChangeClaim]| {
                source.surface(
                    record,
                    kind.or_else(|| claimed_kind(claims)),
                    op.surface.as_ref(),
                )
            };
            let Some(changes) = cleartext_changes(op)? else {
                // A sealed op: the relay decides the claims its envelope
                // carries; devices check the changes once they open them.
                let surface = surface_for(&op.claims);
                return check_claims(&op.claims, record, None, None, surface.as_deref(), allows);
            };
            // A cleartext change names its action in itself, under the op's
            // signature: the envelope carries no second copy.
            if !op.claims.is_empty() {
                return Err(ReceiptRefusal::Unclaimed);
            }
            // The units each change writes, read from the record as this
            // receiver holds it.
            let reads = held.is_some();
            let written = held.and_then(|held| crate::units::units_by_change(held, &changes));
            let claims = changes
                .iter()
                .map(|change| {
                    let units = written
                        .as_ref()
                        .and_then(|written| written.written.get(&change.hash().0))
                        .map_or_else(|| vec![Vec::new()], |written| written.units.clone());
                    ChangeClaim::of_change(change.hash().0, change.message(), units)
                        .map_err(|error| ReceiptRefusal::Unnamed(error.to_string()))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let hashes: Vec<[u8; 32]> = changes.iter().map(|change| change.hash().0).collect();
            if let Some(pinned) = pinned
                && op.surface != Some(pinned)
                && !claims
                    .iter()
                    .any(|claim| claim.action == ClaimedAction::Framework(FrameworkChange::Upgrade))
            {
                return Err(ReceiptRefusal::OtherSurface);
            }
            let surface = surface_for(&claims);
            let claims = unread_claims(surface.as_deref().filter(|_| !reads), claims);
            // What this receiver read from the changes itself: what says a
            // change queued nothing in an outbox. A record whose index does
            // not name its outboxes (indexed before they were units), or a
            // change this receiver could not apply, is taken as queueing.
            let outboxes = surface
                .as_deref()
                .map(ReceiptSurface::outboxes)
                .unwrap_or_default();
            let told_apart = written
                .as_ref()
                .is_some_and(|written| outboxes.iter().all(|outbox| written.indexes(outbox)));
            let read = |hash: &[u8; 32]| match written
                .as_ref()
                .and_then(|written| written.written.get(hash))
            {
                Some(read) if told_apart => read.clone(),
                read => crate::units::Written {
                    units: read.map(|read| read.units.clone()).unwrap_or_default(),
                    added: outboxes.clone(),
                },
            };
            let read: Option<WrittenUnits<'_>> =
                written.is_some().then_some(&read as WrittenUnits<'_>);
            check_claims(
                &claims,
                record,
                Some(&hashes),
                read,
                surface.as_deref(),
                allows,
            )
        }
    }
}

/// `claims` as a receiver that holds no copy of the record to read their
/// changes against (an envelope validator) decides them: it cannot tell
/// what they wrote, so it decides their grants, each change taken as
/// writing what its action may, and a receiver holding the record decides
/// their units. Nor can it tell what they queued: its outboxes are left to
/// that receiver too, which judges a change by what it queued there.
/// `unread` is the surface when the receiver did not read them; the claims
/// stay as read otherwise.
fn unread_claims(
    unread: Option<&dyn ReceiptSurface>,
    claims: Vec<ChangeClaim>,
) -> Vec<ChangeClaim> {
    let Some(surface) = unread else {
        return claims;
    };
    let outboxes = surface.outboxes();
    claims
        .into_iter()
        .map(|claim| ChangeClaim {
            units: surface
                .footprint(&claim.action)
                .into_iter()
                .filter(|unit| {
                    !outboxes
                        .iter()
                        .any(|outbox| crate::surface::covers(outbox, unit))
                })
                .collect(),
            ..claim
        })
        .collect()
}

/// The resource kind the claims name, for a receiver that does not know the
/// record's: the kind of the first verb or field edit that names one.
fn claimed_kind(claims: &[ChangeClaim]) -> Option<&str> {
    claims
        .iter()
        .flat_map(|claim| claim.action.members())
        .find_map(|action| match action {
            ClaimedAction::Verb { action, .. } if !action.kind.is_empty() => {
                Some(action.kind.as_str())
            }
            ClaimedAction::Edit { kind, .. } if !kind.is_empty() => Some(kind.as_str()),
            _ => None,
        })
}

/// The hashes of the changes a cleartext op carries; `None` for a sealed
/// op or one segment of a larger mutation, whose changes are checked once
/// readable.
fn cleartext_changes(
    op: &crate::op::SignedOp,
) -> Result<Option<Vec<automerge::Change>>, ReceiptRefusal> {
    if op.encryption.is_some() {
        return Ok(None);
    }
    match crate::op::DocumentMutation::decode(&op.payload) {
        Ok(crate::op::DocumentMutation::Incremental(bytes)) => crate::claim::parse_changes(&bytes)
            .map(Some)
            .map_err(|_| ReceiptRefusal::Unclaimed),
        // A snapshot is a checkpoint; segments are decided once whole.
        Ok(crate::op::DocumentMutation::Snapshot(_)) | Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests;
