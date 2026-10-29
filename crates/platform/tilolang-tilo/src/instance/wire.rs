//! A view's tree as its window receives it, and the patches that bring the window's copy of it
//! to the view's tree now.
//!
//! The runtime's tree holds values the window cannot read (handles, the module that wrote a
//! binding) and lacks what the window needs and cannot look up (what the room calls a person,
//! what a bound field is called, an enum's cases). [`Convert`] makes the window's node of each
//! of the runtime's; [`diff`] the patches from the tree the window holds to the one it should.

use std::collections::HashSet;

use tilo_render_protocol::patch as wire;

use crate::runtime::rich::{Reference, RichValue};
use crate::runtime::{Binding, NameHost, ResourceRef, UiNode, Value};
use tilo_render_protocol::patch::rich::{self as rich_wire, InlineView};

/// The kind of a node holding a component's named slot.
pub const SLOT: &str = "slot";

/// The prop of a `choice` node the window shows its cases from: `[name, label, icon]` each.
pub const CASES: &str = "cases";

/// A case of an enum as a control shows it: its name, what it says, and its icon's name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Case {
    pub name: String,
    pub label: String,
    pub icon: Option<String>,
}

/// The resource and the field an action toggles.
pub type Toggled = Option<(ResourceRef, String)>;

/// What a conversion reads beside the tree.
pub struct Convert<'a> {
    pub names: &'a dyn NameHost,
    /// What the field or view state a binding edits is called.
    pub field_name: &'a dyn Fn(&Binding) -> String,
    /// The cases of the enum `id`.
    pub cases: &'a dyn Fn(&str) -> Vec<Case>,
    /// The resource and field the event `event` of the node `key` toggles, when it does.
    pub toggles: &'a dyn Fn(&str, &str) -> Toggled,
    /// The resource and the action the event `event` of the node `key` runs, when it runs one.
    pub invokes: &'a dyn Fn(&str, &str) -> Toggled,
    /// How a reference in rich text reads for the person reading.
    pub embeds: &'a dyn Fn(&Reference) -> InlineView,
}

impl Convert<'_> {
    /// The window's node of `node`. A virtual list's rows are left out: the window reads them
    /// as the list shows them.
    #[must_use]
    pub fn node(&self, node: &UiNode) -> wire::Node {
        let virtual_list = wire::is_virtual_list(&node.kind);
        let mut props: std::collections::BTreeMap<String, wire::Value> = node
            .props
            .iter()
            .map(|(name, value)| (name.clone(), self.value(value)))
            .collect();
        if node.kind == "choice"
            && let Some(Value::Text(id)) = node.props.get("enum")
        {
            let cases = (self.cases)(id)
                .into_iter()
                .map(|case| {
                    wire::Value::List(vec![
                        wire::Value::Text(case.name),
                        wire::Value::Text(case.label),
                        wire::Value::Opt(case.icon.map(|icon| Box::new(wire::Value::Text(icon)))),
                    ])
                })
                .collect();
            props.insert(CASES.to_owned(), wire::Value::List(cases));
        }
        wire::Node {
            kind: node.kind.clone(),
            key: node.key.clone(),
            props,
            binding: node.binding.as_ref().map(|binding| self.binding(binding)),
            events: node
                .events
                .keys()
                .map(|event| {
                    let toggles = (self.toggles)(&node.key, event)
                        .map(|(target, field)| (target.id_string(), field));
                    let invokes = (self.invokes)(&node.key, event)
                        .map(|(target, action)| (target.id_string(), action));
                    (event.clone(), wire::Event { toggles, invokes })
                })
                .collect(),
            anchor: node.anchor.clone(),
            children: node
                .children
                .iter()
                .filter(|child| !virtual_list || child.kind == SLOT)
                .map(|child| self.node(child))
                .collect(),
        }
    }

    /// The window's binding of `binding`.
    #[must_use]
    pub fn binding(&self, binding: &Binding) -> wire::Binding {
        wire::Binding {
            field: binding
                .field()
                .map(|(target, field)| (target.id_string(), field.clone())),
            name: (self.field_name)(binding),
        }
    }

    /// The window's value of `value`.
    #[must_use]
    pub fn value(&self, value: &Value) -> wire::Value {
        match value {
            Value::Int(number) => wire::Value::Int(*number),
            Value::Float(number) => wire::Value::Float(*number),
            Value::Bool(flag) => wire::Value::Bool(*flag),
            Value::Text(text) => wire::Value::Text(text.clone()),
            Value::Rich(rich) => wire::Value::Rich(self.rich(rich)),
            Value::List(items) | Value::Tuple(items) => {
                wire::Value::List(items.iter().map(|item| self.value(item)).collect())
            }
            Value::Opt(held) => {
                wire::Value::Opt(held.as_deref().map(|held| Box::new(self.value(held))))
            }
            Value::Enum(case) => wire::Value::Enum(case.variant.clone()),
            Value::Ref(reference) => wire::Value::Ref(resource(&reference.target)),
            Value::Principal(who) => wire::Value::Principal(wire::Principal {
                id: who.0.clone(),
                name: self.names.name(who),
            }),
            Value::Error(text) => wire::Value::Error(text.clone()),
            Value::Map(entries) => wire::Value::Map(
                entries
                    .iter()
                    .map(|(key, value)| (self.value(key), self.value(value)))
                    .collect(),
            ),
            Value::Offered(offered) => wire::Value::Action((**offered).clone()),
            Value::Unit
            | Value::Coll(_)
            | Value::Action(_)
            | Value::Node(_)
            | Value::Rec(_)
            | Value::Event
            | Value::EventPart(_)
            | Value::State { .. }
            | Value::AtEvent { .. } => wire::Value::Unit,
        }
    }
}

impl Convert<'_> {
    /// The window's rich text of `rich`: its paragraphs (a list's items after the list), and
    /// how each reference in it reads for the person reading.
    #[must_use]
    pub fn rich(&self, rich: &RichValue) -> rich_wire::RichText {
        fn push(block: &tilo_doc::snapshot::BlockView, out: &mut Vec<rich_wire::Block>) {
            let attr = |value: &tilo_doc::AttrVal| match value {
                tilo_doc::AttrVal::Int(number) => rich_wire::Attr::Int(*number),
                tilo_doc::AttrVal::Bool(flag) => rich_wire::Attr::Bool(*flag),
                tilo_doc::AttrVal::Str(text) => rich_wire::Attr::Text(text.clone()),
                tilo_doc::AttrVal::Float(number) => rich_wire::Attr::Float(*number),
            };
            out.push(rich_wire::Block {
                kind: block.kind.clone(),
                text: block.text.clone(),
                marks: block
                    .marks
                    .iter()
                    .map(|mark| rich_wire::Mark {
                        start: mark.start,
                        end: mark.end,
                        name: mark.name.clone(),
                        value: match &mark.value {
                            tilo_doc::document::MarkValue::Bool(flag) => {
                                rich_wire::Attr::Bool(*flag)
                            }
                            tilo_doc::document::MarkValue::Int(number) => {
                                rich_wire::Attr::Int(*number)
                            }
                            tilo_doc::document::MarkValue::Str(text) => {
                                rich_wire::Attr::Text(text.clone())
                            }
                        },
                    })
                    .collect(),
                attrs: block
                    .attrs
                    .iter()
                    .map(|(key, value)| (key.clone(), attr(value)))
                    .collect(),
            });
            for child in &block.children {
                push(child, out);
            }
        }
        let mut blocks = Vec::with_capacity(rich.blocks.len());
        for block in &rich.blocks {
            push(block, &mut blocks);
        }
        let embeds = rich
            .references()
            .iter()
            .map(|reference| rich_wire::Embed {
                reference: reference.mark_value(),
                view: (self.embeds)(reference),
            })
            .collect();
        rich_wire::RichText { blocks, embeds }
    }
}

/// The runtime's rich text of what a rich field in the window now holds: its paragraphs with
/// their marks and attributes. How the window read its references is the window's; the text
/// keeps only what each refers to.
#[must_use]
pub fn runtime_rich(rich: &rich_wire::RichText) -> RichValue {
    let attr = |value: &rich_wire::Attr| match value {
        rich_wire::Attr::Int(number) => tilo_doc::AttrVal::Int(*number),
        rich_wire::Attr::Bool(flag) => tilo_doc::AttrVal::Bool(*flag),
        rich_wire::Attr::Text(text) => tilo_doc::AttrVal::Str(text.clone()),
        rich_wire::Attr::Float(number) => tilo_doc::AttrVal::Float(*number),
    };
    let mark = |value: &rich_wire::Attr| match value {
        rich_wire::Attr::Bool(flag) => tilo_doc::document::MarkValue::Bool(*flag),
        rich_wire::Attr::Int(number) => tilo_doc::document::MarkValue::Int(*number),
        rich_wire::Attr::Text(text) => tilo_doc::document::MarkValue::Str(text.clone()),
        rich_wire::Attr::Float(number) => tilo_doc::document::MarkValue::Str(number.to_string()),
    };
    RichValue {
        blocks: rich
            .blocks
            .iter()
            .map(|block| tilo_doc::snapshot::BlockView {
                id: String::new(),
                kind: block.kind.clone(),
                text: block.text.clone(),
                marks: block
                    .marks
                    .iter()
                    .map(|m| tilo_doc::document::MarkSpan {
                        start: m.start,
                        end: m.end,
                        name: m.name.clone(),
                        value: mark(&m.value),
                    })
                    .collect(),
                attrs: block
                    .attrs
                    .iter()
                    .map(|(key, value)| (key.clone(), attr(value)))
                    .collect(),
                children: Vec::new(),
            })
            .collect(),
    }
}

/// The window's reference to `target`.
#[must_use]
pub fn resource(target: &ResourceRef) -> wire::ResourceRef {
    wire::ResourceRef {
        id: target.id_string(),
        ty: target.ty.clone(),
    }
}

/// The runtime's reference to what the window named.
#[must_use]
pub fn runtime_resource(target: &wire::ResourceRef) -> Option<ResourceRef> {
    ResourceRef::parse_id(&target.id, target.ty.clone())
}

/// The runtime's value of what a window's event carries.
#[must_use]
pub fn runtime_value(value: &wire::Value) -> Option<Value> {
    Some(match value {
        wire::Value::Text(text) | wire::Value::Enum(text) => Value::Text(text.clone()),
        wire::Value::Int(number) => Value::Int(*number),
        wire::Value::Float(number) => Value::Float(*number),
        wire::Value::Bool(flag) => Value::Bool(*flag),
        wire::Value::Ref(target) => Value::resource(&runtime_resource(target)?),
        wire::Value::Principal(who) => {
            Value::Principal(crate::runtime::Principal::new(who.id.clone()))
        }
        // What a drop carries: the card and its place.
        wire::Value::List(items) => {
            Value::List(items.iter().map(runtime_value).collect::<Option<_>>()?)
        }
        wire::Value::Unit
        | wire::Value::Opt(_)
        | wire::Value::Rich(_)
        | wire::Value::Error(_)
        | wire::Value::Map(_)
        | wire::Value::Action(_) => {
            return None;
        }
    })
}

/// The patches that bring `old`, the tree the window holds, to `new`, appended to `out`.
pub fn diff(old: &wire::Node, new: &wire::Node, out: &mut Vec<wire::Patch>) {
    if old.kind != new.kind || old.key != new.key || !unique(old) || !unique(new) {
        out.push(wire::Patch::SetRoot(new.clone()));
        return;
    }
    diff_node(old, new, &mut Vec::new(), out);
}

/// Whether the keys of each node's children are unique, all the way down: what patches by
/// key need.
fn unique(node: &wire::Node) -> bool {
    let mut seen = HashSet::with_capacity(node.children.len());
    node.children.iter().all(|child| seen.insert(&child.key)) && node.children.iter().all(unique)
}

fn diff_node(
    old: &wire::Node,
    new: &wire::Node,
    path: &mut Vec<String>,
    out: &mut Vec<wire::Patch>,
) {
    for (name, value) in &new.props {
        if old.props.get(name) != Some(value) {
            out.push(wire::Patch::SetProp {
                path: path.clone(),
                name: name.clone(),
                value: Some(value.clone()),
            });
        }
    }
    for name in old
        .props
        .keys()
        .filter(|name| !new.props.contains_key(*name))
    {
        out.push(wire::Patch::SetProp {
            path: path.clone(),
            name: name.clone(),
            value: None,
        });
    }
    if old.binding != new.binding {
        out.push(wire::Patch::SetBinding {
            path: path.clone(),
            binding: new.binding.clone(),
        });
    }
    for (event, action) in &new.events {
        if old.events.get(event) != Some(action) {
            out.push(wire::Patch::SetEvent {
                path: path.clone(),
                event: event.clone(),
                action: Some(action.clone()),
            });
        }
    }
    for event in old
        .events
        .keys()
        .filter(|event| !new.events.contains_key(*event))
    {
        out.push(wire::Patch::SetEvent {
            path: path.clone(),
            event: event.clone(),
            action: None,
        });
    }
    if old.anchor != new.anchor {
        out.push(wire::Patch::SetAnchor {
            path: path.clone(),
            anchor: new.anchor.clone(),
        });
    }
    diff_children(old, new, path, out);
}

/// The children of `old` brought to `new`'s by key: those gone removed, those kept moved to
/// their place and brought up to date there, those new inserted.
fn diff_children(
    old: &wire::Node,
    new: &wire::Node,
    path: &mut Vec<String>,
    out: &mut Vec<wire::Patch>,
) {
    let wanted: HashSet<&str> = new
        .children
        .iter()
        .map(|child| child.key.as_str())
        .collect();
    let mut held: Vec<&wire::Node> = Vec::with_capacity(old.children.len());
    for child in &old.children {
        if wanted.contains(child.key.as_str()) {
            held.push(child);
        } else {
            out.push(wire::Patch::Remove {
                parent: path.clone(),
                key: child.key.clone(),
            });
        }
    }
    for (index, child) in new.children.iter().enumerate() {
        let at = held.iter().position(|old| old.key == child.key);
        let Some(at) = at else {
            out.push(wire::Patch::Insert {
                parent: path.clone(),
                index: u32::try_from(index).unwrap_or(u32::MAX),
                node: child.clone(),
            });
            held.insert(index.min(held.len()), child);
            continue;
        };
        if at != index {
            out.push(wire::Patch::Move {
                parent: path.clone(),
                key: child.key.clone(),
                to: u32::try_from(index).unwrap_or(u32::MAX),
            });
            let moved = held.remove(at);
            held.insert(index.min(held.len()), moved);
        }
        let before = held.get(index).copied();
        if let Some(before) = before
            && before != child
        {
            if before.kind == child.kind {
                path.push(child.key.clone());
                diff_node(before, child, path, out);
                path.pop();
            } else {
                out.push(wire::Patch::Remove {
                    parent: path.clone(),
                    key: child.key.clone(),
                });
                out.push(wire::Patch::Insert {
                    parent: path.clone(),
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    node: child.clone(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(key: &str, value: &str) -> wire::Node {
        let mut node = wire::Node::empty("ui.Text", key);
        node.props
            .insert("value".to_owned(), wire::Value::text(value));
        node
    }

    fn stack(key: &str, children: Vec<wire::Node>) -> wire::Node {
        wire::Node {
            children,
            ..wire::Node::empty("stack", key)
        }
    }

    /// `old` patched by the diff to `new` is `new`.
    fn round_trip(old: &wire::Node, new: &wire::Node) -> Vec<wire::Patch> {
        let mut patches = Vec::new();
        diff(old, new, &mut patches);
        let mut applied = old.clone();
        for patch in patches.clone() {
            patch.apply(&mut applied).expect("the diff's patches apply");
        }
        assert_eq!(&applied, new, "{patches:#?}");
        patches
    }

    #[test]
    fn the_diff_brings_the_windows_tree_to_the_views_by_key() {
        let old = stack(
            "s",
            vec![
                text("a", "a"),
                text("b", "b"),
                stack("c", vec![text("c1", "1")]),
            ],
        );
        let new = stack(
            "s",
            vec![
                text("n", "n"),
                stack("c", vec![text("c1", "2"), text("c2", "3")]),
                text("a", "a"),
            ],
        );
        let patches = round_trip(&old, &new);
        assert!(
            !patches
                .iter()
                .any(|patch| matches!(patch, wire::Patch::SetRoot(_))),
            "a change by key, not the tree anew: {patches:#?}"
        );
        assert!(round_trip(&new, &new).is_empty(), "no change, no patch");
        // A child that changes kind under the same key is replaced.
        let swapped = stack("s", vec![wire::Node::empty("divider", "a")]);
        round_trip(&old, &swapped);
        // A root of another kind is the tree anew.
        let other = wire::Node::empty("page", "s");
        assert_eq!(
            round_trip(&old, &other),
            vec![wire::Patch::SetRoot(other.clone())]
        );
    }

    #[test]
    fn siblings_sharing_a_key_are_sent_whole() {
        let old = stack("s", vec![text("a", "1"), text("a", "2")]);
        let new = stack("s", vec![text("a", "3")]);
        assert_eq!(
            round_trip(&old, &new),
            vec![wire::Patch::SetRoot(new.clone())]
        );
    }
}
