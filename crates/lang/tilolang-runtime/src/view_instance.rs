//! A live view: its tree, the fields and view state it depends on, and the events it can take.
//!
//! A view runs in a window (workspace-model.md, "Windows") for one person. The state it
//! declares is how that person's window shows the resource and where they are in it; it is
//! kept per person and window, so nobody else's view of the same window moves with them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::authority::{ActionInfo, Decision};
use crate::core::Core;
use crate::error::RuntimeError;
use crate::exec::{ExecCtx, InvokeOutcome};
use crate::host_res::to_scalar;
use crate::host_ui::{
    ANSWER_ACTION, BLUR_ACTION, CALL_ACTION, CALL_INPUT_ACTION, CALL_VOLUME_ACTION, CHOOSE_ACTION,
    CHOOSE_CASE_ACTION, CHOOSE_EDITING_ACTION, CHOOSE_FOLDER_ACTION, CLOSE_ACTION,
    COPY_LINK_ACTION, EXPORT_ACTION, KEEP_ACTION, MARK_SEEN_ACTION, OPEN_ACTION, REVIEW_ACTION,
    RUN_LINE_ACTION, SAVE_ACTION, SET_ACTION, SHORTCUT_ACTION, UNDO_ACTION,
};
use crate::ids::{Actor, AppId, DocId, ModuleHash};
use crate::live::{Live, LiveStats};
use crate::meta::{EffectClass, FieldKind};
use crate::meta::{Ty, parse_ty};
use crate::registry::Package;
use crate::render::ViewTarget;
use crate::space::Space;
use crate::store::{ChangeNote, ChangeSummary, Mutation, SubId};
use crate::value::THEN_ACTION;
use crate::value::{ActionRef, ResourceRef, Value, to_json};
use crate::view::{BindTarget, Binding, Patch, UiNode};
use crate::view_state::StateKey;

/// Something a view asks of the host that the runtime does not do itself.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewEvent {
    /// `open(r)` or `open(r, view: v)` was pressed: go to the window of `target`, showing it
    /// through `view`, or through the default view of its type when `None`.
    Open {
        target: ResourceRef,
        view: Option<String>,
        /// Whether a narrow window brings it in front (`open(r)`), or shows it in the next
        /// column only (`open(r, front: false)`, a selection the keyboard moves).
        front: bool,
    },
    /// A change mark was activated: show what the run `run` (its group) did, where it
    /// changed `target`.
    Review { run: String, target: ResourceRef },
    /// `undo(v)` or `undo(run)` was pressed: take back, through the platform's history, the
    /// step a version is part of, or every step of a run.
    Undo(UndoAsk),
    /// *Mark all seen* was pressed and moved `count` marks: the window says so, and the person
    /// can take the move back with it.
    Seen { count: usize },
    /// A step of the person's own in the system's call held on `target` was pressed: the
    /// window's host carries it out through the call service.
    Call { target: ResourceRef, step: CallStep },
    /// `run_line(r, line)` was pressed: the window's host runs `line` as the person's own step,
    /// through the session's program host, and tells `lines_run(r)` what it answers.
    RunLine { target: ResourceRef, line: String },
    /// Copy link was chosen on `target`, a resource of `app`: the window's host mints its link
    /// and puts it on the person's clipboard.
    CopyLink { target: ResourceRef, app: String },
    /// Export was chosen on `target`, a resource of `app`: the window's host saves it as a file
    /// where the person says ([`crate::export`]).
    Export { target: ResourceRef, app: String },
    /// `blur()` was applied: the field the keyboard is in ends its edit, as closing its editor
    /// does, so what the person types next goes nowhere until they choose another field.
    EndEdit,
    /// `close()` was applied: the window the view stands in closes, as the person closing it
    /// does.
    CloseWindow,
}

/// A step of the person's own in the system's call held on a resource.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallStep {
    /// Join it, leaving any other call this device is in.
    Join,
    /// Leave it.
    Leave,
    /// Turn the microphone off, or back on.
    Mute(bool),
    /// Hear nothing of the call and be heard by no one, or stop.
    Deafen(bool),
    /// Show the camera in it, or stop.
    Camera(bool),
    /// Share the screen in it, or stop.
    Screen(bool),
    /// Send this device's microphone from this input.
    SelectInput(String),
    /// Play this member at this percent of as they arrive, on this device only.
    Volume { member: String, percent: u16 },
}

impl CallStep {
    /// The step `name` names (`join_call` is `join`), turning its switch `on`.
    #[must_use]
    pub fn named(name: &str, on: bool) -> Option<Self> {
        Some(match name {
            "join" => Self::Join,
            "leave" => Self::Leave,
            "mute" => Self::Mute(on),
            "deafen" => Self::Deafen(on),
            "camera" => Self::Camera(on),
            "screen" => Self::Screen(on),
            _ => return None,
        })
    }
}

/// What an `undo(...)` a view applied takes back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoAsk {
    /// The step the change `change` (a version's) of `target` is part of.
    Step { change: String, target: ResourceRef },
    /// Every step of the run `run` (its group).
    Run { run: String },
}

/// How an edit of a bound field ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditOutcome {
    Applied,
    /// The authority asks: the author of the binding may not edit this resource unasked.
    NeedsApproval,
    Denied,
}

pub struct ViewInstance {
    core: Rc<Core>,
    live: Live,
    /// The space set per boundary key, for the host's layout pass.
    spaces: HashMap<String, Space>,
    /// What the store told since the last poll.
    changes: Rc<RefCell<Vec<ChangeSummary>>>,
    subs: Vec<SubId>,
    subscribed: HashSet<DocId>,
    events: RefCell<Vec<ViewEvent>>,
    /// The bindings a field was given by what it began: the `begin` handler's action made a
    /// record, and from then on the field edits that record's name. Keyed by the control's key.
    begun: RefCell<std::collections::BTreeMap<String, Binding>>,
    /// The generation of each stream's state this view took into the slot its views read: what
    /// moved since is a reading its views run again for.
    streams: HashMap<String, u64>,
    /// What follows a step that waited (`a.then(b)`, whose `a` waits on a call): the
    /// invocation it waits on to end, and `b` with the act it is part of, run once `a` is done.
    then_waiting: RefCell<Vec<(crate::ids::RunId, ActionRef, Option<String>)>>,
    /// Whether the person held Shift with the press whose action runs now: on what can be
    /// undone, that press is their yes to what the workspace asks first.
    shift_held: std::cell::Cell<bool>,
}

/// Take what the streams pushed since this view last looked into the slots its views read: a
/// stream that says it is connecting, running or ended is a reading, and the boundary that showed
/// the word before runs again for the new one. A stream the device forgot says nothing.
fn take_stream_states(core: &Core, window: &str, taken: &mut HashMap<String, u64>) -> bool {
    let mut took = false;
    for (slot, state) in core.live_frames.states() {
        if taken.get(&slot) == Some(&state.generation) {
            continue;
        }
        core.view_states.borrow_mut().set(
            crate::view_state::stream_key(window, &slot),
            Value::text(state.text),
        );
        taken.insert(slot, state.generation);
        took = true;
    }
    let held: Vec<String> = taken.keys().cloned().collect();
    for slot in held {
        let live = core.live_frames.state(&slot).is_some();
        if !live && taken.remove(&slot).is_some() {
            core.view_states.borrow_mut().set(
                crate::view_state::stream_key(window, &slot),
                Value::text(""),
            );
            took = true;
        }
    }
    took
}

/// Whether a stream said something this view has not taken, or one it took was forgotten: a
/// frame that runs before the view does.
fn streams_moved(core: &Core, taken: &HashMap<String, u64>) -> bool {
    let states = core.live_frames.states();
    states
        .iter()
        .any(|(slot, state)| taken.get(slot) != Some(&state.generation))
        || taken
            .keys()
            .any(|slot| !states.iter().any(|(held, _)| held == slot))
}

/// The window a view over `args` runs in when the host names none: the window of its first
/// resource argument (a window is identified by its resource), else one of its own.
pub(crate) fn default_window(target: &ViewTarget, args: &[Value]) -> String {
    args.iter()
        .find_map(|a| match a {
            Value::Ref(r) => Some(r.target.id_string()),
            _ => None,
        })
        .unwrap_or_else(|| target.export.clone())
}

/// What the parameter the action leaves out takes.
enum Filled {
    /// The one file the person chose.
    One,
    /// Every file the person chose.
    Many,
    /// The folder the person chose, by the reference that reaches it again.
    Folder,
}

/// Why a chooser's `then:` is not one.
fn no_chooser_handler(value: &Value) -> RuntimeError {
    RuntimeError::BadArgs(format!(
        "`file.choose`'s `then:` takes an action, not {}",
        value.kind_name()
    ))
}

impl ViewInstance {
    pub(crate) fn open(
        core: Rc<Core>,
        (viewer, window): (Actor, String),
        target: ViewTarget,
        args: Vec<Value>,
    ) -> Result<Self, RuntimeError> {
        let mut streams = HashMap::new();
        take_stream_states(&core, &window, &mut streams);
        let live = Live::open(&core, (viewer, window), target, args)?;
        let mut me = Self {
            core,
            live,
            spaces: HashMap::new(),
            changes: Rc::default(),
            subs: Vec::new(),
            subscribed: HashSet::new(),
            events: RefCell::new(Vec::new()),
            begun: RefCell::default(),
            streams,
            then_waiting: RefCell::default(),
            shift_held: std::cell::Cell::new(false),
        };
        me.subscribe_new();
        Ok(me)
    }

    /// The window this view runs in.
    pub fn window(&self) -> &str {
        &self.live.window
    }

    /// Whether the view it shows declares `protect capture`: wherever it is shown, a capture or a
    /// stream leaves it out.
    #[must_use]
    pub fn capture_protected(&self) -> bool {
        let Some(root) = self.live.bound(crate::live::ROOT) else {
            return false;
        };
        let Some(package) = self.core.registry.borrow().view_package(&root.target.view) else {
            return false;
        };
        package
            .view(&root.target.view)
            .is_some_and(|view| view.capture_protected)
    }

    /// The person looking.
    pub fn viewer(&self) -> &Actor {
        &self.live.viewer
    }

    /// Where the person looking is in a document this view draws, as the view declared it with
    /// `presence`: the resource it is in, and the point and what they selected, in the
    /// document's own units.
    #[must_use]
    pub fn declared_spot(&self) -> Option<(crate::value::ResourceRef, [f64; 6])> {
        self.live.bound(crate::live::ROOT)?.spot.clone()
    }

    /// What the view asked of the host since the last call: windows to open.
    pub fn poll_events(&mut self) -> Vec<ViewEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// Listen to every document some boundary read and nobody listens to yet.
    fn subscribe_new(&mut self) {
        let docs: Vec<DocId> = self
            .live
            .docs()
            .filter(|d| !self.subscribed.contains(*d))
            .cloned()
            .collect();
        for doc in docs {
            let queue = Rc::clone(&self.changes);
            let id = self.core.store.borrow_mut().subscribe(
                &doc,
                Box::new(move |summary| queue.borrow_mut().push(summary.clone())),
            );
            self.subs.push(id);
            self.subscribed.insert(doc);
        }
    }

    /// Mark what the changes heard since the last call reach.
    fn absorb(&mut self) {
        self.run_followups();
        take_stream_states(&self.core, &self.live.window, &mut self.streams);
        let heard = std::mem::take(&mut *self.changes.borrow_mut());
        self.live.absorb(&self.core, &heard);
        self.live.check_states(&self.core);
        if let Some(now) = self.time_passed() {
            self.live.pass_time(now);
        }
    }

    /// The current tree. Rows of a `list` that were not shown are not in it (for the patches,
    /// such a row is an empty `fragment`): [`Self::list_len`] and [`Self::row_key`] say which
    /// rows a list has.
    pub fn tree(&self) -> &UiNode {
        &self.live.tree
    }

    /// Set the space layout gave the boundary `key`: the root (key `""` or the root node's key),
    /// or a view call by its `fragment`'s key. Returns whether the view now needs to re-run: the
    /// boundary read a `space` value that differs from the one it saw, or one inside it did.
    /// A view re-runs only when what it read changed, and no other boundary does.
    pub fn set_space(&mut self, boundary: &str, space: Space) -> bool {
        self.spaces.insert(boundary.to_owned(), space);
        self.live.set_space(boundary, space);
        self.is_stale()
    }

    /// The space last set for a boundary.
    pub fn space(&self, boundary: &str) -> Option<Space> {
        self.spaces.get(boundary).copied()
    }

    /// The boundaries that read the space layout gives them, by key, in the order they stand:
    /// what the window measures for the view (`ToUi::Spaces`).
    pub fn space_readers(&self) -> Vec<String> {
        self.live.space_readers()
    }

    /// Take `(resource, field)` reads the host took converting what the view shows, beside what
    /// the view read: a card of a record inside the text it holds. A change to one of them runs
    /// the root boundary, so the host converts what the window shows again. A host calls it
    /// after converting; the boundary's own run replaces the reads with what it read then, and
    /// the host registers its own again.
    pub fn shows(&mut self, reads: impl IntoIterator<Item = (String, String)>) {
        self.live.shows(reads);
    }

    /// Run again the boundaries that show something only the window knows — a member's
    /// name, a day or a time of day in the person's formats, a word in the language their window
    /// speaks — and no others: the person reads another language, chose another clock, or the
    /// window calls someone something else than it did.
    pub fn person_moved(&mut self) {
        self.live.person_moved();
    }

    /// When the time some boundary read (`today` in a query or a filter) next reads
    /// differently, in seconds since 1970 (UTC): the host wakes the view then, and the next
    /// poll runs again exactly the boundaries that read it. `None` when no boundary read the
    /// time: nothing about the time ever wakes the view.
    #[must_use]
    pub fn wakes_at(&self) -> Option<i64> {
        self.live.wakes_at()
    }

    /// The resources of other apps this view's boundaries read: what the host follows while
    /// the view shows one, and what it may stop following once none does.
    #[must_use]
    pub fn foreign_reads(&self) -> Vec<ResourceRef> {
        self.live
            .state_keys_read()
            .iter()
            .filter_map(|key| crate::foreign::reading_target(&key.state))
            .collect()
    }

    /// The host clock's time, when the time some boundary read reads differently by then. The
    /// clock is read only while some boundary waits for it.
    fn time_passed(&self) -> Option<i64> {
        let at = self.live.wakes_at()?;
        let now = tilolang::calendar::instant(&self.core.clock_now())?;
        (now >= at).then_some(now)
    }

    /// Whether anything reached the view since it last ran: the store told of a change to a
    /// document it reads, a view state moved, the time it waits for came, or boundaries are
    /// marked to run. A view nothing reached is not looked at again, however often its host
    /// asks.
    #[must_use]
    pub fn woken(&self) -> bool {
        !self.changes.borrow().is_empty()
            || self.live.is_dirty()
            || self.live.states_moved(&self.core)
            || streams_moved(&self.core, &self.streams)
            || self.time_passed().is_some()
            || self.followup_due()
    }

    /// Whether a step a `then` waited on ended, so what follows it is due.
    fn followup_due(&self) -> bool {
        let awaited = self.core.awaited.borrow();
        self.then_waiting
            .borrow()
            .iter()
            .any(|(invocation, ..)| awaited.get(invocation).is_some_and(Option::is_some))
    }

    /// Run what follows each step a `then` waited on that is done now, as the press that made
    /// it would have once the step was done; a step that failed or was refused ends the press
    /// there, as it would have had it not waited.
    fn run_followups(&self) {
        let ended: Vec<(bool, ActionRef, Option<String>)> = {
            let mut waiting = self.then_waiting.borrow_mut();
            let mut ended = Vec::new();
            waiting.retain(
                |(invocation, next, gesture)| match self.core.awaited_end(invocation) {
                    Some(done) => {
                        ended.push((done, next.clone(), gesture.clone()));
                        false
                    }
                    None => true,
                },
            );
            ended
        };
        for (done, next, gesture) in ended {
            if !done || !self.applies_here(&next) {
                continue;
            }
            if let Err(error) = self.run_in(&next, gesture.as_deref()) {
                tracing::warn!(action = %next.action, %error, "what followed a waiting step failed");
            }
        }
    }

    /// Whether a field or view state some boundary read has changed since it last ran, or the
    /// time one read has moved on: what changed is taken in, marking the boundaries to run
    /// again, so the run that follows does not work it out a second time.
    pub fn is_stale(&mut self) -> bool {
        self.absorb();
        self.live.is_dirty()
    }

    /// Re-run the boundaries whose reads changed, and only those, and return the patches from
    /// the old tree to the new one, in the order they apply. Empty when nothing changed.
    ///
    /// # Errors
    ///
    /// As rendering: a trap or a malformed tree.
    pub fn poll_patches(&mut self) -> Result<Vec<Patch>, RuntimeError> {
        self.absorb();
        if !self.live.is_dirty() {
            return Ok(Vec::new());
        }
        // What this turn fails to read for want of a reading that has not arrived is what it
        // gathers now: the last turn's waiting errors are done with.
        self.core.foreign.borrow_mut().take_waiting();
        let patches = self.live.poll(&self.core);
        self.subscribe_new();
        patches
    }

    /// What the view did so far: how many boundaries ran, how many there are.
    pub fn stats(&self) -> LiveStats {
        self.live.stats()
    }

    /// The resources the window shows now, by [`ResourceRef::id_string`]: the view's target
    /// and the items of the rows rendered (the visible rows and the overscan). What a surface
    /// publishes to a window is bounded by this, not by what the workspace holds.
    pub fn shown_refs(&self) -> Vec<String> {
        self.live.shown_refs(&self.core)
    }

    /// Whether rows of lists render only once shown (the default), or with the list. A host
    /// that draws the whole tree, rather than asking for rows, turns this off before the
    /// first poll; rows already left unrendered render at once, into the tree (no patches:
    /// such a host reads [`Self::tree`] after this).
    ///
    /// # Errors
    ///
    /// As rendering.
    pub fn set_lazy_rows(&mut self, lazy: bool) -> Result<(), RuntimeError> {
        self.live.lazy_rows = lazy;
        if lazy {
            return Ok(());
        }
        let mut lists = Vec::new();
        collect_lists(&self.live.tree, &mut lists);
        for list in lists {
            for i in 0..self.live.list_len(&list).unwrap_or(0) {
                self.live.show_row(&self.core, &list, i)?;
            }
        }
        // Every row is built: the lists hold all their rows from now on.
        self.live.devirtualize();
        self.subscribe_new();
        Ok(())
    }

    /// How many rows the `list` with key `list` has.
    pub fn list_len(&self, list: &str) -> Option<usize> {
        self.live.list_len(list)
    }

    /// The list's length as reading its rows left it: a listing that sizes itself before its
    /// pages are read holds what it holds once a row past its end is read
    /// ([`Live::settle_len`]).
    pub fn settle_len(&mut self, list: &str) {
        self.live.settle_len(list);
    }

    /// The key of row `index` of `list`.
    pub fn row_key(&self, list: &str, index: usize) -> Option<String> {
        self.live.row_key(list, index)
    }

    /// The page of rows the host is about to build: the resources `range` of `list` shows. What
    /// links to them is read once for the page, when a row first reads it ([`Self::row`]), not
    /// once for each row.
    pub fn prefetch_links(&mut self, list: &str, range: std::ops::Range<usize>) {
        self.live.prefetch_links(&self.core, list, range);
    }

    /// The page of rows is done: what it read of their links is let go, so a later read asks the
    /// store again.
    pub fn release_links(&mut self) {
        self.core.release_links();
    }

    /// Row `index` of `list`, rendered now if it was not shown before: what a virtual list
    /// asks for each visible row (plus overscan). Later changes to what the row read come as
    /// patches until the row is hidden again.
    ///
    /// # Errors
    ///
    /// As rendering.
    pub fn row(&mut self, list: &str, index: usize) -> Result<Option<&UiNode>, RuntimeError> {
        let key = self.live.show_row(&self.core, list, index)?;
        self.subscribe_new();
        Ok(key.and_then(|key| self.live.find(&key)))
    }

    /// The host shows the row at `open` of `list` held open into its item: its document is
    /// built with it, the row that stood open before drops its own, and both render again.
    pub fn open_row(&mut self, list: &str, open: Option<&str>) {
        self.live.open_row(list, open);
    }

    /// The host asks for the menu of the item the row `menu` of the virtual list `list` shows:
    /// that row builds it, and the row that had it lets it go.
    pub fn menu_row(&mut self, list: &str, menu: Option<&str>) {
        self.live.menu_row(list, menu);
    }

    /// Forget the rows of `list` outside `keep`, which scrolled out of view: they no longer
    /// run when what they read changes, and render again when shown. Returns the patches that
    /// empty them.
    pub fn hide_rows(&mut self, list: &str, keep: std::ops::Range<usize>) -> Vec<Patch> {
        self.live.hide_rows(&self.core, list, keep)
    }

    /// The node with `key`.
    pub fn find(&self, key: &str) -> Option<&UiNode> {
        self.live.find(key)
    }

    /// Fire the event `event` of the node with `key`.
    ///
    /// # Errors
    ///
    /// `NotFound` when the node or its event does not exist; otherwise as an invocation.
    pub fn fire(&self, key: &str, event: &str) -> Result<InvokeOutcome, RuntimeError> {
        self.fire_with(key, event, None)
    }

    /// The action reference the event `event` of the node `key` applies, as the view attached
    /// it.
    #[must_use]
    pub fn event_action(&self, key: &str, event: &str) -> Option<ActionRef> {
        let node = self.live.find(key)?;
        let retained = node.events.get(event)?;
        match self.core.frames.borrow().retained(*retained) {
            Some(Value::Action(aref)) => Some(aref.clone()),
            _ => None,
        }
    }

    /// Fire the event `event` of the node with `key`, carrying `value`: the action reference
    /// takes it where it holds the event's value (`open(x)` on `select(x)`).
    ///
    /// # Errors
    ///
    /// `NotFound` when the node or its event does not exist, `BadArgs` when the action takes
    /// the event's value and the event carries none; otherwise as an invocation.
    pub fn fire_with(
        &self,
        key: &str,
        event: &str,
        value: Option<&Value>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.fire_in(key, event, value, None)
    }

    /// [`ViewInstance::fire_with`], as one part of the person's act `gesture`: the changes the
    /// event's action makes name it, as the edit it was made with does, so one undo takes both
    /// back (a picture a paste wrote into a document, and the app naming it).
    ///
    /// # Errors
    ///
    /// As [`ViewInstance::fire_with`].
    pub fn fire_in(
        &self,
        key: &str,
        event: &str,
        value: Option<&Value>,
        gesture: Option<&str>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        self.fire_held(key, event, value, gesture, Held::default())
    }

    /// [`ViewInstance::fire_in`], with the modifier keys `held` while the person pressed: a
    /// parameter of the action that takes a `host.Press` gets the index the event carries with
    /// them, so Shift adds to a selection and the command key toggles one.
    ///
    /// # Errors
    ///
    /// As [`ViewInstance::fire_with`].
    pub fn fire_held(
        &self,
        key: &str,
        event: &str,
        value: Option<&Value>,
        gesture: Option<&str>,
        held: Held,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let Some(mut aref) = self.event_action(key, event) else {
            return Err(RuntimeError::NotFound(format!(
                "the action of event `{event}` on `{key}`"
            )));
        };
        // A case's field is the node the case stands in, which only the press names: the action
        // takes the pressed node before the case it was built with.
        if aref.action == CHOOSE_CASE_ACTION {
            aref.args.insert(0, Value::Text(key.to_owned()));
        }
        fill_event(&mut aref, value, held).map_err(|()| {
            RuntimeError::BadArgs(format!("event `{event}` of `{key}` carries no value"))
        })?;
        self.read_states(&mut aref)?;
        // What an entry sends with its `submit` fills the `Text` parameter of the action it
        // commits, and the parameters the words say: the text is the one typed at the press. An
        // entry whose text is nothing but space and what other parameters take ("fri 10am
        // @bo") makes nothing: it does not fire, and leaves what is typed where it is.
        if let Some(Value::Text(text)) = value {
            match self.core.entry_fill(&mut aref, text) {
                Ok(true) => {}
                Ok(false) => return Ok(InvokeOutcome::Done(Value::Unit)),
                Err(fail) => {
                    let fail = match fail {
                        crate::engine::HostFail::Trap(message) => message,
                        crate::engine::HostFail::Suspend => "the entry is suspended".to_owned(),
                    };
                    return Err(RuntimeError::BadArgs(fail));
                }
            }
        }
        let shift = self.shift_held.replace(held.shift);
        let outcome = self.run_in(&aref, gesture);
        self.shift_held.set(shift);
        let outcome = outcome?;
        match event {
            // The first change made the item; the field edits that item's name from now on.
            "begin" => {
                if let InvokeOutcome::Done(Value::Ref(made)) = &outcome {
                    self.began(key, &made.target);
                }
            }
            // Leaving the field ends what it began: an empty one is taken away by the view's own
            // `blur`, and a finished one keeps the record it made.
            "blur" | "submit" => {
                self.begun.borrow_mut().remove(key);
            }
            _ => {}
        }
        Ok(outcome)
    }

    /// The binding of the control `key` given by its `begin`: the record the action made, and the
    /// field of it the type names records by.
    pub fn begun_binding(&self, key: &str) -> Option<Binding> {
        self.begun.borrow().get(key).cloned()
    }

    /// Every begun binding, by its control.
    pub fn begun(&self) -> std::cell::Ref<'_, std::collections::BTreeMap<String, Binding>> {
        self.begun.borrow()
    }

    /// Bind the control `key` to the field `binds` named of the record `target`, for the rest of
    /// what is typed.
    fn began(&self, key: &str, target: &ResourceRef) {
        let Some(field) =
            self.live
                .find(key)
                .and_then(|node| match node.props.get(crate::view::BEGIN_BIND) {
                    Some(Value::Text(field)) => Some(field.clone()),
                    _ => None,
                })
        else {
            return;
        };
        let Ok((_, info)) = self.core.type_of_ref(&target.ty) else {
            return;
        };
        let Some(held) = info.field_by_id_or_name(&field) else {
            return;
        };
        let Some(author) = first_binding(self.tree()).map(|binding| binding.author.clone()) else {
            return;
        };
        self.begun.borrow_mut().insert(
            key.to_owned(),
            Binding {
                target: BindTarget::Field {
                    target: target.clone(),
                    field: held.id.clone(),
                },
                author,
            },
        );
    }

    /// The action the event `event` of the node `key` applies, with its arguments read now, as
    /// firing it would run it: what a host sends through the dispatch instead of running it here.
    /// `None` when the event applies no action.
    ///
    /// # Errors
    ///
    /// As [`Self::fire_with`] reading the event's arguments.
    pub fn fired_action(
        &self,
        key: &str,
        event: &str,
        value: Option<&Value>,
        held: Held,
    ) -> Result<Option<ActionRef>, RuntimeError> {
        let Some(mut aref) = self.event_action(key, event) else {
            return Ok(None);
        };
        fill_event(&mut aref, value, held).map_err(|()| {
            RuntimeError::BadArgs(format!("event `{event}` of `{key}` carries no value"))
        })?;
        self.read_states(&mut aref)?;
        Ok(Some(aref))
    }

    /// The action the event `event` of the node `key` applies, with its arguments read now,
    /// when it still lacks some it has no default for: an action waiting for its arguments,
    /// whose form the window shows instead of running it (`share(n)` from a menu, which needs
    /// whom to share with). `None` when the event runs as it is.
    ///
    /// # Errors
    ///
    /// As [`Self::fire_with`] reading the event's arguments.
    pub fn wanting_arguments(
        &self,
        key: &str,
        event: &str,
        value: Option<&Value>,
    ) -> Result<Option<ActionRef>, RuntimeError> {
        let Some(mut aref) = self.event_action(key, event) else {
            return Ok(None);
        };
        if self.core.missing_arguments(&aref).is_empty() {
            return Ok(None);
        }
        fill_event(&mut aref, value, Held::default()).map_err(|()| {
            RuntimeError::BadArgs(format!("event `{event}` of `{key}` carries no value"))
        })?;
        self.read_states(&mut aref)?;
        Ok(Some(aref))
    }

    /// Run `aref` as this view's person, as a control of the view runs it: an invocation with
    /// undo and history like any other.
    ///
    /// # Errors
    ///
    /// As an invocation.
    pub fn invoke(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        self.run(aref)
    }

    /// The view state `state` of this view's person in its window, when it was ever set.
    #[must_use]
    pub fn view_state(&self, state: &str) -> Option<Value> {
        self.core
            .view_states
            .borrow()
            .get(&self.state_key(state))
            .cloned()
    }

    /// Set the view state `state` of this view's person in its window. A scalar is boxed as a
    /// slot holds it, so the view reads back what the person picked through `unwrap_i`.
    pub fn set_view_state(&self, state: &str, value: Value) {
        let value = match value {
            Value::Int(_) | Value::Float(_) | Value::Bool(_) => Value::some(value),
            other => other,
        };
        self.core
            .view_states
            .borrow_mut()
            .set(self.state_key(state), value);
    }

    /// Ask the view to show `target` where it stands: what its `on show(x: T): ...` handler for
    /// the item's type applies in the view's own state — a list opens the item's row, a place
    /// follows the item. The view declares what showing one means, so nothing about the shape
    /// of the window is guessed from the data.
    ///
    /// Whether the view answers for an item of the target's type; when it does not, the host
    /// shows the item as a page of its own, as it shows anything the view does not open.
    ///
    /// # Errors
    ///
    /// As an invocation.
    pub fn show(&self, target: &ResourceRef) -> Result<bool, RuntimeError> {
        let Some(root) = self.live.bound(crate::live::ROOT) else {
            return Ok(false);
        };
        let Some(pkg) = self.core.registry.borrow().package(&root.target.module) else {
            return Ok(false);
        };
        let Some(show) = pkg
            .view(&root.target.view)
            .and_then(|view| view.shows.iter().find(|show| show.ty == target.ty))
            .cloned()
        else {
            return Ok(false);
        };
        let item = Value::Ref(crate::value::RefValue {
            target: Rc::new(target.clone()),
            author: None,
        });
        let made = self.core.call_in_view(
            &pkg,
            &show.func.id,
            &show.func.params,
            &[item],
            &self.live.window,
            &self.live.viewer,
            &show.func.result,
        )?;
        let Value::Action(mut aref) = made else {
            return Err(RuntimeError::BadArgs(
                "`on show` must make an action".to_owned(),
            ));
        };
        self.read_states(&mut aref)?;
        self.run(&aref)?;
        Ok(true)
    }

    /// Take the window to its place `place`, by its name or its id: what the view's `on show
    /// week: ...` applies in its own state. It is this person's view of this window alone, so
    /// nothing in the workspace changes and no one else's window moves.
    ///
    /// Whether the view declares that place.
    ///
    /// # Errors
    ///
    /// As an invocation.
    pub fn show_place(&self, place: &str) -> Result<bool, RuntimeError> {
        let Some(root) = self.live.bound(crate::live::ROOT) else {
            return Ok(false);
        };
        let Some(pkg) = self.core.registry.borrow().package(&root.target.module) else {
            return Ok(false);
        };
        let Some(declared) = pkg
            .view(&root.target.view)
            .and_then(|view| {
                view.places
                    .iter()
                    .find(|declared| declared.name == place || declared.id == place)
            })
            .cloned()
        else {
            return Ok(false);
        };
        // The place runs over what the window shows: the view's own arguments.
        let args = root.args.clone();
        let made = self.core.call_in_view(
            &pkg,
            &declared.func.id,
            &declared.func.params,
            &args,
            &self.live.window,
            &self.live.viewer,
            &declared.func.result,
        )?;
        let Value::Action(mut aref) = made else {
            return Err(RuntimeError::BadArgs(
                "a place must make an action".to_owned(),
            ));
        };
        self.read_states(&mut aref)?;
        self.run(&aref)?;
        Ok(true)
    }

    fn state_key(&self, state: &str) -> StateKey {
        StateKey::view(
            self.live.window.clone(),
            self.live.viewer.clone(),
            self.state_id(state),
        )
    }

    /// The id of the state the window's view declares as `state`: a plain name of the view its
    /// window shows (`place`), or `view.name` for one a view it draws declares. A name no view of
    /// the window declares is taken as the id it is: what the runtime keeps for itself is named
    /// by the code that keeps it.
    fn state_id(&self, state: &str) -> String {
        let Some(root) = self.live.bound(crate::live::ROOT) else {
            return state.to_owned();
        };
        let (named_view, name) = match state.split_once('.') {
            Some((named_view, name)) => (Some(named_view), name),
            None => (None, state),
        };
        let registry = self.core.registry.borrow();
        let Some(package) = registry.package(&root.target.module) else {
            return state.to_owned();
        };
        let mut found: Option<&str> = None;
        for candidate in package.meta.views.iter().filter(|candidate| {
            named_view.map_or(candidate.id == root.target.view, |chosen| {
                candidate.name == chosen
            })
        }) {
            for declared in candidate
                .state
                .iter()
                .filter(|declared| declared.name == name)
            {
                match found {
                    // One name, two declarations: the caller names the view it means.
                    Some(held) if held != declared.id => return state.to_owned(),
                    _ => found = Some(&declared.id),
                }
            }
        }
        found.map_or_else(|| state.to_owned(), str::to_owned)
    }

    /// Evaluate the arguments of `aref` that read view state (`send(c, draft)`, `set(open, not
    /// open)`) now, at the event, on the view state as it is: what was typed up to the press,
    /// and what an earlier press set, whether or not the view ran again since.
    fn read_states(&self, aref: &mut ActionRef) -> Result<(), RuntimeError> {
        // `set` puts the value in a slot, boxed as slots hold it; an action takes it plain.
        let unbox = aref.action != SET_ACTION;
        for arg in &mut aref.args {
            match arg {
                Value::State { key, scalar } => {
                    *arg = self.state_now(key, *scalar && unbox);
                }
                Value::AtEvent {
                    module,
                    id,
                    args,
                    scalar,
                } => {
                    let value = self.at_event(module, id, args)?;
                    *arg = if *scalar && !unbox {
                        Value::some(value)
                    } else {
                        value
                    };
                }
                Value::Action(inner) => self.read_states(inner)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// The view state slot `key` as it is now; with `unbox`, a scalar's value plain.
    fn state_now(&self, key: &StateKey, unbox: bool) -> Value {
        let stored = self.core.view_states.borrow().get(key).cloned();
        let value = stored.or_else(|| self.live.state_seen(key));
        // A slot the person never set is nothing, not a failure: an action reading it as an
        // argument (a composer's reply target before anything is answered, the run selected
        // before anything is) runs with nothing there, and the view's own default stands. A
        // missing slot used to fail the action, which made every action that names an optional
        // slot fail until a person filled it.
        let value = value.unwrap_or_else(Value::none);
        match value {
            Value::Opt(Some(held)) if unbox => *held,
            other => other,
        }
    }

    /// Run the argument function `id` of `module` over `args`, its view state read now.
    fn at_event(
        &self,
        module: &ModuleHash,
        id: &str,
        args: &[Value],
    ) -> Result<Value, RuntimeError> {
        let mut given = Vec::with_capacity(args.len());
        for arg in args {
            given.push(match arg {
                Value::State { key, scalar } => self.state_now(key, *scalar),
                other => other.clone(),
            });
        }
        let pkg = self
            .core
            .registry
            .borrow()
            .package(module)
            .ok_or_else(|| RuntimeError::NotFound(format!("module {module}")))?;
        let meta = pkg
            .meta
            .at_event
            .iter()
            .find(|f| f.id == id)
            .cloned()
            .ok_or_else(|| RuntimeError::NotFound(format!("the argument `{id}`")))?;
        self.core.call_pure(
            &pkg,
            &tilolang::ir::at_event_export(id),
            &meta.params,
            &given,
            &meta.result,
        )
    }

    /// A key pressed on an item: the action its menu binds to that key runs, unless it does
    /// not apply to the item.
    fn platform_shortcut(
        &self,
        aref: &ActionRef,
        gesture: Option<&str>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::List(keys), Value::Text(pressed)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed shortcut".into()));
        };
        let run = keys.iter().find_map(|key| match key {
            Value::Tuple(fields) => match fields.as_slice() {
                [Value::Text(key), Value::Action(run)] if key == pressed => Some(run),
                _ => None,
            },
            _ => None,
        });
        match run {
            // A key of an action that does not apply to the item does what its menu
            // does, which leaves the action out: nothing.
            Some(run) if !matches!(self.core.inapplicable(run, &self.live.viewer), Ok(Some(_))) => {
                self.run_in(run, gesture)
            }
            _ => Ok(InvokeOutcome::Done(Value::Unit)),
        }
    }

    /// Run an action reference a control applies: a platform action, an action of a package,
    /// or `a.then(b)`, which runs `b` once `a` is done.
    fn run(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        self.run_in(aref, None)
    }

    /// [`ViewInstance::run`], its changes one part of the person's act `gesture`.
    fn run_in(
        &self,
        aref: &ActionRef,
        gesture: Option<&str>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        match aref.action.as_str() {
            // Many of the window's own steps (`set_call_volume` for each person of a call) are
            // the window's to take, one after another: only a package's actions are one change
            // of the store.
            crate::exec::MANY_ACTION if window_steps(aref) => {
                let [Value::List(steps)] = aref.args.as_slice() else {
                    return Err(RuntimeError::BadArgs(
                        "`many` takes a list of actions".into(),
                    ));
                };
                for step in steps {
                    let Value::Action(step) = step else {
                        return Err(RuntimeError::BadArgs("`many` takes actions".into()));
                    };
                    match self.run_in(step, gesture)? {
                        InvokeOutcome::Done(_) => {}
                        other => return Ok(other),
                    }
                }
                Ok(InvokeOutcome::Done(Value::Unit))
            }
            SET_ACTION => self.platform_set(aref),
            KEEP_ACTION => self.platform_keep(aref),
            OPEN_ACTION => self.platform_open(aref),
            REVIEW_ACTION => self.review_mark(aref),
            UNDO_ACTION => self.undo_step(aref),
            MARK_SEEN_ACTION => self.mark_seen(aref),
            CHOOSE_CASE_ACTION => self.choose_case(aref),
            THEN_ACTION => self.run_then(aref, gesture),
            SHORTCUT_ACTION => self.platform_shortcut(aref, gesture),
            ANSWER_ACTION => self.answer_steps(aref, gesture),
            COPY_LINK_ACTION | EXPORT_ACTION => self.system_offer(aref),
            // The system's Copy link as a resource's own verb (`card.share_link()`): an action
            // of the type, whose id ends in the verb the system answers it by.
            action
                if crate::host_ui::system_verb(action)
                    == Some(tilo_actions::remote::SHARE_LINK_ACTION) =>
            {
                self.copy_link_of(aref)
            }
            CHOOSE_ACTION => self.platform_choose(aref, false, false),
            CHOOSE_EDITING_ACTION => self.platform_choose(aref, false, true),
            CHOOSE_FOLDER_ACTION => self.platform_choose(aref, true, false),
            SAVE_ACTION => self.platform_save(aref),
            BLUR_ACTION => {
                self.events.borrow_mut().push(ViewEvent::EndEdit);
                Ok(InvokeOutcome::Done(Value::Unit))
            }
            CLOSE_ACTION => {
                self.events.borrow_mut().push(ViewEvent::CloseWindow);
                Ok(InvokeOutcome::Done(Value::Unit))
            }
            CALL_INPUT_ACTION => self.call_input(aref),
            CALL_VOLUME_ACTION => self.call_volume(aref),
            CALL_ACTION => self.call_step(aref),
            RUN_LINE_ACTION => self.run_line(aref),
            _ => {
                // What the action was invoked from, for `window_state`: the window and the view
                // its control stands in. A nested action of the same run (a `then` step, a call)
                // keeps it, and it is put back once the control's run is done.
                let context = self.live.bound(crate::live::ROOT).map(|root| {
                    crate::view_state::WindowContext {
                        window: self.live.window.clone(),
                        module: root.target.module.clone(),
                        view: root.target.view.clone(),
                    }
                });
                let held = self.core.invoking_window.replace(context);
                // A person's press in their window is the person acting: what it calls on the
                // way runs as theirs. It is not their answer to what the workspace asks them
                // first, which waits for their yes on the system's confirm, unless Shift was held
                // on what can be undone. Anyone else's (an agent driving a view) is decided as a
                // call of theirs.
                let viewer = &self.live.viewer;
                let outcome = if viewer
                    .member_id()
                    .is_some_and(crate::ids::MemberId::is_person)
                {
                    self.core.invoke_pressed(
                        viewer,
                        aref,
                        aref.from.as_ref(),
                        (gesture, self.shift_held.get()),
                    )
                } else {
                    self.core.invoke_direct_in(viewer, aref, gesture)
                };
                self.core.invoking_window.replace(held);
                outcome
            }
        }
    }

    fn review_mark(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let (Some(Value::Text(run)), Some(Value::Ref(r))) = (aref.args.first(), aref.args.get(1))
        else {
            return Err(RuntimeError::BadArgs("a malformed change mark".into()));
        };
        self.events.borrow_mut().push(ViewEvent::Review {
            run: run.clone(),
            target: ResourceRef::clone(&r.target),
        });
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    /// Copy link or Export was chosen on a resource: the window's host mints its link and
    /// copies it, or saves it as a file.
    fn system_offer(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::Ref(r), Value::Text(app)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs(format!(
                "a malformed `{}`",
                aref.action
            )));
        };
        let target = ResourceRef::clone(&r.target);
        let app = app.clone();
        self.events
            .borrow_mut()
            .push(if aref.action == EXPORT_ACTION {
                ViewEvent::Export { target, app }
            } else {
                ViewEvent::CopyLink { target, app }
            });
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    /// The system's Copy link called as the resource type's own verb: the resource is the call's
    /// first argument, and the app whose resource it is comes from its type.
    fn copy_link_of(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let Some(Value::Ref(r)) = aref.args.first() else {
            return Err(RuntimeError::BadArgs("a malformed copy link".into()));
        };
        let app = {
            let registry = self.core.registry.borrow();
            registry
                .type_id(&r.target.ty)
                .and_then(|ty| registry.type_info(&ty))
                .map(|(pkg, _)| pkg.meta.app.clone())
        };
        let Some(app) = app else {
            return Err(RuntimeError::NotFound(format!("type {}", r.target.ty)));
        };
        self.system_offer(&ActionRef::new(
            COPY_LINK_ACTION,
            vec![Value::Ref(r.clone()), Value::text(app)],
        ))
    }

    fn undo_step(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let ask = match aref.args.as_slice() {
            [Value::Text(kind), Value::Text(change), Value::Ref(r)] if kind == "step" => {
                UndoAsk::Step {
                    change: change.clone(),
                    target: ResourceRef::clone(&r.target),
                }
            }
            [Value::Text(kind), Value::Text(run), _] if kind == "run" => {
                UndoAsk::Run { run: run.clone() }
            }
            _ => return Err(RuntimeError::BadArgs("a malformed `undo`".into())),
        };
        self.events.borrow_mut().push(ViewEvent::Undo(ask));
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    fn mark_seen(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::List(ids)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed `mark seen`".into()));
        };
        let ids: Vec<String> = ids
            .iter()
            .filter_map(|id| id.as_text().map(str::to_owned))
            .collect();
        let count = self
            .core
            .mark_all_seen(&self.live.viewer, &ids)
            .map_err(|error| RuntimeError::Package(error.to_string()))?;
        if count > 0 {
            self.events.borrow_mut().push(ViewEvent::Seen { count });
        }
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    /// The case option a person pressed: the field it belongs to takes the case, through the
    /// binding the window's own pick writes.
    fn choose_case(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::Text(option), Value::Text(case)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed case choice".into()));
        };
        let field = self
            .tree()
            .parent_of(option)
            .map(|field| field.key.clone())
            .ok_or_else(|| {
                RuntimeError::NotFound(format!("the field the case `{option}` belongs to"))
            })?;
        Ok(match self.edit(&field, &Value::text(case.as_str()))? {
            EditOutcome::Applied => InvokeOutcome::Done(Value::Unit),
            EditOutcome::Denied => InvokeOutcome::Denied,
            EditOutcome::NeedsApproval => {
                return Err(RuntimeError::BadArgs(
                    "the person must allow this change first".into(),
                ));
            }
        })
    }

    fn run_then(
        &self,
        aref: &ActionRef,
        gesture: Option<&str>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::Action(first), Value::Action(next)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed `then`".into()));
        };
        match self.run_in(first, gesture)? {
            InvokeOutcome::Done(_) if !self.applies_here(next) => {
                Ok(InvokeOutcome::Done(Value::Unit))
            }
            InvokeOutcome::Done(_) => self.run_in(next, gesture),
            // The step waits on a call: what follows it runs when the step is done, not
            // now, while it is paused.
            InvokeOutcome::Waits { run, call } => {
                let invocation = self.core.await_end(&run)?;
                self.then_waiting.borrow_mut().push((
                    invocation,
                    ActionRef::clone(next),
                    gesture.map(str::to_owned),
                ));
                Ok(InvokeOutcome::Waits { run, call })
            }
            // The step waits for a person's yes to the call it is: what follows it runs when
            // that call ends, as one that waits on an answer does.
            InvokeOutcome::Asked(call) => {
                self.core.await_call(&call);
                self.then_waiting.borrow_mut().push((
                    call.clone(),
                    ActionRef::clone(next),
                    gesture.map(str::to_owned),
                ));
                Ok(InvokeOutcome::Asked(call))
            }
            other @ InvokeOutcome::Denied => Ok(other),
        }
    }

    fn call_input(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::Ref(r), Value::Text(input)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed call input".into()));
        };
        self.events.borrow_mut().push(ViewEvent::Call {
            target: ResourceRef::clone(&r.target),
            step: CallStep::SelectInput(input.clone()),
        });
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    fn call_volume(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::Ref(r), Value::Actor(member), Value::Float(percent)] = aref.args.as_slice()
        else {
            return Err(RuntimeError::BadArgs("a malformed call volume".into()));
        };
        let percent = crate::host_ui::call_volume_percent(*percent)
            .map_err(|why| RuntimeError::BadArgs(why.into()))?;
        let member = member
            .member_id()
            .and_then(|member| member.person_id().map(str::to_owned))
            .ok_or_else(|| {
                RuntimeError::BadArgs("a call's volume is a member's, a person of the call".into())
            })?;
        self.events.borrow_mut().push(ViewEvent::Call {
            target: ResourceRef::clone(&r.target),
            step: CallStep::Volume { member, percent },
        });
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    fn call_step(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::Ref(r), Value::Text(step), Value::Bool(on)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed call step".into()));
        };
        let step = CallStep::named(step, *on)
            .ok_or_else(|| RuntimeError::BadArgs(format!("no call step `{step}`")))?;
        self.events.borrow_mut().push(ViewEvent::Call {
            target: ResourceRef::clone(&r.target),
            step,
        });
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    fn run_line(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::Ref(r), Value::Text(line)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed line to run".into()));
        };
        // A line of nothing runs nothing.
        if !line.trim().is_empty() {
            self.events.borrow_mut().push(ViewEvent::RunLine {
                target: ResourceRef::clone(&r.target),
                line: line.trim().to_owned(),
            });
        }
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    /// The steps of a handler after it binds an action's answer: the action runs first, and only
    /// an answer it gives runs the steps, as the view's own code in a frame of its window. An
    /// action that is refused, fails, or waits on a call answers none, so nothing after the
    /// binding runs.
    fn answer_steps(
        &self,
        aref: &ActionRef,
        gesture: Option<&str>,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::Action(first), Value::Text(id), Value::List(captures)] = aref.args.as_slice()
        else {
            return Err(RuntimeError::BadArgs("a malformed handler answer".into()));
        };
        // The steps after a binding run only on the answer its action gives: an action
        // that is refused, fails, or waits on a call answers none.
        match self.run_in(first, gesture)? {
            InvokeOutcome::Done(answer) => {
                let Some(module) = aref.author.as_ref() else {
                    return Err(RuntimeError::NotFound(
                        "the module a handler's steps are in".into(),
                    ));
                };
                // The steps are the view's own code, so they run in a frame of its
                // window: what they read and set is that window's state.
                let Some(package) = self.core.registry.borrow().package(module) else {
                    return Err(RuntimeError::NotFound(format!("module {module}")));
                };
                let Some(steps) = package.meta.at_event.iter().find(|f| f.id == *id) else {
                    return Err(RuntimeError::NotFound(format!(
                        "the steps after a binding: `{id}`"
                    )));
                };
                let mut given = Vec::with_capacity(captures.len().saturating_add(1));
                given.push(answer);
                for capture in captures {
                    given.push(match capture {
                        Value::State { key, scalar } => self.state_now(key, *scalar),
                        other => other.clone(),
                    });
                }
                let next = self.core.call_in_view(
                    &package,
                    &tilolang::ir::at_event_export(id),
                    &steps.params,
                    &given,
                    &self.live.window,
                    &self.live.viewer,
                    &steps.result,
                )?;
                match next {
                    Value::Action(mut next) => {
                        self.read_states(&mut next)?;
                        self.run_in(&next, gesture)
                    }
                    other => Err(RuntimeError::BadArgs(format!(
                        "a handler's steps answer an action, not {}",
                        other.kind_name()
                    ))),
                }
            }
            other => Ok(other),
        }
    }

    /// Whether the follow-up `next` of a `then` applies to what it acts on, as its `applies`
    /// conditions say. One that does not is passed over: `applies` says the action is not
    /// offered where it stands (a place chosen closes a search only while one shows), not that
    /// the gesture went wrong, which `require` says and which still fails the press.
    fn applies_here(&self, next: &ActionRef) -> bool {
        let Some(Value::Ref(target)) = next.args.first() else {
            return true;
        };
        !matches!(
            self.core
                .applies_reading(&next.action, target, &self.live.viewer),
            Ok((Some(_), _))
        )
    }

    /// `file.choose(a, accept)`: ask the person for files and run `a` with what they chose, as
    /// the argument it still needs. A parameter of `File` takes the one file chosen;
    /// `List<File>` takes every file; `file.choose_folder` fills a `Text` with the folder the
    /// person chose, or a `List<File>` with the files it holds. What the person picks is a
    /// [`crate::blob::FileValue`], so a file's invocation records its address and never its
    /// bytes; a folder is the reference the chooser answered, which is what `File.list`,
    /// `File.read` and `File.watch` take.
    fn platform_choose(
        &self,
        aref: &ActionRef,
        folder: bool,
        edit: bool,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let (next, accept, then) = match aref.args.as_slice() {
            [Value::Action(next), Value::List(accept)] => (next, accept, None),
            [Value::Action(next), Value::List(accept), Value::Opt(after)] => {
                (next, accept, after.as_deref())
            }
            _ => return Err(RuntimeError::BadArgs("a malformed `file.choose`".into())),
        };
        let then = match then {
            Some(Value::Action(after)) => Some(after.clone()),
            None => None,
            Some(other) => return Err(no_chooser_handler(other)),
        };
        let accept: Vec<String> = accept
            .iter()
            .filter_map(|media| media.as_text().map(str::to_owned))
            .collect();
        let (filled, request) = self.chooser_asked(next, folder, edit, &accept)?;
        let many = matches!(filled, Filled::Many);
        let filled_value = {
            let blobs = self.core.blobs.borrow();
            let Some(blobs) = blobs.as_ref() else {
                return Err(RuntimeError::Package(
                    "this host chooses no files".to_owned(),
                ));
            };
            if matches!(filled, Filled::Folder) {
                match blobs.choose_folder(&request) {
                    Ok(Some(reference)) => Value::text(reference),
                    Ok(None) => return self.chooser_answered(then, true, ""),
                    Err(why) => return self.chooser_answered(then, false, &why),
                }
            } else {
                let chosen = match blobs.choose(&request) {
                    Ok(chosen) => chosen,
                    Err(why) => return self.chooser_answered(then, false, &why),
                };
                match chosen_files_value(chosen, many, &accept)? {
                    Some(value) => value,
                    None => return self.chooser_answered(then, true, ""),
                }
            }
        };
        let filled = ActionRef {
            args: next
                .args
                .iter()
                .cloned()
                .chain(std::iter::once(filled_value))
                .collect(),
            ..next.clone()
        };
        self.run(&filled)
    }

    /// What the chooser asks for, and what the action leaves out for it to fill: the request the
    /// device's chooser answers.
    fn chooser_asked(
        &self,
        next: &ActionRef,
        folder: bool,
        edit: bool,
        accept: &[String],
    ) -> Result<(Filled, crate::blob::FileRequest), RuntimeError> {
        let Some(package) = self.core.registry.borrow().action_package(&next.action) else {
            return Err(RuntimeError::NotFound(format!("action {}", next.action)));
        };
        let Some(action) = package.action(&next.action) else {
            return Err(RuntimeError::NotFound(format!("action {}", next.action)));
        };
        let next_param = action.params.get(next.args.len());
        let fills =
            next_param.and_then(|param| crate::meta::parse_ty(&param.ty, &package.meta.app).ok());
        let filled = match (folder, fills) {
            (false, Some(Ty::File)) => Filled::One,
            (_, Some(Ty::List(inner))) if inner.as_ref() == &Ty::File => Filled::Many,
            (true, Some(Ty::Text)) => Filled::Folder,
            _ => {
                return Err(RuntimeError::BadArgs(format!(
                    "`{}` has no {} parameter next: {}",
                    crate::platform::label_of(action),
                    if folder { "folder" } else { "file" },
                    if folder {
                        "`file.choose_folder` fills a `Text` with the folder the person chose, \
                         or a `List<File>` with the files it holds"
                    } else {
                        "a file fills the parameter it stands for"
                    }
                )));
            }
        };
        let many = matches!(filled, Filled::Many);
        let label = crate::platform::label_of(action);
        let request = crate::blob::FileRequest {
            title: if folder {
                format!("Choose a folder for {label}")
            } else {
                label
            },
            name: String::new(),
            accept: accept.to_vec(),
            multiple: many && !folder,
            folder,
            edit: edit && !folder && !many,
        };
        Ok((filled, request))
    }

    /// What a chooser came to, told to the action the app named with `then:`: that it was put
    /// away, or why it was refused. With no handler the refusal is the call's failure, as it was
    /// before `then:`; a dismissal was and is nothing.
    fn chooser_answered(
        &self,
        then: Option<ActionRef>,
        cancelled: bool,
        refused: &str,
    ) -> Result<InvokeOutcome, RuntimeError> {
        let Some(mut after) = then else {
            return if refused.is_empty() {
                Ok(InvokeOutcome::Done(Value::Unit))
            } else {
                Err(RuntimeError::Package(refused.to_owned()))
            };
        };
        let outcome = Value::Tuple(vec![Value::Bool(cancelled), Value::text(refused)]);
        fill_event(&mut after, Some(&outcome), Held::default())
            .map_err(|()| RuntimeError::BadArgs("a malformed `file.choose`".into()))?;
        self.run(&after)
    }

    /// `file.save(a, name, then: b)`: run `a`, and put what it made — text, or a file — in a
    /// file the person names. `b`, when given, runs with what the save came to: where it was
    /// written, that the chooser was closed, or why it was refused. What is saved is the
    /// device's: nothing is recorded.
    fn platform_save(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let (next, name, then) = match aref.args.as_slice() {
            [Value::Action(next), Value::Text(name)] => (next, name, None),
            [Value::Action(next), Value::Text(name), Value::Opt(after)] => {
                (next, name, after.as_deref())
            }
            _ => return Err(RuntimeError::BadArgs("a malformed `file.save`".into())),
        };
        let then = match then {
            Some(Value::Action(after)) => Some(after.clone()),
            None => None,
            Some(other) => {
                return Err(RuntimeError::BadArgs(format!(
                    "`file.save`'s `then:` takes an action, not {}",
                    other.kind_name()
                )));
            }
        };
        let made = match self.run(next)? {
            InvokeOutcome::Done(made) => made,
            other => return Ok(other),
        };
        // An action that could not make what it saves answers nothing: there is nothing to save,
        // and what went wrong is the action's own notice.
        let made = match made {
            Value::Opt(Some(held)) => *held,
            Value::Opt(None) => return Ok(InvokeOutcome::Done(Value::Unit)),
            other => other,
        };
        let (content, media) = match &made {
            Value::Text(text) => (text.clone().into_bytes(), crate::blob::media_of(name)),
            Value::File(file) => {
                let bytes = self
                    .core
                    .blobs
                    .borrow()
                    .as_ref()
                    .and_then(|blobs| blobs.read(&file.hash))
                    .ok_or_else(|| {
                        RuntimeError::Package(format!(
                            "the content of `{}` is not on this device",
                            file.name
                        ))
                    })?;
                (bytes, file.media.clone())
            }
            other => {
                return Err(RuntimeError::BadArgs(format!(
                    "`file.save` saves text or a file, and the action made a {}",
                    other.kind_name()
                )));
            }
        };
        // A file made by an export stands under the name it gives itself, as the system's one
        // Export… saves it: nothing else knows that format's name.
        let name = if name.is_empty() {
            match &made {
                Value::File(file) if !file.name.is_empty() => file.name.clone(),
                _ => {
                    return Err(RuntimeError::BadArgs(
                        "`file.save` names what it saves, and what it made has no name of its own"
                            .to_owned(),
                    ));
                }
            }
        } else {
            name.clone()
        };
        let request = crate::blob::SaveRequest { name, media };
        let saved = {
            let blobs = self.core.blobs.borrow();
            let Some(blobs) = blobs.as_ref() else {
                return Err(RuntimeError::Package("this host saves no files".to_owned()));
            };
            blobs.save(&request, &content)
        };
        let Some(mut after) = then else {
            saved.map_err(RuntimeError::Package)?;
            return Ok(InvokeOutcome::Done(Value::Unit));
        };
        // What the save came to, as the app reads it: where it went, that the chooser was
        // closed, or why it was refused.
        let outcome = match &saved {
            Ok(crate::blob::Saved::At(to)) => {
                Value::Tuple(vec![Value::text(to), Value::Bool(false), Value::text("")])
            }
            Ok(crate::blob::Saved::Cancelled) => {
                Value::Tuple(vec![Value::text(""), Value::Bool(true), Value::text("")])
            }
            Err(why) => Value::Tuple(vec![Value::text(""), Value::Bool(false), Value::text(why)]),
        };
        fill_event(&mut after, Some(&outcome), Held::default())
            .map_err(|()| RuntimeError::BadArgs("a malformed `file.save`".into()))?;
        self.run(&after)
    }

    /// `set(name, value)`: change the view state slot the reference names, in this view's
    /// person's window. A slot belongs to the person it was rendered for; nobody presses
    /// another's controls.
    fn platform_set(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [window, actor, state, value] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed `set`".into()));
        };
        let text = |v: &Value| v.as_text().map(str::to_owned);
        let actor = match actor {
            Value::Opt(Some(who)) => match who.as_ref() {
                Value::Actor(who) => Some(who.clone()),
                _ => None,
            },
            Value::Opt(None) => None,
            _ => return Err(RuntimeError::BadArgs("a malformed `set`".into())),
        };
        let (Some(window), Some(state)) = (text(window), text(state)) else {
            return Err(RuntimeError::BadArgs("a malformed `set`".into()));
        };
        let key = StateKey {
            window,
            actor,
            state,
            declared: true,
        };
        self.core.view_states.borrow_mut().write(key, value.clone());
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    /// What the host wires to a selection it keeps: put what the control reported in this
    /// window's slot `state`, which is the runtime's reading and not kept in a record: a
    /// selection is where a person is, not what they set.
    fn platform_keep(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [state, value] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed `keep`".into()));
        };
        let Some(state) = state.as_text() else {
            return Err(RuntimeError::BadArgs("a malformed `keep`".into()));
        };
        let key = StateKey::runtime(
            self.live.window.clone(),
            Some(self.live.viewer.clone()),
            state.to_owned(),
        );
        // A scalar is boxed as a slot holds it, so a view reads back what the person moved.
        let value = match value {
            Value::Int(_) | Value::Float(_) | Value::Bool(_) => Value::some(value.clone()),
            other => other.clone(),
        };
        self.core.view_states.borrow_mut().set(key, value);
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    /// `open(r, view, front)`: the host goes to the window of `r`. The view and whether a narrow
    /// window brings it in front are left out where a host wires the reference itself (a change
    /// mark's item), and take what `open` declares for them: the type's default view, in front.
    fn platform_open(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let Some(Value::Ref(r)) = aref.args.first() else {
            return Err(RuntimeError::BadArgs("a malformed `open`".into()));
        };
        let view = aref
            .args
            .get(1)
            .and_then(Value::as_text)
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        let front = !matches!(aref.args.get(2), Some(Value::Bool(false)));
        self.events.borrow_mut().push(ViewEvent::Open {
            target: ResourceRef::clone(&r.target),
            view,
            front,
        });
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    /// The field `field_id` of `target`, with the package that declares its type. Another
    /// app's resource, composed here, is typed `<app>#<Type>`: its declaration is the one its
    /// loaded package gives that type.
    fn bound_field(
        &self,
        target: &ResourceRef,
        field_id: &str,
    ) -> Result<(Rc<Package>, crate::registry::FieldInfo), RuntimeError> {
        let registry = self.core.registry.borrow();
        let (pkg, info) = registry
            .type_id(&target.ty)
            .and_then(|ty| registry.type_info(&ty))
            .ok_or_else(|| RuntimeError::NotFound(format!("type {}", target.ty)))?;
        let field = info
            .field(field_id)
            .cloned()
            .ok_or_else(|| RuntimeError::NotFound(format!("field {field_id}")))?;
        Ok((pkg, field))
    }

    /// What the control `key` is bound to holds now: the field's value, or the view state's
    /// (its default while it was never set).
    pub fn bound_value(&self, key: &str) -> Option<Value> {
        let binding = self.live.find(key)?.binding.clone()?;
        match &binding.target {
            BindTarget::Field { target, field } => {
                let (_, f) = self.bound_field(target, field).ok()?;
                self.core.read_value(target, &f, None).ok()
            }
            BindTarget::State { key, at, .. } => {
                let stored = self.core.view_states.borrow().get(key).cloned();
                let value = stored.or_else(|| self.live.state_seen(key));
                match at {
                    // A place the list does not reach yet holds nothing typed.
                    Some(at) => Some(match value {
                        Some(Value::List(items)) => {
                            items.get(*at).cloned().unwrap_or_else(|| Value::text(""))
                        }
                        _ => Value::text(""),
                    }),
                    None => Some(crate::host_res::unboxed(value?)),
                }
            }
        }
    }

    /// The resource whose field the control `key` edits, as the view draws it now: the binding a
    /// `begin` handler made counts, as [`Self::edit_in`] counts it. `None` for a control the
    /// view draws no such node for, or one bound to the view's own state.
    ///
    /// A control the view draws for an item it shows still names that item's field, so a
    /// window's edit of it lands in the item it was made in whatever the view shows by the time
    /// it arrives.
    pub fn bound_field_target(&self, key: &str) -> Option<ResourceRef> {
        let node = self.live.find(key)?;
        let binding = node.binding.clone().or_else(|| self.begun_binding(key))?;
        binding.field().map(|(at, _)| at.clone())
    }

    /// Whether the control `key` is the view's own state: it holds no resource's field, and is
    /// the view's wherever the view draws it.
    pub fn bound_state(&self, key: &str) -> bool {
        self.live
            .find(key)
            .and_then(|node| node.binding.as_ref())
            .is_some_and(|binding| binding.field().is_none())
    }

    /// The text the control `key` holds for the resource `target`: the field it edits, read at
    /// `target`, which the view need not be drawing now, or the view's own state's text. `None`
    /// when it edits no text.
    ///
    /// A window sends what a person typed into a field of the item they typed it in, and the
    /// app lays it over that item's text, never over the text of whatever the page shows when
    /// the typing arrives. A control the view draws for a record held inside the item it shows
    /// (a form of a draft, kept in the document's own record) edits that record's field: the
    /// text it holds is read where the edit lands, not from the document the window named.
    pub fn text_shown(&self, key: &str, target: &ResourceRef) -> Option<String> {
        let node = self.live.find(key)?;
        let binding = node.binding.clone().or_else(|| self.begun_binding(key))?;
        let Some((bound, field)) = binding.field() else {
            return match self.bound_value(key)? {
                Value::Text(text) => Some(text),
                _ => None,
            };
        };
        let at = if self.bound_field(target, field).is_ok() {
            target
        } else {
            bound
        };
        let (_, f) = self.bound_field(at, field).ok()?;
        match self.core.read_value(at, &f, None).ok()? {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    /// An edit of a control bound to view state: the slot changes, nothing is recorded. Text
    /// typed into a number's input is read as that number.
    fn edit_state(
        &self,
        key: &StateKey,
        ty: &str,
        value: &Value,
    ) -> Result<EditOutcome, RuntimeError> {
        let ty = parse_ty(ty, "").map_err(RuntimeError::BadArgs)?;
        let bad = || RuntimeError::BadArgs(format!("a {} does not fit {ty}", value.kind_name()));
        let stored = match (&ty, value) {
            (Ty::Text, Value::Text(t)) => Value::Text(t.clone()),
            // A rich input's draft: its paragraphs and the references marked in them.
            (Ty::RichText, Value::Rich(r)) => Value::Rich(r.clone()),
            (Ty::Int, Value::Int(i)) => Value::some(Value::Int(*i)),
            (Ty::Int, Value::Text(t)) => {
                Value::some(Value::Int(t.trim().parse().map_err(|_| bad())?))
            }
            (Ty::Float, Value::Float(f)) => Value::some(Value::Float(*f)),
            (Ty::Float, Value::Text(t)) => {
                Value::some(Value::Float(t.trim().parse().map_err(|_| bad())?))
            }
            (Ty::Bool, Value::Bool(b)) => Value::some(Value::Int(i64::from(*b))),
            (Ty::Date | Ty::Time, Value::Text(t)) => {
                crate::host_res::check_dated(t, &ty).map_err(RuntimeError::BadArgs)?;
                Value::Text(t.clone())
            }
            (Ty::Option(inner), Value::Text(t)) if matches!(**inner, Ty::Date | Ty::Time) => {
                if t.is_empty() {
                    Value::none()
                } else {
                    crate::host_res::check_dated(t, inner).map_err(RuntimeError::BadArgs)?;
                    Value::some(Value::Text(t.clone()))
                }
            }
            (Ty::Option(inner), Value::Opt(_)) if matches!(**inner, Ty::Date | Ty::Time) => {
                value.clone()
            }
            _ => return Err(bad()),
        };
        self.core
            .view_states
            .borrow_mut()
            .write(key.clone(), stored);
        Ok(EditOutcome::Applied)
    }

    /// An edit of a control bound to the place `at` of a list of texts the view state holds
    /// (`bind: typed.at(i)`): that place takes the text, and a list that does not reach it yet
    /// grows to it with empty texts, so each row of a form types into its own place whatever
    /// order they are typed in.
    fn edit_state_at(
        &self,
        key: &StateKey,
        at: usize,
        value: &Value,
    ) -> Result<EditOutcome, RuntimeError> {
        let Value::Text(text) = value else {
            return Err(RuntimeError::BadArgs(format!(
                "a {} does not fit a place of a List<Text>",
                value.kind_name()
            )));
        };
        let stored = self.core.view_states.borrow().get(key).cloned();
        let mut items = match stored.or_else(|| self.live.state_seen(key)) {
            Some(Value::List(items)) => items,
            _ => Vec::new(),
        };
        while items.len() <= at {
            items.push(Value::text(""));
        }
        if let Some(slot) = items.get_mut(at) {
            *slot = Value::Text(text.clone());
        }
        self.core
            .view_states
            .borrow_mut()
            .write(key.clone(), Value::List(items));
        Ok(EditOutcome::Applied)
    }

    /// Press the node with `key`.
    ///
    /// # Errors
    ///
    /// As [`ViewInstance::fire`].
    pub fn press(&self, key: &str) -> Result<InvokeOutcome, RuntimeError> {
        self.fire(key, "press")
    }

    /// Send the edit of `field` of `target`, a resource of the app `app` this runtime does not
    /// run, to the host's invoker. A record held in a field of the resource stands inside its
    /// reading: the edit goes to the resource itself, at the path down to the field, so an edit
    /// of a nested record is the resource's own field edit like any other.
    fn edit_foreign(
        &self,
        app: String,
        target: &ResourceRef,
        field: &crate::registry::FieldInfo,
        value: &Value,
    ) -> Result<EditOutcome, RuntimeError> {
        use crate::delegate::{Invocation, Invoked};
        let (root, path) = self.core.foreign.borrow().root_of(target);
        let invocation = Invocation::Edit {
            app: AppId::from(app),
            target: root,
            path,
            field: field.name.clone(),
            field_id: field.id.clone(),
            value: value.clone(),
        };
        let answer = match self.core.invoker.borrow().as_ref() {
            Some(invoker) => invoker.invoke(&invocation),
            None => {
                return Err(RuntimeError::Package(format!(
                    "`{}` is a resource of an app this host does not run, and it sends no \
                     change on",
                    target.id_string()
                )));
            }
        };
        match answer {
            Invoked::Done(_) | Invoked::Pending(_) => Ok(EditOutcome::Applied),
            Invoked::Failed(reason) => Err(RuntimeError::Trap(reason)),
            Invoked::Refused(_) => Ok(EditOutcome::Denied),
        }
    }

    /// Set `field` of `target` to `value` by running `action`, the action that settles it, as
    /// the view `view_module`'s person runs a control's action. A field that already holds the
    /// value is no edit, as for any other bound field.
    fn settle(
        &self,
        action: String,
        target: &ResourceRef,
        field: &crate::registry::FieldInfo,
        value: &Value,
        view_module: ModuleHash,
        gesture: Option<&str>,
    ) -> Result<EditOutcome, RuntimeError> {
        // The value as the field holds it, which is what the action's parameter takes: a control
        // hands a person, a day or nothing as its text.
        let value = match to_scalar(value, &field.ty).map_err(RuntimeError::BadArgs)? {
            crate::store::Scalar::Json(json) => {
                crate::value::from_json(&json).map_err(RuntimeError::BadArgs)?
            }
            crate::store::Scalar::Bool(flag) => Value::Bool(flag),
            crate::store::Scalar::Int(number) => Value::Int(number),
            crate::store::Scalar::Float(number) => Value::Float(number),
            crate::store::Scalar::Text(text) => Value::Text(text),
        };
        if self.core.read_value(target, field, None).ok().as_ref() == Some(&value) {
            return Ok(EditOutcome::Applied);
        }
        let target = Value::Ref(crate::value::RefValue {
            target: Rc::new(target.clone()),
            author: Some(view_module.clone()),
        });
        let mut aref = ActionRef::new(action, vec![target, value]);
        aref.author = Some(view_module);
        Ok(match self.run_in(&aref, gesture)? {
            InvokeOutcome::Done(_) | InvokeOutcome::Waits { .. } => EditOutcome::Applied,
            InvokeOutcome::Asked(_) => EditOutcome::NeedsApproval,
            InvokeOutcome::Denied => EditOutcome::Denied,
        })
    }

    /// Edit the field a node is bound to, as an attributed change.
    ///
    /// # Errors
    ///
    /// `NotFound` for a missing node or binding; `BadArgs` for a value the field cannot take.
    pub fn edit(&self, key: &str, value: &Value) -> Result<EditOutcome, RuntimeError> {
        self.edit_in(key, value, None)
    }

    /// What a control handed over, as a field of type `ty` holds it.
    fn as_field_holds(&self, ty: &Ty, value: &Value) -> Result<Value, RuntimeError> {
        // A control hands over what it shows, and the field takes it as its type holds it: a case
        // comes by its name, a whole number stays whole, and a value given for an optional field
        // becomes its value, where an empty text is none.
        let case_of = |key: &crate::meta::TypeKey, case: &str| -> Result<Value, RuntimeError> {
            Ok(Value::Enum(
                self.core
                    .registry
                    .borrow()
                    .enum_value(key, case)
                    .map_err(RuntimeError::BadArgs)?,
            ))
        };
        Ok(match (ty, value) {
            (Ty::Enum(key), Value::Text(case)) => case_of(key, case)?,
            (Ty::Int, Value::Float(number)) => Value::Int(whole(*number)),
            (Ty::Float, Value::Int(number)) => Value::Float(integer(*number)),
            (Ty::Option(_), Value::Opt(_)) => value.clone(),
            (Ty::Option(inner), given) => match (&**inner, given) {
                (Ty::Text, Value::Text(text)) if text.is_empty() => Value::none(),
                (Ty::Enum(key), Value::Text(case)) => Value::some(case_of(key, case)?),
                (Ty::Int | Ty::Float, Value::Int(_) | Value::Float(_)) => {
                    Value::some(hold(inner, given))
                }
                (Ty::Text, Value::Text(_)) | (Ty::Bool, Value::Bool(_) | Value::Int(_)) => {
                    Value::some(given.clone())
                }
                _ => value.clone(),
            },
            (ty, given) => hold(ty, given),
        })
    }

    /// [`ViewInstance::edit`] as one part of the person's act `gesture`: the change it makes
    /// names it, as the event that came with it does, so one undo takes both back.
    ///
    /// # Errors
    ///
    /// As [`ViewInstance::edit`].
    pub fn edit_in(
        &self,
        key: &str,
        value: &Value,
        gesture: Option<&str>,
    ) -> Result<EditOutcome, RuntimeError> {
        let node = self
            .live
            .find(key)
            .ok_or_else(|| RuntimeError::NotFound(format!("a unique node `{key}`")))?;
        let binding = node
            .binding
            .clone()
            .or_else(|| self.begun_binding(key))
            .ok_or_else(|| RuntimeError::NotFound(format!("a binding on `{key}`")))?;
        let view_module = binding.author;
        let (target, field_id) = match binding.target {
            BindTarget::Field { target, field } => (target, field),
            BindTarget::State {
                key, ty, at: None, ..
            } => return self.edit_state(&key, &ty, value),
            BindTarget::State {
                key, at: Some(at), ..
            } => return self.edit_state_at(&key, at, value),
        };
        let (pkg, field) = self.bound_field(&target, &field_id)?;
        let value = self.as_field_holds(&field.ty, value)?;
        // A field an action settles (`settles: t.status`) is set by that action: what it keeps
        // in step with the field (a task's `done`, its subtasks) follows a chip as it follows a
        // menu, and the change is the action's, with its permission, history and undo.
        if let Some(action) = crate::registry::settling_action(&pkg, &field.id) {
            return self.settle(action, &target, &field, &value, view_module, gesture);
        }
        let value = &value;
        let ctx = ExecCtx {
            invoker: self.live.viewer.clone(),
            app: None,
            chain: vec![view_module.clone()],
            approved_by: None,
            identity: None,
            from: None,
            at: None,
            gesture: gesture.map(str::to_owned),
        };
        let decision = self.core.decide_with(
            &ctx,
            &ActionInfo {
                action: "$edit".into(),
                app: AppId::from(pkg.meta.app.as_str()),
                class: EffectClass::Undoable,
                own: view_module == pkg.hash,
                module: pkg.hash.clone(),
                target: None,
                admitted: false,
            },
        );
        match decision {
            Decision::Deny => return Ok(EditOutcome::Denied),
            Decision::Ask => return Ok(EditOutcome::NeedsApproval),
            Decision::Allow => {}
        }
        // A resource of an app this runtime does not run is changed where that app runs: the
        // edit goes to the host's invoker, which sends it to the resource's action API, and the new
        // reading that comes back draws it.
        if crate::foreign::is_foreign(&target) {
            return self.edit_foreign(pkg.meta.app.clone(), &target, &field, value);
        }
        let mutation = field_mutation(&field, value)?;
        // A control hands over the whole of what it holds, so a field drawn again and given
        // back exactly what it already says is no edit: nothing is written, and nothing is
        // recorded as a step of the resource's history.
        let spec = crate::store::FieldSpec {
            id: field.id.clone(),
            kind: field.kind,
        };
        let held = self.core.store.borrow().read(&target, &spec);
        let unchanged = already_holds(&mutation, held);
        if unchanged {
            return Ok(EditOutcome::Applied);
        }
        let change = ChangeNote {
            invoked: None,
            action: "$edit".into(),
            args: serde_json::json!([field.id, to_json(value).map_err(RuntimeError::BadArgs)?]),
            invoker: Some(self.live.viewer.clone()),
            chain: vec![view_module.0.clone()],
            module: view_module.0,
            approved_by: None,
            at: Some(self.core.clock_now()),
            touched: vec![target.id_string()],
            count: 1,
            group: None,
            from: None,
            effects: Vec::new(),
            invocation: None,
            gesture: gesture.map(str::to_owned),
            unfinished: false,
        };
        let mut store = self.core.store.borrow_mut();
        let txn = store.begin()?;
        if let Err(e) = store.apply(txn, &target, &mutation) {
            store.rollback(txn);
            return Err(e.into());
        }
        store.commit(txn, &change)?;
        Ok(EditOutcome::Applied)
    }
}

/// The files the chooser answered, as the value the action takes next: every file of a
/// `List<File>`, or the one a `File` takes. `None` when the person answered nothing. A file
/// larger than one a person may choose, or of a media type the request does not accept, is
/// refused with why.
fn chosen_files_value(
    chosen: Vec<crate::blob::FileValue>,
    many: bool,
    accept: &[String],
) -> Result<Option<Value>, RuntimeError> {
    if chosen.is_empty() {
        return Ok(None);
    }
    for file in &chosen {
        if file.size > crate::blob::MAX_CHOSEN_BYTES {
            return Err(RuntimeError::Package(format!(
                "`{}` is {} bytes: more than the {MAX_CHOSEN} a file may be",
                file.name,
                file.size,
                MAX_CHOSEN = crate::blob::MAX_CHOSEN_BYTES
            )));
        }
        if !crate::blob::accepts(accept, &file.media, &file.name) {
            return Err(RuntimeError::Package(format!(
                "`{}` is a {}, and what is chosen here is {}",
                file.name,
                file.media,
                if accept.is_empty() {
                    "any file".to_owned()
                } else {
                    accept.join(", ")
                }
            )));
        }
    }
    let value = if many {
        Value::List(chosen.into_iter().map(Value::file).collect())
    } else if let [file] = chosen.as_slice() {
        Value::file(file.clone())
    } else {
        return Err(RuntimeError::Package(
            "the chooser answered more than the one file this asks for".to_owned(),
        ));
    };
    Ok(Some(value))
}

/// Fill what the event carries (`value`) into the place an argument holds it: the value the
/// view wrote as the event's own, an option the view set it into, or the part of a tuple the
/// view reads it as. Anything else is a value of the view's own, already as it stands.
fn fill_event_value(held: &mut Value, value: Option<&Value>, keys: Held) -> Result<(), ()> {
    match held {
        Value::Event => *held = value.cloned().ok_or(())?,
        // A press's index, with the keys held with it, as the `host.Press` it is read as.
        Value::EventPress => {
            let Some(Value::Int(index)) = value else {
                return Err(());
            };
            *held = Value::Tuple(vec![
                Value::Int(*index),
                Value::Bool(keys.shift),
                Value::Bool(keys.command),
                Value::Bool(keys.alt),
            ]);
        }
        Value::EventPart(part) => {
            let Some(Value::List(parts)) = value else {
                return Err(());
            };
            *held = parts.get(*part).cloned().ok_or(())?;
        }
        // A view that sets the event's value into a state of its own wraps it in the state's
        // option (`set(editing, c)` on an `Option<Comment>`): the value stands where the
        // wrapper holds it, and the wrapper stays around what the event conveys.
        Value::Opt(Some(inner)) => fill_event_value(inner, value, keys)?,
        // A value the view computes from the event's (`set(expr, x.expr)`) runs once it fires,
        // over the value it carries.
        Value::AtEvent { args, .. } => {
            for arg in args {
                fill_event_value(arg, value, keys)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The modifier keys held with a press, as the window says: what a `host.Press` carries beside
/// the index. `command` is Ctrl, or ⌘ on a Mac.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub shift: bool,
    pub command: bool,
    pub alt: bool,
}

/// Put the value an event carries where the action reference holds it, through `then`: the
/// whole of it, or the part of `[value, at]` a place names.
fn fill_event(aref: &mut ActionRef, value: Option<&Value>, keys: Held) -> Result<(), ()> {
    for arg in &mut aref.args {
        match arg {
            Value::Action(inner) if aref.action == THEN_ACTION || aref.action == ANSWER_ACTION => {
                fill_event(inner, value, keys)?;
            }
            // What a handler's steps are handed besides the answer (`let id = act(x)`): the
            // values it keeps, filled where they hold the event's.
            Value::List(items) if aref.action == ANSWER_ACTION => {
                for item in items {
                    fill_event_value(item, value, keys)?;
                }
            }
            held => fill_event_value(held, value, keys)?,
        }
    }
    Ok(())
}

impl Drop for ViewInstance {
    fn drop(&mut self) {
        let mut store = self.core.store.borrow_mut();
        for s in self.subs.drain(..) {
            store.unsubscribe(s);
        }
        drop(store);
        self.live.release_all(&self.core);
    }
}

fn collect_lists(node: &UiNode, out: &mut Vec<String>) {
    if crate::view::is_virtual(&node.kind) {
        out.push(node.key.clone());
    }
    for child in &node.children {
        collect_lists(child, out);
    }
}

/// `number` as the whole number a field of whole numbers holds, rounded to the nearest.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a setting's whole number is far inside i64"
)]
fn whole(number: f64) -> i64 {
    number.round() as i64
}

/// `number`, a whole number a control shows, as the float a field of floats holds.
#[expect(
    clippy::cast_precision_loss,
    reason = "a setting's whole number is far inside f64"
)]
fn integer(number: i64) -> f64 {
    number as f64
}

/// `given`, a value a control handed its field, as the field's type holds it: a whole number a
/// number control gives a field of whole numbers, and any other as it came.
fn hold(ty: &Ty, given: &Value) -> Value {
    match (ty, given) {
        (Ty::Int, Value::Float(number)) => Value::Int(whole(*number)),
        (Ty::Float, Value::Int(number)) => Value::Float(integer(*number)),
        _ => given.clone(),
    }
}

/// Whether the field already holds what `mutation` would write: a control hands over the whole of
/// what it holds, so a field given back exactly what it says is no edit.
fn already_holds(
    mutation: &Mutation,
    held: Result<crate::store::FieldValue, crate::store::StoreError>,
) -> bool {
    match (mutation, held) {
        // A rich text that says nothing is the same rich text however many empty
        // paragraphs hold the nothing: a control that has never been typed into hands
        // back no paragraph at all, where the field holds the one it started with.
        (Mutation::SetRich { value, .. }, Ok(crate::store::FieldValue::Rich(now))) => {
            now == *value || (now.is_empty() && value.is_empty())
        }
        (Mutation::SetText { text, .. }, Ok(crate::store::FieldValue::Text(now))) => now == *text,
        _ => false,
    }
}

/// The change that sets `field` to `value`, as an edit of a control bound to it gives it.
fn field_mutation(
    field: &crate::registry::FieldInfo,
    value: &Value,
) -> Result<Mutation, RuntimeError> {
    Ok(match (field.kind, value) {
        (FieldKind::Text, Value::Text(t)) => Mutation::SetText {
            field: field.id.clone(),
            text: t.clone(),
        },
        (FieldKind::RichText, Value::Rich(r)) => Mutation::SetRich {
            field: field.id.clone(),
            value: r.clone(),
        },
        (FieldKind::Register, v) => Mutation::SetRegister {
            field: field.id.clone(),
            value: to_scalar(v, &field.ty).map_err(RuntimeError::BadArgs)?,
        },
        _ => {
            return Err(RuntimeError::BadArgs(format!(
                "field `{}` cannot be edited with a {}",
                field.name,
                value.kind_name()
            )));
        }
    })
}

/// Whether `aref` is `many` over steps the window takes itself (the platform's, `$`-named), none
/// of them another `many`: those run one by one where the window is, not as a change of the store.
fn window_steps(aref: &ActionRef) -> bool {
    match aref.args.as_slice() {
        [Value::List(steps)] => {
            !steps.is_empty()
                && steps.iter().all(|step| {
                    matches!(step, Value::Action(step)
                        if step.action.starts_with('$') && step.action != crate::exec::MANY_ACTION)
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod event_value_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn the_event_value_fills_only_what_the_view_left_open;

        phase the_event_value_fills_the_option_a_view_set_it_into;
        // A view that sets what an event carries into a state of its own holds it in the state's
        // option (`set(editing, c)` on an `Option<Comment>`), and the value fills it there: the
        // slot is the item the person opened, not the place the event's value stood in the view.
        {
            let comment = Value::text("said");
            let mut aref = ActionRef::new(
                "$set",
                vec![
                    Value::text("w"),
                    Value::text("ada"),
                    Value::text("editing"),
                    Value::some(Value::Event),
                ],
            );
            fill_event(&mut aref, Some(&comment), Held::default()).expect("the event carries a value");
            assert_eq!(aref.args[3], Value::some(comment));
        }

        phase the_event_value_leaves_what_the_view_wrote_alone;
        // A value the view wrote itself is the view's, whatever the event carries.
        {
            let mut aref = ActionRef::new(
                "$set",
                vec![
                    Value::text("w"),
                    Value::text("ada"),
                    Value::text("editing"),
                    Value::none(),
                ],
            );
            fill_event(&mut aref, Some(&Value::text("said")), Held::default()).expect("the event carries a value");
            assert_eq!(aref.args[3], Value::none());
        }
    }
}

#[cfg(test)]
mod orphan_tests {
    use super::*;

    /// An authority that allows every action these tests run.
    struct Allow;
    impl crate::Authority for Allow {
        fn decide(
            &self,
            _: &crate::Actor,
            _: &[crate::ModuleHash],
            _: &crate::ActionInfo,
        ) -> crate::Decision {
            crate::Decision::Allow
        }
    }

    /// A boundary whose `fragment` the tree does not hold writes nowhere.
    ///
    /// This is the state a swapped branch of a view leaves behind: the boundary made for a view
    /// call is still a boundary, still marked to run by a change it read, and its cached trail of
    /// child indices names a `fragment` that has left the tree. Here the trail is the empty one —
    /// the trail that names the root — so a write through it puts one call's content where the
    /// whole view's belongs, and the diff that follows drops every other child of the screen.
    #[test]
    fn a_boundary_the_tree_does_not_hold_writes_nowhere() {
        let app = tilolang::compile_package(SOURCE, &tilolang::IdsLock::new(), &[])
            .unwrap_or_else(|d| panic!("{d:#?}"));
        let rt = crate::Runtime::new(crate::AmStore::new(), Allow).expect("a runtime");
        rt.load_compiled(&app).expect("the app loads");
        let board = rt.create("Board").expect("a board");
        let mut view = rt
            .open_view(
                &crate::Actor::Member(crate::ids::MemberId::person("me")),
                &view_id(&app, "board"),
                vec![Value::resource(&board)],
            )
            .expect("the view opens");

        let whole = view.tree().count();
        assert!(whole >= 3, "the view draws its children: {whole}");

        // A boundary a swapped-out branch left behind: a call whose `fragment` has left the
        // tree, with the empty trail of a call never placed.
        let target = rt
            .core
            .view_target(&view_id(&app, "chosen"))
            .expect("the chosen view");
        let id = view
            .live
            .plant_orphan("", target, vec![Value::resource(&board)], Vec::new());
        view.poll_patches()
            .expect("a boundary that left is not a failure");

        assert_eq!(
            view.tree().count(),
            whole,
            "the tree keeps every child it drew: {:?}",
            view.live.child_keys()
        );
        assert!(
            view.live.bound(id).is_none(),
            "the boundary that left is forgotten"
        );
    }

    fn view_id(app: &tilolang::Compiled, name: &str) -> String {
        app.meta
            .views
            .iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("no view {name}"))
            .id
            .clone()
    }

    const SOURCE: &str = r#"app so.tilo.banner "Banner"

type Board {
    way: Text
    said: Text
}

root Board

default view board for Board

view chosen(b: Board) {
    section("Chosen") {
        text("chosen says {b.said}")
    }
}

view board(b: Board) {
    stack {
        text("a banner")
        chosen(b)
        text("a footer")
    }
}
"#;
}

/// The first binding in `tree`, if a control of it has one: the module the view's own fields are
/// written by, which a field a `begin` made edits by too.
fn first_binding(tree: &UiNode) -> Option<&Binding> {
    if let Some(binding) = &tree.binding {
        return Some(binding);
    }
    tree.children.iter().find_map(first_binding)
}
