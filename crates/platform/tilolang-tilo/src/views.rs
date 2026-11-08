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
        window: &str,
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
        convert(tree, &cx.recorded(), window, &person.0)
    }
}

/// The apps whose views this process links as Rust apps: a resource of one of their types is
/// drawn by the app's own view, not by a package's.
#[must_use]
pub fn declared_apps() -> Vec<&'static str> {
    tilo_view::ViewRegistry::apps()
}

/// A view's tree as the runtime's, and what its events do, by key and event name.
#[must_use]
pub fn convert(
    tree: Node,
    effects: &BTreeMap<(String, String), Effect>,
    window: &str,
    person: &str,
) -> RustRendered {
    let events = effects
        .iter()
        .filter_map(|((key, event), effect)| {
            effect_to_runtime(effect, window, person)
                .map(|done| ((key.clone(), event.clone()), done))
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
        binding: node.binding.map(convert_binding),
        anchor: node.anchor,
        children: node.children.into_iter().map(convert_node).collect(),
    }
}

/// What a view's bound control edits: a resource's field, or the view's own state (whose name
/// is the binding's, as a tilolang view's `state` is named).
fn convert_binding(binding: tilo_view::Binding) -> tilolang_runtime::rust_views::RustBinding {
    use tilolang_runtime::rust_views::RustBinding;
    match binding.field {
        Some((target, field)) => RustBinding::Field { target, field },
        None => RustBinding::State { name: binding.name },
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
        // A view names the person it draws (`who.name`), and that name is what a byline, an
        // avatar's initials and a quote's "In reply to" show: the id is what a record keeps.
        ViewValue::Principal(who) => Value::Principal(Principal::new(who.id).named(who.name)),
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
fn effect_to_runtime(effect: &Effect, window: &str, person: &str) -> Option<RustEvent> {
    Some(match effect {
        Effect::Action { action, args } => RustEvent::Invoke {
            resource: String::new(),
            action: action.clone(),
            args: args
                .iter()
                .map(|arg| arg_into_runtime(arg, window, person))
                .collect(),
        },
        Effect::On {
            target,
            action,
            args,
        } => RustEvent::Invoke {
            resource: target.id.clone(),
            action: action.clone(),
            args: args
                .iter()
                .map(|arg| arg_into_runtime(arg, window, person))
                .collect(),
        },
        Effect::Open { target, view } => RustEvent::Open {
            target: target.id.clone(),
            ty: target.ty.clone(),
            view: view.clone(),
        },
        Effect::OpenEvent { view } => RustEvent::OpenEvent { view: view.clone() },
        Effect::State { name } => RustEvent::State { name: name.clone() },
        Effect::None => return None,
    })
}

/// One argument of a view's action: the value it read, [`Value::Event`] for what the control's
/// event carries at the press (the runtime fills it where it fires), or the view's own state
/// `name`, read at the press — the slot a bound control holds, as a tilolang view's `draft`.
fn arg_into_runtime(arg: &tilo_view::cx::Arg, window: &str, person: &str) -> Value {
    use tilo_view::cx::Arg;
    match arg {
        Arg::Value(value) => value_into_runtime(value.clone()),
        Arg::Event => Value::Event,
        Arg::State(name) => Value::State {
            key: tilolang_runtime::view_state::StateKey::view(window, person, name.as_str()),
            scalar: false,
        },
    }
}
