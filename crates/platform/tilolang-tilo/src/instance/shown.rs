//! The resources a window shows, as the app publishes them.
//!
//! A window's palette offers the app's actions on what the window draws: the resource of each
//! lane, and every resource the view it shows refers to. Those rows are the ones whose
//! conditions the app answers when it publishes; the rest are answered when a caller asks
//! about one.

use std::collections::BTreeSet;

use super::Instance;
use crate::runtime::{UiNode, Value};

impl Instance {
    /// The published resources the window shows, as the app publishes them
    /// (`tl:so.tilo.lang.tasks:root-fjqi4yze`): each lane's own, and every resource the view it
    /// shows refers to.
    #[must_use]
    pub fn shown_resources(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for shown in self.root.iter().chain(self.detail.iter()) {
            out.insert(crate::publish::resource_id(
                &self.app,
                &shown.target.id_string(),
            ));
            let view = shown.view.borrow();
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
            out.insert(crate::publish::resource_id(
                app,
                &reference.target.id_string(),
            ));
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
