//! The Tilo application SDK.
//!
//! The golden path is [`framework`]: declare a resource type and its actions
//! with [`tilo_resource`], write views against a [`framework::ViewCx`], and
//! [`framework::run`] provides storage, sync, collaboration, history, undo,
//! permissions, the action palette, the agent and script API, and search.
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

/// The document editor and the rich text widgets it is made of: a block editor, a rich text
/// line, a rich input, the slash menu and tables.
pub use tilo_rich_edit as rich_edit;

/// The host's raw operation protocol stays the toolkit's: this empty module
/// takes the place of `ui_core::host_operation` in the glob above, so an app
/// cannot name the protocol through the facade. Tests deliver operations with
/// [`framework::HostDelivery`]; the framework reaches `ui_core` directly.
#[doc(hidden)]
pub mod host_operation {}

/// The app framework: a workspace's records, their views, and the chains
/// kept beside them, read and written by every app that opens them.
pub mod framework;
pub mod linked_accounts;
mod runner;

/// The toolkit's launch vocabulary, with `run` resolved to the runner this
/// build targets: the native event loop on Linux and macOS. A wasm build has
/// no runner: what hosts it mounts the app's definition itself.
pub mod launch {
    #[cfg(not(target_family = "wasm"))]
    pub use crate::runner::{run, run_or_exit};
    pub use ui_core::launch::*;
}

/// The binary entry point of an app, identical on every target.
///
/// ```ignore
/// ui::main!(hello::App);                           // a `ResourceApp`
/// ui::main!(hello::App, init = hello::initialize);  // optional process setup
/// ui::main!(definition = hello::definition);       // fn() -> &'static ApplicationDefinition
/// // the release binary runs the app from its engine library (`engine_main!`)
/// ui::main!(hello::App, init = hello::initialize, engine = "tilo_hello_engine");
/// ```
///
/// On Linux and macOS it runs the native runner with the standard launch
/// modes (`--shots`, `--headless`, …). A wasm build's binary is empty: what
/// hosts it mounts the app's definition itself.
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
        $crate::main!($app, init = || {});
    };
    ($app:ty, init = $init:expr) => {
        fn main() {
            ($init)();
            #[cfg(not(target_arch = "wasm32"))]
            $crate::framework::run::<$app>();
        }
    };
    ($app:ty, init = $init:expr, engine = $engine:literal) => {
        $crate::main!(
            $app,
            init = $init,
            engine = $engine,
            entry = tilo_app_engine_main
        );
    };
    ($app:ty, init = $init:expr, engine = $engine:literal, entry = $entry:ident) => {
        // The app's own build (`tilolang dev`, tests, the browser) runs it
        // here, as any app runs.
        #[cfg(not(feature = "runtime-engine"))]
        $crate::main!($app, init = $init);

        // The release build's binary is one call into the app's engine
        // library ([`engine_main!`]), which carries the app's library: the
        // engine depends on the app's package, so the binary names the
        // library itself rather than through Cargo.
        #[cfg(feature = "runtime-engine")]
        #[link(name = $engine, kind = "dylib")]
        unsafe extern "C-unwind" {
            safe fn $entry();
        }

        #[cfg(feature = "runtime-engine")]
        fn main() {
            $entry();
        }
    };
}

/// The entry an app's engine library exports for the app's binary: run the
/// app, after `init`, as [`main!`] would. Invoke it once at the root of the
/// engine library (a runtime engine that carries the app's library), and
/// name the library in the app's `ui::main!(App, init = .., engine = "..")`.
/// The app's package then holds only that call, and its code ships in the
/// runtime bundle beside the runtime it is built against.
///
/// The app enables the engine form with a `runtime-engine` feature; without
/// it the binary runs the app itself.
///
/// An engine that carries several apps names each one's entry
/// (`entry = tilo_hello_main`), and each app names the same entry in its
/// `ui::main!`.
#[macro_export]
macro_rules! engine_main {
    ($app:ty, init = $init:expr) => {
        $crate::engine_main!($app, init = $init, entry = tilo_app_engine_main);
    };
    ($app:ty, init = $init:expr, entry = $entry:ident) => {
        /// Run the app: the whole of its binary, which names this library and
        /// calls this function by its unmangled name. A panic unwinds
        /// through it as through any Rust call.
        #[unsafe(no_mangle)]
        pub extern "C-unwind" fn $entry() {
            ($init)();
            $crate::framework::run::<$app>();
        }
    };
}

/// Export the entry points every platform's host enters the app library by.
///
/// Invoke it once at the root of the app library, naming the [`framework::ResourceApp`]
/// type. The reload symbols `tilolang dev` loads into the process that already
/// owns the windows compile only for a `thin` build, the build that links one
/// shared `ui` so the new view is the same type as the windows already on
/// screen. On every target it also defines the library's `definition()`, the
/// entry a web build registers with the browser's toolkit core, as component
/// apps define theirs. On Android it defines `android_main`, which the
/// `NativeActivity` of `tilolang build --platform android` enters: the library
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

        /// The app's toolkit definition, which a host running the app in its
        /// own process (the web build, the shell's built-in apps) mounts. It
        /// carries the app's committed surface: such a host runs no package
        /// whose payload could name one. The app's own binary never calls it.
        #[must_use]
        pub fn definition() -> &'static $crate::prelude::ApplicationDefinition {
            $crate::built_in_definition!($app)
        }
    };
}

/// The definition of the [`framework::ResourceApp`] `$app` for a process that
/// starts the app itself (the shell's built-in apps, the web desktop, an
/// agent's member process), naming the app crate's compiled surface
/// (`surface.json`) through `tilo_resource::committed_surface!`: such a process
/// loads no package whose payload names one. An app's library defines its `definition()` with it; the app's
/// own binary uses [`framework::definition`], so it does not carry the file.
#[macro_export]
macro_rules! built_in_definition {
    ($app:ty) => {{
        $crate::register_built_in_surface(
            <$app as $crate::framework::ResourceApp>::ID,
            $crate::tilo_resource::committed_surface!(),
        );
        $crate::framework::definition::<$app>()
    }};
}

/// Name the committed surface of an app a process starts itself, unless a
/// package already named one. Called by [`built_in_definition!`] with the
/// compiled surface the app ships.
#[doc(hidden)]
pub fn register_built_in_surface(app_id: &str, surface: Option<&'static str>) {
    if let Some(json) = surface {
        ui_core::app_surface::register_built_in(app_id, json);
    }
}

/// The resource layer, re-exported under its own name so `#[resource]` and
/// `#[derive(Model)]` expansions resolve through `use ui::prelude::*`.
pub use tilo_resource;

/// The workspace's own emoji and its agents: records every window of the
/// workspace opens, re-exported for the app that keeps them and for hosts
/// that show them.
pub use tilo_workspace_agent;
pub use tilo_workspace_emoji;

/// References to what apps keep ([`tilo_reference::ResourceRef`]), which
/// [`framework::ViewCx::reference`] shows and opens, re-exported so an app
/// names them without a dependency of its own.
pub use tilo_reference;

/// The package registry's vocabulary (entries, artifacts, moderation cases)
/// that the SDK's moderation and package APIs speak, re-exported so an app
/// names those types without a dependency of its own.
pub use registry_types as registry;
/// JSON values for action arguments, component props and native-input
/// snapshots, re-exported so an app needs no dependency of its own.
pub use serde_json;
mod copy_catalogue;
/// A view written in the app language, shown where an app places it.
pub mod written_view;
/// Files on every target: capability-scoped directories on Linux and macOS,
/// the origin-private file system in the browser. Use it where `std::fs`
/// would fail on the web.
pub use tilo_files as files;

/// What a hand-written [`Widget`] draws and lays
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
    pub use crate::framework::testing::HeadlessApp;
    pub use crate::framework::{
        ActionControl, Actions, Bindable, Command, Dispatcher, Each, EditsText, EffectCx,
        EffectStatus, Effected, Fixture, Form, Jobs, LaunchInput, Live, LiveRow, PersonKind,
        PersonResource, Query, ReadFailure, ResourceApp, ResourceSelection, ResourceSurface,
        Resources, RunMode, SeedCx, Setting, Submit, SurfaceCx, TextControl, ViewCx,
        WorkspaceResource,
    };
    pub use tilo_resource::{
        self, ActionCx, ActionDecl, ActionKind, Atom, Collection, Counter, Delta, Demand, Edit,
        Effect, Id, Invoke, MemberId, Model, Outbox, Params, Path, PlainText, Refusal, Replica,
        Resource, SearchEntry, Set, Settled, TextEdit, Undoing, resource,
    };
    pub use ui_core::i18n::LabelKey;

    pub use settings_model::{DeviceClass, Scope, SettingsSchema};
    pub use ui_core::application::{
        ANY_MEDIA_TYPE, AppComponent, ApplicationCatalog, ApplicationDefinition,
        ComponentBehaviour, ComponentRunnerInputs, LaunchTarget, OpenFile, PlaybackHandoff,
        PreparedComponentInputs, ProcessContext, ProcessContextInputs, RestoreInput,
    };
    pub use ui_core::application_view::{InstanceViewStore, SavedInstanceView};
    pub use ui_core::arriving::{Arrival, Arriving};
    pub use ui_core::launch::ComponentHostPreparer;
    pub use ui_core::resource::{
        DOCUMENT_ID_XATTR, DocumentCheckout, DocumentId, ResourceSaveStatus, ResourceType,
        WorkspaceAnswer,
    };
    pub use ui_core::tilo_actions;
    pub use ui_core::tilo_actions::{ActionError, ActionNeed, ActionRegistry, ExecutionLocus};
    pub use ui_core::tokens::TextRole;
    pub use ui_core::vello::kurbo::{Point, Rect, Size};
    pub use ui_core::widget::data_table::{
        ColumnDef, ColumnWidth, DataTable, TableModel, TableModelChange,
    };
    pub use ui_core::widget::resource_actions;
    pub use ui_core::widget::resource_actions::{ActionForm, ActionValue};
    pub use ui_core::widget::virtual_list::{ListModel, ListModelChange, VirtualList};
    pub use ui_core::widget::{
        ActionEmitter, AudiencePresentationProvider, Banner, Button, Card, Checkbox, Collaborative,
        Column, CompleteContentProvider, Container, ContentKind, Continuable, ControlBindingKind,
        ControlBindingMetadata, Dropdown, Element, EmptyState, EventCtx, FilterCount, Filterable,
        Filtered, FindBar, FindMatch, FindOptions, FindScope, Findable, FloatingSurfaceProvider,
        FullscreenProvider, ModelView, NativeViewLifecycle, NavItem, NavList, NavMode, NavPane,
        NavSection, NavShell, NumberInput, QrWidget, Row, Scrollable, SearchField,
        SearchResultsView, Slider, Spacer, Spinner, Stack, Text, TextInput, Toggle, Widget,
        labeled_check, labeled_field,
    };
    pub use ui_core::{
        LatestRequest, LatestTask, LocalState, PresenceCtx, SearchRegistry, State, TextRenderer,
        Theme, UndoAccess, UndoRequest, UndoStatus, collaboration, column, history_source,
        presence, row, search, stack,
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
