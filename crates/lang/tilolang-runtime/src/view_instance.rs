//! A live view: its tree, the fields and view state it depends on, and the events it can take.
//!
//! A view runs in a window (workspace-model.md, "Windows") for one person. The window's id is
//! what its `window` state is kept under, shared by everyone who opens a view in the same
//! window; `lens` state is kept per person and window.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::authority::{ActionInfo, Decision};
use crate::core::Core;
use crate::error::RuntimeError;
use crate::exec::{ExecCtx, InvokeOutcome};
use crate::host_res::to_scalar;
use crate::host_ui::{OPEN_ACTION, SET_ACTION, THEN_ACTION};
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
        let patches = self.live.poll(&self.core);
        self.subscribe_new();
        patches
    }

    /// What the view did so far: how many boundaries ran, how many there are.
    pub fn stats(&self) -> LiveStats {
        self.live.stats()
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

    /// The lens state `state` of this view's person in its window, when it was ever set.
    #[must_use]
    pub fn lens(&self, state: &str) -> Option<Value> {
        self.core
            .view_states
            .borrow()
            .get(&self.lens_key(state))
            .cloned()
    }

    /// Set the lens state `state` of this view's person in its window.
    pub fn set_lens(&self, state: &str, value: Value) {
        self.core
            .view_states
            .borrow_mut()
            .set(self.lens_key(state), value);
    }

    fn lens_key(&self, state: &str) -> StateKey {
        StateKey {
            window: self.live.window.clone(),
            principal: Some(self.live.viewer.0.clone()),
            state: state.to_owned(),
        }
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
            OPEN_ACTION => self.platform_open(aref),
            THEN_ACTION => {
                let [Value::Action(first), Value::Action(next)] = aref.args.as_slice() else {
                    return Err(RuntimeError::BadArgs("a malformed `then`".into()));
                };
                match self.run(first)? {
                    InvokeOutcome::Done(_) => self.run(next),
                    other => Ok(other),
                }
            }
            _ => self.core.invoke_direct(&self.live.viewer, aref),
        }
    }

    /// `set(name, value)`: change the view state slot the reference names. A lens slot belongs
    /// to the person it was rendered for; nobody presses another's controls.
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
        let key = StateKey {
            window,
            principal: (!principal.is_empty()).then_some(principal),
            state,
        };
        self.core.view_states.borrow_mut().set(key, value.clone());
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    /// `open(r)`: the host goes to the window of `r`.
    fn platform_open(&self, aref: &ActionRef) -> Result<InvokeOutcome, RuntimeError> {
        let (Some(Value::Ref(r)), Some(view)) = (aref.args.first(), aref.args.get(1)) else {
            return Err(RuntimeError::BadArgs("a malformed `open`".into()));
        };
        let view = view.as_text().filter(|v| !v.is_empty()).map(str::to_owned);
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
                let (_, info) = self.core.registry.borrow().type_info(&target.ty)?;
                let f = info.field(field)?.clone();
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
        self.core.view_states.borrow_mut().set(key.clone(), stored);
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
        let (pkg, info) = reg
            .type_info(&target.ty)
            .ok_or_else(|| RuntimeError::NotFound(format!("type {}", target.ty)))?;
        let field = info
            .field(&field_id)
            .cloned()
            .ok_or_else(|| RuntimeError::NotFound(format!("field {field_id}")))?;
        drop(reg);
        let ctx = ExecCtx {
            invoker: self.live.viewer.clone(),
            chain: vec![view_module.clone()],
            approved_by: None,
            origin: None,
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
        let mutation = match (field.kind, value) {
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
        };
        let change = ChangeNote {
            action: "$edit".into(),
            args: serde_json::json!([field.id, to_json(value).map_err(RuntimeError::BadArgs)?]),
            invoker: self.live.viewer.0.clone(),
            chain: vec![view_module.0.clone()],
            module: view_module.0,
            approved_by: None,
            at: Some(self.core.clock_now()),
            touched: vec![target.id_string()],
            group: None,
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

/// Put the value an event carries where the action reference holds it, through `then`: the
/// whole of it, or the part of `[value, at]` a place names.
fn fill_event(aref: &mut ActionRef, value: Option<&Value>) -> Result<(), ()> {
    for arg in &mut aref.args {
        match arg {
            Value::Event => *arg = value.cloned().ok_or(())?,
            Value::EventPart(part) => {
                let Some(Value::List(parts)) = value else {
                    return Err(());
                };
                *arg = parts.get(*part).cloned().ok_or(())?;
            }
            Value::Action(inner) if aref.action == THEN_ACTION => fill_event(inner, value)?,
            _ => {}
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
