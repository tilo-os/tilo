//! The resources a window shows, as the app publishes them.
//!
//! A window shows the resource of each of its lanes, and every resource the view it shows
//! refers to (its rows, and what a row's controls name). That set is one notion for both
//! readers: the palette offers the app's actions on it and the app answers their conditions
//! when it publishes, and what the window's connection publishes is bounded by it, so a change
//! in a workspace of ten thousand tasks costs the rows on screen. Every other row is answered,
//! and published, when a caller asks about it.

use std::collections::BTreeSet;

use tilo_render_protocol::patch::Lane;

use super::{Instance, Shown};
use crate::publish::resource_id;
use crate::runtime::{UiNode, Value};

impl Instance {
    /// The root view and, when a row went somewhere, the detail view.
    fn shown(&self) -> impl Iterator<Item = &Shown> {
        [Lane::Root, Lane::Detail]
            .into_iter()
            .filter_map(|lane| self.lane(lane))
    }

    /// The published resources the window shows, as the app publishes them
    /// (`tl:so.tilo.lang.tasks:root-fjqi4yze`): each lane's own, every resource the view it
    /// shows refers to, and the items of the rows its lists hold — a row's actions are published
    /// on its resource, whether or not the list has rendered that row yet.
    #[must_use]
    pub fn shown_resources(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for shown in self.shown() {
            let view = shown.view.borrow();
            out.insert(resource_id(&self.app, &shown.target.id_string()));
            for path in view.shown_refs() {
                out.insert(resource_id(&self.app, &path));
            }
            referents(view.tree(), &self.app, &mut out);
        }
        out
    }
}

/// Every resource the props of `node` and its children refer to, as the app publishes it.
fn referents(node: &UiNode, app: &str, out: &mut BTreeSet<String>) {
    for value in node.props.values() {
        referred(value, app, out);
    }
    for child in &node.children {
        referents(child, app, out);
    }
}

/// Every resource `value` refers to, at any depth.
fn referred(value: &Value, app: &str, out: &mut BTreeSet<String>) {
    match value {
        Value::Ref(reference) => {
            out.insert(resource_id(app, &reference.target.id_string()));
        }
        Value::List(items) | Value::Tuple(items) => {
            for item in items {
                referred(item, app, out);
            }
        }
        Value::Opt(Some(item)) => referred(item, app, out),
        Value::Enum(found) => {
            for field in found.fields.values() {
                referred(field, app, out);
            }
        }
        _ => {}
    }
}
