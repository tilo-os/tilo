//! Hex editor as a [`tilo_resource::Resource`]: bytes stay device-local;
//! open/save/goto/search are Effects and Views in [`local_operations`].

use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    HEX_EDITOR_ACTION_TYPE, HexEditorAction, HexResourceCtx, provide_resources, register,
};
use crate::view::HexView;

/// Empty document projection — the buffer never enters Automerge.
#[derive(Clone, Debug, Default)]
pub struct HexState {
    pub label: String,
}

/// The hex document resource (device-local bytes; collaborative kind for the door).
pub struct HexEditor;

impl Resource for HexEditor {
    const KIND: &'static str = "hex_editor.document";
    const LABEL: &'static str = "Hex document";
    const ACTION_TYPE: &'static str = HEX_EDITOR_ACTION_TYPE;

    type State = HexState;
    type Action = HexEditorAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(_doc: &Doc) -> Result<HexState, ModelError> {
        Ok(HexState {
            label: "Untitled".into(),
        })
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Open/save/search/goto — all local. Legacy HexEditorAction::apply
        // refuses collaborative replay; palette clients use HexOperation.
        let ctx = HexResourceCtx {
            label: "Untitled".into(),
            cursor: 0,
            len: 0,
            selection: None,
            has_file: false,
            can_undo: false,
            can_redo: false,
        };
        provide_resources(&ctx)
            .into_iter()
            .flat_map(|resource| resource.actions)
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Framework app: the hex view over [`HexEditor`].
pub struct HexEditorShell;

impl ResourceApp for HexEditorShell {
    type Resource = HexEditor;
    const ID: &'static str = "so.tilo.hex-editor";
    const TITLE: &'static str = "Hex Editor";
    const ICON: Option<&'static str> = Some("hex-editor");
    const SHOT_VIEWPORT: (u32, u32) = (1100, 720);

    fn view(cx: &mut ViewCx<HexEditor>) -> Element {
        cx.surface(HexView::empty())
    }
}

impl ResourceSurface<HexEditor> for HexView {
    fn present(&mut self, _state: &std::rc::Rc<HexState>) {}

    fn attach(&mut self, _cx: SurfaceCx<HexEditor>) {}
}

/// Live launch through the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<HexEditorShell>()
}
