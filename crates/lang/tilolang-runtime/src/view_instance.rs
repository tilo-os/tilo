//! A live view: its tree, the fields it depends on, and the events it can take.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::authority::{ActionInfo, Decision};
use crate::core::Core;
use crate::error::RuntimeError;
use crate::exec::{ExecCtx, InvokeOutcome};
use crate::handles::Retained;
use crate::host_res::to_scalar;
use crate::ids::{DocId, Principal};
use crate::meta::{EffectClass, FieldKind};
use crate::render::{Rendered, ViewTarget};
use crate::space::Space;
use crate::store::{ChangeNote, Mutation, SubId};
use crate::value::{Value, to_json};
use crate::view::{Patch, UiNode, diff};

/// What a view read, and whether any of it changed since the last render.
#[derive(Default)]
struct Watch {
    reads: HashSet<(String, String)>,
    /// `space` fields read, with the values they had.
    space_reads: Vec<(String, f64)>,
    dirty: bool,
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
    target: ViewTarget,
    args: Vec<Value>,
    space: Space,
    boundaries: HashMap<String, Space>,
    tree: UiNode,
    retained: Vec<Retained>,
    watch: Rc<RefCell<Watch>>,
    subs: Vec<SubId>,
    subscribed: HashSet<DocId>,
    /// Whether the tree shows an embedded invocation, and the resolution count it was
    /// rendered at: deciding a suggestion re-renders such a view.
    shows_runs: bool,
    resolutions_seen: u64,
}

impl ViewInstance {
    pub(crate) fn open(
        core: Rc<Core>,
        viewer: Principal,
        target: ViewTarget,
        args: Vec<Value>,
    ) -> Result<Self, RuntimeError> {
        let space = Space::default();
        let rendered = core.render(&target, &args, space, &viewer)?;
        let mut me = Self {
            core,
            viewer,
            target,
            args,
            space,
            boundaries: HashMap::new(),
            tree: rendered.root.clone(),
            retained: Vec::new(),
            watch: Rc::new(RefCell::new(Watch::default())),
            subs: Vec::new(),
            subscribed: HashSet::new(),
            shows_runs: false,
            resolutions_seen: 0,
        };
        me.adopt(rendered);
        Ok(me)
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
        }
        self.retained = rendered.retained;
        self.tree = rendered.root;
        self.shows_runs = rendered.runs;
        self.resolutions_seen = self.core.resolutions.get();
    }

    /// A suggestion was decided since this view rendered an invocation.
    fn runs_changed(&self) -> bool {
        self.shows_runs && self.core.resolutions.get() != self.resolutions_seen
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

    /// Whether a field the view read has changed since the last render.
    pub fn is_stale(&self) -> bool {
        self.watch.borrow().dirty || self.runs_changed()
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
        let rendered = self
            .core
            .render(&self.target, &self.args, self.space, &self.viewer)?;
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
        self.core.invoke_direct(&self.viewer, &aref)
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
        let view_module = binding.author.clone();
        let reg = self.core.registry.borrow();
        let (pkg, info) = reg
            .type_info(&binding.target.ty)
            .ok_or_else(|| RuntimeError::NotFound(format!("type {}", binding.target.ty)))?;
        let field = info
            .field(&binding.field)
            .cloned()
            .ok_or_else(|| RuntimeError::NotFound(format!("field {}", binding.field)))?;
        drop(reg);
        let ctx = ExecCtx {
            invoker: self.viewer.clone(),
            chain: vec![view_module.clone()],
            approved_by: None,
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
        if let Err(e) = store.apply(txn, &binding.target, &mutation) {
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
