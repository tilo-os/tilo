//! The UI host's half of a tilolang app's window.
//!
//! A tilolang app's views run in an app instance (`tilolang_tilo::instance`), which holds the
//! data and sends its window component patches (`tilo_render_protocol::patch`). This crate is
//! the other half, the one the room's UI host runs for every window: it keeps a copy of each
//! view's tree, composes the window's one shell from them, builds and reconciles the toolkit's
//! widgets through the one generic adapter, and reports what the person did. Text is edited in
//! the toolkit's own fields, so typing shows on the next frame; the app hears of it as a splice.
//!
//! - [`window::AppWindow`]: one app window, as a widget: what it is sent, applied; what the
//!   person does, sent back.
//! - [`adapter`]: a node of a described component as the toolkit's widget, read through the
//!   component's description.
//! - [`reconcile`]: a tree as retained widgets, reconciled by key.
//! - [`shell`]: the window's one shell, composed from its views' shells.
//! - [`rows`]: the rows of virtual lists, as the app renders them for what the list shows.
//!
//! Every batch the window receives is checked against the toolkit's catalogue and the window's
//! budget before it is applied (`tilo_render_protocol::patch::validate`); one that fails is
//! refused and the window shows the platform's error state.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "layout weights, counts and sizes are small values; see crates/platform/tilolang-tilo"
)]

pub mod adapter;
pub mod attach;
pub mod body;
pub mod date;
pub mod form;
pub mod i18n;
pub mod pane;
pub mod reconcile;
pub mod rows;
pub mod shell;
pub mod window;

pub use tilo_render_protocol::patch::{
    Binding, Event, Lane, Node as UiNode, Principal, ResourceRef, Value,
};
pub use window::AppWindow;

/// Whether a node of `kind` is a virtual list: a `ui.VirtualList`, whose rows the app renders
/// as the list shows them rather than in the tree. A content `list` comes whole.
#[must_use]
pub fn is_virtual(kind: &str) -> bool {
    tilo_render_protocol::patch::is_virtual_list(kind)
}
