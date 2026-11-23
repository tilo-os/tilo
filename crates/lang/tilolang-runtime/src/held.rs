//! A record a field holds: a field of a record type that is not given when its resource is made
//! starts as a record of its own, built from its type's defaults. That record is an item of the
//! holder's document, placed under the field as a collection's items are placed under theirs
//! ([`crate::place`]), so it shares the holder's readers, history, sync and removal. Its id is
//! keyed by the holder and the field, so two devices that make the same root make the same item,
//! and a root nobody made yet reads the reference it will hold.

use std::rc::Rc;

use crate::core::Core;
use crate::meta::{FieldKind, Ty};
use crate::registry::{FieldInfo, TypeInfo};
use crate::store::{Applied, Mutation, NewResource, Store};
use crate::value::{RefValue, ResourceRef, Value};

/// How deep held records may nest: the checker refuses a record that holds itself, so a deeper
/// nesting is a package the checker did not make.
const DEPTH: usize = 32;

/// Whether `field` holds a record of its own when its resource is made without it: a field of a
/// declared record type with no default. A field given a value holds what it was given.
#[must_use]
pub fn holds(field: &FieldInfo) -> bool {
    field.kind == FieldKind::Register
        && field.default.is_none()
        && matches!(&field.ty, Ty::Record(key)
            if !tilolang::host::is_value(key) && *key != tilolang::resource_key())
}

/// Whether a resource of `ty` holds a record of its own in a field it is made without.
#[must_use]
pub fn holds_any(ty: &TypeInfo) -> bool {
    ty.fields.iter().any(holds)
}

/// The id of the item the field `field` of `holder` holds.
#[must_use]
pub fn held_id(holder: &ResourceRef, field: &str) -> String {
    crate::host_res::keyed_id(holder, field, "")
}

/// Whether `target` is a root nobody made yet, or a record in one: it reads as it would start.
#[must_use]
pub fn unmade(store: &dyn Store, target: &ResourceRef) -> bool {
    target.is_unmade_root()
        || (target.item.is_some()
            && target.doc.starts_with("root-")
            && store
                .type_of(&ResourceRef::root(target.doc.clone(), target.ty.clone()))
                .is_err())
}

impl Core {
    /// [`unmade`] in this runtime's store.
    pub(crate) fn unmade(&self, target: &ResourceRef) -> bool {
        unmade(&**self.store.borrow(), target)
    }

    /// The type of the record `field` holds, when it holds one ([`holds`]).
    fn held_type(&self, field: &FieldInfo) -> Option<Rc<TypeInfo>> {
        let Ty::Record(key) = &field.ty else {
            return None;
        };
        let registry = self.registry.borrow();
        let id = registry.type_for(key)?;
        registry.type_info(&id).map(|(_, info)| info)
    }

    /// The reference the field `field` of `holder` holds while its register says nothing: the
    /// record made with it, or, in a root not made yet, the one it will be made with.
    pub(crate) fn held_ref(&self, holder: &ResourceRef, field: &FieldInfo) -> Option<Value> {
        if !holds(field) {
            return None;
        }
        let ty = self.held_type(field)?;
        Some(Value::Ref(RefValue {
            target: Rc::new(holder.item(&held_id(holder, &field.id), ty.id.clone())),
            author: None,
        }))
    }

    /// Make the records the fields of `holder` (a resource of `ty`) hold, those `given` leaves
    /// out, with `apply` writing in the change that makes `holder`: each an item placed under its
    /// field, its own held records made with it. Returns the ids of what it made.
    ///
    /// # Errors
    ///
    /// A message when a record type is not loaded or a write fails.
    pub(crate) fn make_held(
        &self,
        holder: &ResourceRef,
        ty: &TypeInfo,
        given: &dyn Fn(&str) -> bool,
        apply: &dyn Fn(&ResourceRef, &Mutation) -> Result<Applied, String>,
    ) -> Result<Vec<String>, String> {
        let mut made = Vec::new();
        self.make_held_in(holder, ty, given, apply, &mut made, 0)?;
        Ok(made)
    }

    fn make_held_in(
        &self,
        holder: &ResourceRef,
        ty: &TypeInfo,
        given: &dyn Fn(&str) -> bool,
        apply: &dyn Fn(&ResourceRef, &Mutation) -> Result<Applied, String>,
        made: &mut Vec<String>,
        depth: usize,
    ) -> Result<(), String> {
        for field in ty.fields.iter().filter(|f| holds(f) && !given(&f.id)) {
            if depth >= DEPTH {
                return Err(format!(
                    "`{}` of `{}` holds records {DEPTH} deep",
                    field.name, ty.name
                ));
            }
            let Some(held) = self.held_type(field) else {
                return Err(format!(
                    "`{}` of `{}` holds a record of a type no loaded package declares",
                    field.name, ty.name
                ));
            };
            let item = NewResource {
                ty: held.id.clone(),
                app: crate::ids::AppId::default(),
                fields: held.specs(),
                text_defaults: held.text_defaults(),
            };
            let insert = Mutation::CollInsert {
                field: field.id.clone(),
                item,
                index: None,
                id: Some(held_id(holder, &field.id)),
            };
            let Applied::Item(id) = apply(holder, &insert)? else {
                return Err("the store did not return the held record".to_owned());
            };
            let target = holder.item(&id, held.id.clone());
            let reference = Value::Ref(RefValue {
                target: Rc::new(target.clone()),
                author: None,
            });
            let holds_it = Mutation::SetRegister {
                field: field.id.clone(),
                value: crate::host_res::to_scalar(&reference, &field.ty)?,
            };
            apply(holder, &holds_it)?;
            made.push(target.id_string());
            self.make_held_in(
                &target,
                &held,
                &|_| false,
                apply,
                made,
                depth.saturating_add(1),
            )?;
        }
        Ok(())
    }
}
