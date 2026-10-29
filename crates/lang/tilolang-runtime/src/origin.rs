//! What an outside service says, written into the resources that hold its kinds
//! (`tilolang::origin`, convergence.md "Outside services").
//!
//! The platform's side of an origin (a mail account's connection) gets resources from the
//! service and hands them here as one batch of writes: items inserted into a collection, fields
//! set, items removed. The batch is one change, made by the origin rather than by package code,
//! so history shows it as the service's and undo never takes it back as someone's edit.

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

impl Runtime {
    /// Write what the origin `origin` said as one change, at the time `at`. Each insert's new
    /// item is returned in its place; other writes give `None`.
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
        let mut made = Vec::with_capacity(writes.len());
        for w in writes {
            match self.origin_write(txn, w) {
                Ok(r) => made.push(r),
                Err(e) => {
                    self.core.store.borrow_mut().rollback(txn);
                    return Err(e);
                }
            }
        }
        self.core.store.borrow_mut().commit(txn, &note)?;
        Ok(made)
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
