//! A view's tree: the nodes a `#[view]` function builds, and what a component's
//! description says of them.
//!
//! A Rust app's view returns the same component kinds, props, slots and events the
//! toolkit describes (`crates/dev/ui-graph/components.json`), so the host builds
//! the widget tree once from the one description, whichever language wrote it.
//!
//! [`Node`] is the wired tree the UI host receives over the component-patch
//! stream, so a theme, a motion and an accessibility role reach both through the
//! same code.

use std::collections::BTreeMap;

pub use tilo_render_protocol::patch::{
    Binding, Event, Node, Offered, Picture, Principal, ResourceRef, Value,
};

pub use tilo_render_protocol::patch;

use tilo_ui_graph::component::{Description, Type};

#[must_use]
pub fn kind(name: &str) -> String {
    format!("ui.{name}")
}

#[must_use]
pub fn component_name(kind: &str) -> Option<&str> {
    kind.strip_prefix("ui.")
}

#[must_use]
pub fn catalogue() -> &'static tilo_ui_graph::component::Catalogue {
    tilo_ui_graph::component::catalogue()
}

#[must_use]
pub fn description(name: &str) -> Option<&'static Description> {
    catalogue().get(name)
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{what}")]
pub struct BuildError {
    pub what: String,
}

impl BuildError {
    #[must_use]
    pub fn new(what: impl Into<String>) -> Self {
        Self { what: what.into() }
    }

    #[must_use]
    pub fn unknown(name: &str) -> Self {
        Self::new(format!("`ui.{name}` is no component the toolkit describes"))
    }

    #[must_use]
    pub fn undeclared(component: &str, prop: &str) -> Self {
        Self::new(format!(
            "`ui.{component}` declares no prop `{prop}`; its props are in the catalogue \
             (crates/dev/ui-graph/components.json)"
        ))
    }

    #[must_use]
    pub fn wrong_type(component: &str, prop: &str, want: &Type, got: &Value) -> Self {
        Self::new(format!(
            "`ui.{component}`'s `{prop}` is a {}, not {}",
            want.show(),
            shape(got)
        ))
    }

    #[must_use]
    pub fn undeclared_slot(component: &str, slot: &str) -> Self {
        Self::new(format!(
            "`ui.{component}` declares no slot `{slot}`; its slots are in the catalogue \
             (crates/dev/ui-graph/components.json)"
        ))
    }

    #[must_use]
    pub fn undeclared_event(component: &str, event: &str) -> Self {
        Self::new(format!(
            "`ui.{component}` declares no event `{event}`; its events are in the catalogue \
             (crates/dev/ui-graph/components.json)"
        ))
    }

    #[must_use]
    pub fn undeclared_binding(component: &str, binding: &str) -> Self {
        Self::new(format!(
            "`ui.{component}` declares no binding `{binding}`; its bindings are in the catalogue \
             (crates/dev/ui-graph/components.json)"
        ))
    }
}

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

    #[must_use]
    pub fn component(&self) -> &str {
        self.component
    }

    #[must_use]
    pub fn values(&self) -> &BTreeMap<String, Value> {
        &self.values
    }

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

#[must_use]
pub fn primitive(kind: &str, key: &str) -> Node {
    Node {
        kind: kind.to_owned(),
        key: key.to_owned(),
        ..Node::default()
    }
}

#[must_use]
pub fn slot(mut node: Node, name: &str, children: Vec<Node>) -> Node {
    let key = format!("{}#{name}", node.key);
    let mut held = primitive("slot", &key);
    held.props.insert("name".to_owned(), Value::text(name));
    held.children = children;
    node.children.push(held);
    node
}

#[must_use]
pub fn with(mut node: Node, children: Vec<Node>) -> Node {
    node.children.extend(children);
    node
}

#[must_use]
pub fn on(mut node: Node, name: &str, event: Event) -> Node {
    node.events.insert(name.to_owned(), event);
    node
}

#[must_use]
pub fn bind(mut node: Node, binding: Binding) -> Node {
    node.binding = Some(binding);
    node
}

#[must_use]
pub fn anchor(mut node: Node, anchor: &str) -> Node {
    node.anchor = Some(anchor.to_owned());
    node
}

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

#[must_use]
pub fn at<'a>(node: &'a Node, path: &[String]) -> Option<&'a Node> {
    tilo_render_protocol::patch::diff::at(node, path)
}

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
