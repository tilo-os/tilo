//! The Tilo application SDK.
//!
//! The golden path is [`framework`]: declare a resource type and its actions
//! with [`tilo_resource`], write views against a [`framework::ViewCx`], and
//! [`framework::run`] provides storage, sync, collaboration, history, undo,
//! permissions, the action palette, the agent and script door, and search.
//! The CRDT engine is not part of that surface.
//!
//! The rendering toolkit is re-exported unchanged for widgets, custom
//! surfaces and the imperative escape hatches.
//!
//! Application manifests normally rename this package to `ui`:
//!
//! ```toml
//! ui = { package = "tilo-app", path = "../../crates/sdk/tilo-app" }
//! ```
//!
//! That one line is the whole manifest an app needs. Every OS affordance is on
//! by default — history, undo, permissions, collaboration and device sync,
//! the host file chooser, media controls, notifications and GPU video — so an
//! app never picks features to get them. A headless tool that wants only the
//! toolkit sets `default-features = false`.
//!
//! Shell and framework internals may depend on the lower `tilo-ui` package directly;
//! ordinary applications should use this facade.

pub use ui_core::*;

/// The host's raw operation protocol stays the toolkit's: this empty module
/// takes the place of `ui_core::host_operation` in the glob above, so an app
/// cannot name the protocol through the facade. Tests deliver operations with
/// [`framework::HostDelivery`]; the framework reaches `ui_core` directly.
#[doc(hidden)]
pub mod host_operation {}

pub mod framework;
mod runner;

/// The toolkit's launch vocabulary, with `run` resolved to the runner this
/// build targets: the native event loop on Linux and macOS, registration with
/// the page runtime in the browser.
pub mod launch {
    pub use crate::runner::{run, run_or_exit};
    pub use ui_core::launch::*;
}

/// The binary entry point of an app, identical on every target.
///
/// ```ignore
/// ui::main!(hello::App);                           // a `ResourceApp`
/// ui::main!(definition = hello::definition);       // fn() -> &'static ApplicationDefinition
/// ```
///
/// On Linux and macOS it runs the native runner with the standard launch
/// modes (`--shots`, `--headless`, …). In the browser it registers the app
/// for the page runtime to mount.
#[macro_export]
macro_rules! main {
    (definition = $definition:expr) => {
        fn main() {
            // A browser page mounts the definition itself; its binary is empty.
            #[cfg(not(target_arch = "wasm32"))]
            $crate::launch::run_or_exit(&$crate::framework::definition_launch_spec($definition()));
        }
    };
    ($app:ty) => {
        fn main() {
            #[cfg(not(target_arch = "wasm32"))]
            $crate::framework::run::<$app>();
        }
    };
}

/// Export the entry points every platform's host enters the app library by.
///
/// Invoke it once at the root of the app library, naming the [`framework::ResourceApp`]
/// type. The reload symbols `tilo-app dev` loads into the process that already
/// owns the windows compile only for a `thin` build, the build that links one
/// shared `ui` so the new view is the same type as the windows already on
/// screen. On every target it also defines the library's `definition()`, the
/// entry a web build registers with the browser's toolkit core, as component
/// apps define theirs. On Android it defines `android_main`, which the
/// `NativeActivity` of `tilo-app build --platform android` enters: the library
/// built as a `cdylib` is the whole app there, run as [`framework::run`] runs it.
#[macro_export]
macro_rules! dev_reload_entry {
    ($app:ty) => {
        $crate::android_main!($crate::framework::run::<$app>);

        #[cfg(feature = "thin")]
        extern crate runtime_dylib;

        #[cfg(feature = "thin")]
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn tilo_dev_apply(
            id: *const u8,
            id_len: usize,
            state: *const u8,
            state_len: usize,
            out: *mut u8,
            out_cap: usize,
        ) -> isize {
            let _ = (id, id_len);
            unsafe {
                $crate::framework::dev_reload::copy_window_state(state, state_len, out, out_cap)
            }
        }

        #[cfg(feature = "thin")]
        #[unsafe(no_mangle)]
        pub extern "C" fn tilo_dev_linked_ui() -> *const () {
            $crate::framework::dev_reload::linked_ui()
        }

        #[cfg(feature = "thin")]
        #[unsafe(no_mangle)]
        pub extern "C" fn tilo_dev_publish_root() -> i32 {
            $crate::framework::dev_reload::publish_root::<$app>()
        }

        /// The app's toolkit definition, which the web build registers.
        #[must_use]
        pub fn definition() -> &'static $crate::prelude::ApplicationDefinition {
            $crate::framework::definition::<$app>()
        }
    };
}

/// The resource layer, re-exported under its own name so `#[resource]` and
/// `#[derive(Model)]` expansions resolve through `use ui::prelude::*`.
pub use tilo_resource;

/// JSON values for action arguments, component props and native-input
/// snapshots, re-exported so an app needs no dependency of its own.
pub use serde_json;
/// Reviewed feedback and durable submission, shared by every host.
pub use tilo_feedback as feedback;
pub mod feedback_composer;
/// Files on every target: capability-scoped directories on Linux and macOS,
/// the origin-private file system in the browser. Use it where `std::fs`
/// would fail on the web.
pub use tilo_files as files;

/// What a hand-written [`Widget`](ui_core::widget::Widget) draws and lays
/// out with: the renderer's scene and transforms, the layout tree and the
/// windowing events. Views built from [`framework::ViewCx`] never name them.
pub mod advanced {
    pub use ui_core::taffy::{NodeId, TaffyTree};
    pub use ui_core::vello::Scene;
    pub use ui_core::vello::kurbo::Affine;
    pub use ui_core::winit::event::WindowEvent;
}

/// The usual imports for application code.
pub mod prelude {
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    pub use crate::framework::run;
    #[cfg(not(target_arch = "wasm32"))]
    pub use crate::framework::testing::{HeadlessApp, TestApp};
    pub use crate::framework::{
        ActionControl, Actions, Bindable, Command, Dispatcher, Each, EditsText, EffectCx,
        EffectStatus, Effected, Fixture, Form, Jobs, LaunchInput, Live, LiveRow, PersonRecord,
        ResourceApp, ResourceSelection, ResourceSurface, RunMode, SeedCx, Setting, Submit,
        SurfaceCx, TextControl, ViewCx,
    };
    pub use tilo_resource::{
        self, ActionCx, ActionDecl, Atom, Collection, Counter, Delta, Demand, Edit, Effect, Id,
        Invoke, Json, Model, Outbox, Params, Path, PermissionRequirement, PlainText, Refusal,
        Replica, Resource, SearchEntry, Set, Settled, TextEdit, Undoing, resource,
    };
    pub use ui_core::i18n::LabelKey;

    pub use ui_core::application::{
        ANY_MEDIA_TYPE, AppComponent, ApplicationCatalog, ApplicationDefinition,
        ComponentBehaviour, ComponentRunnerInputs, HostContext, HostContextInputs, LaunchTarget,
        OpenFile, PlaybackHandoff, PreparedComponentInputs, RestoreInput,
    };
    pub use ui_core::application_view::{InstanceViewStore, SavedInstanceView};
    pub use ui_core::launch::ComponentHostPreparer;
    pub use ui_core::record::{RecordCheckout, RecordId, RecordKind, RecordSaveStatus};
    pub use ui_core::storage::{Scope, SettingsSchema};
    pub use ui_core::tilo_actions;
    pub use ui_core::tilo_actions::{ActionError, ActionRegistry, ExecutionLocus};
    pub use ui_core::tokens::TextRole;
    pub use ui_core::vello::kurbo::{Point, Rect, Size};
    pub use ui_core::widget::data_table::{
        ColumnDef, ColumnWidth, DataTable, TableModel, TableModelChange, TableRowState,
    };
    pub use ui_core::widget::resource_actions;
    pub use ui_core::widget::resource_actions::{ActionForm, ActionValue};
    pub use ui_core::widget::virtual_list::{
        ListModel, ListModelChange, ListRowState, VirtualList,
    };
    pub use ui_core::widget::{
        ActionEmitter, AudiencePresentationProvider, Banner, Button, Card, Checkbox, Collaborative,
        Column, CompleteContentProvider, Container, Continuable, ControlBindingKind,
        ControlBindingMetadata, Dropdown, Element, EmptyState, EventCtx, FilterCount, Filterable,
        Filtered, FindBar, FindMatch, FindOptions, FindScope, Findable, FloatingSurfaceProvider,
        FullscreenProvider, ModelView, NativeViewLifecycle, NavItem, NavList, NavMode, NavPane,
        NavSection, NavShell, NumberInput, QrWidget, Row, Scrollable, SearchField,
        SearchResultsView, Slider, Spacer, Spinner, Stack, Text, TextInput, Toggle, TreeNode,
        TreeView, Widget, labeled_check, labeled_field,
    };
    pub use ui_core::{
        LatestRequest, LatestTask, LocalState, PresenceCtx, RevisionHistory, SearchRegistry, State,
        TextRenderer, Theme, UndoAccess, UndoRequest, UndoStatus, collaboration, column,
        history_source, presence, row, search, stack,
    };
}

#[cfg(test)]
mod tests {
    use super::prelude::*;
    // Implementing the toolkit's raw Widget contract is an escape hatch; the
    // document types it names are imported explicitly, not from the prelude.

    fn custom_event_handler(_event: &ui_core::winit::event::WindowEvent, ctx: &mut EventCtx<'_>) {
        // App-defined widgets receive the same event and CRDT context as
        // built-ins; implementing Widget requires no adapter framework.
        let _ = (&mut ctx.state, ctx.path, ctx.node_id);
    }

    /// Extending a widget costs only the hooks it changes: a decorator
    /// forwards every other `Widget` method to the widget it wraps.
    struct CustomWidget {
        inner: Text,
    }

    impl crate::widget::Decorator for CustomWidget {
        type Inner = Text;

        fn inner(&self) -> &Text {
            &self.inner
        }

        fn inner_mut(&mut self) -> &mut Text {
            &mut self.inner
        }

        fn native_input_snapshot(
            &self,
            _doc: &ui_core::widget::AppModel,
            _path: &ui_core::widget::ModelPath,
        ) -> Option<serde_json::Value> {
            Some(serde_json::json!({ "width": self.inner.get_rect().width() }))
        }

        fn name(&self) -> &'static str {
            "CustomWidget"
        }
    }

    #[test]
    fn prelude_exposes_the_integrated_custom_widget_surface() {
        std::hint::black_box(custom_event_handler);
        let custom = CustomWidget {
            inner: Text::new("custom-widget"),
        };
        let widget: &dyn Widget = &custom;
        assert_eq!(widget.name(), "CustomWidget");
        assert_eq!(widget.get_rect(), custom.inner.get_rect());
        std::hint::black_box(SearchRegistry::new());
        std::hint::black_box(ActionForm::new("note", "Note", "save", "Save"));
    }
}
