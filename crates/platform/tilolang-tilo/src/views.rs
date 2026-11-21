//! The views a Rust app declares, as the runtime draws them.
//!
//! A `#[view]` function builds [`tilo_view::Node`]s, described components checked against the
//! catalogue. The runtime draws a resource of a Rust app's type through its view the way it draws
//! a tilolang package's: a boundary, keyed and wired, whose tree the host builds from the one
//! catalogue (one-app-model.md §2). This module is the boundary: it takes the views a crate registered
//! ([`tilo_view::ViewRegistry`]) and hands the runtime a [`RustViews`] over them, converting each
//! tree the view builds into the runtime's own node kinds, props and events.

use std::collections::BTreeMap;

use tilo_view::registry::ViewInput;
use tilo_view::{Effect, Node, Value as ViewValue, ViewCx};
use tilolang_runtime::ids::Actor;
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
        person: &Actor,
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
            tilo_view::Actor {
                id: person.clone(),
                name: person.id().to_owned(),
                hue: None,
            },
            BTreeMap::new(),
        );
        let input = ViewInput { target, reading };
        let tree = (view.render)(&input, &cx);
        convert(tree, &cx.recorded(), window, person)
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
    person: &Actor,
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
        // A link a Rust view builds names a document, which a value of the runtime's does not
        // carry: what the runtime's own views send, and this one, is the document itself.
        ViewValue::Unit | ViewValue::Case { .. } | ViewValue::Link { .. } => Value::Unit,
        ViewValue::Int(number) => Value::Int(number),
        ViewValue::Float(number) => Value::Float(number),
        ViewValue::Bool(flag) => Value::Bool(flag),
        ViewValue::Text(text) => Value::Text(text),
        ViewValue::Enum(case) => Value::Text(case),
        ViewValue::List(items) => Value::List(items.into_iter().map(value_into_runtime).collect()),
        ViewValue::Opt(held) => Value::Opt(held.map(|held| Box::new(value_into_runtime(*held)))),
        ViewValue::Ref(target) => ResourceRef::parse_id(&target.id, target.ty.clone())
            .map_or(Value::Unit, |at| Value::resource(&at)),
        ViewValue::Actor(who) => Value::Actor(who.id),
        // Its paragraphs as the view set them: each block's kind (a list's items, a heading),
        // marks and attributes, never only its words — a reply's bulleted list stays a list.
        ViewValue::Rich(rich) => Value::Rich(crate::instance::wire::runtime_rich(&rich)),
        ViewValue::Error(text) => Value::Error(text),
        ViewValue::Map(entries) => Value::Map(
            entries
                .into_iter()
                .map(|(k, v)| (value_into_runtime(k), value_into_runtime(v)))
                .collect(),
        ),
        ViewValue::Action(offered) => Value::Offered(std::rc::Rc::new(offered)),
        ViewValue::Picture(picture) => Value::Picture(picture.src),
        // A file a person handed the window, as the runtime's `File` reads it: its name, its
        // media type, its size and the hash its bytes are kept at.
        ViewValue::File(file) => {
            let hash = file
                .url
                .strip_prefix(crate::runtime::blob::SCHEME)
                .unwrap_or(&file.url)
                .to_owned();
            Value::File(std::rc::Rc::new(crate::runtime::blob::FileValue::new(
                file.name, file.media, file.size, hash,
            )))
        }
    }
}

/// What a view's event does, as the runtime's.
fn effect_to_runtime(effect: &Effect, window: &str, person: &Actor) -> Option<RustEvent> {
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
fn arg_into_runtime(arg: &tilo_view::cx::Arg, window: &str, person: &Actor) -> Value {
    use tilo_view::cx::Arg;
    match arg {
        Arg::Value(value) => value_into_runtime(value.clone()),
        Arg::Event => Value::Event,
        Arg::State(name) => Value::State {
            key: tilolang_runtime::view_state::StateKey::view(
                window,
                person.clone(),
                name.as_str(),
            ),
            scalar: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use tilo_view::patch::rich::{Attr, Block, RichText};

    use super::*;

    /// A Rust view's rich text reaches the runtime as the view set it: a reply's bulleted list
    /// keeps its items' kind and level, and a bold run its mark, never only the words of each line.
    #[test]
    fn a_rust_views_rich_text_keeps_its_lists_and_marks() {
        let block = |kind: &str, text: &str| Block {
            kind: kind.to_owned(),
            text: text.to_owned(),
            marks: Vec::new(),
            attrs: BTreeMap::new(),
        };
        let mut item = block("list_item", "Book a haircut");
        item.attrs.insert("level".to_owned(), Attr::Int(0));
        item.attrs.insert("ordered".to_owned(), Attr::Bool(false));
        let mut lead = block("paragraph", "Overdue:");
        lead.marks.push(tilo_view::patch::rich::Mark {
            start: 0,
            end: 7,
            name: "strong".to_owned(),
            value: Attr::Bool(true),
        });
        let rich = RichText {
            blocks: vec![lead, item],
            ..RichText::default()
        };
        let Value::Rich(read) = value_into_runtime(ViewValue::Rich(rich)) else {
            panic!("rich text stays rich");
        };
        let kinds: Vec<(&str, &str)> = read
            .blocks
            .iter()
            .map(|b| (b.kind.as_str(), b.text.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [("paragraph", "Overdue:"), ("list_item", "Book a haircut")]
        );
        assert_eq!(
            read.blocks[1].attrs.get("ordered"),
            Some(&tilo_doc::AttrVal::Bool(false))
        );
        assert_eq!(read.blocks[0].marks.len(), 1, "the bold run keeps its mark");
    }
}
