//! A live view: its tree, the fields and view state it depends on, and the events it can take.
//!
//! A view runs in a window (workspace-model.md section 3) for one person. The window's id is
//! what its `window` state is kept under, shared by everyone who opens a view in the same
//! window; `lens` state is kept per person and window.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::authority::{ActionInfo, Decision};
use crate::core::Core;
use crate::error::RuntimeError;
use crate::exec::{ExecCtx, InvokeOutcome};
use crate::handles::Retained;
use crate::host_res::to_scalar;
use crate::host_ui::{OPEN_ACTION, SET_ACTION};
use crate::ids::{DocId, Principal};
use crate::meta::{EffectClass, FieldKind};
use crate::meta::{Ty, parse_ty};
use crate::render::{Rendered, ViewTarget};
use crate::space::Space;
use crate::store::{ChangeNote, Mutation, SubId};
use crate::value::{ActionRef, ResourceRef, Value, to_json};
use crate::view::{BindTarget, Patch, UiNode, diff};
use crate::view_state::StateKey;

/// What a view read, and whether any of it changed since the last render.
#[derive(Default)]
struct Watch {
    reads: HashSet<(String, String)>,
    /// `space` fields read, with the values they had.
    space_reads: Vec<(String, f64)>,
    /// View state slots read, with their generations then.
    state_reads: Vec<(StateKey, u64)>,
    dirty: bool,
}

/// Something a view asks of the host that the runtime does not do itself.
#[derive(Clone, Debug, PartialEq)]
pub enum ViewEvent {
    /// `open(r)` or `open(r, view: v)` was pressed: go to the window of `target`, showing it
    /// through `view`, or through the default view of its type when `None`.
    Open {
        target: ResourceRef,
        view: Option<String>,
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
    viewer: Principal,
    window: String,
    target: ViewTarget,
    args: Vec<Value>,
    space: Space,
    boundaries: HashMap<String, Space>,
    tree: UiNode,
    retained: Vec<Retained>,
    watch: Rc<RefCell<Watch>>,
    subs: Vec<SubId>,
    subscribed: HashSet<DocId>,
    /// The value each view state slot had at the last render, defaults included.
    state_seen: Vec<(StateKey, Value)>,
    events: RefCell<Vec<ViewEvent>>,
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
        let space = Space::default();
        let rendered = core.render(&target, &args, space, (&viewer, &window))?;
        let mut me = Self {
            core,
            viewer,
            window,
            target,
            args,
            space,
            boundaries: HashMap::new(),
            tree: rendered.root.clone(),
            retained: Vec::new(),
            watch: Rc::new(RefCell::new(Watch::default())),
            subs: Vec::new(),
            subscribed: HashSet::new(),
            state_seen: Vec::new(),
            events: RefCell::new(Vec::new()),
        };
        me.adopt(rendered);
        Ok(me)
    }

    /// The window this view runs in.
    pub fn window(&self) -> &str {
        &self.window
    }

    /// The person looking.
    pub fn viewer(&self) -> &Principal {
        &self.viewer
    }

    /// What the view asked of the host since the last call: windows to open.
    pub fn poll_events(&mut self) -> Vec<ViewEvent> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    /// Whether a view state slot the view read changed since it read it.
    fn state_changed(&self) -> bool {
        let states = self.core.view_states.borrow();
        self.watch
            .borrow()
            .state_reads
            .iter()
            .any(|(key, seen)| states.generation(key) != *seen)
    }

    /// Take a render's reads and retained handles as the current state.
    fn adopt(&mut self, rendered: Rendered) {
        let reads: HashSet<(String, String)> = rendered.reads.into_iter().collect();
        for (doc, _) in &reads {
            if self.subscribed.insert(doc.clone()) {
                let watch = Rc::clone(&self.watch);
                let id = self.core.store.borrow_mut().subscribe(
                    doc,
                    Box::new(move |s| {
                        let mut w = watch.borrow_mut();
                        let hit = if s.fields.is_empty() {
                            w.reads.iter().any(|(d, _)| *d == s.doc)
                        } else {
                            s.fields
                                .iter()
                                .any(|f| w.reads.contains(&(s.doc.clone(), f.clone())))
                        };
                        if hit {
                            w.dirty = true;
                        }
                    }),
                );
                self.subs.push(id);
            }
        }
        {
            let mut w = self.watch.borrow_mut();
            w.reads = reads;
            w.space_reads = rendered.space_reads;
            w.state_reads = rendered.state_reads;
        }
        self.state_seen = rendered.state_seen;
        self.retained = rendered.retained;
        self.tree = rendered.root;
    }

    /// The current tree.
    pub fn tree(&self) -> &UiNode {
        &self.tree
    }

    /// Set the space offered at a boundary: the root (key `""` or the root node's key), or a
    /// nested view call by its node key. In v1 nested view calls are inlined by the compiler
    /// and inherit the root's space, so only the root's value is consulted; other keys are
    /// kept for the host's layout pass. Returns whether the view now needs to re-run: it read
    /// a `space` value that differs from the one it saw. Crossing no class boundary it read
    /// leaves the view untouched.
    pub fn set_space(&mut self, boundary: &str, space: Space) -> bool {
        self.boundaries.insert(boundary.to_owned(), space);
        if !boundary.is_empty() && boundary != self.tree.key {
            return false;
        }
        self.space = space;
        let mut w = self.watch.borrow_mut();
        if w.space_reads
            .iter()
            .any(|(name, seen)| space.field(name) != Some(*seen))
        {
            w.dirty = true;
        }
        w.dirty
    }

    /// The space last set for a boundary.
    pub fn space(&self, boundary: &str) -> Option<Space> {
        self.boundaries.get(boundary).copied()
    }

    /// Run the view again at the next poll although nothing it read changed: what it shows
    /// without reading it (the names of principals) has changed.
    pub fn refresh(&self) {
        self.watch.borrow_mut().dirty = true;
    }

    /// Whether a field or view state the view read has changed since the last render.
    pub fn is_stale(&self) -> bool {
        self.watch.borrow().dirty || self.state_changed()
    }

    /// Re-run the view if a field it read changed and return the patches from the old tree
    /// to the new one. Empty when nothing it read changed.
    ///
    /// # Errors
    ///
    /// As rendering: a trap or a malformed tree.
    pub fn poll_patches(&mut self) -> Result<Vec<Patch>, RuntimeError> {
        if !self.is_stale() {
            return Ok(Vec::new());
        }
        self.watch.borrow_mut().dirty = false;
        let rendered = self.core.render(
            &self.target,
            &self.args,
            self.space,
            (&self.viewer, &self.window),
        )?;
        let core = Rc::clone(&self.core);
        let same = move |a: Retained, b: Retained| {
            let frames = core.frames.borrow();
            frames.retained(a) == frames.retained(b)
        };
        let patches = diff(&self.tree, &rendered.root, &same);
        let old = std::mem::take(&mut self.retained);
        self.adopt(rendered);
        let mut frames = self.core.frames.borrow_mut();
        for r in old {
            frames.release(r);
        }
        Ok(patches)
    }

    /// Fire the event `event` of the node with `key`.
    ///
    /// # Errors
    ///
    /// `NotFound` when the node or its event does not exist; otherwise as an invocation.
    pub fn fire(&self, key: &str, event: &str) -> Result<InvokeOutcome, RuntimeError> {
        let node = self
            .tree
            .find(key)
            .ok_or_else(|| RuntimeError::NotFound(format!("a unique node `{key}`")))?;
        let retained = node
            .events
            .get(event)
            .ok_or_else(|| RuntimeError::NotFound(format!("event `{event}` on `{key}`")))?;
        let value = self.core.frames.borrow().retained(*retained).cloned();
        let Some(Value::Action(aref)) = value else {
            return Err(RuntimeError::NotFound(format!("action of `{key}`")));
        };
        match aref.action.as_str() {
            SET_ACTION => self.platform_set(&aref),
            OPEN_ACTION => self.platform_open(&aref),
            _ => self.core.invoke_direct(&self.viewer, &aref),
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
        self.events.borrow_mut().push(ViewEvent::Open {
            target: r.target.clone(),
            view,
        });
        Ok(InvokeOutcome::Done(Value::Unit))
    }

    /// What the control `key` is bound to holds now: the field's value, or the view state's
    /// (its default while it was never set).
    pub fn bound_value(&self, key: &str) -> Option<Value> {
        let binding = self.tree.find(key)?.binding.clone()?;
        match &binding.target {
            BindTarget::Field { target, field } => {
                let (_, info) = self.core.registry.borrow().type_info(&target.ty)?;
                let f = info.field(field)?.clone();
                self.core.read_value(target, &f, None).ok()
            }
            BindTarget::State { key, .. } => {
                let stored = self.core.view_states.borrow().get(key).cloned();
                let value = stored.or_else(|| {
                    self.state_seen
                        .iter()
                        .rev()
                        .find(|(k, _)| k == key)
                        .map(|(_, v)| v.clone())
                })?;
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
            .tree
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
            invoker: self.viewer.clone(),
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
            invoker: self.viewer.0.clone(),
            chain: vec![view_module.0.clone()],
            module: view_module.0,
            approved_by: None,
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

impl Drop for ViewInstance {
    fn drop(&mut self) {
        let mut store = self.core.store.borrow_mut();
        for s in self.subs.drain(..) {
            store.unsubscribe(s);
        }
        let mut frames = self.core.frames.borrow_mut();
        for r in self.retained.drain(..) {
            frames.release(r);
        }
    }
}
