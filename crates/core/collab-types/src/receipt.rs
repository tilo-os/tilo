//! Deciding a received document op change by change, before it is merged.
//!
//! Every peer checks every incoming change. This
//! is the check that needs no app code and reads no content, so a server runs
//! it on cleartext and sealed ops alike: each change's [`ChangeClaim`] names
//! an action the record's app publishes, the author is granted that action
//! on the record, and the change writes only the storage units the action
//! may write. What the app publishes (each verb's kind, which fields offer
//! edits, which units an action may write) comes from its signed package
//! surface, never from the claim; a receiver without that surface holds the
//! change rather than trusting it.
//!
//! A change that passes is loaded as it is: no device replays it. So an
//! author granted an action can write anything inside that action's units.
//! The action is run again only before its recorded inverse is used for
//! undo ([`tilo_actions::verify_change`]).

use tilo_actions::stamp::Label;
use std::collections::{BTreeMap, BTreeSet};

use tilo_action_policy::{Acting, ActionKind, Answer, Context, Level, MemberAccess, TreePath};
use tilo_actions::{HistoryOp, PathStep};

use crate::claim::{ChangeClaim, ClaimedAction, FrameworkChange, UnitPath};

/// Whether who made a change may take an action of `kind` at `node`: the
/// change's invoker when it names one, as its whole tree bounded by the
/// op's signer ([`MemberAccess::invoked_by`]), else the signer alone. Ask is
/// a refusal on the wire.
pub type Allows<'a> = dyn Fn(Option<&Acting>, &TreePath, ActionKind) -> bool + 'a;

/// Who a received op's changes are decided as: its signer, and the invoker a
/// change names bounded by its signer.
pub trait ChangeAuthor {
    /// The answer for an action of `kind` at `node` by `invoker` (the
    /// signer itself when `None`) at `cx`. A member the signer's device may
    /// not write for ([`MemberAccess::may_write_for`]) is refused.
    fn answer(
        &self,
        invoker: Option<&Acting>,
        node: &TreePath,
        kind: ActionKind,
        cx: &Context<'_>,
    ) -> Answer;
}

impl ChangeAuthor for MemberAccess {
    fn answer(
        &self,
        invoker: Option<&Acting>,
        node: &TreePath,
        kind: ActionKind,
        cx: &Context<'_>,
    ) -> Answer {
        match invoker {
            Some(invoker) => self
                .invoked_by(invoker)
                .map_or(Answer::Deny, |access| access.answer(node, kind, cx)),
            None => Self::answer(self, node, kind, cx),
        }
    }
}

/// An authority alone holds no policy to decide an invoker by: it decides a
/// change naming none, and refuses one that names one.
impl ChangeAuthor for tilo_action_policy::Authority<'_> {
    fn answer(
        &self,
        invoker: Option<&Acting>,
        node: &TreePath,
        kind: ActionKind,
        cx: &Context<'_>,
    ) -> Answer {
        match invoker {
            None => self.decide(node, kind, cx).answer,
            Some(_) => Answer::Deny,
        }
    }
}

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
    /// The name the app publishes `action` under to the workspace's policy, where a change
    /// records its declaration id instead: a tilolang app keeps an action's id across a rename,
    /// while programs and grants name it as the catalogue publishes it. Its own id where the app
    /// publishes no other (a native app's verb is named what its id is).
    fn action_name<'a>(&'a self, _action: &str) -> Option<&'a str> {
        None
    }
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

/// The document an op writes, as the tree names it.
#[derive(Clone, Copy, Debug)]
pub struct WrittenDocument<'a> {
    pub workspace: &'a str,
    pub app: &'a str,
    pub document: &'a str,
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
        actor: tilo_action_policy::Actor,
        /// Why the appearance is refused.
        reason: String,
    },
    /// A change carries no claim, a claim names no change, or one change is
    /// claimed twice.
    #[error("the claims do not name exactly the op's changes")]
    Unclaimed,
    /// A change made for a caller carries no invocation that caller signed, or
    /// one that does not check.
    #[error("{0}")]
    SignedInvocation(String),
    /// A change's history entry names no invoker: no person, agent or caller
    /// it was made as.
    #[error("a change names no invoker")]
    NoInvoker,
    /// The person a change was made for is not granted its action under the
    /// governance the change names.
    #[error("the caller {person} is not granted {kind:?} on {node}")]
    CallerNotGranted {
        person: String,
        node: String,
        kind: ActionKind,
    },
    /// The claimed action is not one the record's app publishes.
    #[error("the document's app publishes no {0}")]
    NotPublished(String),
    /// The author is not granted the claimed action on the record.
    #[error("the author is not granted {kind:?} on {node}")]
    NotGranted { node: String, kind: ActionKind },
    /// The claim names a unit its action may not write.
    #[error("{action} may not write the unit {unit}")]
    OutsideFootprint { action: String, unit: String },
    /// The op names another app surface than the one its record's identity
    /// names, and is not the change that names a new one.
    #[error("the op names another app surface than its document's")]
    OtherSurface,
    /// A change names no action.
    #[error("a change names no action: {0}")]
    Unnamed(String),
    /// A continuation whose gesture's first entry is nowhere the receiver
    /// holds: who made it cannot be told.
    #[error("a continued change whose first entry is not here")]
    NoFirstEntry,
    /// An opened sealed change does not say what its claim says: another
    /// action, or another invoker.
    #[error("a sealed change says other than its claim: {0}")]
    Misclaimed(String),
    /// The change writes a unit its claim does not name.
    #[error("the change writes the unit {0}, which its claim does not name")]
    UnclaimedUnit(String),
    /// The invocation a change keeps does not show who asked for it: its signature or its
    /// signer's certificate does not check, or an agent acting with its own key asked under
    /// no key of its own. `code` is the identity layer's stable name for why.
    #[error("{code}: {reason}")]
    Invocation { code: &'static str, reason: String },
}

/// The node of the published tree `action` acts on in `record`.
#[must_use]
pub fn claim_node(
    document: &WrittenDocument<'_>,
    surface: &dyn ReceiptSurface,
    action: &ClaimedAction,
) -> TreePath {
    let resource = tilo_actions::resource_node(
        &TreePath::workspace(document.workspace),
        document.app,
        surface.kind(),
        &published_resource(document.document),
    );
    match action {
        ClaimedAction::Verb { action, item } => {
            let node = match item {
                Some(item) => resource.item(item.as_str()),
                None => resource,
            };
            let named = surface
                .action_name(&action.action)
                .unwrap_or(action.action.as_str());
            match surface.verb(&action.action).and_then(|(_, field)| field) {
                Some(field) => node.field(&field),
                None => node,
            }
            .action(named)
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

/// `workspace › app › kind › record [› item] › field… › field-edit`, as the API
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
    document: &WrittenDocument<'_>,
    surface: &dyn ReceiptSurface,
    action: &ClaimedAction,
) -> Result<ActionKind, ReceiptRefusal> {
    match action {
        ClaimedAction::Verb { action, .. } => {
            // A continuation names its verb alone: the record's app and kind.
            if (!action.app.is_empty() && action.app != document.app)
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

/// Whether `path` lies inside `unit`: a write to what it holds (an entry of
/// an outbox, and what is inside such an entry), never to the unit itself.
fn inside_unit(unit: &[PathStep], path: &[PathStep]) -> bool {
    let matches = |(step, want): (&PathStep, &PathStep)| match (want, step) {
        (PathStep::Item(any), PathStep::Item(_)) if any == crate::surface::ANY_ITEM => true,
        (want, step) => want == step,
    };
    let under = path.len() > unit.len() && unit.iter().zip(path).all(matches);
    let entry = matches!(unit.last(), Some(PathStep::Item(item)) if item == crate::surface::ANY_ITEM)
        && crate::surface::covers(unit, path);
    under || entry
}

fn unit_label(unit: &[PathStep]) -> String {
    if unit.is_empty() {
        return "(document)".into();
    }
    unit.iter()
        .map(|step| match step {
            PathStep::Field(key) | PathStep::Key(key) | PathStep::Item(key) => key.clone(),
            PathStep::Cell(row, column) => format!("{row}:{column}"),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// How a refusal names `action`.
#[must_use]
pub fn label(action: &ClaimedAction) -> String {
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
/// - `allows`: whether who made each change is granted an action of `kind`
///   at `node` ([`Allows`]).
///
/// # Errors
/// The first [`ReceiptRefusal`] of any change.
pub fn check_claims(
    claims: &[ChangeClaim],
    document: &WrittenDocument<'_>,
    changes: Option<&[[u8; 32]]>,
    written: Option<WrittenUnits<'_>>,
    surface: Option<&dyn ReceiptSurface>,
    allows: &Allows<'_>,
) -> Result<Receipt, ReceiptRefusal> {
    check_claimed_changes(claims, changes)?;
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
        // A history step or a framework change may touch any unit, and its
        // claim says so rather than naming the entries it queued: what it
        // really queued is left to the devices that open it, as for a
        // sealed op.
        let claims_any_unit = claim.action.members().into_iter().all(|member| {
            matches!(
                member,
                ClaimedAction::History(_) | ClaimedAction::Framework(_)
            )
        });
        // A sealed op's claim names what its action may write, not what it
        // queued. An edit whose path lies inside an entry writes there, and a
        // claim cannot tell that from a push on units alone: any other write
        // of the outbox may have put an entry in it.
        let inside = match &claim.action {
            ClaimedAction::Edit { path, .. } => {
                outboxes.iter().any(|outbox| inside_unit(outbox, path))
            }
            _ => false,
        };
        let queues = match &read {
            Some(read) => in_outbox(&read.added),
            None if claims_any_unit || inside => false,
            None => in_outbox(&claim.units),
        };
        let queued_nothing = !queues && !outboxes.is_empty();
        let mut footprint: Vec<UnitPath> = Vec::new();
        if queues || in_outbox(&claim.units) {
            footprint.extend(outboxes.iter().cloned());
        }
        for member in claim.action.members() {
            let kind = match published_kind(document, surface, member)? {
                _ if queues => ActionKind::Outside,
                ActionKind::Outside if queued_nothing => ActionKind::Undoable,
                kind => kind,
            };
            let node = claim_node(document, surface, member);
            if !allows(claim.invoker.as_deref(), &node, kind) {
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
        document: &WrittenDocument<'_>,
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

    /// Where the surfaces claims are decided against are found, when they are.
    #[must_use]
    pub fn source(&self) -> Option<&std::sync::Arc<dyn SurfaceSource>> {
        self.0.as_ref()
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
        document: &WrittenDocument<'_>,
        kind: Option<&str>,
        named: Option<&[u8; 32]>,
        claims: Vec<ChangeClaim>,
    ) -> Vec<ChangeClaim> {
        let Some(source) = &self.0 else {
            return claims;
        };
        // Each claim by the resource it is decided against ([`by_kind`]), in its place.
        // A claim over records of two kinds may write what the surface of each lets it.
        let mut footprints: BTreeMap<[u8; 32], Vec<UnitPath>> = BTreeMap::new();
        for (group_kind, group) in by_kind(kind, claims.clone()) {
            let Some(surface) = source.surface(document, group_kind.as_deref(), named) else {
                continue;
            };
            for member in group {
                let units = footprints.entry(member.change).or_default();
                for unit in surface.footprint(&member.action) {
                    if !units.contains(&unit) {
                        units.push(unit);
                    }
                }
            }
        }
        let mut claims = claims;
        for claim in &mut claims {
            if let Some(units) = footprints.remove(&claim.change) {
                claim.units = units;
            }
        }
        claims
    }
}

impl std::fmt::Debug for Surfaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.enforced() {
            "Surfaces(enforced)"
        } else {
            "Surfaces(document only)"
        })
    }
}

/// The published resource of the document `document` (a route's id), as
/// grants name it: a chain's part is its chain (`chain:<chain>`), a chunk is
/// the document it splits, and every other document is itself.
///
/// The answer comes from the id alone, which the receiver parses and
/// validates itself: a writer names no resource, so it cannot attach its
/// change to another chain's or document's grants.
#[must_use]
pub fn published_resource(document: &str) -> String {
    let mut id = crate::document::DocumentId::parse(document);
    while let Some((base, _)) = id.as_ref().and_then(crate::document::DocumentId::chunk_of) {
        id = Some(base);
    }
    match id {
        Some(id) => id.chain_part().map_or_else(
            || id.as_str().to_owned(),
            |(chain, _)| crate::document::chain_resource_id(&chain),
        ),
        None => document.to_owned(),
    }
}

/// The node of a document, `workspace › app › resource`, as a change is
/// decided without its app's surface: a part or chunk is decided as the
/// resource its tree is published under ([`published_resource`]).
#[must_use]
pub fn written_node(document: &WrittenDocument<'_>) -> TreePath {
    let resource =
        if tilo_action_policy::roles::is_workspace_moderation(document.app, document.document) {
            String::new()
        } else {
            published_resource(document.document)
        };
    let app = TreePath::workspace(document.workspace).app(document.app);
    // A record the workspace keeps apart (the vault, a mailbox's copies) is decided at its kind,
    // as every reader of it decides it ([`crate::document_node`]), whether or not an app here
    // publishes a surface for it.
    match tilo_action_policy::roles::protected_resource_kind(document.app, &resource) {
        Some(kind) => app.kind(kind),
        None => app,
    }
    .resource(resource)
}

/// Refuse changes whose invoker shows an appearance unless they come from
/// the one bot actor their author device may show other names as
/// (`persona_author`, the bot of a bot device whose installation allows `Personas`), and the appearance is valid as written.
/// The server cannot read a sealed change, so devices run this once they hold
/// the plaintext.
///
/// # Errors
/// [`ReceiptRefusal::Appearance`] for the first change that breaks the rule.
pub fn check_appearances(
    changes: &[automerge::Change],
    persona_author: Option<&tilo_action_policy::MemberId>,
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
        let actor = invoker.acting.by.clone();
        if !invoker.acting.is_bot() || persona_author != invoker.acting.member_id() {
            return Err(ReceiptRefusal::Appearance {
                actor,
                reason: "its author is not a bot allowed to post under other names".into(),
            });
        }
        appearance
            .validate()
            .map_err(|error| ReceiptRefusal::Appearance {
                actor: actor.clone(),
                reason: error.to_string(),
            })?;
    }
    Ok(())
}

/// The invoker a continuation names must be its gesture's first entry's:
/// the writer's last whole entry before it, among `changes` or in `held`, the
/// document as the receiver holds it ([`first_entry`]). With `holds` (the
/// receiver holds the document) a continuation whose first entry is found
/// nowhere is refused; without, it is left to the receivers that hold it.
///
/// # Errors
/// [`ReceiptRefusal::NoFirstEntry`] or [`ReceiptRefusal::Misclaimed`].
pub fn check_continuations(
    changes: &[automerge::Change],
    held: Option<&automerge::Automerge>,
    holds: bool,
) -> Result<(), ReceiptRefusal> {
    for change in changes {
        let Some(message) = change
            .message()
            .filter(|message| Label::of(message) == Some(Label::Continuation))
        else {
            continue;
        };
        let Some(first) = first_entry(change, changes, held) else {
            if holds {
                return Err(ReceiptRefusal::NoFirstEntry);
            }
            continue;
        };
        let named = tilo_actions::context::ContinuedAction::invoker_of_message(message);
        if named != first.invoker.map(|invoker| invoker.acting) {
            return Err(ReceiptRefusal::Misclaimed("a continued gesture".into()));
        }
    }
    Ok(())
}

/// How many of its writer's changes back a continuation's first entry may be,
/// as the executor reads it.
const MAX_CONTINUATIONS: u64 = 256;

/// The whole entry the continuation `change` continues: its writer's last
/// whole entry before it, among `changes` or in `held`; `None` when it is in
/// neither, or more than [`MAX_CONTINUATIONS`] back.
fn first_entry(
    change: &automerge::Change,
    changes: &[automerge::Change],
    held: Option<&automerge::Automerge>,
) -> Option<tilo_actions::ActionContext> {
    let actor = change.actor_id();
    for back in 1..=MAX_CONTINUATIONS {
        let seq = change.seq().checked_sub(back)?;
        let message = if let Some(earlier) = changes
            .iter()
            .find(|earlier| earlier.actor_id() == actor && earlier.seq() == seq)
        {
            earlier.message().map(str::to_owned)
        } else {
            let held = held?;
            let hash = held.hash_of_actor_seq(actor, seq)?;
            held.get_change_meta_by_hash(&hash)?
                .message
                .map(|message| message.to_string())
        }?;
        if let Some(entry) = tilo_actions::ActionContext::from_change_message(&message) {
            return Some(entry);
        }
        if Label::of(&message) != Some(Label::Continuation) {
            return None;
        }
    }
    None
}

/// Finds the app surface of a resource kind (the app's own resource for
/// `None`), or `None` while it is not here.
pub type SurfaceFor<'a> = dyn Fn(Option<&str>) -> Option<std::sync::Arc<dyn ReceiptSurface>> + 'a;

/// Decide `claims`, each against the resource of the app's surface it is
/// decided against ([`by_kind`]), as a receiver that reads none of their
/// changes does: a server deciding a sealed op, or its author checking what
/// it is about to send. `surface_for` finds the surface of a resource kind
/// (the app's own resource for `None`); one that is not here holds the op.
/// `replay` for a write admitted when it was made (see `collab_admission::decide_op_replayed`).
///
/// # Errors
/// The first [`ReceiptRefusal`] of any claim.
pub fn check_claims_by_kind(
    claims: &[ChangeClaim],
    document: &WrittenDocument<'_>,
    kind: Option<&str>,
    replay: bool,
    surface_for: &SurfaceFor<'_>,
    allows: &Allows<'_>,
) -> Result<Receipt, ReceiptRefusal> {
    check_claimed_changes(claims, None)?;
    check_store_kinds(claims, document, replay)?;
    // What each change may write, before it is split by kind: a change over records of two
    // kinds writes what the surface of each of them lets it.
    let mut allowed: BTreeMap<[u8; 32], Vec<UnitPath>> = BTreeMap::new();
    for claim in claims {
        allowed.insert(claim.change, claim_units(claim, surface_for));
    }
    let groups = by_kind(kind, claims.to_vec());
    // Every group's surface, before any claim is decided: a change over records of two kinds is
    // decided by each, and one whose surface is not here holds the whole op.
    let mut resolved = Vec::with_capacity(groups.len());
    for (group_kind, group) in groups {
        let Some(surface) = surface_for(group_kind.as_deref()) else {
            return Ok(Receipt::Hold);
        };
        resolved.push((group, surface));
    }
    for (group, surface) in resolved {
        for claim in group {
            let units = allowed.get(&claim.change).cloned().unwrap_or_default();
            let writable = AllowedUnits {
                of: &*surface,
                units: &units,
            };
            check_claims(
                std::slice::from_ref(&claim),
                document,
                None,
                None,
                Some(&writable),
                allows,
            )?;
        }
    }
    Ok(Receipt::Admit)
}

/// `surface` as `claim` is decided against, writing what the surfaces of every kind the claim
/// names may write: a change over records of two kinds may write what each of them lets it.
struct AllowedUnits<'a> {
    of: &'a dyn ReceiptSurface,
    units: &'a [UnitPath],
}

impl ReceiptSurface for AllowedUnits<'_> {
    fn kind(&self) -> &str {
        self.of.kind()
    }
    fn verb(&self, action: &str) -> Option<(ActionKind, Option<String>)> {
        self.of.verb(action)
    }
    fn editable(&self, path: &[PathStep]) -> bool {
        self.of.editable(path)
    }
    fn footprint(&self, _: &ClaimedAction) -> Vec<UnitPath> {
        self.units.to_vec()
    }
    fn outboxes(&self) -> Vec<UnitPath> {
        self.of.outboxes()
    }
}

/// What `claim` may write across the surfaces of every kind it names ([`per_kind`]): one for a
/// claim of one kind, each of a group of them.
fn claim_units(claim: &ChangeClaim, surface_for: &SurfaceFor<'_>) -> Vec<UnitPath> {
    let mut units: Vec<UnitPath> = Vec::new();
    for piece in per_kind(claim.clone()) {
        let Some(surface) = surface_for(claimed_kind(std::slice::from_ref(&piece))) else {
            continue;
        };
        for unit in surface.footprint(&piece.action) {
            if !units.contains(&unit) {
                units.push(unit);
            }
        }
    }
    units
}

/// Refuse claims that do not name exactly the op's changes: a change with
/// no claim, a claim naming no change (`changes`, when the receiver read
/// them), or one change claimed twice.
///
/// # Errors
/// [`ReceiptRefusal::Unclaimed`].
pub fn check_claimed_changes(
    claims: &[ChangeClaim],
    changes: Option<&[[u8; 32]]>,
) -> Result<(), ReceiptRefusal> {
    let claimed: BTreeSet<&[u8; 32]> = claims.iter().map(|claim| &claim.change).collect();
    if claimed.len() != claims.len()
        || changes.is_some_and(|changes| {
            changes.len() != claims.len() || changes.iter().any(|hash| !claimed.contains(hash))
        })
    {
        return Err(ReceiptRefusal::Unclaimed);
    }
    Ok(())
}

/// `claims` grouped by the resource kind of the app's surface each is decided
/// against, in the order they come: the kind its own action names, as a record
/// that holds resources of several kinds (a tilolang app's record, whose
/// tasks and projects each have their own actions) carries them. A claim that
/// names no kind (a record's creation, an undo) goes with `kind`, the record's
/// own, else with the first kind the op names.
pub fn by_kind(
    kind: Option<&str>,
    claims: Vec<ChangeClaim>,
) -> Vec<(Option<String>, Vec<ChangeClaim>)> {
    let fallback = kind
        .map(str::to_owned)
        .or_else(|| claimed_kind(&claims).map(str::to_owned));
    let mut groups: Vec<(Option<String>, Vec<ChangeClaim>)> = Vec::new();
    for claim in claims.into_iter().flat_map(per_kind) {
        let own = claimed_kind(std::slice::from_ref(&claim))
            .map(str::to_owned)
            .or_else(|| fallback.clone());
        match groups.iter_mut().find(|(group, _)| *group == own) {
            Some((_, members)) => members.push(claim),
            None => groups.push((own, vec![claim])),
        }
    }
    if groups.is_empty() {
        // An op of no change is decided as it was: against the record's resource.
        groups.push((fallback, Vec::new()));
    }
    groups
}

/// `claims` as a receiver that holds no copy of the record to read their
/// changes against (an envelope validator) decides them: it cannot tell
/// what they wrote, so it decides their grants, each change taken as
/// writing what its action may, and a receiver holding the record decides
/// their units. Nor can it tell what they queued: its outboxes are left to
/// that receiver too, which judges a change by what it queued there.
/// `unread` is the surface when the receiver did not read them; the claims
/// stay as read otherwise.
#[must_use]
pub fn unread_claims(
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

/// `claim` as one claim for each resource kind its members name, each with the members of that
/// kind and those that name none: a change over records of two kinds is decided against the
/// surface of each.
fn per_kind(claim: ChangeClaim) -> Vec<ChangeClaim> {
    let kind_of = |action: &ClaimedAction| match action {
        ClaimedAction::Verb { action, .. } if !action.kind.is_empty() => Some(action.kind.clone()),
        ClaimedAction::Edit { kind, .. } if !kind.is_empty() => Some(kind.clone()),
        _ => None,
    };
    let ClaimedAction::Group(members) = &claim.action else {
        return vec![claim];
    };
    let kinds: BTreeSet<String> = members.iter().filter_map(kind_of).collect();
    if kinds.len() < 2 {
        return vec![claim];
    }
    kinds
        .into_iter()
        .map(|kind| ChangeClaim {
            action: ClaimedAction::Group(
                members
                    .iter()
                    .filter(|member| kind_of(member).is_none_or(|own| own == kind))
                    .cloned()
                    .collect(),
            ),
            ..claim.clone()
        })
        .collect()
}

/// Refuse a change to a resource store's document that claims no kind of the records it writes:
/// the document has no kind of its own to decide it against, so a claim naming none cannot be
/// resolved without guessing. `replay` for a write admitted before changes claimed their kinds,
/// which is decided as it was admitted (`collab_admission::decide_op_replayed`).
///
/// # Errors
/// [`ReceiptRefusal::Misclaimed`].
pub fn check_store_kinds(
    claims: &[ChangeClaim],
    document: &WrittenDocument<'_>,
    replay: bool,
) -> Result<(), ReceiptRefusal> {
    if replay || !crate::is_store_document(document.document) {
        return Ok(());
    }
    if claims
        .iter()
        .any(|claim| claimed_kind(std::slice::from_ref(claim)).is_none())
    {
        return Err(ReceiptRefusal::Misclaimed(
            "a change to a store's document claims no kind of the records it writes".into(),
        ));
    }
    Ok(())
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
