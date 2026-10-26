//! Whether a new build of a package can take over a running one in place.
//!
//! A running app keeps its state outside its module: resources in the room record, by
//! declaration id; `window` and `lens` state in the runtime, by window and declaration id;
//! scroll, selection and the open detail in the window. A new module can take over all of it
//! when it reads that state as the old one wrote it. [`compare`] says whether it does, and when
//! it does not, names the first declaration that stands in the way, in one line.
//!
//! Declarations are matched by id (the ids lock keeps an id across a rename), so renaming a type,
//! a field or a variant keeps state. A declaration the new build no longer has keeps its stored
//! values in the record: nothing reads them, and nothing deletes them.

use tilolang_runtime::meta::{EnumMeta, FieldMeta, Meta, TypeMeta};

/// What a new build of a running package can keep.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Upgrade {
    /// The new module reads every resource and every piece of view state the old one wrote: it
    /// takes over in place.
    InPlace,
    /// It does not, for the reason given: the app restarts on the new build, as an update of it
    /// would, with its data where it was.
    Restart(String),
}

impl Upgrade {
    #[must_use]
    pub fn in_place(&self) -> bool {
        matches!(self, Self::InPlace)
    }
}

/// Whether `new` can take over from `old`, a build of the same app, in place.
#[must_use]
pub fn compare(old: &Meta, new: &Meta) -> Upgrade {
    match first_obstacle(old, new) {
        None => Upgrade::InPlace,
        Some(reason) => Upgrade::Restart(reason),
    }
}

fn first_obstacle(old: &Meta, new: &Meta) -> Option<String> {
    if old.app != new.app {
        return Some(format!("the build is the app {}, not {}", new.app, old.app));
    }
    // What the platform shows of an app (its window title, its icon, its place in the menu) is
    // the installed package's manifest, which only a start of the app reads.
    if old.title != new.title {
        return Some(format!(
            "the app's name changed from \"{}\" to \"{}\"",
            old.title, new.title
        ));
    }
    if old.icon != new.icon {
        return Some("the app's icon changed".to_owned());
    }
    if old.category != new.category {
        return Some("the app's category changed".to_owned());
    }
    for before in &old.types {
        let Some(after) = new.types.iter().find(|ty| ty.id == before.id) else {
            continue;
        };
        if let Some(reason) = fields_obstacle(&before.fields, &after.fields) {
            return Some(format!("{}: {reason}", type_path(after)));
        }
    }
    for before in &old.enums {
        let Some(after) = new.enums.iter().find(|ty| ty.id == before.id) else {
            continue;
        };
        if let Some(reason) = enum_obstacle(before, after) {
            return Some(format!("enum {}: {reason}", after.name));
        }
    }
    for view in &old.views {
        let Some(after) = new.views.iter().find(|candidate| candidate.id == view.id) else {
            continue;
        };
        for state in &view.state {
            let Some(now) = after.state.iter().find(|now| now.id == state.id) else {
                continue;
            };
            if now.ty != state.ty || now.layer != state.layer {
                return Some(format!(
                    "view {}: the {} state `{}` changed from {} {} to {} {}",
                    after.name, state.layer, now.name, state.layer, state.ty, now.layer, now.ty
                ));
            }
        }
    }
    None
}

fn type_path(ty: &TypeMeta) -> String {
    format!("type {}", ty.name)
}

fn fields_obstacle(before: &[FieldMeta], after: &[FieldMeta]) -> Option<String> {
    for field in before {
        let Some(now) = after.iter().find(|now| now.id == field.id) else {
            continue;
        };
        if now.ty != field.ty {
            return Some(format!(
                "the field `{}` changed from {} to {}",
                now.name, field.ty, now.ty
            ));
        }
        if now.merge != field.merge {
            return Some(format!(
                "the field `{}` changed how it merges, from {:?} to {:?}",
                now.name, field.merge, now.merge
            ));
        }
    }
    None
}

fn enum_obstacle(before: &EnumMeta, after: &EnumMeta) -> Option<String> {
    for variant in &before.variants {
        let Some(now) = after.variants.iter().find(|now| now.id == variant.id) else {
            return Some(format!("the case `{}` was removed", variant.name));
        };
        if let Some(reason) = fields_obstacle(&variant.fields, &now.fields) {
            return Some(format!("case {}: {reason}", now.name));
        }
    }
    None
}
