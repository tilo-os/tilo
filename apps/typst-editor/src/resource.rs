//! Typst Editor as a [`tilo_resource::Resource`]: source text stays on the
//! CRDT-native text path; compile / open / save / zoom are View and Effect
//! operations published from the resource.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions;
use crate::app::TypstApp;

/// No replayable document action beyond the shared text object — see
/// [`crate::actions`]. The empty enum still satisfies the resource contract
/// so APPLY/GROUP exist for a future structural vocabulary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypstDocAction {}

impl Action for TypstDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

/// Workspace resource. Kind matches the published tree root.
pub struct TypstEditor;

impl Resource for TypstEditor {
    const KIND: &'static str = "typst.workspace";
    const LABEL: &'static str = "Typst workspace";
    const ACTION_TYPE: &'static str = "typst.action";

    type State = ();
    type Action = TypstDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<TypstDocAction>(Self::ACTION_TYPE);
    }

    fn read(_doc: &Doc) -> Result<(), ModelError> {
        Ok(())
    }

    fn children(_state: &()) -> Vec<tilo_actions::Resource> {
        actions::resources(&automerge::AutoCommit::new())
            .into_iter()
            .next()
            .map(|root| root.children)
            .unwrap_or_default()
    }

    fn local_operations() -> Vec<ActionSpec> {
        actions::resources(&automerge::AutoCommit::new())
            .into_iter()
            .flat_map(|resource| {
                let nested = resource.children.into_iter().flat_map(|child| {
                    child
                        .actions
                        .into_iter()
                        .chain(child.children.into_iter().flat_map(|g| g.actions))
                });
                resource.actions.into_iter().chain(nested)
            })
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Framework app: the typst widget is the surface over [`TypstEditor`].
pub struct TypstEditorShell;

impl ResourceApp for TypstEditorShell {
    type Resource = TypstEditor;
    const ID: &'static str = "so.tilo.typst-editor";
    const TITLE: &'static str = "Typst";
    const ICON: Option<&'static str> = Some("typst-editor");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<TypstEditor>) -> Element {
        // Live path starts empty (no demo seed).
        cx.surface(TypstApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "preview",
            "menu",
            "problems",
            "scrolled",
            "errors",
            "editor-select",
            "preview-zoom",
            "preview-pan",
            "select",
            "stale",
            "status",
        ]
    }
}

impl ResourceSurface<TypstEditor> for TypstApp {
    fn present(&mut self, _state: &Rc<()>) {}

    fn attach(&mut self, _cx: SurfaceCx<TypstEditor>) {
        self.set_framework_hosted(true);
    }
}

/// Live launch on the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<TypstEditorShell>().configure_app(|app| {
        app.set_window_title(Some("Typst".into()));
    })
}
