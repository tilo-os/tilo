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
    pub use ui_core::automerge::transaction::Transactable;
    pub use ui_core::automerge::{AutoCommit, ObjId};
    pub use ui_core::launch::ComponentHostPreparer;
    pub use ui_core::record::{RecordCheckout, RecordId, RecordKind};
    pub use ui_core::taffy::{NodeId, TaffyTree};
    pub use ui_core::tilo_actions;
    pub use ui_core::tilo_actions::{ActionError, ActionRegistry, Doc, ExecutionLocus, Resource};
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
        ControlBindingMetadata, Dropdown, EmptyState, EventCtx, FloatingSurfaceProvider,
        FullscreenProvider, ModelView, NarrowScreen, NativeViewLifecycle, NumberInput, QrWidget,
        ResourceOperationStart, Row, SearchField, SearchResultsView, Slider, Spacer, Spinner,
        Stack, Text, TextInput, Toggle, TreeNode, TreeView, Widget, labeled_check, labeled_field,
    };
    pub use ui_core::winit::event::WindowEvent;
    pub use ui_core::{
        App, LatestRequest, LatestTask, LocalState, PresenceCtx, RevisionHistory, SearchRegistry,
        State, TextRenderer, Theme, UndoAccess, UndoRequest, UndoStatus, collaboration, column,
        history_source, presence, row, search, stack,
    };
}

#[cfg(test)]
mod tests {
    use super::prelude::*;
    use std::collections::HashMap;

    fn custom_event_handler(_event: &ui_core::winit::event::WindowEvent, ctx: &mut EventCtx<'_>) {
        // App-defined widgets receive the same event and CRDT context as
        // built-ins; implementing Widget requires no adapter framework.
        let _ = (&mut ctx.state, ctx.path, ctx.node_id);
    }

    struct CustomWidget {
        inner: Text,
    }

    impl Widget for CustomWidget {
        fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
            self.inner.sync(state, path);
        }

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.inner.build_layout(taffy, theme, node_map)
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.inner.apply_layout(taffy, node, origin);
        }

        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text_renderer: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.inner
                .draw(scene, transform, text_renderer, theme, presence);
        }

        fn handle_event_ctx(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            self.inner.handle_event_ctx(event, ctx);
        }

        fn name(&self) -> &'static str {
            "CustomWidget"
        }

        fn get_rect(&self) -> Rect {
            self.inner.get_rect()
        }
    }

    #[test]
    fn prelude_exposes_the_integrated_custom_widget_surface() {
        std::hint::black_box(custom_event_handler);
        std::hint::black_box(CustomWidget {
            inner: Text::new("custom-widget"),
        });
        std::hint::black_box(SearchRegistry::new());
        std::hint::black_box(ActionForm::new("note", "Note", "save", "Save"));
    }
}
