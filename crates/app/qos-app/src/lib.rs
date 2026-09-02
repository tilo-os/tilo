//! The complete qOS application SDK.
//!
//! This crate is deliberately a superset, not a restrictive wrapper around
//! the rendering toolkit. Applications retain every layout, widget, canvas,
//! native-view and custom-painting capability exported by `ui-core`; this is
//! simply the stable application boundary where qOS can require the resource,
//! action, collaboration, history and accessibility contracts that turn a
//! graphical process into an OS-native application.
//!
//! Application manifests normally rename this package to `ui`, so existing
//! and generated source continues to use the concise `ui::...` vocabulary:
//!
//! ```toml
//! ui = { package = "qos-app", path = "../../crates/app/qos-app" }
//! ```
//!
//! Shell/framework internals may depend on the lower `ui` package directly.
//! Ordinary applications should not: keeping this facade as capable as the
//! core means choosing the native route costs no visual or compositional
//! freedom, while giving tooling one machine-checkable dependency boundary.

pub use ui_core::*;

/// The usual imports for application code that prefers a prelude.
pub mod prelude {
    pub use ui_core::qos_actions;
    pub use ui_core::widget::Widget;
    pub use ui_core::{LocalState, State, Theme};
}
