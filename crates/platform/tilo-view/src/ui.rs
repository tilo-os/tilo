//! The `ui` builders: the toolkit's described components, as a Rust view calls them.
//!
//! Each is generated from the catalogue (`crates/dev/ui-graph/components.json`, [`build.rs`]),
//! the same file tilolang's `ui.X` is checked against. A view writes what a tilolang view
//! writes, in Rust:
//!
//! ```
//! # use tilo_view::ui;
//! let title = ui::TextBuilder::new("title", "Groceries");
//! let row: tilo_view::node::Node = ui::ListRowBuilder::new("row").into();
//! ```
//!
//! [`build`] makes a node of a component by name, checked against the catalogue where it is
//! made; [`put`] sets one prop, checked by the component's description. The generated
//! builders call those, so a prop no component declares, or a value of the wrong type, is a
//! message the developer reads (never a patch the host refuses later).
//!
//! The layout primitives ([`stack`], [`row`], [`column`], [`page`], [`grid`], [`split`],
//! [`layers`]) are the language's own, not described components: the host builds them from
//! their names, and a view composes components in them as a tilolang view does.

use crate::node::{self, BuildError, Node, Value};

include!(concat!(env!("OUT_DIR"), "/ui_builders.rs"));

/// A node of the component `name`, checked against the catalogue.
///
/// # Panics
///
/// When the catalogue does not describe `name`: the generated builders pass only names it
/// knows, so this is a build-time error the developer fixes, not a thing a view can cause.
#[must_use]
#[expect(
    clippy::panic,
    reason = "the generated builders name only the catalogue's components, so this is a \
              programming error a developer fixes at the call site (as `get` on a missing \
              component is), never a view's to cause at run time"
)]
pub fn build(name: &str, key: &str) -> Node {
    match node::component(name, key) {
        Ok(node) => node,
        Err(error) => panic!("{error}"),
    }
}

/// `node` with the prop `name` set to `value`, checked against the component's description.
///
/// # Panics
///
/// When the component does not declare `name`, or `value` is of another type: the generated
/// builders pass only the catalogue's props with values of their declared type.
#[must_use]
pub fn put(node: Node, name: &str, value: Value) -> Node {
    set(node, name, value)
}

/// `node` with the prop `name` set to `value`.
///
/// # Errors
///
/// [`BuildError::undeclared`] or [`BuildError::wrong_type`].
pub fn try_set(mut node: Node, name: &str, value: Value) -> Result<Node, BuildError> {
    let component = crate::node::component_name(&node.kind)
        .ok_or_else(|| BuildError::new(format!("`{}` is no described component", node.kind)))?;
    let description = crate::node::description(component).ok_or_else(|| {
        BuildError::new(format!(
            "`{}` is no component the toolkit describes",
            node.kind
        ))
    })?;
    let Some(declared) = description.prop(name) else {
        return Err(BuildError::undeclared(component, name));
    };
    if !tilo_render_protocol::patch::validate::fits(&value, &declared.ty) {
        return Err(BuildError::wrong_type(
            component,
            name,
            &declared.ty,
            &value,
        ));
    }
    node.props.insert(name.to_owned(), value);
    Ok(node)
}

/// [`try_set`], as the generated builders call it.
///
/// # Panics
///
/// As [`put`].
#[must_use]
#[expect(
    clippy::panic,
    reason = "the generated builders pass only the catalogue's props with values of their \
              declared type, so this is a programming error at the call site"
)]
pub fn set(node: Node, name: &str, value: Value) -> Node {
    match try_set(node, name, value) {
        Ok(node) => node,
        Err(error) => panic!("{error}"),
    }
}

/// A text value from anything that is one (`String`, `&str`, a `Text` of the app's model).
#[must_use]
pub fn text(value: impl Into<String>) -> Value {
    Value::Text(value.into())
}

/// A list's items as the wire holds them.
#[must_use]
pub fn list(items: Vec<Value>) -> Value {
    Value::List(items)
}

/// A map's entries as the wire holds them, each key once, in key order.
#[must_use]
pub fn map(mut entries: Vec<(Value, Value)>) -> Value {
    entries.sort_by_cached_key(|(key, _)| format!("{key:?}"));
    entries.dedup_by(|(a, _), (b, _)| a == b);
    Value::Map(entries)
}

/// A stack: children one under another, with the gap `gap` (`sibling` unless said). The
/// language's layout primitive.
#[must_use]
pub fn stack(key: &str, gap: Gap, children: Vec<Node>) -> Node {
    let mut node = node::primitive("stack", key);
    node.props
        .insert("gap".to_owned(), Value::text(gap.as_str()));
    node.children = children;
    node
}

/// A row: children side by side, with the gap `gap`, aligned `align`, and wrapped when asked.
#[must_use]
pub fn row(key: &str, gap: Gap, children: Vec<Node>) -> Node {
    let mut node = node::primitive("row", key);
    node.props
        .insert("gap".to_owned(), Value::text(gap.as_str()));
    node.children = children;
    node
}

/// A column: children one under another, filling the width they are given.
#[must_use]
pub fn column(key: &str, gap: Gap, children: Vec<Node>) -> Node {
    let mut node = node::primitive("column", key);
    node.props
        .insert("gap".to_owned(), Value::text(gap.as_str()));
    node.children = children;
    node
}

/// A page: the outermost layout of a window (a gutter and a readable measure), or a stack when
/// it stands within another view's layout.
#[must_use]
pub fn page(key: &str, children: Vec<Node>) -> Node {
    let mut node = node::primitive("page", key);
    node.children = children;
    node
}

/// A virtual list: the rows of a long list, built as they scroll into view.
///
/// The rows are the list's [`ui::ListItem`](crate::ui::ListItemBuilder) children; the list's own
/// `key` is what hosts a view's virtualization on.
#[must_use]
pub fn virtual_list(key: &str, rows: Vec<Node>) -> Node {
    let mut node = build("VirtualList", key);
    node.children = rows;
    node
}

/// The gap between a layout's children: the design system's rhythms, never a measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gap {
    /// Neighbours that meet: a window's bar and the content under it, a row's lead and its
    /// message, as the language's `.none` gap.
    None,
    Tight,
    Related,
    Sibling,
    Section,
    Group,
}

impl Gap {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Tight => "tight",
            Self::Related => "related",
            Self::Sibling => "sibling",
            Self::Section => "section",
            Self::Group => "group",
        }
    }
}

/// Give `node` the sizing argument `arg` (the language's `fill`, `min`, `max`, `at`).
#[must_use]
pub fn arg(mut node: Node, name: &str, value: Value) -> Node {
    node.props.insert(name.to_owned(), value);
    node
}

/// A node that takes the room its parent's layout leaves it (`fill`).
#[must_use]
pub fn fill(node: Node) -> Node {
    arg(node, "fill", Value::Float(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generated_builders_make_nodes_the_host_reads() {
        let text = TextBuilder::new("greeting", "Hello").role(TextRole::Title);
        let node: Node = text.into();
        assert_eq!(node.kind, "ui.Text");
        assert_eq!(node.key, "greeting");
        assert_eq!(
            node.props.get("value"),
            Some(&Value::text("Hello")),
            "the positional prop is given"
        );
        assert_eq!(
            node.props.get("role"),
            Some(&Value::Enum("title".to_owned())),
            "a choice travels as its case"
        );
    }

    #[test]
    fn a_required_prop_is_a_constructor_argument() {
        let node: Node = ButtonBuilder::new("send", "Send").into();
        assert_eq!(node.props.get("label"), Some(&Value::text("Send")));
    }

    #[test]
    fn the_generated_code_names_every_component_the_catalogue_describes() {
        // A component the catalogue adds is a builder in the generated code: the test reads the
        // generated source back and finds a builder for each, so a projection that silently
        // dropped one is a failure.
        let generated = include_str!(concat!(env!("OUT_DIR"), "/ui_builders.rs"));
        for description in &crate::node::catalogue().components {
            let builder = format!("pub struct {}Builder", description.name);
            assert!(
                generated.contains(&builder),
                "{}'s builder is generated",
                description.name
            );
        }
    }
}
