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
use crate::ids::ModuleHash;
use crate::meta::{FieldKind, Ty};
use crate::num::{counter_to_float, float_to_counter, to_usize};
use crate::registry::{FieldInfo, TypeInfo};
use crate::store::{Applied, FieldSpec, FieldValue, Mutation, Scalar, Txn};
use crate::value::{RecordInit, RefValue, ResourceRef, Value, from_json, to_json};

/// Everything a mutation import needs to know before it touches the store.
struct MutCtx {
    target: ResourceRef,
    field: FieldInfo,
    txn: Txn,
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
        Ty::Option(_) => Value::none(),
        Ty::List(_) | Ty::Set(_) | Ty::Map(_) | Ty::Collection(_) => Value::List(Vec::new()),
        Ty::Tuple(items) => Value::Tuple(items.iter().map(type_default).collect::<Result<_, _>>()?),
        Ty::Unit => Value::Unit,
        Ty::Record(_)
        | Ty::Principal
        | Ty::Action
        | Ty::View
        | Ty::Length(_)
        | Ty::Enum(_)
        | Ty::Error => {
            return Err("the field has no value yet and no default".to_owned());
        }
    })
}

fn declared_default(ty: &Ty, default: Option<&Json>) -> Result<Value, String> {
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
        (Ty::Text, Some(j)) => j
            .as_str()
            .map(Value::text)
            .ok_or_else(|| "default is not text".into()),
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
        (Value::Int(_) | Value::Float(_) | Value::Bool(_) | Value::Text(_), _) => Err(format!(
            "a {} cannot be stored in a field of that type",
            v.kind_name()
        )),
        (other, _) => to_json(other).map(Scalar::Json),
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
        other => Err(format!(
            "a {} cannot be a set element or map key",
            other.kind_name()
        )),
    }
}

fn inner_ty(ty: &Ty) -> Option<&Ty> {
    match ty {
        Ty::Collection(t) | Ty::Set(t) | Ty::Map(t) | Ty::List(t) | Ty::Option(t) => Some(t),
        _ => None,
    }
}

impl Core {
    /// The type of `ty`'s package, and of the type itself.
    fn type_of_ref(
        &self,
        ty: &str,
    ) -> Result<(Rc<crate::registry::Package>, Rc<TypeInfo>), String> {
        self.registry
            .borrow()
            .type_info(ty)
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
        match info.field(field) {
            Some(f) => {
                let f = f.clone();
                Ok((info, f))
            }
            None => trap(format!(
                "type `{}` has no field `{field}`; fields are: {}",
                info.name,
                info.field_names()
            )),
        }
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
        let fv = self
            .store
            .borrow()
            .read(target, &spec(field))
            .map_err(|e| e.to_string())?;
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
            (FieldKind::Collection, FieldValue::Items(ids)) => {
                let item = self.item_type(&field.ty)?;
                Ok(Value::List(
                    ids.iter()
                        .map(|id| {
                            Value::Ref(RefValue {
                                target: target.item(&field.id, id, item.id.clone()),
                                author: author.cloned(),
                            })
                        })
                        .collect(),
                ))
            }
            (FieldKind::Set, FieldValue::Keys(keys)) => self.keys_value(field, keys, author),
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
        let items = keys
            .into_iter()
            .map(|k| match inner_ty(&field.ty) {
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
                        target,
                        author: author.cloned(),
                    }))
                }
                _ => Ok(Value::Text(k)),
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Value::List(items))
    }

    fn res_get(&self, rh: i32, fh: i32) -> HResult<Value> {
        let r = self.ref_arg(rh)?;
        let field = self.text(fh)?;
        let (_, f) = self.lookup_field(&r.target, &field)?;
        let author = self.module_of_frame()?;
        let v = self.journaled_read(|| Ok(self.read_value(&r.target, &f, Some(&author))?))?;
        if let FrameKind::View(vs) = &mut self.frames.borrow_mut().top_mut()?.kind {
            vs.reads.push((r.target.doc, f.id));
        }
        Ok(v)
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
            target: r.target,
            field: f,
            txn,
        })
    }

    fn apply_mut(&self, ctx: &MutCtx, m: &Mutation) -> HResult<Applied> {
        self.store
            .borrow_mut()
            .apply(ctx.txn, &ctx.target, m)
            .map_err(|e| crate::engine::HostFail::Trap(e.to_string()))
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

    fn coll_field(&self, ctx: &MutCtx) -> HResult<Rc<TypeInfo>> {
        if ctx.field.kind != FieldKind::Collection {
            return trap(format!("field `{}` is not a collection", ctx.field.name));
        }
        Ok(self.item_type(&ctx.field.ty)?)
    }

    fn item_arg(&self, ctx: &MutCtx, item: i32) -> HResult<String> {
        let r = self.ref_arg(item)?;
        match r.target.path.split_last() {
            Some((last, parent))
                if last.field == ctx.field.id
                    && parent == ctx.target.path.as_slice()
                    && r.target.doc == ctx.target.doc =>
            {
                Ok(last.item.clone())
            }
            _ => trap("the item does not belong to this collection"),
        }
    }

    fn coll_insert(&self, rh: i32, fh: i32, init: i32) -> HResult<Option<Wasm>> {
        let ctx = self.mut_ctx(rh, fh)?;
        let item_ty = self.coll_field(&ctx)?;
        let Value::Rec(rec) = self.val(init)? else {
            return trap("expected a record under construction");
        };
        if rec.ty != item_ty.id {
            return trap(format!(
                "the collection holds `{}`, not `{}`",
                item_ty.name, rec.ty
            ));
        }
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
        let target = ctx.target.item(&ctx.field.id, &id, item_ty.id.clone());
        self.init_item(&ctx, &item_ty, &target, &rec)?;
        let author = Some(self.module_of_frame()?);
        self.ret_h(Value::Ref(RefValue { target, author }))
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
                let v = self.val(a.h(2)?)?;
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
            Imp::CollMove => {
                let ctx = self.mut_ctx(a.h(0)?, a.h(1)?)?;
                self.coll_field(&ctx)?;
                let item = self.item_arg(&ctx, a.h(2)?)?;
                let Some(index) = to_usize(a.long(3)?) else {
                    return trap("move to a negative index");
                };
                let field = ctx.field.id.clone();
                self.apply_mut(&ctx, &Mutation::CollMove { field, item, index })?;
                ret_void()
            }
            Imp::SetAdd | Imp::SetRemove | Imp::MapPut | Imp::MapRemove => {
                self.set_map_import(imp, a)
            }
            Imp::MapGetH | Imp::MapKeys => self.map_read_import(imp, a),
            Imp::RecNew | Imp::RecSetI | Imp::RecSetF | Imp::RecSetH => self.rec_import(imp, a),
            Imp::RefLit => {
                let (_app, ty, id) = (
                    self.text(a.h(0)?)?,
                    self.text(a.h(1)?)?,
                    self.text(a.h(2)?)?,
                );
                let Some(target) = ResourceRef::parse_id(&id, ty.clone()) else {
                    return trap(format!("`{id}` is not a reference id"));
                };
                self.type_of_ref(&ty)?;
                self.verify_ref(&target)?;
                let author = Some(self.module_of_frame()?);
                self.ret_h(Value::Ref(RefValue { target, author }))
            }
            Imp::RefEq => {
                let (x, y) = (self.ref_arg(a.h(0)?)?, self.ref_arg(a.h(1)?)?);
                ret_i32(i32::from(x.target == y.target))
            }
            Imp::RefId => {
                let r = self.ref_arg(a.h(0)?)?;
                self.ret_h(Value::Text(r.target.id_string()))
            }
            _ => trap("not a resource import"),
        }
    }

    fn set_map_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::SetAdd | Imp::SetRemove => {
                let ctx = self.mut_ctx(a.h(0)?, a.h(1)?)?;
                if ctx.field.kind != FieldKind::Set {
                    return trap(format!("field `{}` is not a set", ctx.field.name));
                }
                let key = key_of(&unboxed(self.val(a.h(2)?)?))?;
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
                    let value = to_scalar(&unboxed(self.val(a.h(3)?)?), &elem)?;
                    Mutation::MapPut { field, key, value }
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
            vs.reads.push((res.target.doc, info.id));
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
            _ => self.val(a.h(2)?)?,
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
