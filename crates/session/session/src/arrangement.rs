//! The person's arrangement as their own typed record (shell.md §2,
//! workspace-model.md §9): what is open where, kept in their own record
//! (`records: .own`, [`own_arrangement::record_id`]) with history and undo,
//! synced to their devices that use the same shell, and shared with no other
//! member.
//!
//! Today's desk in [`DesktopTree`] is the arrangement's first reading: the
//! migration ([`Strips::from_tree`]) keeps every workspace, row, strip,
//! place and id. From then on each arranging action ([`ArrangementAction`])
//! is one change of the record, attributed to whoever asked for it — the
//! person's own input included — and undone as that one step.
//!
//! One rule is fixed (shell.md §1): nothing moves or takes focus under an
//! ongoing gesture or while the person types. An arranging action that
//! arrives then waits here and runs when the gesture or the typing ends; it
//! is never dropped.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};

use collab_types::presence::UserId;
use serde_json::Value as Json;
use tilo_actions::{
    ActionArgs, ActionContract, ActionParam, ActionSpec, EncodedAction, ExecutionMode,
    OperationKind, ParamType, ResourceSpec, ResultSpec,
};
use tilo_desktop_model::own_arrangement::{self, Place, SHELL, Strips};
use tilo_desktop_model::{DesktopTree, GroupId, StripId, TabId, WindowWidth};
use tilo_desktop_surface::DesktopHandle;
use ui::automerge::{AutoCommit, ChangeHash};
use ui::state::UndoLog;

/// The operation type every arranging action carries.
pub const OPERATION: &str = "tilo.shell.arranging";
/// The resource the person's arrangement is published as.
pub const RESOURCE: &str = "shell:arrangement";
/// The resource its history is published as: its undo and redo.
pub const RECORD_RESOURCE: &str = "shell:arrangement-record";

/// An arranging action: one change of the person's arrangement.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "do", rename_all = "kebab-case")]
pub enum ArrangementAction {
    /// Open `shows` (an app's id) into the strip `strip`, after the place
    /// `after` or last, under the new place's id `place`.
    OpenInto {
        strip: u64,
        #[serde(default)]
        after: Option<u64>,
        place: u64,
        shows: String,
        #[serde(default)]
        view: String,
        #[serde(default)]
        record: String,
        /// The window's state, as the desktop model keeps it; absent for a
        /// place this build opens by itself.
        #[serde(default)]
        state: Json,
    },
    /// Move the place `place` into the row `row`, before the place `before`
    /// or last.
    Move {
        place: u64,
        row: u64,
        #[serde(default)]
        before: Option<u64>,
    },
    /// Resize the strip `strip` to `permille` of its row's width.
    Resize { strip: u64, permille: u32 },
    /// Close the place `place`; its window goes with it.
    Close { place: u64 },
    /// Pin the place `place` (`pinned`), so an agent's arrangement leaves it
    /// alone (shell.md §5), or unpin it.
    Pin { place: u64, pinned: bool },
}

impl ArrangementAction {
    /// The action as an operation's payload decodes, when it is one.
    #[must_use]
    pub fn decode(type_id: &str, payload: &[u8]) -> Option<Self> {
        (type_id == OPERATION)
            .then(|| serde_json::from_slice(payload).ok())
            .flatten()
    }
}

/// What the person is in the middle of, as shell.md §1's one fixed rule
/// reads it: nothing moves under a gesture or while they type.
pub use tilo_compositor::Underway as Busy;

/// The person's arrangement, and the changes waiting for the person to
/// finish what they are doing.
pub struct Arrangement {
    /// The person whose record it is.
    person: String,
    /// The record as [`own_arrangement`] keeps it.
    doc: AutoCommit,
    /// The record's history: each arranging action is one undo step of
    /// whoever made it.
    undo: UndoLog,
    /// The desk whose windows the arrangement places.
    handle: DesktopHandle,
    /// Actions that arrived under a gesture or while the person typed: they
    /// run in order once it ends.
    waiting: RefCell<VecDeque<(String, ArrangementAction)>>,
    /// What the desk last had, as the arrangement read it, so a desk that
    /// changed on its own (a window opened, a title set) is taken in once.
    seen: RefCell<Option<Strips>>,
    /// The desk's revision `seen` was read at: a desk that has not moved
    /// since costs nothing to look at again.
    seen_at: Cell<u64>,
}

impl Arrangement {
    /// `person`'s arrangement in `doc`: their desk as `keep` shows it,
    /// migrated into the record when the record holds none yet, with
    /// `records` naming the record each place shows.
    #[must_use]
    pub fn new(
        person: &str,
        mut doc: AutoCommit,
        handle: DesktopHandle,
        keep: &DesktopTree,
        records: &BTreeMap<TabId, String>,
    ) -> Self {
        // Nothing kept: today's desk is the arrangement's first reading.
        let mut kept = own_arrangement::read(&doc);
        if kept.is_empty() {
            kept = Strips::from_tree(keep, records);
            if let Err(error) = own_arrangement::write(&mut doc, &kept) {
                tracing::warn!(%error, "the desk could not be migrated into the person's record");
            }
        }
        // A place whose window was this device's own surface is not open any
        // more: its app was not running when the session stopped, and it
        // takes its place again when it runs. The rows, strips, widths and
        // canvas places stay.
        let live: Vec<TabId> = kept
            .places()
            .filter(|place| place.state.get("Foreign").is_some())
            .map(|place| place.id)
            .collect();
        if !live.is_empty() {
            let mut pruned = kept.clone();
            for place in live {
                pruned.close_place(place);
            }
            if let Err(error) = own_arrangement::write(&mut doc, &pruned) {
                tracing::warn!(%error, "the closed windows could not leave the record");
            }
        }
        let undo = UndoLog::new(&mut doc);
        Self {
            person: person.to_owned(),
            doc,
            undo,
            handle,
            waiting: RefCell::new(VecDeque::new()),
            seen: RefCell::new(None),
            seen_at: Cell::new(0),
        }
    }

    /// The arrangement as this shell reads it.
    #[must_use]
    pub fn strips(&self) -> Strips {
        own_arrangement::read(&self.doc)
    }

    /// The record itself, for this person's devices and its history.
    #[must_use]
    pub const fn doc(&self) -> &AutoCommit {
        &self.doc
    }

    /// The record's heads: they move with each change here or merged from
    /// another of the person's devices.
    pub fn heads(&mut self) -> Vec<ChangeHash> {
        self.doc.get_heads()
    }

    /// Keep the record under `state`, replacing the one before.
    ///
    /// # Errors
    /// The state directory could not be written.
    pub fn save(&mut self, state: &std::path::Path) -> std::io::Result<()> {
        save_at(state, &mut self.doc)
    }

    /// The id the record is kept under: `person`'s own, which only their
    /// devices read and change.
    #[must_use]
    pub fn record_id(&self) -> String {
        own_arrangement::record_id(&self.person)
    }

    /// The person whose arrangement it is.
    #[must_use]
    pub fn person(&self) -> &str {
        &self.person
    }

    /// Carry out `action` as `asked`'s: one change of the record, attributed
    /// to them and undone as one step. Under an ongoing gesture or while the
    /// person types it waits ([`Self::waiting`]) and runs when they end.
    ///
    /// Returns whether it ran now.
    ///
    /// # Errors
    /// Why the action names something the arrangement does not hold.
    pub fn ask(
        &mut self,
        asked: &str,
        action: ArrangementAction,
        busy: Busy,
    ) -> Result<bool, String> {
        if busy.holds() {
            self.waiting
                .borrow_mut()
                .push_back((asked.to_owned(), action));
            return Ok(false);
        }
        self.write(asked, &action, true)?;
        Ok(true)
    }

    /// Run one of the actions that waited, now the person's gesture or typing
    /// ended. One per call, so a queue never holds a frame.
    ///
    /// # Errors
    /// Why the action could not be made.
    pub fn release(&mut self) -> Result<(), String> {
        let Some((asked, action)) = self.waiting.borrow_mut().pop_front() else {
            return Ok(());
        };
        self.write(&asked, &action, true)
    }

    /// How many actions wait for the person to finish.
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.waiting.borrow().len()
    }

    /// The last step of the record's history, as who made it and what it
    /// did: what a history view names it by.
    #[must_use]
    pub fn last_step(&mut self) -> Option<(String, String)> {
        let entry = self
            .undo
            .next_undo_as(&self.doc, Some(&user(&self.person)))?;
        let step = entry.step?;
        // Who asked for it: the invoker the step records, else the person
        // whose record it is.
        let who = step
            .invoker
            .map_or_else(|| self.person.clone(), |invoker| invoker.actor);
        let what = match step.what {
            Some(tilo_actions::Did::Verb(call)) => call.action.action,
            _ => String::new(),
        };
        Some((who, what))
    }

    /// The record's document, shared as the person's own: what their other
    /// devices read the arrangement from.
    #[must_use]
    pub fn shared_record(&self) -> &AutoCommit {
        &self.doc
    }

    /// Take in `doc`, the record as this person's other device wrote it
    /// last: what that device opened, moved and closed, merged entry by
    /// entry. The desk is laid out again from it.
    pub fn adopt(&mut self, doc: &AutoCommit) {
        // The other device's heads, as its record has them.
        let mut other = doc.clone();
        if other.get_heads() == self.doc.get_heads() {
            return;
        }
        if let Err(error) = own_arrangement::merge_into(&mut self.doc, &other) {
            tracing::warn!(%error, "the person's arrangement did not merge");
            return;
        }
        // What the other device changed is not this desk doing something of
        // its own: the history takes the other device's changes as its own
        // baseline.
        self.undo = UndoLog::new(&mut self.doc);
        self.place();
    }

    /// The person's own change, made as their gesture's end says: their own
    /// input never waits for itself.
    ///
    /// # Errors
    /// Why the action names something the arrangement does not hold.
    pub fn ask_person(&mut self, action: &ArrangementAction) -> Result<(), String> {
        let person = self.person.clone();
        self.write(&person, action, true)
    }

    /// Undo `person`'s last change of the arrangement. Whether there was one.
    pub fn undo(&mut self, person: &str) -> bool {
        let stepped = self
            .undo
            .undo_as(&mut self.doc, Some(&user(person)), None, None)
            .is_some();
        if stepped {
            self.place();
        }
        stepped
    }

    /// Redo `person`'s last undone change of the arrangement.
    pub fn redo(&mut self, person: &str) -> bool {
        let stepped = self
            .undo
            .redo_as(&mut self.doc, Some(&user(person)), None, None)
            .is_some();
        if stepped {
            self.place();
        }
        stepped
    }

    /// Make `action` one change of the record: written, attributed to `asked`
    /// as its undo step, and (with `place`) laid out on the desk.
    fn write(
        &mut self,
        asked: &str,
        action: &ArrangementAction,
        place: bool,
    ) -> Result<(), String> {
        let before: Vec<ChangeHash> = self.doc.get_heads();
        let mut strips = own_arrangement::read(&self.doc);
        // An agent arranges the person's desk like the person does, except
        // where they pinned something: a pinned place is theirs to move
        // (shell.md §5's default, "not what they pinned").
        let by_the_person = asked == self.person;
        if !change(&mut strips, action, by_the_person)? {
            return Ok(());
        }
        own_arrangement::write(&mut self.doc, &strips).map_err(|error| error.to_string())?;
        self.doc.commit_with(
            ui::automerge::transaction::CommitOptions::default()
                .with_message(Self::step_message(asked, action)),
        );
        // The step is the person's to take back, whoever acted for them; who
        // it was is the step's invoker, which history names.
        let person = self.person.clone();
        self.undo.record_committed_action_authored(
            &mut self.doc,
            before,
            None,
            None,
            Some(user(&person)),
        );
        if place {
            self.place();
        }
        Ok(())
    }

    /// The message the step `action` commits under: what it did, and who
    /// asked for it.
    fn step_message(asked: &str, action: &ArrangementAction) -> String {
        let context = tilo_actions::ActionContext {
            invoker: Some(tilo_actions::ActionInvoker::actor(asked.to_owned())),
            what: Some(tilo_actions::Did::Verb(tilo_actions::VerbCall::new(
                tilo_actions::ActionKey::new("so.tilo.session", "arrangement", verb(action)),
                std::iter::empty(),
            ))),
            ..tilo_actions::ActionContext::default()
        };
        context
            .change_message()
            .unwrap_or_else(|| format!("arrange: {}", verb(action)))
    }

    /// Take in what the desk did on its own since the last pass — a row or
    /// strip made, a window opened, a width dragged, a tab closed — as
    /// arranging actions of the person's: their own input goes through the
    /// same actions an agent's does, so it is attributed to them and undone
    /// as one step.
    pub fn observe(&mut self) {
        // A desk that has not moved since it was last read is the same desk:
        // there is nothing to take in.
        let at = self.handle.revision();
        if at == self.seen_at.replace(at) {
            return;
        }
        let became = self
            .handle
            .read(|tree| Strips::from_tree(tree, &BTreeMap::new()));
        if self
            .seen
            .borrow()
            .as_ref()
            .is_some_and(|seen| same_places(seen, &became))
        {
            return;
        }
        let was = self.seen.borrow_mut().take();
        let Some(was) = was else {
            *self.seen.borrow_mut() = Some(became);
            return;
        };
        *self.seen.borrow_mut() = Some(became.clone());
        let before: Vec<ChangeHash> = self.doc.get_heads();
        let mut strips = own_arrangement::read(&self.doc);
        let mut changed = adopt_structure(&mut strips, &became);
        for action in actions_between(&was, &became) {
            match change(&mut strips, &action, true) {
                Ok(made) => changed |= made,
                Err(why) => tracing::warn!(why, "the desk's own change could not be kept"),
            }
        }
        if !changed {
            return;
        }
        if let Err(error) = own_arrangement::write(&mut self.doc, &strips) {
            tracing::warn!(%error, "the desk's own change could not be kept");
            return;
        }
        self.doc.commit_with(
            ui::automerge::transaction::CommitOptions::default()
                .with_message("arrange: the desk's own change"),
        );
        let person = self.person.clone();
        self.undo.record_committed_action_authored(
            &mut self.doc,
            before,
            None,
            None,
            Some(user(&person)),
        );
    }

    /// Lay the desk out from the record now, as the session starts: the
    /// arrangement's rows, strips and places become the desk's. What the
    /// desk shows from here on is the record's, so what it does next is
    /// taken in from this point.
    pub fn place_now(&mut self) {
        self.place();
        *self.seen.borrow_mut() = Some(self.strips());
    }

    /// Lay the desk out from the record. The workspaces and their names are
    /// the desk's own, and so is which of them is in front on this device;
    /// the rows, strips and places are the record's.
    fn place(&self) {
        let (names, shown) = self.handle.read(|tree| {
            let names: BTreeMap<u64, String> = tree
                .workspaces
                .iter()
                .map(|workspace| (workspace.id.0, workspace.name.clone()))
                .collect();
            let shown = (
                tree.active,
                tree.active_strip.clone(),
                tree.active_group.clone(),
                tree.outputs.clone(),
            );
            (names, shown)
        });
        let strips = own_arrangement::read(&self.doc);
        let mut tree = strips.to_tree(|id| names.get(&id.0).cloned());
        // Which workspace and strip are in front is this device's, and
        // survives the record's change.
        let (active, active_strip, active_group, outputs) = shown;
        if tree
            .workspaces
            .iter()
            .any(|workspace| workspace.id == active)
        {
            tree.active = active;
            tree.active_strip = active_strip
                .into_iter()
                .filter(|(_, strip)| {
                    tree.workspaces
                        .iter()
                        .any(|workspace| workspace.strips.iter().any(|row| row.id == *strip))
                })
                .collect();
            tree.active_group = active_group;
            tree.outputs = outputs;
        }
        self.handle.replace_tree(tree);
        // What the desk shows now is the record's: it is not the desk doing
        // something of its own.
        *self.seen.borrow_mut() = Some(self.strips());
    }
}

/// One change of `strips`, as `action` names it. `by_the_person` is whether
/// the person's own input asked for it: an agent's move leaves a place the
/// person pinned where it is. Whether anything changed.
fn change(
    strips: &mut Strips,
    action: &ArrangementAction,
    by_the_person: bool,
) -> Result<bool, String> {
    match action {
        ArrangementAction::OpenInto {
            strip,
            after,
            place,
            shows,
            view,
            record,
            state,
        } => {
            if strips.strip(*strip).is_none() {
                return Err("that strip is not in the arrangement".into());
            }
            let opened = Place {
                id: *place,
                shows: shows.clone(),
                view: view.clone(),
                record: record.clone(),
                state: state.clone(),
                title: String::new(),
                pinned: false,
                shell: SHELL.to_owned(),
            };
            if strips.open_into(*strip, opened, *after) {
                Ok(true)
            } else {
                Err("that place is open already".into())
            }
        }
        ArrangementAction::Move { place, row, before } => {
            if !by_the_person && strips.place(*place).is_some_and(|held| held.pinned) {
                return Err("the person pinned that place: it stays where it is".into());
            }
            // A strip the row has none of yet is minted from the place, so
            // two devices making one at once agree.
            let minted: GroupId = row.wrapping_mul(31).wrapping_add(1) & PEER_MASK;
            strips
                .move_place(*place, StripId(*row), *before, minted)
                .map(|_| true)
                .ok_or_else(|| "that place is not in the arrangement".to_owned())
        }
        ArrangementAction::Resize { strip, permille } => {
            Ok(strips.resize(*strip, WindowWidth::Fraction(*permille)))
        }
        ArrangementAction::Close { place } => {
            if strips.close_place(*place) {
                Ok(true)
            } else {
                Err("that place is not in the arrangement".into())
            }
        }
        ArrangementAction::Pin { place, pinned } => {
            if strips.place(*place).is_none() {
                return Err("that place is not in the arrangement".into());
            }
            Ok(strips.pin(*place, *pinned))
        }
    }
}

/// Where the person's arrangement is kept on this device: beside the desk,
/// as its Automerge document.
const FILE: &str = "arrangement.automerge";

/// Where the record lives under `state`, this device's state directory.
fn path(state: &std::path::Path) -> std::path::PathBuf {
    state.join("tilo").join(FILE)
}

/// The record kept under `state`, if there is one this build reads.
#[must_use]
pub fn load_at(state: &std::path::Path) -> Option<Vec<u8>> {
    std::fs::read(path(state))
        .map_err(|error| tracing::debug!(%error, "no arrangement is kept"))
        .ok()
}

/// Keep `doc` under `state`, replacing the one before.
///
/// # Errors
/// The state directory could not be written.
pub fn save_at(state: &std::path::Path, doc: &mut AutoCommit) -> std::io::Result<()> {
    let path = path(state);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // Written beside and moved into place: a session that dies mid-write
    // leaves the arrangement it had.
    let beside = path.with_extension("automerge.tmp");
    std::fs::write(&beside, doc.save())?;
    std::fs::rename(&beside, &path)
}

/// The id space's counter mask, for an id this build derives rather than
/// mints.
const PEER_MASK: u64 = tilo_desktop_model::PEER_ID_COUNTER_MASK;

/// The verb `action` is, as its step names it.
fn verb(action: &ArrangementAction) -> &'static str {
    match action {
        ArrangementAction::OpenInto { .. } => "open",
        ArrangementAction::Move { .. } => "move",
        ArrangementAction::Resize { .. } => "resize",
        ArrangementAction::Close { .. } => "close",
        ArrangementAction::Pin { .. } => "pin",
    }
}

/// The person `person`, as the history names them.
fn user(person: &str) -> UserId {
    UserId(person.to_owned())
}

/// The person's arranging actions, published as every other action is: a
/// person's own drag, the palette, a script and an agent all arrange through
/// them, and the catalogue lists them.
#[must_use]
pub fn resources() -> Vec<ResourceSpec> {
    vec![
        ResourceSpec::new(RESOURCE, "shell", "The person's arrangement")
            .with_action(action(
                "open-into",
                "Open a place into a strip",
                vec![
                    required("strip", ParamType::Integer, "The strip to open into"),
                    required("place", ParamType::Integer, "The id the new place takes"),
                    required("shows", ParamType::Text, "The app it shows"),
                    optional("after", ParamType::Integer, "The place it follows"),
                    optional("view", ParamType::Text, "The view it shows through"),
                    optional("record", ParamType::Text, "The record it shows"),
                ],
                |args| ArrangementAction::OpenInto {
                    strip: count(args, "strip"),
                    place: count(args, "place"),
                    shows: text(args, "shows"),
                    after: args
                        .integer("after")
                        .map(|after| after.max(0).cast_unsigned()),
                    view: args.text("view").unwrap_or_default().to_owned(),
                    record: args.text("record").unwrap_or_default().to_owned(),
                    state: Json::Null,
                },
            ))
            .with_action(action(
                "move-place",
                "Move a place into a row",
                vec![
                    required("place", ParamType::Integer, "The place to move"),
                    required("row", ParamType::Integer, "The row it goes into"),
                    optional("before", ParamType::Integer, "The place it goes before"),
                ],
                |args| ArrangementAction::Move {
                    place: count(args, "place"),
                    row: count(args, "row"),
                    before: args
                        .integer("before")
                        .map(|before| before.max(0).cast_unsigned()),
                },
            ))
            .with_action(action(
                "resize-strip",
                "Resize a strip",
                vec![
                    required("strip", ParamType::Integer, "The strip to resize"),
                    required(
                        "permille",
                        ParamType::Integer,
                        "Its width in per-mille of the row",
                    ),
                ],
                |args| ArrangementAction::Resize {
                    strip: count(args, "strip"),
                    permille: u32::try_from(args.integer("permille").unwrap_or(1)).unwrap_or(1),
                },
            ))
            .with_action(action(
                "close-place",
                "Close a place",
                vec![required("place", ParamType::Integer, "The place to close")],
                |args| ArrangementAction::Close {
                    place: count(args, "place"),
                },
            ))
            .with_action(action(
                "pin-place",
                "Pin a place, so an agent's arrangement leaves it alone",
                vec![
                    required("place", ParamType::Integer, "The place to pin"),
                    optional("pinned", ParamType::Boolean, "Whether it is pinned"),
                ],
                |args| ArrangementAction::Pin {
                    place: count(args, "place"),
                    pinned: args.boolean("pinned").unwrap_or(true),
                },
            ))
            .with_action(step_action("undo", "Undo the last arranging step"))
            .with_action(step_action("redo", "Redo the last undone arranging step")),
        ResourceSpec::new(RECORD_RESOURCE, "shell", "The person's arrangement record")
            .with_action(step_action("undo", "Undo the last arranging step"))
            .with_action(step_action("redo", "Redo the last undone arranging step")),
    ]
}

/// An immediate effect action `id` the session answers itself: a step of the
/// arrangement's history.
#[expect(
    clippy::expect_used,
    reason = "the step action always serializes to JSON"
)]
fn step_action(id: &'static str, label: &str) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        label.to_owned(),
        String::new(),
        OperationKind::Effect,
        OPERATION,
        Vec::new(),
        |_| {
            EncodedAction::encode_operation(OPERATION, &Json::Null)
                .expect("a step action always serializes")
        },
    );
    spec.type_mut().contract = ActionContract {
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        ..ActionContract::default()
    };
    spec
}

/// An immediate effect action `id` that builds an [`ArrangementAction`].
#[expect(
    clippy::expect_used,
    reason = "an arranging action always serializes to JSON"
)]
fn action(
    id: &'static str,
    label: &str,
    params: Vec<ActionParam>,
    build: impl Fn(&ActionArgs) -> ArrangementAction + Send + Sync + 'static,
) -> ActionSpec {
    let mut spec = ActionSpec::typed_operation(
        id,
        label.to_owned(),
        String::new(),
        OperationKind::Effect,
        OPERATION,
        params,
        move |args| {
            EncodedAction::encode_operation(OPERATION, &build(args))
                .expect("an arranging action always serializes")
        },
    );
    spec.type_mut().contract = ActionContract {
        execution: ExecutionMode::Immediate,
        result: ResultSpec::None,
        ..ActionContract::default()
    };
    spec
}

fn required(name: &'static str, ty: ParamType, help: &str) -> ActionParam {
    ActionParam::required_owned(name, ty, help.to_owned())
}

fn optional(name: &'static str, ty: ParamType, help: &str) -> ActionParam {
    ActionParam::optional_owned(name, ty, help.to_owned())
}

fn count(args: &ActionArgs, name: &str) -> u64 {
    args.integer(name)
        .unwrap_or_default()
        .max(0)
        .cast_unsigned()
}

fn text(args: &ActionArgs, name: &str) -> String {
    args.text(name).unwrap_or_default().to_owned()
}

/// The session's side of the arrangement's published actions: the one door
/// the palette, a script and an agent reach them through, beside the
/// desktop's own.
pub struct ArrangementDoor {
    arrangement: std::rc::Rc<RefCell<Arrangement>>,
    /// How the session says whether the person is mid-gesture or typing, as
    /// the compositor last saw it.
    busy: std::rc::Rc<dyn Fn() -> Busy>,
    /// Who asks, for an action that names no one (see [`Self::named`]).
    actor: RefCell<String>,
}

impl ArrangementDoor {
    /// The door over `arrangement`, whose changes wait while `busy` says the
    /// person is arranging or typing.
    #[must_use]
    pub fn new(
        arrangement: std::rc::Rc<RefCell<Arrangement>>,
        busy: std::rc::Rc<dyn Fn() -> Busy>,
    ) -> Self {
        Self {
            arrangement,
            busy,
            actor: RefCell::new(String::new()),
        }
    }

    /// Arrange as `actor` from now on: what the person's own input does, so
    /// their drag, their resize and their close are attributed to them.
    pub fn arrange_as(&self, actor: &str) {
        actor.clone_into(&mut self.actor.borrow_mut());
    }
}

impl crate::Published for ArrangementDoor {
    fn resources(&mut self) -> Vec<ResourceSpec> {
        resources()
    }

    fn revision(&self) -> Option<u64> {
        // The actions never change, so what is published does not either.
        Some(0)
    }

    fn invoke(
        &mut self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        if address.resource == RECORD_RESOURCE {
            return self.step(address);
        }
        let built = resources()
            .into_iter()
            .flat_map(|resource| {
                if resource.id.as_str() != address.resource {
                    return Vec::new();
                }
                resource.actions
            })
            .find(|action| action.id.as_str() == address.action)
            .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?;
        let encoded = built
            .build_json(arguments)
            .map_err(|error| tilo_actions::DoorError::InvalidArguments(error.to_string()))?;
        let action =
            ArrangementAction::decode(&encoded.type_id, &encoded.payload).ok_or_else(|| {
                tilo_actions::DoorError::UnregisteredType("tilo.shell.arranging".to_owned())
            })?;
        let asked = self.actor.borrow().clone();
        // The person's own change is what their gesture is making; only
        // anyone else's waits for them to finish.
        let busy = if asked.is_empty() || asked == self.person() {
            Busy::default()
        } else {
            (self.busy)()
        };
        self.arrangement
            .borrow_mut()
            .ask(&asked, action, busy)
            .map(drop)
            .map_err(tilo_actions::DoorError::Failed)?;
        Ok(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    fn invoke_as(
        &mut self,
        actor: &str,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, Json>,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        actor.clone_into(&mut self.actor.borrow_mut());
        self.invoke(address, arguments)
    }

    fn accessible_labels(&mut self) -> Vec<String> {
        Vec::new()
    }
}

impl ArrangementDoor {
    /// Undo or redo the session's last change of the record.
    fn step(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Result<ui::host_operation::ResourceOperationStart, tilo_actions::DoorError> {
        let person = self.person();
        let stepped = match address.action.as_str() {
            "undo" => self.arrangement.borrow_mut().undo(&person),
            "redo" => self.arrangement.borrow_mut().redo(&person),
            _ => return Err(tilo_actions::DoorError::Unknown(address.clone())),
        };
        if !stepped {
            return Err(tilo_actions::DoorError::Unavailable {
                code: "nothing_to_undo".to_owned(),
                reason: "the arrangement has no step left".to_owned(),
            });
        }
        Ok(ui::host_operation::ResourceOperationStart::Completed(
            tilo_actions::ActionOutput::default(),
        ))
    }

    /// The person whose arrangement it is.
    fn person(&self) -> String {
        self.arrangement.borrow().person().to_owned()
    }
}

/// Whether two readings hold the same rows, strips and places: what the desk
/// shows, ignoring the shell stamps and orders.
fn same_places(one: &Strips, other: &Strips) -> bool {
    one.rows.len() == other.rows.len()
        && one.strips.len() == other.strips.len()
        && one.strips.iter().zip(&other.strips).all(|(a, b)| {
            a.id == b.id
                && a.places.len() == b.places.len()
                && a.places.iter().zip(&b.places).all(|(a, b)| a.id == b.id)
        })
}

/// Take in the rows and strips the desk made or removed, so the places the
/// same pass opened have a strip to be in. Whether anything changed.
fn adopt_structure(strips: &mut Strips, became: &Strips) -> bool {
    let was = strips.clone();
    for workspace in &became.workspaces {
        if !strips.workspaces.iter().any(|held| held.id == workspace.id) {
            strips.workspaces.push(workspace.clone());
        }
    }
    for row in &became.rows {
        if strips.row(row.workspace, row.id).is_none() {
            strips.rows.push(row.clone());
        }
    }
    // A row the desk no longer has goes, with its strips.
    strips.rows.retain(|row| {
        became
            .rows
            .iter()
            .any(|held| held.workspace == row.workspace && held.id == row.id)
    });
    for strip in &became.strips {
        if !strips.strips.iter().any(|held| held.id == strip.id) {
            strips.strips.push(strip.clone());
        }
    }
    let live: std::collections::BTreeSet<u64> =
        became.strips.iter().map(|strip| strip.id).collect();
    strips.strips.retain(|strip| live.contains(&strip.id));
    *strips != was
}

/// The arranging actions that take `was` to `became`: what the desk opened,
/// closed, moved and resized since the last pass.
fn actions_between(was: &Strips, became: &Strips) -> Vec<ArrangementAction> {
    let mut actions = Vec::new();
    let held: BTreeMap<TabId, (GroupId, StripId)> = was
        .strips
        .iter()
        .flat_map(|strip| {
            strip
                .places
                .iter()
                .map(move |place| (place.id, (strip.id, strip.row)))
        })
        .collect();
    for strip in &became.strips {
        for place in &strip.places {
            match held.get(&place.id) {
                // Opened on the desk: a window the person started.
                None => actions.push(ArrangementAction::OpenInto {
                    strip: strip.id,
                    after: None,
                    place: place.id,
                    shows: place.shows.clone(),
                    view: place.view.clone(),
                    record: place.record.clone(),
                    state: place.state.clone(),
                }),
                // Moved to another row by the person.
                Some((_, row)) if *row != strip.row => {
                    actions.push(ArrangementAction::Move {
                        place: place.id,
                        row: strip.row.0,
                        before: None,
                    });
                }
                Some(_) => {}
            }
        }
    }
    let now: std::collections::BTreeSet<TabId> = became.places().map(|place| place.id).collect();
    for place in was.places() {
        if !now.contains(&place.id) {
            actions.push(ArrangementAction::Close { place: place.id });
        }
    }
    // A width the person dragged, to where the desk has it.
    for strip in &became.strips {
        let changed = was
            .strip(strip.id)
            .is_some_and(|before| before.width != strip.width);
        if changed {
            actions.push(ArrangementAction::Resize {
                strip: strip.id,
                permille: strip.width.permille(),
            });
        }
    }
    actions
}
