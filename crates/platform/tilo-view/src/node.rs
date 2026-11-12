//! A view's tree: the nodes a `#[view]` function builds, and what a component's description says
//! of them.
//!
//! A Rust app's view returns described components, exactly as a tilolang view does: the same
//! component kinds the toolkit describes (`crates/dev/ui-graph/components.json`), the same
//! props, the same slots and events. The host builds the widget tree once, from the one
//! description, whichever language wrote the view (one-app-model.md §2).
//!
//! [`Node`] is the wired tree, the one the UI host receives over the component-patch stream
//! ([`tilo_render_protocol::patch`]): it is what a tilolang view's tree becomes too, so a
//! theme, a motion and an accessibility role reach both through the same code.
//!
//! A view builds one with the typed builders in [`ui`](crate::ui), which check each prop against
//! the catalogue where the node is made: a prop the component does not declare, or a value of
//! the wrong type, is a build error rather than a patch the host refuses.

use std::collections::BTreeMap;

pub use tilo_render_protocol::patch::{
    Binding, Event, Node, Offered, Picture, Principal, ResourceRef, Value,
};

/// The patch stream a view's tree travels on, re-exported for the builders.
pub use tilo_render_protocol::patch;

use tilo_ui_graph::component::{Description, Type};

/// The kind of node a described component is: `ui.` and its name (`ui.ListRow`).
#[must_use]
pub fn kind(name: &str) -> String {
    format!("ui.{name}")
}

/// The component a node kind names (`ui.ListRow`: `ListRow`), when it is one.
#[must_use]
pub fn component_name(kind: &str) -> Option<&str> {
    kind.strip_prefix("ui.")
}

/// The catalogue every prop is checked against: the toolkit's, as the shipped JSON declares it.
#[must_use]
pub fn catalogue() -> &'static tilo_ui_graph::component::Catalogue {
    tilo_ui_graph::component::catalogue()
}

/// The description of the component `name`.
#[must_use]
pub fn description(name: &str) -> Option<&'static Description> {
    catalogue().get(name)
}

/// Why a node could not be built: a component the catalogue does not describe, a prop it does
/// not declare, or a value of another type.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{what}")]
pub struct BuildError {
    /// What was wrong, in words a developer reads.
    pub what: String,
}

impl BuildError {
    #[must_use]
    pub fn new(what: impl Into<String>) -> Self {
        Self { what: what.into() }
    }

    /// A component the catalogue does not describe.
    #[must_use]
    pub fn unknown(name: &str) -> Self {
        Self::new(format!("`ui.{name}` is no component the toolkit describes"))
    }

    /// A prop `name` the component `name` does not declare.
    #[must_use]
    pub fn undeclared(component: &str, prop: &str) -> Self {
        Self::new(format!(
            "`ui.{component}` declares no prop `{prop}`; its props are in the catalogue \
             (crates/dev/ui-graph/components.json)"
        ))
    }

    /// A value of another type given to `prop`.
    #[must_use]
    pub fn wrong_type(component: &str, prop: &str, want: &Type, got: &Value) -> Self {
        Self::new(format!(
            "`ui.{component}`'s `{prop}` is a {}, not {}",
            want.show(),
            shape(got)
        ))
    }

    /// A slot `name` the component does not declare.
    #[must_use]
    pub fn undeclared_slot(component: &str, slot: &str) -> Self {
        Self::new(format!(
            "`ui.{component}` declares no slot `{slot}`; its slots are in the catalogue \
             (crates/dev/ui-graph/components.json)"
        ))
    }

    /// An event `name` the component does not declare.
    #[must_use]
    pub fn undeclared_event(component: &str, event: &str) -> Self {
        Self::new(format!(
            "`ui.{component}` declares no event `{event}`; its events are in the catalogue \
             (crates/dev/ui-graph/components.json)"
        ))
    }

    /// A binding `name` the component does not declare.
    #[must_use]
    pub fn undeclared_binding(component: &str, binding: &str) -> Self {
        Self::new(format!(
            "`ui.{component}` declares no binding `{binding}`; its bindings are in the catalogue \
             (crates/dev/ui-graph/components.json)"
        ))
    }
}

/// What kind of value `value` is, for a type error's message.
fn shape(value: &Value) -> &'static str {
    match value {
        Value::Unit => "nothing",
        Value::Int(_) => "a whole number",
        Value::Float(_) => "a number",
        Value::Bool(_) => "a flag",
        Value::Text(_) | Value::Enum(_) => "text",
        Value::List(_) => "a list",
        Value::Opt(_) => "an option",
        Value::Case { .. } => "a case with fields",
        Value::Ref(_) => "a resource reference",
        Value::Principal(_) => "a participant",
        Value::Rich(_) => "rich text",
        Value::Error(_) => "an error",
        Value::Map(_) => "a map",
        Value::Action(_) => "an action",
        Value::Picture(_) => "a picture",
        Value::Link { .. } => "a link",
    }
}

/// The props a node may give, checked against the component's description as they are given.
///
/// [`Self::node`] checks that every required prop is given. A slot's children are the node's
/// `slot` child, as [`slot`] builds it.
#[derive(Clone, Debug)]
pub struct Props<'a> {
    component: &'a str,
    description: &'static Description,
    values: BTreeMap<String, Value>,
}

impl<'a> Props<'a> {
    /// The props of `component`, which the catalogue must describe.
    ///
    /// # Errors
    ///
    /// [`BuildError::unknown`].
    pub fn of(component: &'a str) -> Result<Self, BuildError> {
        let description = description(component).ok_or_else(|| BuildError::unknown(component))?;
        Ok(Self {
            component,
            description,
            values: BTreeMap::new(),
        })
    }

    /// Give the prop `name` the value `value`.
    ///
    /// # Errors
    ///
    /// [`BuildError::undeclared`] or [`BuildError::wrong_type`].
    pub fn set(mut self, name: &str, value: Value) -> Result<Self, BuildError> {
        let declared = self
            .description
            .prop(name)
            .ok_or_else(|| BuildError::undeclared(self.component, name))?;
        if !tilo_render_protocol::patch::validate::fits(&value, &declared.ty) {
            return Err(BuildError::wrong_type(
                self.component,
                name,
                &declared.ty,
                &value,
            ));
        }
        self.values.insert(name.to_owned(), value);
        Ok(self)
    }

    /// The component this is for.
    #[must_use]
    pub fn component(&self) -> &str {
        self.component
    }

    /// The props as given.
    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, Value> {
        &self.values
    }

    /// The node of this component, keyed by `key`, with nothing in its slots yet.
    ///
    /// # Errors
    ///
    /// A required prop the caller did not give.
    pub fn node(self, key: &str) -> Result<Node, BuildError> {
        for prop in self.description.props.iter().filter(|prop| prop.required) {
            if !self.values.contains_key(&prop.name) {
                return Err(BuildError::new(format!(
                    "`ui.{}` needs its `{}` prop",
                    self.component, prop.name
                )));
            }
        }
        Ok(Node {
            kind: kind(self.component),
            key: key.to_owned(),
            props: self.values,
            ..Node::default()
        })
    }
}

/// A node of the described component `component`, keyed by `key`, with nothing else.
///
/// # Errors
///
/// [`BuildError::unknown`] when the catalogue does not describe it.
pub fn component(component: &str, key: &str) -> Result<Node, BuildError> {
    if description(component).is_none() {
        return Err(BuildError::unknown(component));
    }
    Ok(Node {
        kind: kind(component),
        key: key.to_owned(),
        ..Node::default()
    })
}

/// A node of the language's primitive `kind` (`stack`, `row`, `page`): the layouts the host
/// builds itself, which every view composes its components in.
#[must_use]
pub fn primitive(kind: &str, key: &str) -> Node {
    Node {
        kind: kind.to_owned(),
        key: key.to_owned(),
        ..Node::default()
    }
}

/// `node` with its named slot `name` filled with `children`: the node's `slot` child of that
/// name, which the host reads as that component's slot.
#[must_use]
pub fn slot(mut node: Node, name: &str, children: Vec<Node>) -> Node {
    let key = format!("{}#{name}", node.key);
    let mut held = primitive("slot", &key);
    held.props.insert("name".to_owned(), Value::text(name));
    held.children = children;
    node.children.push(held);
    node
}

/// `node` with `children` in its default slot (or, for a primitive, its content): what a
/// component's block holds when it names no slot.
#[must_use]
pub fn with(mut node: Node, children: Vec<Node>) -> Node {
    node.children.extend(children);
    node
}

/// `node` with its event `name` set to `event`: the action the event runs, or the field it
/// toggles.
#[must_use]
pub fn on(mut node: Node, name: &str, event: Event) -> Node {
    node.events.insert(name.to_owned(), event);
    node
}

/// `node` bound through its `name` binding (a text field, a checkbox's flag) to what it edits.
#[must_use]
pub fn bind(mut node: Node, binding: Binding) -> Node {
    node.binding = Some(binding);
    node
}

/// `node` named by the resource or part it is about, for people and agents to reach by.
#[must_use]
pub fn anchor(mut node: Node, anchor: &str) -> Node {
    node.anchor = Some(anchor.to_owned());
    node
}

/// The names of `node`'s slots, as its children declare them.
pub fn slots(node: &Node) -> impl Iterator<Item = (&str, &[Node])> {
    node.children.iter().filter_map(|child| {
        if child.kind != "slot" {
            return None;
        }
        let Some(Value::Text(name)) = child.props.get("name") else {
            return None;
        };
        Some((name.as_str(), child.children.as_slice()))
    })
}

/// The node at `path` (keys from `node`'s children down).
#[must_use]
pub fn at<'a>(node: &'a Node, path: &[String]) -> Option<&'a Node> {
    tilo_render_protocol::patch::diff::at(node, path)
}

/// How many nodes `node`'s tree holds, `node` included.
#[must_use]
pub fn count(node: &Node) -> usize {
    1usize.saturating_add(node.children.iter().map(count).sum::<usize>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prop_the_catalogue_does_not_declare_is_a_build_error() {
        let err = Props::of("ListRow")
            .expect("ListRow is described")
            .set("colour", Value::text("red"))
            .expect_err("ListRow has no colour");
        assert!(err.what.contains("colour"), "{err}");
    }

    #[test]
    fn a_prop_of_another_type_is_a_build_error() {
        let err = Props::of("Badge")
            .expect("Badge is described")
            .set("count", Value::text("many"))
            .expect_err("a count is a whole number");
        assert!(err.what.contains("count"), "{err}");
    }

    #[test]
    fn the_required_props_must_be_given() {
        let err = Props::of("Button")
            .expect("Button is described")
            .node("b")
            .expect_err("a Button needs its label");
        assert!(err.what.contains("label"), "{err}");
    }

    #[test]
    fn a_component_the_catalogue_lacks_is_refused() {
        assert!(Props::of("NoSuchWidget").is_err());
        assert!(component("NoSuchWidget", "x").is_err());
    }

    #[test]
    fn slots_are_named_children_the_host_reads() {
        let row = component("ListRow", "row").expect("ListRow");
        let badge = component("Badge", "b").expect("Badge");
        let node = slot(row, "trailing", vec![badge]);
        let names: Vec<&str> = slots(&node).map(|(name, _)| name).collect();
        assert_eq!(names, vec!["trailing"]);
    }
}
