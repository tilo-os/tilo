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
    pub use ui_core::record::{RecordCheckout, RecordId, RecordKind};
    pub use ui_core::tilo_actions;
    pub use ui_core::widget::resource_actions;
    pub use ui_core::widget::resource_actions::{ActionForm, ActionValue};
    pub use ui_core::widget::{
        ActionEmitter, AudiencePresentationProvider, Button, Checkbox, Collaborative,
        CompleteContentProvider, Continuable, ControlBindingKind, ControlBindingMetadata, Dropdown,
        FloatingSurfaceProvider, FullscreenProvider, NativeViewLifecycle, NumberInput,
        ResourceOperationStart, Slider, Text, TextInput, Widget, labeled_check, labeled_field,
    };
    pub use ui_core::{
        App, LocalState, RevisionHistory, SearchRegistry, State, Theme, collaboration,
        history_source, presence, search,
    };
}

/// Portable folder copy and readiness operations; native authority stays private.
///
/// ```
/// use tilo_app::folders::{copy::MigrationJob, review::DirectoryIdentity, scan::FolderScan};
/// ```
/// ```compile_fail
/// use tilo_app::folders::connection::endpoint::EndpointContext;
/// ```
/// ```compile_fail
/// use tilo_app::folders::copy::MigrationStore;
/// fn steal(store: MigrationStore) { let _ = store.root.file; }
/// ```
#[cfg(feature = "folder-copy")]
pub mod folders {
    pub use tilo_folder_service::{connected, copy, review, scan, session, startup};
}

#[cfg(feature = "install-artifacts")]
pub mod install_artifacts;
