//! The package the toolkit provides: its described components, read as the package `ui`.
//!
//! Every module sees it as `ui`, the way it sees the host's package as `host`. The descriptions
//! are the toolkit's own (`tilo_ui_graph::component`), generated from each widget's Rust
//! definition, so `ui.ListRow(title: ...)` is checked against the widget as it is.

pub use tilo_ui_graph::component::{Accepts, Catalogue, Description, Offered, Type};

/// The app id of the toolkit's package.
pub const UI_APP: &str = "tilo.ui";
/// The name every module reaches it by.
pub const UI_ALIAS: &str = "ui";

/// The prefix of a component node's kind: `ui.ListRow`.
pub const KIND_PREFIX: &str = "ui.";

/// The toolkit's component descriptions.
#[must_use]
pub fn catalogue() -> &'static Catalogue {
    tilo_ui_graph::component::catalogue()
}

/// The node kind of the component `name`.
#[must_use]
pub fn kind(name: &str) -> String {
    format!("{KIND_PREFIX}{name}")
}

/// The node kind a primitive of the language builds, when it builds a described component:
/// `text` is the toolkit's `Text`, `row(title)` its `ListRow`.
#[must_use]
pub fn prim_component(prim: &str) -> Option<&'static str> {
    Some(match prim {
        "text" => "Text",
        "rich_text" => "RichText",
        "button" => "Button",
        "input" => "TextInput",
        "rich_input" => "RichInput",
        "section" => "Section",
        "card" => "Card",
        "facts" => "PropertyList",
        "fact" => "Property",
        "avatar" => "Avatar",
        "list_row" => "ListRow",
        _ => return None,
    })
}
