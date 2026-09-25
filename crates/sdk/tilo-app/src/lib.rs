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
//! Shell and framework internals may depend on the lower `tilo-ui` package directly;
//! ordinary applications should use this facade.

pub use ui_core::*;

pub mod framework;

/// The resource layer, re-exported under its own name so `#[resource]` and
/// `#[derive(Model)]` expansions resolve through `use ui::prelude::*`.
pub use tilo_resource;

/// The usual imports for application code.
pub mod prelude {
    pub use crate::framework::testing::{HeadlessApp, TestApp};
    pub use crate::framework::{
        ActionControl, Actions, Dispatcher, Each, Live, ResourceApp, ResourceSelection,
        ResourceSurface, SurfaceCx, ViewCx, run,
    };
    pub use tilo_resource::{
        self, ActionCx, Atom, Collection, Id, Invoke, Json, Model, Refusal, Replica, Resource,
        resource,
    };

    pub use ui_core::application::{
        AppComponent, ApplicationCatalog, ApplicationDefinition, ComponentRunnerInputs,
        HostContext, LaunchTarget, PreparedComponentInputs, RestoreInput,
    };
    pub use ui_core::application_view::{InstanceViewStore, SavedInstanceView};
    pub use ui_core::launch::ComponentHostPreparer;
    pub use ui_core::record::{RecordCheckout, RecordId, RecordKind};
    pub use ui_core::taffy::{NodeId, TaffyTree};
    pub use ui_core::tilo_actions;
    pub use ui_core::tilo_actions::{ActionError, ActionRegistry, ExecutionLocus};
    pub use ui_core::tokens::TextRole;
    pub use ui_core::vello::Scene;
    pub use ui_core::vello::kurbo::{Affine, Point, Rect, Size};
    pub use ui_core::widget::data_table::{
        ColumnDef, ColumnWidth, DataTable, TableModel, TableModelChange, TableRowState,
    };
    pub use ui_core::widget::resource_actions;
    pub use ui_core::widget::resource_actions::{ActionForm, ActionValue};
    pub use ui_core::widget::virtual_list::{
        ListModel, ListModelChange, ListRowState, VirtualList,
    };
    pub use ui_core::widget::{
        ActionEmitter, AppShell, AudiencePresentationProvider, Banner, Button, Card, Checkbox,
        Collaborative, Column, CompleteContentProvider, Container, Continuable, ControlBindingKind,
        ControlBindingMetadata, Dropdown, Element, EmptyState, EventCtx, FloatingSurfaceProvider,
        FullscreenProvider, ModelView, NarrowScreen, NativeViewLifecycle, NumberInput, QrWidget,
        ResourceOperationStart, Row, Scrollable, SearchField, SearchResultsView, Slider, Spacer,
        Spinner, Stack, Text, TextInput, Toggle, TreeNode, TreeView, Widget, labeled_check,
        labeled_field,
    };
    pub use ui_core::winit::event::WindowEvent;
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
    use ui_core::automerge::{AutoCommit, ObjId};

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
            _doc: &AutoCommit,
            _path: &ObjId,
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
