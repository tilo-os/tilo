//! A Rust app's views: named functions of a resource that return described components,
//! exactly as a tilolang view returns them.
//!
//! The app sends that tree the way a tilolang app sends its views, over the
//! component-patch stream, and the shell draws it in whatever place shows the
//! resource. Nothing in the app knows about windows, places or themes.
//! ```
//! # use tilo_view::{ui, ViewCx};
//! # use tilo_view::node::{Node, ResourceRef, Principal};
//! fn page(cx: &ViewCx) -> Node {
//!     let node = ui::TextBuilder::new(&cx.key("title"), "Groceries").into();
//!     ui::stack(&cx.key("page"), ui::Gap::Sibling, vec![node])
//! }
//! ```
//!
//! The pieces are [`node`] (the tree a view builds, and the catalogue check each
//! prop passes), [`ui`] (the generated builders), [`cx`] (what the view is handed
//! besides the resource) and [`registry`] (the views an app declares, by resource
//! type and name). What a view looks like is the platform's: it is built from the
//! same described components a tilolang view is, so a theme, a motion, an
//! accessibility role and the keyboard reach it through the same code.

pub mod cx;
pub mod node;
pub mod registry;
#[allow(
    clippy::redundant_closure,
    clippy::map_identity,
    clippy::should_implement_trait,
    reason = "the generated builders pass every item through its converter, even where a \
              function reference or no call would do; and a component's prop may be named \
              `add`, which becomes a method"
)]
pub mod ui;

pub use cx::{Arg, Effect, ViewCx};
pub use node::{Binding, BuildError, Event, Node, Offered, Picture, Principal, ResourceRef, Value};
pub use registry::{View, ViewRegistry, ViewResource};

pub use tilo_render_protocol::patch;

pub use {inventory, serde, serde_json};
