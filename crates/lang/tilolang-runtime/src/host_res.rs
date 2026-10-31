//! Imports over resources: reads, mutations, collections, record init, references.
//!
//! Reads are typed by the metadata, never by the guest: the stored shape is turned into a
//! value of the field's declared type. Mutations are checked twice: they need an action
//! frame (so a transaction), and the action's module must own the resource's type (the
//! compiler guarantees both; the host does not trust it).

use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value as Json;

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_f64, ret_i32, ret_i64, ret_void, trap};
use crate::engine::Wasm;
use crate::handles::FrameKind;
use crate::ids::{ModuleHash, Principal};
use crate::meta::{FieldKind, Ty};
use crate::num::{counter_to_float, float_to_counter};
use crate::registry::{FieldInfo, TypeInfo};
use crate::store::{Applied, FieldSpec, FieldValue, Mutation, Scalar, Txn};
use crate::value::{CollRef, RecordInit, RefValue, ResourceRef, Value, from_json, to_json};

/// Everything a mutation import needs to know before it touches the store.
struct MutCtx {
    target: ResourceRef,
    field: FieldInfo,
    txn: Txn,
}

/// The field `field` (by id) of a type.
fn field_in<'a>(info: &'a TypeInfo, field: &str) -> HResult<&'a FieldInfo> {
    match info.field(field) {
        Some(f) => Ok(f),
        None => trap(format!(
            "type `{}` has no field `{field}`; fields are: {}",
            info.name,
            info.field_names()
        )),
    }
}

fn spec(f: &FieldInfo) -> FieldSpec {
    FieldSpec {
        id: f.id.clone(),
        kind: f.kind,
    }
}

fn type_default(ty: &Ty) -> Result<Value, String> {
    Ok(match ty {
        Ty::Int => Value::Int(0),
        Ty::Float => Value::Float(0.0),
        Ty::Bool => Value::Bool(false),
        Ty::Text => Value::text(""),
        Ty::RichText => Value::Rich(crate::rich::RichValue::default()),
        Ty::Option(_) => Value::none(),
        Ty::List(_) | Ty::Set(_) | Ty::Map(_) | Ty::Collection(_) => Value::List(Vec::new()),
        Ty::Tuple(items) => Value::Tuple(items.iter().map(type_default).collect::<Result<_, _>>()?),
        Ty::Unit => Value::Unit,
        Ty::Record(_)
        | Ty::Union(_)
        | Ty::Principal
        | Ty::Date
        | Ty::Time
        | Ty::Identifier
        | Ty::Action
        | Ty::View
        | Ty::Length(_)
        | Ty::Choice(_)
        | Ty::Enum(_)
        | Ty::Result(..)
        | Ty::Error => {
            return Err("the field has no value yet and no default".to_owned());
        }
    })
}

pub(crate) fn declared_default(ty: &Ty, default: Option<&Json>) -> Result<Value, String> {
    match (ty, default) {
        (Ty::Int, Some(j)) => j
            .as_i64()
            .map(Value::Int)
            .ok_or_else(|| "default is not an int".into()),
        (Ty::Float, Some(j)) => j
            .as_f64()
            .map(Value::Float)
            .ok_or_else(|| "default is not a float".into()),
        (Ty::Bool, Some(j)) => j
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| "default is not a bool".into()),
        (Ty::Text | Ty::Date | Ty::Time | Ty::Identifier, Some(j)) => j
            .as_str()
            .map(Value::text)
            .ok_or_else(|| "default is not text".into()),
        (Ty::Enum(_), Some(j)) => from_json(j),
        _ => type_default(ty),
    }
}

/// The scalar to store for `v` in a register of type `ty`.
pub(crate) fn to_scalar(v: &Value, ty: &Ty) -> Result<Scalar, String> {
    match (v, ty) {
        (Value::Int(i), Ty::Bool) => Ok(Scalar::Bool(*i != 0)),
        (Value::Bool(b), Ty::Bool) => Ok(Scalar::Bool(*b)),
        (Value::Int(i), Ty::Int) => Ok(Scalar::Int(*i)),
        (Value::Float(f), Ty::Float) => Ok(Scalar::Float(*f)),
        (Value::Text(s), Ty::Text) => Ok(Scalar::Text(s.clone())),
        (Value::Text(s), Ty::Date | Ty::Time | Ty::Identifier) => {
            check_dated(s, ty)?;
            Ok(Scalar::Text(s.clone()))
        }
        // A control picking an optional day hands its text, empty for none.
        (Value::Text(s), Ty::Option(inner)) if matches!(**inner, Ty::Date | Ty::Time) => {
            if s.is_empty() {
                return to_json(&Value::none()).map(Scalar::Json);
            }
            check_dated(s, inner)?;
            to_json(&Value::some(Value::Text(s.clone()))).map(Scalar::Json)
        }
        (Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Text(_), _) => Err(format!(
            "a {} cannot be stored in a field of that type",
            v.kind_name()
        )),
        (other, _) => to_json(other).map(Scalar::Json),
    }
}

/// Why `text` is not a value of `ty`, a Date or a Time.
pub(crate) fn check_dated(text: &str, ty: &Ty) -> Result<(), String> {
    match ty {
        Ty::Date => tilolang::types::check_date(text),
        Ty::Time => tilolang::types::check_time(text),
        Ty::Identifier => tilolang::identifier::normalize(text).and_then(|held| {
            if held == text {
                Ok(())
            } else {
                Err(format!(
                    "`{text}` is not an identifier as it is held; write `{held}`"
                ))
            }
        }),
        _ => Ok(()),
    }
}

/// A scalar the compiler boxed with `some_i` / `some_f` to pass it as a handle, unboxed. Any
/// other value is returned as it is.
pub(crate) fn unboxed(v: Value) -> Value {
    match v {
        Value::Opt(Some(inner))
            if matches!(*inner, Value::Int(_) | Value::Float(_) | Value::Bool(_)) =>
        {
            *inner
        }
        other => other,
    }
}

/// The key under which a set element or map key is stored.
fn key_of(v: &Value) -> Result<String, String> {
    match v {
        Value::Text(s) => Ok(s.clone()),
        Value::Int(i) => Ok(i.to_string()),
        Value::Float(f) => Ok(f.to_string()),
        Value::Ref(r) => Ok(r.target.id_string()),
        Value::Principal(p) => Ok(p.0.clone()),
        other => Err(format!(
            "a {} cannot be a set element or map key",
            other.kind_name()
        )),
    }
}

/// Separates a key of a map of sets from one of its elements in the entry that stores the
/// element: never part of a key or an element ([`MAP_SET_SEPARATOR`]).
///
/// A `Map<Set<T>>` stores each element of each key's set as an entry of its own,
/// `"{key}\u{1f}{element}" = true`, so two people adding to one key's set at once both stay
/// (as `tilo_resource::Reactions` keys reactions); reading it groups them again.
const MAP_SET_SEPARATOR: char = '\u{1f}';

/// The entry that stores `element` in the set of `key`.
fn map_set_entry(key: &str, element: &str) -> String {
    format!("{key}{MAP_SET_SEPARATOR}{element}")
}

fn inner_ty(ty: &Ty) -> Option<&Ty> {
    match ty {
        Ty::Collection(t) | Ty::Set(t) | Ty::Map(t) | Ty::List(t) | Ty::Option(t) => Some(t),
        _ => None,
    }
}

impl Core {
    /// The type of `ty`'s package, and of the type itself.
    pub(crate) fn type_of_ref(
        &self,
        ty: &str,
    ) -> Result<(Rc<crate::registry::Package>, Rc<TypeInfo>), String> {
        let registry = self.registry.borrow();
        // A resource of an app this runtime does not run names its type `app#Type`: the app's
        // description (`load_foreign`) declares it.
        let id = match crate::foreign::foreign_type(ty) {
            Some(key) => registry.type_for(&key),
            None => Some(ty.to_owned()),
        };
        let id = id.ok_or_else(|| format!("type `{ty}` is not declared by any loaded package"))?;
        registry
            .type_info(&id)
            .ok_or_else(|| format!("type `{ty}` is not declared by any loaded package"))
    }

    pub(crate) fn ref_arg(&self, h: i32) -> HResult<RefValue> {
        self.with_val(h, |v| match v {
            Value::Ref(r) => Ok(r.clone()),
            other => trap(format!(
                "expected a resource reference, got {}",
                other.kind_name()
            )),
        })
    }

    pub(crate) fn lookup_field(
        &self,
        r: &ResourceRef,
        field: &str,
    ) -> HResult<(Rc<TypeInfo>, FieldInfo)> {
        let (_, info) = self.type_of_ref(&r.ty)?;
        let f = field_in(&info, field)?.clone();
        Ok((info, f))
    }

    /// Check a stored or parsed reference against the type stored in the document.
    pub(crate) fn verify_ref(&self, r: &ResourceRef) -> Result<(), String> {
        let stored = self.store.borrow().type_of(r).map_err(|e| e.to_string())?;
        if stored == r.ty {
            Ok(())
        } else {
            Err(format!(
                "reference {} is a `{stored}`, not a `{}`",
                r.id_string(),
                r.ty
            ))
        }
    }

    /// The type a resource is stored as: what an item of a collection of several types is.
    pub(crate) fn stored_type(&self, at: &ResourceRef) -> String {
        self.store.borrow().type_of(at).unwrap_or_default()
    }

    /// Check a reference read from a field. A target that is gone (removed, or not synced yet)
    /// is still a reference: it compares equal and reading its fields fails then. Only a
    /// target of another type than the reference claims is refused.
    pub(crate) fn verify_stored_ref(&self, r: &ResourceRef) -> Result<(), String> {
        match self.store.borrow().type_of(r) {
            Ok(stored) if stored == r.ty => Ok(()),
            Ok(stored) => Err(format!(
                "reference {} is a `{stored}`, not a `{}`",
                r.id_string(),
                r.ty
            )),
            Err(crate::store::StoreError::NotFound(_)) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Re-create the references inside a stored value as the reading module's.
    fn adopt(&self, v: Value, author: Option<&ModuleHash>) -> Result<Value, String> {
        Ok(match v {
            Value::Ref(r) => {
                self.verify_stored_ref(&r.target)?;
                Value::Ref(RefValue {
                    target: r.target,
                    author: author.cloned(),
                })
            }
            Value::List(items) => Value::List(self.adopt_all(items, author)?),
            Value::Tuple(items) => Value::Tuple(self.adopt_all(items, author)?),
            Value::Opt(Some(x)) => Value::some(self.adopt(*x, author)?),
            Value::Enum(mut e) => {
                for v in e.fields.values_mut() {
                    *v = self.adopt(std::mem::replace(v, Value::Unit), author)?;
                }
                Value::Enum(e)
            }
            other => other,
        })
    }

    fn adopt_all(
        &self,
        items: Vec<Value>,
        author: Option<&ModuleHash>,
    ) -> Result<Vec<Value>, String> {
        items.into_iter().map(|x| self.adopt(x, author)).collect()
    }

    fn scalar_value(&self, s: Scalar, author: Option<&ModuleHash>) -> Result<Value, String> {
        Ok(match s {
            Scalar::Bool(b) => Value::Bool(b),
            Scalar::Int(i) => Value::Int(i),
            Scalar::Float(f) => Value::Float(f),
            Scalar::Text(t) => Value::Text(t),
            Scalar::Json(j) => self.adopt(from_json(&j)?, author)?,
        })
    }

    fn item_type(&self, ty: &Ty) -> Result<Rc<TypeInfo>, String> {
        let Some(Ty::Record(key)) = inner_ty(ty) else {
            return Err("the field's element type is not a record".to_owned());
        };
        let reg = self.registry.borrow();
        let id = reg.type_for(key).ok_or_else(|| {
            format!(
                "element type `{}` is not declared by any loaded package",
                key.name
            )
        })?;
        reg.type_info(&id)
            .map(|(_, t)| t)
            .ok_or_else(|| format!("element type `{}` is not loaded", key.name))
    }

    /// Read a field of a resource as a value of its declared type. References in it are
    /// attributed to `author`.
    ///
    /// # Errors
    ///
    /// A message when the store cannot read the field or the stored shape does not fit.
    pub fn read_value(
        &self,
        target: &ResourceRef,
        field: &FieldInfo,
        author: Option<&ModuleHash>,
    ) -> Result<Value, String> {
        // A resource of an app this runtime does not run has no record here: what it shows is
        // its reading, as the session published it.
        if crate::foreign::is_foreign(target) {
            return self.foreign.borrow().field(target, field, author);
        }
        let fv = match self.store.borrow().read(target, &spec(field)) {
            Ok(fv) => fv,
            // A root nobody opened yet reads as it would start (`root T` names it anywhere).
            Err(crate::store::StoreError::NotFound(_)) if target.is_unmade_root() => {
                match field.kind {
                    FieldKind::Collection => FieldValue::Items(Vec::new()),
                    FieldKind::Set => FieldValue::Keys(Vec::new()),
                    FieldKind::Map => FieldValue::Entries(Vec::new()),
                    FieldKind::Counter => FieldValue::Counter(0),
                    FieldKind::Text => FieldValue::Text(String::new()),
                    FieldKind::RichText => FieldValue::Rich(crate::rich::RichValue::default()),
                    FieldKind::Register => FieldValue::Absent,
                }
            }
            Err(e) => return Err(e.to_string()),
        };
        self.field_value(target, field, fv, author)
    }

    /// A field of `target` as the store holds it (`fv`), as a value of its declared type:
    /// [`Self::read_value`] of a value read some other way (as a change wrote it).
    ///
    /// # Errors
    ///
    /// A message when the stored shape does not fit the field.
    pub fn field_value(
        &self,
        target: &ResourceRef,
        field: &FieldInfo,
        fv: FieldValue,
        author: Option<&ModuleHash>,
    ) -> Result<Value, String> {
        match (field.kind, fv) {
            (FieldKind::Register, FieldValue::Absent) => {
                declared_default(&field.ty, field.default.as_ref())
            }
            (FieldKind::Register, FieldValue::Scalar(s)) => self.scalar_value(s, author),
            (FieldKind::Counter, FieldValue::Counter(c)) => Ok(if field.ty == Ty::Float {
                Value::Float(counter_to_float(c))
            } else {
                Value::Int(c)
            }),
            (FieldKind::Text, FieldValue::Text(t)) => Ok(Value::Text(t)),
            (FieldKind::RichText, FieldValue::Rich(r)) => Ok(Value::Rich(r)),
            (FieldKind::Collection, FieldValue::Items(ids)) => {
                let one = match inner_ty(&field.ty) {
                    Some(Ty::Union(_)) => None,
                    _ => Some(self.item_type(&field.ty)?.id.clone()),
                };
                Ok(Value::List(
                    ids.iter()
                        .map(|id| {
                            let ty = match &one {
                                Some(ty) => ty.clone(),
                                None => self.stored_type(&target.item(id, String::new())),
                            };
                            Value::Ref(RefValue {
                                target: Rc::new(target.item(id, ty)),
                                author: author.cloned(),
                            })
                        })
                        .collect(),
                ))
            }
            (FieldKind::Set, FieldValue::Keys(keys)) => self.keys_value(field, keys, author),
            (FieldKind::Map, FieldValue::Entries(entries))
                if matches!(inner_ty(&field.ty), Some(Ty::Set(_))) =>
            {
                let element = inner_ty(&field.ty).and_then(inner_ty);
                // Each element is an entry of its own: grouped by key again, in key order.
                let mut sets: BTreeMap<String, Vec<String>> = BTreeMap::new();
                for (entry, _) in entries {
                    if let Some((key, el)) = entry.split_once(MAP_SET_SEPARATOR) {
                        sets.entry(key.to_owned()).or_default().push(el.to_owned());
                    }
                }
                sets.into_iter()
                    .map(|(key, mut elements)| {
                        elements.sort();
                        Ok(Value::Tuple(vec![
                            Value::Text(key),
                            self.elements_value(element, elements, author)?,
                        ]))
                    })
                    .collect::<Result<Vec<_>, String>>()
                    .map(Value::List)
            }
            (FieldKind::Map, FieldValue::Entries(entries)) => entries
                .into_iter()
                .map(|(k, v)| {
                    Ok(Value::Tuple(vec![
                        Value::Text(k),
                        self.scalar_value(v, author)?,
                    ]))
                })
                .collect::<Result<Vec<_>, String>>()
                .map(Value::List),
            (kind, other) => Err(format!(
                "field `{}` of kind {kind:?} holds {other:?}",
                field.name
            )),
        }
    }

    fn keys_value(
        &self,
        field: &FieldInfo,
        keys: Vec<String>,
        author: Option<&ModuleHash>,
    ) -> Result<Value, String> {
        self.elements_value(inner_ty(&field.ty), keys, author)
    }

    /// The elements a set of `element` type stores as `keys`.
    fn elements_value(
        &self,
        element: Option<&Ty>,
        keys: Vec<String>,
        author: Option<&ModuleHash>,
    ) -> Result<Value, String> {
        let items = keys
            .into_iter()
            .map(|k| match element {
                Some(Ty::Principal) => Ok(Value::Principal(Principal::new(k))),
                Some(Ty::Int) => k
                    .parse::<i64>()
                    .map(Value::Int)
                    .map_err(|e| format!("set element `{k}`: {e}")),
                Some(Ty::Float) => k
                    .parse::<f64>()
                    .map(Value::Float)
                    .map_err(|e| format!("set element `{k}`: {e}")),
                Some(Ty::Record(key)) => {
                    let ty = self
                        .registry
                        .borrow()
                        .type_for(key)
                        .ok_or_else(|| format!("element type `{}` is not loaded", key.name))?;
                    let target = ResourceRef::parse_id(&k, ty).ok_or("malformed set element")?;
                    self.verify_stored_ref(&target)?;
                    Ok(Value::Ref(RefValue {
                        target: Rc::new(target),
                        author: author.cloned(),
                    }))
                }
                _ => Ok(Value::Text(k)),
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Value::List(items))
    }

    fn res_get(&self, rh: i32, fh: i32) -> HResult<Value> {
        let in_flow = matches!(self.frames.borrow().top()?.kind, FrameKind::Flow(_));
        if in_flow {
            // A flow's reads go through its journal, which takes the frames to record them.
            let target = Rc::unwrap_or_clone(self.ref_arg(rh)?.target);
            let field = self.text(fh)?;
            let (_, f) = self.lookup_field(&target, &field)?;
            let author = self.module_of_frame()?;
            let value =
                self.journaled_read(|| Ok(self.read_value(&target, &f, Some(&author))?))?;
            return Ok(whole_map(f.kind, value));
        }
        // The hottest import: a view over a list reads a field of every item. The reference,
        // the field's name and the author are read where they are held, not copied.
        let (value, read, reading) = {
            let frames = self.frames.borrow();
            let target = match frames.get(rh)? {
                Value::Ref(r) => &r.target,
                other => {
                    return trap(format!(
                        "expected a resource reference, got {}",
                        other.kind_name()
                    ));
                }
            };
            let (_, info) = self.type_of_ref(&target.ty)?;
            let f = match frames.get(fh)? {
                Value::Text(field) => field_in(&info, field)?,
                other => return trap(format!("expected text, got {}", other.kind_name())),
            };
            let top = frames.top()?;
            let author = &top.module;
            // A resource of an app this runtime does not run reads from its reading, which the
            // host sets as it follows the session's watch: the slot it moves is what marks the
            // views that read it.
            if crate::foreign::is_foreign(target) {
                let value = self.read_value(target, f, Some(author))?;
                (value, None, Some(crate::foreign::reading_key(target)))
            } else if f.kind == FieldKind::Collection && matches!(top.kind, FrameKind::View(_)) {
                // A collection of a union's types names no one item type: each item's is its own.
                let item = match inner_ty(&f.ty) {
                    Some(Ty::Union(_)) => String::new(),
                    _ => self.item_type(&f.ty)?.id.clone(),
                };
                let coll = Value::Coll(Rc::new(CollRef {
                    target: Rc::clone(target),
                    field: f.id.clone(),
                    item,
                    author: Some(author.clone()),
                    filter: None,
                    order: None,
                }));
                (coll, None, None)
            } else {
                let value = whole_map(f.kind, self.read_value(target, f, Some(author))?);
                (value, Some((target.id_string(), f.id.clone())), None)
            }
        };
        if let Some(key) = reading {
            let generation = self.view_states.borrow().generation(&key);
            let seen = i64::try_from(generation).unwrap_or(i64::MAX);
            self.note_state_read(key, generation, &Value::Int(seen))?;
        }
        if let Some(read) = read
            && let FrameKind::View(vs) = &mut self.frames.borrow_mut().top_mut()?.kind
        {
            vs.reads.push(read);
        }
        Ok(value)
    }

    fn mut_ctx(&self, rh: i32, fh: i32) -> HResult<MutCtx> {
        let r = self.ref_arg(rh)?;
        let field = self.text(fh)?;
        let (info, f) = self.lookup_field(&r.target, &field)?;
        let (txn, module) = {
            let frames = self.frames.borrow();
            let frame = frames.top()?;
            match &frame.kind {
                FrameKind::Action(a) => (a.txn, frame.module.clone()),
                _ => return trap("resources can only be changed inside an action"),
            }
        };
        let owner = self.registry.borrow().type_owner(&r.target.ty).cloned();
        if owner.as_ref() != Some(&module) {
            return trap(format!(
                "an action may only change its own module's types; `{}` belongs to another module",
                info.name
            ));
        }
        Ok(MutCtx {
            target: Rc::unwrap_or_clone(r.target),
            field: f,
            txn,
        })
    }

    fn apply_mut(&self, ctx: &MutCtx, m: &Mutation) -> HResult<Applied> {
        let applied = self
            .store
            .borrow_mut()
            .apply(ctx.txn, &ctx.target, m)
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        self.touched(ctx.target.id_string());
        Ok(applied)
    }

    /// Note that the running action changed the resource `id`: a version of it (reflection).
    fn touched(&self, id: String) {
        if let Ok(frame) = self.frames.borrow_mut().top_mut()
            && let FrameKind::Action(state) = &mut frame.kind
        {
            state.touched.insert(id);
        }
    }

    fn res_set(&self, rh: i32, fh: i32, v: &Value) -> HResult<Option<Wasm>> {
        let ctx = self.mut_ctx(rh, fh)?;
        let id = ctx.field.id.clone();
        let m = match ctx.field.kind {
            FieldKind::Register => Mutation::SetRegister {
                field: id,
                value: to_scalar(v, &ctx.field.ty)?,
            },
            FieldKind::Text => match v {
                Value::Text(t) => Mutation::SetText {
                    field: id,
                    text: t.clone(),
                },
                other => return trap(format!("expected text, got {}", other.kind_name())),
            },
            FieldKind::RichText => match v {
                Value::Rich(r) => Mutation::SetRich {
                    field: id,
                    value: r.clone(),
                },
                other => return trap(format!("expected rich text, got {}", other.kind_name())),
            },
            FieldKind::Counter => {
                return trap("`=` on a counter field loses concurrent updates; use res_add");
            }
            _ => return trap("container fields change through their methods, not `=`"),
        };
        self.apply_mut(&ctx, &m)?;
        ret_void()
    }

    fn res_add(&self, rh: i32, fh: i32, delta: Result<i64, f64>) -> HResult<Option<Wasm>> {
        let ctx = self.mut_ctx(rh, fh)?;
        if ctx.field.kind != FieldKind::Counter {
            return trap(format!("field `{}` is not a counter", ctx.field.name));
        }
        let units = match (delta, &ctx.field.ty) {
            (Ok(i), Ty::Int) => i,
            (Err(f), Ty::Float) => match float_to_counter(f) {
                Some(u) => u,
                None => return trap("counter delta is not finite or out of range"),
            },
            _ => return trap("counter delta does not match the field's type"),
        };
        let field = ctx.field.id.clone();
        self.apply_mut(
            &ctx,
            &Mutation::AddCounter {
                field,
                delta: units,
            },
        )?;
        ret_void()
    }

    /// The types a collection's items may be: one, or each case of a union.
    fn coll_field(&self, ctx: &MutCtx) -> HResult<Vec<Rc<TypeInfo>>> {
        if ctx.field.kind != FieldKind::Collection {
            return trap(format!("field `{}` is not a collection", ctx.field.name));
        }
        Ok(self.item_types(&ctx.field.ty)?)
    }

    /// The record types the elements of `ty` may be: its element type, or each case of a union.
    pub(crate) fn item_types(&self, ty: &Ty) -> Result<Vec<Rc<TypeInfo>>, String> {
        match inner_ty(ty) {
            Some(Ty::Union(keys)) => keys.iter().map(|key| self.record_info(key)).collect(),
            _ => Ok(vec![self.item_type(ty)?]),
        }
    }

    fn record_info(&self, key: &tilolang::types::TypeKey) -> Result<Rc<TypeInfo>, String> {
        let reg = self.registry.borrow();
        let id = reg
            .type_for(key)
            .ok_or_else(|| format!("type `{}` is not declared by any loaded package", key.name))?;
        reg.type_info(&id)
            .map(|(_, t)| t)
            .ok_or_else(|| format!("type `{}` is not loaded", key.name))
    }

    /// The item `item` names, which must be in this collection.
    fn item_arg(&self, ctx: &MutCtx, item: i32) -> HResult<String> {
        let r = self.ref_arg(item)?;
        let within = self
            .store
            .borrow()
            .container(&r.target)
            .ok()
            .flatten()
            .is_some_and(|(holder, field)| {
                field == ctx.field.id
                    && holder.doc == ctx.target.doc
                    && holder.item == ctx.target.item
            });
        match &r.target.item {
            Some(id) if within => Ok(id.clone()),
            _ => trap("the item does not belong to this collection"),
        }
    }

    /// The item `item` names, to move into this collection: an item of the same document, of a
    /// type the collection holds, and not this collection's holder or an item holding it.
    fn moved_arg(&self, ctx: &MutCtx, item_tys: &[Rc<TypeInfo>], item: i32) -> HResult<String> {
        let r = self.ref_arg(item)?;
        let Some(id) = r.target.item.clone() else {
            return trap("a document is in no collection to move it from");
        };
        if r.target.doc != ctx.target.doc {
            return trap("an item moves within its document; this collection is in another one");
        }
        let store = self.store.borrow();
        let ty = store
            .type_of(&r.target)
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        if !item_tys.iter().any(|t| t.id == ty) {
            let held: Vec<&str> = item_tys.iter().map(|t| t.name.as_str()).collect();
            return trap(format!(
                "the collection holds `{}`, not the item's type",
                held.join(" | ")
            ));
        }
        // Into itself, or into an item inside it, it would hold itself.
        let mut at = Some(ctx.target.clone());
        while let Some(holder) = at {
            if holder.item.as_deref() == Some(id.as_str()) {
                return trap("an item cannot go inside itself");
            }
            at = store
                .container(&holder)
                .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?
                .map(|(parent, _)| parent);
        }
        Ok(id)
    }

    fn coll_insert(&self, rh: i32, fh: i32, init: i32) -> HResult<Option<Wasm>> {
        let ctx = self.mut_ctx(rh, fh)?;
        let item_tys = self.coll_field(&ctx)?;
        let Value::Rec(rec) = self.val(init)? else {
            return trap("expected a record under construction");
        };
        let Some(item_ty) = item_tys.iter().find(|t| t.id == rec.ty).cloned() else {
            let held: Vec<&str> = item_tys.iter().map(|t| t.name.as_str()).collect();
            return trap(format!(
                "the collection holds `{}`, not `{}`",
                held.join(" | "),
                rec.ty
            ));
        };
        let new = crate::store::NewRecord {
            ty: item_ty.id.clone(),
            app: String::new(),
            fields: item_ty.specs(),
        };
        let applied = self.apply_mut(
            &ctx,
            &Mutation::CollInsert {
                field: ctx.field.id.clone(),
                item: new,
                index: None,
            },
        )?;
        let Applied::Item(id) = applied else {
            return trap("the store did not return the new item");
        };
        let target = ctx.target.item(&id, item_ty.id.clone());
        self.touched(target.id_string());
        self.init_item(&ctx, &item_ty, &target, &rec)?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Ref(RefValue {
            target: Rc::new(target),
            author,
        }))
    }

    fn init_item(
        &self,
        ctx: &MutCtx,
        ty: &TypeInfo,
        target: &ResourceRef,
        rec: &RecordInit,
    ) -> HResult<()> {
        for (fid, v) in &rec.fields {
            let Some(f) = ty.field(fid) else {
                return trap(format!("`{}` has no field `{fid}`", ty.name));
            };
            let m = match (f.kind, v) {
                (FieldKind::Register, v) => Mutation::SetRegister {
                    field: f.id.clone(),
                    value: to_scalar(v, &f.ty)?,
                },
                (FieldKind::Counter, Value::Int(i)) => Mutation::AddCounter {
                    field: f.id.clone(),
                    delta: *i,
                },
                (FieldKind::Text, Value::Text(t)) => Mutation::SetText {
                    field: f.id.clone(),
                    text: t.clone(),
                },
                (FieldKind::RichText, Value::Rich(r)) => Mutation::SetRich {
                    field: f.id.clone(),
                    value: r.clone(),
                },
                _ => {
                    return trap(format!(
                        "cannot initialise field `{}` of `{}`",
                        f.name, ty.name
                    ));
                }
            };
            self.store
                .borrow_mut()
                .apply(ctx.txn, target, &m)
                .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        }
        Ok(())
    }

    /// `@app.Type:id` or a root: the reference, checked against the store; a root nobody
    /// opened yet is still the root and reads as it would start.
    fn ref_lit(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let (_app, ty, id) = (
            self.text(a.h(0)?)?,
            self.text(a.h(1)?)?,
            self.text(a.h(2)?)?,
        );
        let Some(target) = ResourceRef::parse_id(&id, ty.clone()) else {
            return trap(format!("`{id}` is not a reference id"));
        };
        self.type_of_ref(&ty)?;
        // A root nobody opened yet is still the root: it reads as it would start.
        if !target.is_unmade_root() || self.store.borrow().type_of(&target).is_ok() {
            self.verify_ref(&target)?;
        }
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Ref(RefValue {
            target: Rc::new(target),
            author,
        }))
    }

    pub(crate) fn resource_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::ResGetI => {
                let v = self.res_get(a.h(0)?, a.h(1)?)?;
                ret_i64(v.as_int().map_or_else(
                    || trap(format!("field holds {}, not an int", v.kind_name())),
                    Ok,
                )?)
            }
            Imp::ResGetF => match self.res_get(a.h(0)?, a.h(1)?)? {
                Value::Float(f) => ret_f64(f),
                other => trap(format!("field holds {}, not a float", other.kind_name())),
            },
            Imp::ResGetH => {
                let v = self.res_get(a.h(0)?, a.h(1)?)?;
                self.ret_h(v)
            }
            Imp::ResSetI => self.res_set(a.h(0)?, a.h(1)?, &Value::Int(a.long(2)?)),
            Imp::ResSetF => self.res_set(a.h(0)?, a.h(1)?, &Value::Float(a.float(2)?)),
            Imp::ResSetH | Imp::ResTextSet => {
                let v = self.val_whole(a.h(2)?)?;
                self.res_set(a.h(0)?, a.h(1)?, &v)
            }
            Imp::ResAddI => self.res_add(a.h(0)?, a.h(1)?, Ok(a.long(2)?)),
            Imp::ResAddF => self.res_add(a.h(0)?, a.h(1)?, Err(a.float(2)?)),
            Imp::CollInsert => self.coll_insert(a.h(0)?, a.h(1)?, a.h(2)?),
            Imp::CollRemove => {
                let ctx = self.mut_ctx(a.h(0)?, a.h(1)?)?;
                self.coll_field(&ctx)?;
                let item = self.item_arg(&ctx, a.h(2)?)?;
                let field = ctx.field.id.clone();
                self.apply_mut(&ctx, &Mutation::CollRemove { field, item })?;
                ret_void()
            }
            // `move(x, after: y)`: beside an item of this collection, from wherever `x` is in
            // the document; it keeps its id.
            Imp::CollMove => {
                let ctx = self.mut_ctx(a.h(0)?, a.h(1)?)?;
                let item_tys = self.coll_field(&ctx)?;
                let item = self.moved_arg(&ctx, &item_tys, a.h(2)?)?;
                let neighbour = self.item_arg(&ctx, a.h(3)?)?;
                let after = a.h(4)? != 0;
                // Next to itself, it is where it was asked to be.
                if neighbour == item {
                    return ret_void();
                }
                // The neighbour's place among the other items, as this change sees them.
                let listing = self
                    .store
                    .borrow()
                    .listing(&ctx.target, &spec(&ctx.field))
                    .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
                let others: Vec<Rc<str>> =
                    listing.iter().filter(|id| **id != *item.as_str()).collect();
                let Some(at) = others.iter().position(|id| **id == *neighbour.as_str()) else {
                    return trap("the neighbour is not in this collection");
                };
                let index = if after { at.saturating_add(1) } else { at };
                let field = ctx.field.id.clone();
                self.apply_mut(&ctx, &Mutation::CollMove { field, item, index })?;
                ret_void()
            }
            // `move(x, to: i)`: to the place `i` among the other items, as the invoker sees
            // them now. The store records it beside the item it then comes before, so a move
            // made elsewhere at the same time leaves it where this one meant.
            Imp::CollMoveTo => {
                let ctx = self.mut_ctx(a.h(0)?, a.h(1)?)?;
                let item_tys = self.coll_field(&ctx)?;
                let item = self.moved_arg(&ctx, &item_tys, a.h(2)?)?;
                let Ok(index) = usize::try_from(a.long(3)?) else {
                    return trap("a collection has no place before its first");
                };
                let field = ctx.field.id.clone();
                self.apply_mut(&ctx, &Mutation::CollMove { field, item, index })?;
                ret_void()
            }
            Imp::CollMoveAmong => self.coll_move_among(a),
            Imp::SetAdd
            | Imp::SetRemove
            | Imp::MapPut
            | Imp::MapRemove
            | Imp::MapSetAdd
            | Imp::MapSetRemove => self.set_map_import(imp, a),
            Imp::MapGetH | Imp::MapKeys => self.map_read_import(imp, a),
            Imp::RecNew | Imp::RecSetI | Imp::RecSetF | Imp::RecSetH => self.rec_import(imp, a),
            Imp::RefLit => self.ref_lit(a),
            Imp::RefEq => {
                let (x, y) = (self.ref_arg(a.h(0)?)?, self.ref_arg(a.h(1)?)?);
                ret_i32(i32::from(x.target == y.target))
            }
            // Whether a resource is of the type `ty` (a case of a union).
            Imp::RefIs => {
                let r = self.ref_arg(a.h(0)?)?;
                let ty = self.text(a.h(1)?)?;
                let stored = if r.target.ty.is_empty() {
                    self.stored_type(&r.target)
                } else {
                    r.target.ty.clone()
                };
                ret_i32(i32::from(stored == ty))
            }
            Imp::RefId => {
                let r = self.ref_arg(a.h(0)?)?;
                self.ret_h(Value::Text(r.target.id_string()))
            }
            _ => trap("not a resource import"),
        }
    }

    /// `move(x, to: i, among: run)`: to the place `i` among the items of `run` other than `x`
    /// (a board's lane), as the invoker sees them now: just before the item of the run at
    /// that place, or just after the run's last. Like `move(x, before: y)`, the store records
    /// it beside that neighbour; into an empty run it stays where it is.
    fn coll_move_among(&self, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let ctx = self.mut_ctx(a.h(0)?, a.h(1)?)?;
        let item_tys = self.coll_field(&ctx)?;
        let item = self.moved_arg(&ctx, &item_tys, a.h(2)?)?;
        let Ok(at) = usize::try_from(a.long(3)?) else {
            return trap("a run has no place before its first");
        };
        let run = match self.val(a.h(4)?)? {
            Value::List(run) => run,
            other => return trap(format!("expected a list, got {}", other.kind_name())),
        };
        let mut lane = Vec::with_capacity(run.len());
        for v in &run {
            let Value::Ref(r) = v else {
                return trap(format!("a run holds items, not {}", v.kind_name()));
            };
            match &r.target.item {
                Some(id) if *id != item => lane.push(id.clone()),
                Some(_) => {}
                None => return trap("a run holds items of this collection"),
            }
        }
        let (neighbour, after) = match lane.get(at).or_else(|| lane.last()) {
            Some(next) => (next, at >= lane.len()),
            None => return ret_void(),
        };
        let listing = self
            .store
            .borrow()
            .listing(&ctx.target, &spec(&ctx.field))
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))?;
        let others: Vec<Rc<str>> = listing.iter().filter(|id| **id != *item.as_str()).collect();
        let Some(place) = others.iter().position(|id| **id == *neighbour.as_str()) else {
            return trap("an item of the run is not in this collection");
        };
        let index = if after {
            place.saturating_add(1)
        } else {
            place
        };
        let field = ctx.field.id.clone();
        self.apply_mut(&ctx, &Mutation::CollMove { field, item, index })?;
        ret_void()
    }

    fn set_map_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::SetAdd | Imp::SetRemove => {
                let ctx = self.mut_ctx(a.h(0)?, a.h(1)?)?;
                if ctx.field.kind != FieldKind::Set {
                    return trap(format!("field `{}` is not a set", ctx.field.name));
                }
                let key = key_of(&unboxed(self.val_whole(a.h(2)?)?))?;
                let field = ctx.field.id.clone();
                let m = if imp == Imp::SetAdd {
                    Mutation::SetAdd { field, key }
                } else {
                    Mutation::SetRemove { field, key }
                };
                self.apply_mut(&ctx, &m)?;
                ret_void()
            }
            Imp::MapPut | Imp::MapRemove => {
                let ctx = self.mut_ctx(a.h(0)?, a.h(1)?)?;
                if ctx.field.kind != FieldKind::Map {
                    return trap(format!("field `{}` is not a map", ctx.field.name));
                }
                let key = self.text(a.h(2)?)?;
                let field = ctx.field.id.clone();
                let m = if imp == Imp::MapPut {
                    let elem = inner_ty(&ctx.field.ty).cloned().unwrap_or(Ty::Text);
                    let value = to_scalar(&unboxed(self.val_whole(a.h(3)?)?), &elem)?;
                    Mutation::MapPut { field, key, value }
                } else {
                    Mutation::MapRemove { field, key }
                };
                self.apply_mut(&ctx, &m)?;
                ret_void()
            }
            Imp::MapSetAdd | Imp::MapSetRemove => {
                let ctx = self.mut_ctx(a.h(0)?, a.h(1)?)?;
                if ctx.field.kind != FieldKind::Map
                    || !matches!(inner_ty(&ctx.field.ty), Some(Ty::Set(_)))
                {
                    return trap(format!("field `{}` is not a map of sets", ctx.field.name));
                }
                let key = self.text(a.h(2)?)?;
                if key.contains(MAP_SET_SEPARATOR) {
                    return trap("a map key cannot hold the unit separator");
                }
                let element = key_of(&unboxed(self.val_whole(a.h(3)?)?))?;
                let key = map_set_entry(&key, &element);
                let field = ctx.field.id.clone();
                let m = if imp == Imp::MapSetAdd {
                    Mutation::MapPut {
                        field,
                        key,
                        value: Scalar::Bool(true),
                    }
                } else {
                    Mutation::MapRemove { field, key }
                };
                self.apply_mut(&ctx, &m)?;
                ret_void()
            }
            _ => trap("not a set or map import"),
        }
    }

    /// `map_get_h(r, field, key) -> Option<V>` and `map_keys(r, field) -> List<Text>`.
    fn map_read_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let res = self.ref_arg(a.h(0)?)?;
        let field = self.text(a.h(1)?)?;
        let (_, info) = self.lookup_field(&res.target, &field)?;
        if info.kind != FieldKind::Map {
            return trap(format!("field `{}` is not a map", info.name));
        }
        let author = self.module_of_frame()?;
        let read =
            self.journaled_read(|| Ok(self.read_value(&res.target, &info, Some(&author))?))?;
        let Value::List(entries) = read else {
            return trap("a map reads as a list of entries");
        };
        if let FrameKind::View(vs) = &mut self.frames.borrow_mut().top_mut()?.kind {
            vs.reads.push((res.target.id_string(), info.id));
        }
        let pairs = entries.into_iter().filter_map(|entry| match entry {
            Value::Tuple(mut kv) if kv.len() == 2 => {
                let val = kv.pop()?;
                let name = kv.pop()?;
                Some((name, val))
            }
            _ => None,
        });
        if imp == Imp::MapKeys {
            return self.ret_h(Value::List(pairs.map(|(k, _)| k).collect()));
        }
        let key = self.text(a.h(2)?)?;
        let found = pairs
            .into_iter()
            .find(|(k, _)| k.as_text() == Some(key.as_str()))
            .map(|(_, v)| Box::new(v));
        self.ret_h(Value::Opt(found))
    }

    fn rec_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        if imp == Imp::RecNew {
            let ty = self.text(a.h(0)?)?;
            self.type_of_ref(&ty)?;
            return self.ret_h(Value::Rec(RecordInit {
                ty,
                fields: BTreeMap::default(),
            }));
        }
        let rec_h = a.h(0)?;
        let field = self.text(a.h(1)?)?;
        let v = match imp {
            Imp::RecSetI => Value::Int(a.long(2)?),
            Imp::RecSetF => Value::Float(a.float(2)?),
            _ => self.val_whole(a.h(2)?)?,
        };
        let ty = self.with_val(rec_h, |r| match r {
            Value::Rec(rec) => Ok(rec.ty.clone()),
            other => trap(format!("expected a record, got {}", other.kind_name())),
        })?;
        let (_, info) = self.type_of_ref(&ty)?;
        if info.field(&field).is_none() {
            return trap(format!(
                "type `{}` has no field `{field}`; fields are: {}",
                info.name,
                info.field_names()
            ));
        }
        if let Value::Rec(rec) = self.frames.borrow_mut().get_mut(rec_h)? {
            rec.fields.insert(field, v);
        }
        ret_void()
    }
}

/// A field read whole: a map as its entries ([`Value::Map`]), which the store holds as a list
/// of key and value pairs; any other field as it is.
fn whole_map(kind: FieldKind, value: Value) -> Value {
    match (kind, value) {
        (FieldKind::Map, Value::List(pairs)) => Value::Map(
            pairs
                .into_iter()
                .filter_map(|pair| match pair {
                    Value::Tuple(kv) => {
                        let mut kv = kv.into_iter();
                        Some((kv.next()?, kv.next()?))
                    }
                    _ => None,
                })
                .collect(),
        ),
        (_, value) => value,
    }
}
