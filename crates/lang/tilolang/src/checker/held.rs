//! A record a field holds: a field of a record type that a resource is made without starts as a
//! record of its own, built from its type's defaults, kept in the holder's document. The checker
//! refuses one that cannot start that way where the making leaves the field out, so a read of it
//! never finds nothing.

use crate::ast::{self, Item, TypeExprKind};
use crate::diag::Span;
use crate::resolve::FieldInfo;
use crate::types::{Ty, TypeKey};

use super::Checker;

/// Whether a field of type `ty` with no default of its own starts at a value of its type, as the
/// store reads one nothing was written to.
fn starts_empty(ty: &Ty) -> bool {
    match ty {
        Ty::Int
        | Ty::Float
        | Ty::Bool
        | Ty::Text
        | Ty::Date
        | Ty::Time
        | Ty::Identifier
        | Ty::RichText
        | Ty::Unit
        | Ty::Option(_)
        | Ty::List(_)
        | Ty::Set(_)
        | Ty::Map(_)
        | Ty::Collection(_) => true,
        Ty::Tuple(items) => items.iter().all(starts_empty),
        _ => false,
    }
}

/// The record type a field holds when its resource is made without it: a declared record type
/// and no default.
pub(super) fn held_key(field: &FieldInfo) -> Option<&TypeKey> {
    match &field.ty {
        Ty::Record(key)
            if field.default.is_none()
                && !crate::host::is_value(key)
                && *key != super::program::resource_key() =>
        {
            Some(key)
        }
        _ => None,
    }
}

impl Checker<'_> {
    /// Why a `key` a field holds cannot start on its own: a field of it with no default, a type of
    /// another app (whose resources only its own actions make), an initial action of its own, or a
    /// record that holds itself again. `None` when it starts from its fields' defaults.
    pub(super) fn held_cannot_start(&self, key: &TypeKey) -> Option<String> {
        self.held_cannot_start_in(key, &mut Vec::new())
    }

    fn held_cannot_start_in(&self, key: &TypeKey, path: &mut Vec<String>) -> Option<String> {
        if path.contains(&key.name) {
            path.push(key.name.clone());
            return Some(format!(
                "making it would never end: {}",
                path.iter()
                    .map(|name| format!("a `{name}`"))
                    .collect::<Vec<_>>()
                    .join(" holds ")
            ));
        }
        if key.app != self.env.app {
            return Some(format!(
                "`{}` is declared by another app, and only its own actions make one",
                key.name
            ));
        }
        let info = self.env.type_info(key)?;
        let initial = self.env.callables.iter().any(|c| {
            c.present.role.as_deref() == Some("initial")
                && c.params
                    .first()
                    .is_some_and(|p| p.ty == Ty::Record(key.clone()))
        });
        if initial {
            return Some(format!(
                "a `{}` starts with what its initial action writes, and a held one starts from its fields' defaults alone",
                key.name
            ));
        }
        path.push(key.name.clone());
        for field in &info.fields {
            if let Some(inner) = held_key(field) {
                if let Some(why) = self.held_cannot_start_in(inner, path) {
                    return Some(why);
                }
            } else if field.default.is_none() && !starts_empty(&field.ty) {
                return Some(format!(
                    "the `{}` of a `{}` has no default",
                    field.name, key.name
                ));
            }
        }
        path.pop();
        None
    }

    /// Refuse a record field a new `key` leaves out (those not among `given`) that cannot start on
    /// its own. Only this package makes resources of its own types: another app's record written
    /// here is a reading it stands in for (`stage(device.network, device.Network(...))`), never
    /// made.
    pub(super) fn check_held(&mut self, key: &TypeKey, given: &[&str], span: Span) {
        if key.app != self.env.app {
            return;
        }
        let Some(info) = self.env.type_info(key) else {
            return;
        };
        let refused: Vec<(String, String, String)> = info
            .fields
            .iter()
            .filter(|f| !given.contains(&f.name.as_str()))
            .filter_map(|f| {
                let held = held_key(f)?;
                let why = self.held_cannot_start(held)?;
                Some((f.name.clone(), held.name.clone(), why))
            })
            .collect();
        for (field, held, why) in refused {
            self.err(
                "E019",
                span,
                format!(
                    "a `{}` made without `{field}` starts holding a new `{held}`, and {why}; give `{field}:`, or give the field a default",
                    key.name
                ),
            );
        }
    }

    /// Refuse a root whose record fields cannot start on their own: the platform makes it, and
    /// gives it nothing.
    pub(super) fn check_held_roots(&mut self, module: &ast::Module) {
        for item in &module.items {
            let Item::Root(r) = item else { continue };
            let TypeExprKind::Named(path) = &r.ty.kind else {
                continue;
            };
            let Some(name) = path.last() else { continue };
            let Some(key) = self.env.roots.iter().find(|k| k.name == name.name).cloned() else {
                continue;
            };
            self.check_held(&key, &[], r.span);
        }
    }
}
