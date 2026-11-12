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
    CHOOSE_ACTION, KEEP_ACTION, MARK_SEEN_ACTION, OPEN_ACTION, REVIEW_ACTION, SAVE_ACTION,
    SET_ACTION, THEN_ACTION, UNDO_ACTION,
};
use crate::ids::{DocId, ModuleHash, Principal};
use crate::live::{Live, LiveStats};
use crate::meta::{EffectClass, FieldKind};
use crate::meta::{Ty, parse_ty};
use crate::render::ViewTarget;
use crate::space::Space;
use crate::store::{ChangeNote, ChangeSummary, Mutation, SubId};
use crate::value::{ActionRef, ResourceRef, Value, to_json};
use crate::view::{BindTarget, Patch, UiNode};
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
    /// Run every boundary again at the next poll.
    refresh_all: std::cell::Cell<bool>,
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

impl ViewInstance {
    pub(crate) fn open(
        core: Rc<Core>,
        (viewer, window): (Principal, String),
        target: ViewTarget,
        args: Vec<Value>,
    ) -> Result<Self, RuntimeError> {
        let live = Live::open(&core, (viewer, window), target, args)?;
        let mut me = Self {
            core,
            live,
            spaces: HashMap::new(),
            changes: Rc::default(),
            subs: Vec::new(),
            subscribed: HashSet::new(),
            events: RefCell::new(Vec::new()),
            refresh_all: std::cell::Cell::new(false),
        };
        me.subscribe_new();
        Ok(me)
    }

    /// The window this view runs in.
    pub fn window(&self) -> &str {
        &self.live.window
    }

    /// The person looking.
    pub fn viewer(&self) -> &Principal {
        &self.live.viewer
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
        let heard = std::mem::take(&mut *self.changes.borrow_mut());
        for change in &heard {
            self.live.absorb(&self.core, change);
        }
        self.live.check_states(&self.core);
        if let Some(now) = self.time_passed() {
            self.live.pass_time(now);
        }
        if self.refresh_all.take() {
            self.live.mark_all();
        }
    }

    /// The current tree. Rows of a `list` that were not shown are not in it (for the patches,
    /// such a row is an empty `fragment`): [`Self::list_len`] and [`Self::row_key`] say which
    /// rows a list has.
    pub fn tree(&self) -> &UiNode {
        &self.live.tree
    }

    /// Set the space offered at a boundary: the root (key `""` or the root node's key), or a
    /// nested view call by its node key. Every boundary is offered the root's space; other keys
    /// are kept for the host's layout pass. Returns whether the view now needs to re-run: a
    /// boundary read a `space` value that differs from the one it saw. Crossing no class
    /// boundary it read leaves the view untouched.
    pub fn set_space(&mut self, boundary: &str, space: Space) -> bool {
        self.spaces.insert(boundary.to_owned(), space);
        if !boundary.is_empty() && boundary != self.live.tree.key {
            return false;
        }
        self.live.check_space(space);
        self.is_stale()
    }

    /// The space last set for a boundary.
    pub fn space(&self, boundary: &str) -> Option<Space> {
        self.spaces.get(boundary).copied()
    }

    /// Take `(resource, field)` reads the host took converting what the view shows, beside what
    /// the view read: a card of a record inside the text it holds. A change to one of them runs
    /// the root boundary, so the host converts what the window shows again. A host calls it
    /// after converting; the boundary's own run replaces the reads with what it read then, and
    /// the host registers its own again.
    pub fn shows(&mut self, reads: impl IntoIterator<Item = (String, String)>) {
        self.live.shows(reads);
    }

    /// Run the view again at the next poll although nothing it read changed: what it shows
    /// without reading it (the names of principals) has changed.
    pub fn refresh(&self) {
        self.refresh_all.set(true);
    }

    /// When the time some boundary read (`today` in a query or a filter) next reads
    /// differently, in seconds since 1970 (UTC): the host wakes the view then, and the next
    /// poll runs again exactly the boundaries that read it. `None` when no boundary read the
    /// time: nothing about the time ever wakes the view.
    #[must_use]
    pub fn wakes_at(&self) -> Option<i64> {
        self.live.wakes_at()
    }

    /// The host clock's time, when the time some boundary read reads differently by then. The
    /// clock is read only while some boundary waits for it.
    fn time_passed(&self) -> Option<i64> {
        let at = self.live.wakes_at()?;
        let now = tilolang::calendar::instant(&self.core.clock_now())?;
        (now >= at).then_some(now)
    }

    /// Whether a field or view state some boundary read has changed since it last ran, or the
    /// time one read has moved on.
    pub fn is_stale(&self) -> bool {
        self.refresh_all.get()
            || self.live.is_dirty()
            || self.time_passed().is_some()
            || self
                .changes
                .borrow()
                .iter()
                .any(|c| self.live.reaches(&self.core, c))
            || self.live.states_moved(&self.core)
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

    /// The resources the window shows now, by [`ResourceRef::id_string`]: the view's subject
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

    /// The host shows the row at `open` of `list` standing open into its item: its document is
    /// built with it, the row that stood open before drops its own, and both render again.
    pub fn open_row(&mut self, list: &str, open: Option<&str>) {
        self.live.open_row(list, open);
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
        let Some(mut aref) = self.event_action(key, event) else {
            return Err(RuntimeError::NotFound(format!(
                "the action of event `{event}` on `{key}`"
            )));
        };
        fill_event(&mut aref, value).map_err(|()| {
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
        self.run(&aref)
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
        fill_event(&mut aref, value).map_err(|()| {
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

    /// Set the view state `state` of this view's person in its window.
    pub fn set_view_state(&self, state: &str, value: Value) {
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
            .and_then(|view| view.show.clone())
        else {
            return Ok(false);
        };
        if show.ty != target.ty {
            return Ok(false);
        }
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

    fn state_key(&self, state: &str) -> StateKey {
        StateKey::view(
            self.live.window.clone(),
            self.live.viewer.0.clone(),
            state.to_owned(),
        )
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
                    *arg = self.state_now(key, *scalar && unbox)?;
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
    fn state_now(&self, key: &StateKey, unbox: bool) -> Result<Value, RuntimeError> {
        let stored = self.core.view_states.borrow().get(key).cloned();
        let Some(value) = stored.or_else(|| self.live.state_seen(key)) else {
            return Err(RuntimeError::NotFound(format!(
                "the view state `{}`",
                key.state
            )));
        };
        Ok(match value {
            Value::Opt(Some(held)) if unbox => *held,
            other => other,
        })
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
                Value::State { key, scalar } => self.state_now(key, *scalar)?,
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

    /// Run an action reference a control applies: a platform action, an action of a package,
    /// or `a.then(b)`, which runs `b` once `a` is done.
    fn run(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        match aref.action.as_str() {
            SET_ACTION => self.platform_set(aref),
            KEEP_ACTION => self.platform_keep(aref),
            OPEN_ACTION => self.platform_open(aref),
            REVIEW_ACTION => {
                let (Some(Value::Text(run)), Some(Value::Ref(r))) =
                    (aref.args.first(), aref.args.get(1))
                else {
                    return Err(RuntimeError::BadArgs("a malformed change mark".into()));
                };
                self.events.borrow_mut().push(ViewEvent::Review {
                    run: run.clone(),
                    target: ResourceRef::clone(&r.target),
                });
                Ok(InvokeOutcome::Done(Value::Unit))
            }
            UNDO_ACTION => {
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
            MARK_SEEN_ACTION => {
                let [Value::List(ids)] = aref.args.as_slice() else {
                    return Err(RuntimeError::BadArgs("a malformed `mark seen`".into()));
                };
                let ids: Vec<String> = ids
                    .iter()
                    .filter_map(|id| id.as_text().map(str::to_owned))
                    .collect();
                let count = self
                    .core
                    .mark_all_seen(&self.live.viewer.0, &ids)
                    .map_err(|error| RuntimeError::Package(error.to_string()))?;
                if count > 0 {
                    self.events.borrow_mut().push(ViewEvent::Seen { count });
                }
                Ok(InvokeOutcome::Done(Value::Unit))
            }
            THEN_ACTION => {
                let [Value::Action(first), Value::Action(next)] = aref.args.as_slice() else {
                    return Err(RuntimeError::BadArgs("a malformed `then`".into()));
                };
                match self.run(first)? {
                    InvokeOutcome::Done(_) => self.run(next),
                    other => Ok(other),
                }
            }
            CHOOSE_ACTION => self.platform_choose(aref),
            SAVE_ACTION => self.platform_save(aref),
            _ => self.core.invoke_direct(&self.live.viewer, aref),
        }
    }

    /// `file.choose(a, accept)`: ask the person for a file and run `a` with it, as the argument
    /// it still needs. What the person picks is a [`crate::blob::FileValue`], so the invocation
    /// records its address and never its bytes.
    fn platform_choose(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::Action(next), Value::List(accept)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed `file.choose`".into()));
        };
        let accept: Vec<String> = accept
            .iter()
            .filter_map(|media| media.as_text().map(str::to_owned))
            .collect();
        let request = crate::blob::FileRequest {
            title: {
                let label = self
                    .core
                    .registry
                    .borrow()
                    .action_package(&next.action)
                    .and_then(|package| {
                        package.action(&next.action).map(crate::platform::label_of)
                    });
                label.unwrap_or_else(|| "Choose a file".to_owned())
            },
            name: String::new(),
            accept: accept.clone(),
        };
        let chosen = {
            let blobs = self.core.blobs.borrow();
            let Some(blobs) = blobs.as_ref() else {
                return Err(RuntimeError::Package(
                    "this host chooses no files".to_owned(),
                ));
            };
            blobs.choose(&request).map_err(RuntimeError::Package)?
        };
        let Some(file) = chosen else {
            // The person dismissed the chooser: nothing runs.
            return Ok(InvokeOutcome::Done(Value::Unit));
        };
        if file.size > crate::blob::MAX_CHOSEN_BYTES {
            return Err(RuntimeError::Package(format!(
                "`{}` is {} bytes: more than the {MAX_CHOSEN} a file may be",
                file.name,
                file.size,
                MAX_CHOSEN = crate::blob::MAX_CHOSEN_BYTES
            )));
        }
        if !crate::blob::accepts(&accept, &file.media) {
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
        let Some(package) = self.core.registry.borrow().action_package(&next.action) else {
            return Err(RuntimeError::NotFound(format!("action {}", next.action)));
        };
        let Some(action) = package.action(&next.action) else {
            return Err(RuntimeError::NotFound(format!("action {}", next.action)));
        };
        let next_param = action.params.get(next.args.len());
        let fills_a_file = next_param.is_some_and(|param| {
            crate::meta::parse_ty(&param.ty, &package.meta.app).is_ok_and(|ty| ty == Ty::File)
        });
        if !fills_a_file {
            return Err(RuntimeError::BadArgs(format!(
                "`{}` has no file parameter next: a file fills the parameter it stands for",
                crate::platform::label_of(action)
            )));
        }
        let filled = ActionRef {
            args: next
                .args
                .iter()
                .cloned()
                .chain(std::iter::once(Value::file(file)))
                .collect(),
            ..next.clone()
        };
        self.run(&filled)
    }

    /// `file.save(a, name)`: run `a`, and put what it made — text, or a file — in a file the
    /// person names. What is saved is the device's: nothing is recorded.
    fn platform_save(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [Value::Action(next), Value::Text(name)] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed `file.save`".into()));
        };
        let made = match self.run(next)? {
            InvokeOutcome::Done(made) => made,
            other => return Ok(other),
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
        let request = crate::blob::SaveRequest {
            name: name.clone(),
            media,
        };
        let blobs = self.core.blobs.borrow();
        let Some(blobs) = blobs.as_ref() else {
            return Err(RuntimeError::Package("this host saves no files".to_owned()));
        };
        blobs
            .save(&request, &content)
            .map_err(RuntimeError::Package)?;
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    /// `set(name, value)`: change the view state slot the reference names, in this view's
    /// person's window. A slot belongs to the person it was rendered for; nobody presses
    /// another's controls.
    fn platform_set(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let [window, principal, state, value] = aref.args.as_slice() else {
            return Err(RuntimeError::BadArgs("a malformed `set`".into()));
        };
        let text = |v: &Value| v.as_text().map(str::to_owned);
        let (Some(window), Some(principal), Some(state)) =
            (text(window), text(principal), text(state))
        else {
            return Err(RuntimeError::BadArgs("a malformed `set`".into()));
        };
        let key = StateKey::view(window, principal, state);
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
            Some(self.live.viewer.0.clone()),
            state.to_owned(),
        );
        self.core.view_states.borrow_mut().set(key, value.clone());
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

    /// What the control `key` is bound to holds now: the field's value, or the view state's
    /// (its default while it was never set).
    pub fn bound_value(&self, key: &str) -> Option<Value> {
        let binding = self.live.find(key)?.binding.clone()?;
        match &binding.target {
            BindTarget::Field { target, field } => {
                // Another app's resource, composed here, is typed `<app>#<Type>`: its
                // declaration is the one its loaded package gives that type.
                let f = {
                    let registry = self.core.registry.borrow();
                    let ty = registry.type_id(&target.ty)?;
                    let (_, info) = registry.type_info(&ty)?;
                    info.field(field)?.clone()
                };
                self.core.read_value(target, &f, None).ok()
            }
            BindTarget::State { key, .. } => {
                let stored = self.core.view_states.borrow().get(key).cloned();
                let value = stored.or_else(|| self.live.state_seen(key))?;
                Some(crate::host_res::unboxed(value))
            }
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

    /// Press the node with `key`.
    ///
    /// # Errors
    ///
    /// As [`ViewInstance::fire`].
    pub fn press(&self, key: &str) -> Result<InvokeOutcome, RuntimeError> {
        self.fire(key, "press")
    }

    /// Send the edit of `field` of `target`, a resource of the app `app` this runtime does not
    /// run, to the host's invoker.
    fn edit_foreign(
        &self,
        app: String,
        target: &ResourceRef,
        field: &crate::registry::FieldInfo,
        value: &Value,
    ) -> Result<EditOutcome, RuntimeError> {
        use crate::delegate::{Invocation, Invoked};
        let invocation = Invocation::Edit {
            app,
            target: target.clone(),
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
            Invoked::Done(_) | Invoked::Sent => Ok(EditOutcome::Applied),
            Invoked::Failed(reason) => Err(RuntimeError::Trap(reason)),
            Invoked::Refused(_) => Ok(EditOutcome::Denied),
            Invoked::Asks(_) => Ok(EditOutcome::NeedsApproval),
        }
    }

    /// Edit the field a node is bound to, as an attributed change.
    ///
    /// # Errors
    ///
    /// `NotFound` for a missing node or binding; `BadArgs` for a value the field cannot take.
    pub fn edit(&self, key: &str, value: &Value) -> Result<EditOutcome, RuntimeError> {
        let node = self
            .live
            .find(key)
            .ok_or_else(|| RuntimeError::NotFound(format!("a unique node `{key}`")))?;
        let binding = node
            .binding
            .clone()
            .ok_or_else(|| RuntimeError::NotFound(format!("a binding on `{key}`")))?;
        let view_module = binding.author;
        let (target, field_id) = match binding.target {
            BindTarget::Field { target, field } => (target, field),
            BindTarget::State { key, ty, .. } => return self.edit_state(&key, &ty, value),
        };
        let reg = self.core.registry.borrow();
        // Another app's resource names its type as `app#Type`.
        let (pkg, info) = reg
            .type_id(&target.ty)
            .and_then(|ty| reg.type_info(&ty))
            .ok_or_else(|| RuntimeError::NotFound(format!("type {}", target.ty)))?;
        let field = info
            .field(&field_id)
            .cloned()
            .ok_or_else(|| RuntimeError::NotFound(format!("field {field_id}")))?;
        // A control that picks a case hands over its name (the id the field holds it by): the
        // field takes the case itself, so the name becomes that case before the field is
        // written, as a case written in the language already is.
        let value = match (&field.ty, value) {
            (Ty::Enum(key), Value::Text(case)) => {
                Value::Enum(reg.enum_value(key, case).map_err(RuntimeError::BadArgs)?)
            }
            _ => value.clone(),
        };
        let value = &value;
        drop(reg);
        let ctx = ExecCtx {
            invoker: self.live.viewer.clone(),
            chain: vec![view_module.clone()],
            approved_by: None,
            suggested_by: None,
            identity: None,
            from: None,
            at: None,
        };
        let decision = self.core.decide_with(
            &ctx,
            &ActionInfo {
                action: "$edit".into(),
                app: pkg.meta.app.clone(),
                class: EffectClass::Undoable,
                own: view_module == pkg.hash,
                module: pkg.hash.clone(),
            },
        );
        match decision {
            Decision::Deny => return Ok(EditOutcome::Denied),
            Decision::Ask => return Ok(EditOutcome::NeedsApproval),
            Decision::Allow => {}
        }
        // A resource of an app this runtime does not run is changed where that app runs: the
        // edit goes to the host's invoker, which sends it to the resource's door, and the new
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
        let unchanged = match (&mutation, held) {
            // A rich text that says nothing is the same rich text however many empty
            // paragraphs hold the nothing: a control that has never been typed into hands
            // back no paragraph at all, where the field holds the one it started with.
            (Mutation::SetRich { value, .. }, Ok(crate::store::FieldValue::Rich(now))) => {
                now == *value || (now.is_empty() && value.is_empty())
            }
            (Mutation::SetText { text, .. }, Ok(crate::store::FieldValue::Text(now))) => {
                now == *text
            }
            _ => false,
        };
        if unchanged {
            return Ok(EditOutcome::Applied);
        }
        let change = ChangeNote {
            action: "$edit".into(),
            args: serde_json::json!([field.id, to_json(value).map_err(RuntimeError::BadArgs)?]),
            invoker: self.live.viewer.0.clone(),
            chain: vec![view_module.0.clone()],
            module: view_module.0,
            approved_by: None,
            at: Some(self.core.clock_now()),
            touched: vec![target.id_string()],
            count: 1,
            group: None,
            from: None,
            suggested_by: None,
            effects: Vec::new(),
            invocation: None,
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

/// Fill what the event carries (`value`) into the place an argument holds it: the value the
/// view wrote as the event's own, an option the view set it into, or the part of a tuple the
/// view reads it as. Anything else is a value of the view's own, already as it stands.
fn fill_event_value(held: &mut Value, value: Option<&Value>) -> Result<(), ()> {
    match held {
        Value::Event => *held = value.cloned().ok_or(())?,
        Value::EventPart(part) => {
            let Some(Value::List(parts)) = value else {
                return Err(());
            };
            *held = parts.get(*part).cloned().ok_or(())?;
        }
        // A view that sets the event's value into a state of its own wraps it in the state's
        // option (`set(editing, c)` on an `Option<Comment>`): the value stands where the
        // wrapper holds it, and the wrapper stays around what the event conveys.
        Value::Opt(Some(inner)) => fill_event_value(inner, value)?,
        _ => {}
    }
    Ok(())
}

/// Put the value an event carries where the action reference holds it, through `then`: the
/// whole of it, or the part of `[value, at]` a place names.
fn fill_event(aref: &mut ActionRef, value: Option<&Value>) -> Result<(), ()> {
    for arg in &mut aref.args {
        match arg {
            Value::Action(inner) if aref.action == THEN_ACTION => fill_event(inner, value)?,
            held => fill_event_value(held, value)?,
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

#[cfg(test)]
mod event_value_tests {
    use super::*;

    /// A view that sets what an event carries into a state of its own holds it in the state's
    /// option (`set(editing, c)` on an `Option<Comment>`), and the value fills it there: the
    /// slot is the item the person opened, not the place the event's value stood in the view.
    #[test]
    fn the_event_value_fills_the_option_a_view_set_it_into() {
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
        fill_event(&mut aref, Some(&comment)).expect("the event carries a value");
        assert_eq!(aref.args[3], Value::some(comment));
    }

    /// A value the view wrote itself is the view's, whatever the event carries.
    #[test]
    fn the_event_value_leaves_what_the_view_wrote_alone() {
        let mut aref = ActionRef::new(
            "$set",
            vec![
                Value::text("w"),
                Value::text("ada"),
                Value::text("editing"),
                Value::none(),
            ],
        );
        fill_event(&mut aref, Some(&Value::text("said"))).expect("the event carries a value");
        assert_eq!(aref.args[3], Value::none());
    }
}

#[cfg(test)]
mod orphan_tests {
    use super::*;

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
        let rt = crate::Runtime::new(crate::AmStore::new(), crate::SimpleAuthority::new())
            .expect("a runtime");
        rt.load_compiled(&app).expect("the app loads");
        let board = rt.create("Board").expect("a board");
        let mut view = rt
            .open_view(
                &crate::Principal::new("me"),
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
