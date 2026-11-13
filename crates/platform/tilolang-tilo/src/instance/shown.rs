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
use crate::runtime::{ResourceRef, UiNode, Value, ViewInstance};

impl Instance {
    /// The root view and, when a row went somewhere, the detail view.
    fn shown(&self) -> impl Iterator<Item = &Shown> {
        [Lane::Root, Lane::Detail]
            .into_iter()
            .filter_map(|lane| self.lane(lane))
    }

    /// The published resources the window shows, as the app that holds each publishes it
    /// (`tl:so.tilo.lang.tasks:root-fjqi4yze`): each lane's own, every resource the view it
    /// shows refers to or runs an action on, and the items of the rows its lists hold — a
    /// row's actions are published on its resource, whether or not the list has rendered that
    /// row yet.
    #[must_use]
    pub fn shown_resources(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for shown in self.shown() {
            let view = shown.view.borrow();
            out.insert(resource_id(&self.app, &shown.target.id_string()));
            for path in view.shown_refs() {
                out.insert(resource_id(&self.app, &path));
            }
            let tree = view.tree();
            referents(tree, &mut out);
            runs(tree, &view, &mut out);
        }
        out
    }

    /// Whether a view has something to run again: a reading it took moved, the time it read
    /// passed, or a record it shows changed
    /// ([`crate::runtime::ViewInstance::is_stale`]). A slot the host fills moves one — the
    /// person's own state, given back when their window opens or kept on another device of
    /// theirs, and what they have seen — so the frame runs the views that read it rather than
    /// waiting for a change of the person's here.
    pub(super) fn views_stale(&self) -> bool {
        self.shown().any(|shown| shown.view.borrow_mut().is_stale())
    }
}

/// The resource `target` names — a record, or an item of one — as the app that publishes its
/// type publishes it. A view of one app shows another's records (Today's places are Tasks'
/// tasks), and a record is published by the app that declares its type, whichever app's view
/// reached it.
fn published(target: &ResourceRef) -> String {
    let path = target.id_string();
    match crate::schema::lookup(&target.ty) {
        Some(schema) => resource_id(&schema.app, &path),
        None => path,
    }
}

/// Every resource the props of `node` and its children refer to.
fn referents(node: &UiNode, out: &mut BTreeSet<String>) {
    for value in node.props.values() {
        referred(value, out);
    }
    for child in &node.children {
        referents(child, out);
    }
}

/// Every resource the actions the controls of `node` and its children run act on: an action's
/// subject is the record it changes, and what a window draws a control for, it reaches.
fn runs(node: &UiNode, view: &ViewInstance, out: &mut BTreeSet<String>) {
    for event in node.events.keys() {
        let Some(action) = view.event_action(&node.key, event) else {
            continue;
        };
        for arg in &action.acting().args {
            referred(arg, out);
        }
    }
    for child in &node.children {
        runs(child, view, out);
    }
}

/// Every resource `value` refers to, at any depth.
fn referred(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Ref(reference) => {
            out.insert(published(&reference.target));
        }
        Value::List(items) | Value::Tuple(items) => {
            for item in items {
                referred(item, out);
            }
        }
        Value::Opt(Some(item)) => referred(item, out),
        Value::Enum(found) => {
            for field in found.fields.values() {
                referred(field, out);
            }
        }
        _ => {}
    }
}
