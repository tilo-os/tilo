//! The Tilo application SDK. It re-exports the rendering toolkit and provides
//! the stable boundary for resource, action, collaboration, history and
//! accessibility contracts.
//!
//! Application manifests normally rename this package to `ui`:
//!
//! ```toml
//! ui = { package = "tilo-app", path = "../../crates/sdk/tilo-app" }
//! ```
//!
//! Shell and framework internals may depend on the lower `tilo-ui` package directly;
//! ordinary applications should use this facade.

pub use ui_core::*;

/// The usual imports for application code that prefers a prelude.
pub mod prelude {
    pub use ui_core::application::{
        AppComponent, ApplicationCatalog, ApplicationDefinition, ComponentRunnerInputs,
        HostContext, LaunchTarget, PreparedComponentInputs, RestoreInput,
    };
    pub use ui_core::application_view::{InstanceViewStore, SavedInstanceView};
    pub use ui_core::launch::ComponentHostPreparer;
    pub use ui_core::tilo_actions;
    pub use ui_core::widget::Widget;
    pub use ui_core::{LocalState, State, Theme};
}
