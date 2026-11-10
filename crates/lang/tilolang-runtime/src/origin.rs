//! What an outside service says, written into the resources that hold its kinds
//! (`tilolang::origin`, convergence.md "Outside services").
//!
//! The platform's side of an origin (a mail account's connection) gets resources from the
//! service and hands them here as writes: items inserted into a collection, fields set, items
//! removed. They are one change, made by the origin rather than by package code, so history
//! shows it as the service's and undo never takes it back as someone's edit. A step of a sync
//! that takes several writes — a conversation, the messages it read, what they add up to — is
//! written as one change too, so it is kept whole or not at all. A change the store does not
//! take leaves nothing behind, and what the service said waits in [`Unwritten`] until the next
//! poll writes it.

use crate::error::RuntimeError;
use crate::host_res::to_scalar;
use crate::meta::FieldKind;
use crate::registry::TypeInfo;
use crate::runtime::Runtime;
use crate::store::{Applied, ChangeNote, FieldSpec, FieldValue, Mutation, NewRecord, Txn};
use crate::value::{ResourceRef, Value};

/// One write of a batch.
#[derive(Clone, Debug)]
pub enum OriginWrite {
    /// A new item of the type `ty` (an id) in the collection `field` of `owner`, with fields.
    Insert {
        owner: ResourceRef,
        field: String,
        ty: String,
        fields: Vec<(String, Value)>,
    },
    /// Fields of a resource. A `Set<Text>` field takes a `Value::List` of its texts.
    Set {
        target: ResourceRef,
        fields: Vec<(String, Value)>,
    },
    /// An item that is gone from the service.
    Remove {
        owner: ResourceRef,
        field: String,
        item: ResourceRef,
    },
}

/// What a service said that the workspace could not take, kept until it can: every step is
/// written again by the origin's next poll, before anything newer, so an answer is never lost
/// with the change that could not keep it.
///
/// An origin holds one of these beside the rest of its state, because taking a step again is the
/// origin's own writing: it takes the queue into [`Unwritten::take_again`], which gives it back
/// once the steps are written.
pub struct Unwritten<T> {
    steps: Vec<T>,
}

impl<T> Default for Unwritten<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Unwritten<T> {
    #[must_use]
    pub fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Whether anything waits to be written.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// The steps that wait, oldest first.
    #[must_use]
    pub fn steps(&self) -> &[T] {
        &self.steps
    }

    /// Keep `step` for the next poll, behind the steps kept before it: what a service said is
    /// taken in in the order it gave it.
    pub fn keep(&mut self, step: T) {
        self.steps.push(step);
    }

    /// The step that waits and `matches`, when one does: a newer answer about the same thing is
    /// taken in with it rather than behind it.
    pub fn waiting_mut(&mut self, mut matches: impl FnMut(&T) -> bool) -> Option<&mut T> {
        self.steps.iter_mut().find(|step| matches(*step))
    }

    /// Whether a step that waits matches `matches`.
    #[must_use]
    pub fn waits(&self, matches: impl FnMut(&T) -> bool) -> bool {
        self.steps.iter().any(matches)
    }

    /// Write each step that waits again, oldest first, by `write`, which says whether the step is
    /// done with: one that is no longer waits, and one that is not keeps its place for the next
    /// poll. While `write` runs the origin holds no steps, so it keeps none — a step it did not
    /// write is kept by giving `false`.
    pub fn take_again(&mut self, mut write: impl FnMut(&mut T) -> bool) {
        let steps = std::mem::take(&mut self.steps);
        let mut left = Vec::with_capacity(steps.len());
        for mut step in steps {
            if !write(&mut step) {
                left.push(step);
            }
        }
        self.steps = left;
    }
}

/// One change of an origin as it is being written: what an origin makes of one answer of a
/// service — the conversations a server started, the messages it read, what they add up to — is
/// written into it as many times as the step takes and committed once, by
/// [`Runtime::write_origin`]. A step the store does not take keeps nothing of itself, so the
/// origin takes the whole step again rather than leaving half of it behind.
pub struct OriginChange<'a> {
    runtime: &'a Runtime,
    txn: Txn,
    note: ChangeNote,
    /// Whether it can still be written: a write that failed took the whole change back.
    open: bool,
}

impl OriginChange<'_> {
    /// Write `writes` into the change. Each insert's new item is returned in its place; other
    /// writes give `None`.
    ///
    /// # Errors
    ///
    /// An unknown type or field, a value that does not fit its field, or a store failure; then
    /// the whole change is taken back and none of what it wrote stays.
    pub fn write(
        &mut self,
        writes: Vec<OriginWrite>,
    ) -> Result<Vec<Option<ResourceRef>>, RuntimeError> {
        if !self.open {
            return Err(RuntimeError::Package(
                "the origin's change was taken back".into(),
            ));
        }
        let mut made = Vec::with_capacity(writes.len());
        for w in writes {
            match self.runtime.origin_write(self.txn, w) {
                Ok(r) => made.push(r),
                Err(error) => {
                    self.take_back();
                    return Err(error);
                }
            }
        }
        Ok(made)
    }

    /// Nothing of the change stays.
    fn take_back(&mut self) {
        if std::mem::replace(&mut self.open, false) {
            self.runtime.core.store.borrow_mut().rollback(self.txn);
        }
    }
}

impl Runtime {
    /// Write what the origin `origin` says as one change, at the time `at`, made of whatever
    /// writes `step` makes — see [`OriginChange::write`]. What `step` gives back is given back
    /// here, once the change is committed.
    ///
    /// # Errors
    ///
    /// What `step` failed with, or a store that cannot take the change; then nothing is written.
    pub fn write_origin<T>(
        &self,
        origin: &str,
        at: Option<String>,
        step: impl FnOnce(&mut OriginChange<'_>) -> Result<T, RuntimeError>,
    ) -> Result<T, RuntimeError> {
        let note = ChangeNote {
            action: "$origin".into(),
            args: serde_json::Value::String(origin.to_owned()),
            invoker: format!("origin:{origin}"),
            chain: Vec::new(),
            module: "origin".into(),
            at,
            ..ChangeNote::default()
        };
        let txn = self.core.store.borrow_mut().begin()?;
        let mut change = OriginChange {
            runtime: self,
            txn,
            note,
            open: true,
        };
        let value = match step(&mut change) {
            Ok(value) => value,
            Err(error) => {
                change.take_back();
                return Err(error);
            }
        };
        if !change.open {
            return Err(RuntimeError::Package(
                "the origin's change was taken back".into(),
            ));
        }
        change.open = false;
        let committed = self.core.store.borrow_mut().commit(txn, &change.note);
        match committed {
            Ok(()) => Ok(value),
            Err(error) => {
                self.core.store.borrow_mut().rollback(txn);
                Err(error.into())
            }
        }
    }

    /// Write what the origin `origin` said as one change of one write, at the time `at`. Each
    /// insert's new item is returned in its place; other writes give `None`.
    ///
    /// # Errors
    ///
    /// An unknown type or field, a value that does not fit its field, or a store failure; then
    /// nothing is written.
    pub fn apply_origin(
        &self,
        origin: &str,
        writes: Vec<OriginWrite>,
        at: Option<String>,
    ) -> Result<Vec<Option<ResourceRef>>, RuntimeError> {
        if writes.is_empty() {
            return Ok(Vec::new());
        }
        self.write_origin(origin, at, |change| change.write(writes))
    }

    fn type_of(&self, ty: &str) -> Result<std::rc::Rc<TypeInfo>, RuntimeError> {
        self.core
            .registry
            .borrow()
            .type_info(ty)
            .map(|(_, t)| t)
            .ok_or_else(|| RuntimeError::NotFound(format!("type {ty}")))
    }

    fn origin_write(&self, txn: Txn, w: OriginWrite) -> Result<Option<ResourceRef>, RuntimeError> {
        match w {
            OriginWrite::Insert {
                owner,
                field,
                ty,
                fields,
            } => {
                let info = self.type_of(&ty)?;
                let new = NewRecord {
                    ty: info.id.clone(),
                    app: String::new(),
                    fields: info.specs(),
                };
                let applied = self.core.store.borrow_mut().apply(
                    txn,
                    &owner,
                    &Mutation::CollInsert {
                        field,
                        item: new,
                        index: None,
                        id: None,
                    },
                )?;
                let Applied::Item(id) = applied else {
                    return Err(RuntimeError::Package(
                        "the store did not return the new item".into(),
                    ));
                };
                let target = owner.item(&id, info.id.clone());
                self.set_fields(txn, &info, &target, fields)?;
                Ok(Some(target))
            }
            OriginWrite::Set { target, fields } => {
                let info = self.type_of(&target.ty)?;
                self.set_fields(txn, &info, &target, fields)?;
                Ok(None)
            }
            OriginWrite::Remove { owner, field, item } => {
                let Some(last) = item.item else {
                    return Err(RuntimeError::BadArgs(
                        "a removed item is a collection item".into(),
                    ));
                };
                self.core.store.borrow_mut().apply(
                    txn,
                    &owner,
                    &Mutation::CollRemove { field, item: last },
                )?;
                Ok(None)
            }
        }
    }

    fn set_fields(
        &self,
        txn: Txn,
        info: &TypeInfo,
        target: &ResourceRef,
        fields: Vec<(String, Value)>,
    ) -> Result<(), RuntimeError> {
        for (id, value) in fields {
            let f = info
                .field_by_id_or_name(&id)
                .ok_or_else(|| RuntimeError::NotFound(format!("field {id} of {}", info.name)))?;
            let mutations = match (f.kind, value) {
                (FieldKind::Register, v) => vec![Mutation::SetRegister {
                    field: f.id.clone(),
                    value: to_scalar(&v, &f.ty).map_err(RuntimeError::BadArgs)?,
                }],
                (FieldKind::Text, Value::Text(text)) => vec![Mutation::SetText {
                    field: f.id.clone(),
                    text,
                }],
                (FieldKind::RichText, Value::Rich(value)) => vec![Mutation::SetRich {
                    field: f.id.clone(),
                    value,
                }],
                (FieldKind::Counter, Value::Int(want)) => {
                    let spec = FieldSpec {
                        id: f.id.clone(),
                        kind: f.kind,
                    };
                    let have = match self.core.store.borrow().read(target, &spec)? {
                        FieldValue::Counter(n) => n,
                        _ => 0,
                    };
                    vec![Mutation::AddCounter {
                        field: f.id.clone(),
                        delta: want.saturating_sub(have),
                    }]
                }
                (FieldKind::Set, Value::List(items)) => {
                    let spec = FieldSpec {
                        id: f.id.clone(),
                        kind: f.kind,
                    };
                    let have = match self.core.store.borrow().read(target, &spec)? {
                        FieldValue::Keys(keys) => keys,
                        _ => Vec::new(),
                    };
                    let want: Vec<String> = items
                        .into_iter()
                        .filter_map(|v| match v {
                            Value::Text(t) => Some(t),
                            _ => None,
                        })
                        .collect();
                    let mut ms: Vec<Mutation> = have
                        .iter()
                        .filter(|k| !want.contains(k))
                        .map(|k| Mutation::SetRemove {
                            field: f.id.clone(),
                            key: k.clone(),
                        })
                        .collect();
                    ms.extend(want.iter().filter(|k| !have.contains(k)).map(|k| {
                        Mutation::SetAdd {
                            field: f.id.clone(),
                            key: k.clone(),
                        }
                    }));
                    ms
                }
                (kind, v) => {
                    return Err(RuntimeError::BadArgs(format!(
                        "an origin cannot write a {} into the {kind:?} field `{}` of `{}`",
                        v.kind_name(),
                        f.name,
                        info.name
                    )));
                }
            };
            for m in mutations {
                self.core.store.borrow_mut().apply(txn, target, &m)?;
            }
        }
        Ok(())
    }
}
