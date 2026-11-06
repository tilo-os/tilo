//! The views a Rust app declares, as the runtime draws them.
//!
//! A `#[view]` function builds [`tilo_view::Node`]s, described components checked against the
//! catalogue. The runtime draws a resource of a Rust app's type through its view the way it draws
//! a tilolang package's: a boundary, keyed and wired, whose tree the host builds from the one
//! catalogue (one-app-model.md §2). This module is the seam: it takes the views a crate registered
//! ([`tilo_view::ViewRegistry`]) and hands the runtime a [`RustViews`] over them, converting each
//! tree the view builds into the runtime's own node kinds, props and events.

use std::collections::BTreeMap;

use tilo_view::registry::ViewInput;
use tilo_view::{Effect, Node, Value as ViewValue, ViewCx};
use tilolang_runtime::ids::Principal;
use tilolang_runtime::rust_views::{
    RustEvent, RustNode, RustRendered, RustResource, RustViewDecl, RustViews,
};
use tilolang_runtime::value::{ResourceRef, Value};

/// The views of the apps a build declares, as the runtime draws them: one registry over every
/// `#[view]` linked into the process, drawn through one host.
pub struct DeclaredViews;

impl RustViews for DeclaredViews {
    fn app(&self) -> &'static str {
        // A crate's views name their app through their resource type; the process may hold more
        // than one app's. Each lookup is by the app a resource's type starts with, so no single
        // name is this registry's; the empty string names every app.
        tilolang_runtime::rust_views::EVERY_APP
    }

    fn kinds(&self) -> Vec<String> {
        let mut kinds: Vec<String> = tilo_view::ViewRegistry::views()
            .map(|view| view.kind.to_owned())
            .collect();
        kinds.sort();
        kinds.dedup();
        kinds
    }

    fn views_of(&self, kind: &str) -> Vec<RustViewDecl> {
        tilo_view::registry::views_of(kind)
            .into_iter()
            .map(|view| RustViewDecl {
                name: view.name.to_owned(),
                is_default: view.is_default,
            })
            .collect()
    }

    fn resolve(&self, kind: &str, name: &str) -> Option<RustViewDecl> {
        tilo_view::ViewRegistry::resolve(kind, name).map(|view| RustViewDecl {
            name: view.name.to_owned(),
            is_default: view.is_default,
        })
    }

    fn render(
        &self,
        kind: &str,
        name: &str,
        target: &RustResource,
        reading: Option<&serde_json::Value>,
        person: &Principal,
        _window: &str,
    ) -> RustRendered {
        let Some(view) = tilo_view::ViewRegistry::resolve(kind, name) else {
            return RustRendered {
                tree: RustNode::default(),
                events: BTreeMap::new(),
            };
        };
        let target = tilo_view::ResourceRef {
            id: target.id.clone(),
            ty: target.kind.clone(),
        };
        let cx = ViewCx::new(
            target.clone(),
            view.name.to_owned(),
            tilo_view::Principal {
                id: person.0.clone(),
                name: person.0.clone(),
            },
            BTreeMap::new(),
        );
        let input = ViewInput { target, reading };
        let tree = (view.render)(&input, &cx);
        convert(tree, &cx.recorded())
    }
}

/// A view's tree as the runtime's, and what its events do, by key and event name.
#[must_use]
pub fn convert(tree: Node, effects: &BTreeMap<(String, String), Effect>) -> RustRendered {
    let events = effects
        .iter()
        .filter_map(|((key, event), effect)| {
            effect_to_runtime(effect).map(|done| ((key.clone(), event.clone()), done))
        })
        .collect();
    RustRendered {
        tree: convert_node(tree),
        events,
    }
}

/// One node of a view's tree as the runtime's.
fn convert_node(node: Node) -> RustNode {
    RustNode {
        kind: node.kind,
        key: node.key,
        props: node
            .props
            .into_iter()
            .map(|(name, value)| (name, value_into_runtime(value)))
            .collect(),
        binding: node.binding.and_then(|binding| {
            let (target, field) = binding.field?;
            Some(tilolang_runtime::rust_views::RustBinding { target, field })
        }),
        anchor: node.anchor,
        children: node.children.into_iter().map(convert_node).collect(),
    }
}

/// One of a view's values as the runtime's.
fn value_into_runtime(value: ViewValue) -> Value {
    match value {
        ViewValue::Unit | ViewValue::Case { .. } => Value::Unit,
        ViewValue::Int(number) => Value::Int(number),
        ViewValue::Float(number) => Value::Float(number),
        ViewValue::Bool(flag) => Value::Bool(flag),
        ViewValue::Text(text) => Value::Text(text),
        ViewValue::Enum(case) => Value::Text(case),
        ViewValue::List(items) => Value::List(items.into_iter().map(value_into_runtime).collect()),
        ViewValue::Opt(held) => Value::Opt(held.map(|held| Box::new(value_into_runtime(*held)))),
        ViewValue::Ref(target) => ResourceRef::parse_id(&target.id, target.ty.clone())
            .map_or(Value::Unit, |at| Value::resource(&at)),
        ViewValue::Principal(who) => Value::Principal(Principal(who.id)),
        ViewValue::Rich(rich) => Value::Rich(tilolang_runtime::rich::RichValue::from_text(
            &rich
                .blocks
                .iter()
                .map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        ViewValue::Error(text) => Value::Error(text),
        ViewValue::Map(entries) => Value::Map(
            entries
                .into_iter()
                .map(|(k, v)| (value_into_runtime(k), value_into_runtime(v)))
                .collect(),
        ),
        ViewValue::Action(offered) => Value::Offered(std::rc::Rc::new(offered)),
        ViewValue::Picture(picture) => Value::Picture(picture.src),
    }
}

/// What a view's event does, as the runtime's.
fn effect_to_runtime(effect: &Effect) -> Option<RustEvent> {
    Some(match effect {
        Effect::Action { action } => RustEvent::Invoke {
            resource: String::new(),
            action: action.clone(),
            args: Vec::new(),
        },
        Effect::On { target, action } => RustEvent::Invoke {
            resource: target.id.clone(),
            action: action.clone(),
            args: Vec::new(),
        },
        Effect::Open { target, view } => RustEvent::Open {
            target: target.id.clone(),
            ty: target.ty.clone(),
            view: view.clone(),
        },
        Effect::State { name } => RustEvent::State { name: name.clone() },
        Effect::None => return None,
    })
}
