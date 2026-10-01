//! Hex editor as a [`tilo_resource::Resource`]: bytes stay device-local;
//! open/save/goto/search are Effects and Views in [`local_operations`].
//! Hosted [`PublishesResources`](ui::resources::PublishesResources) must not
//! republish a second `hex_editor.document` beside ResourceRoot — only live
//! cursor / selection byte extras.

use std::rc::Rc;

use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    HEX_EDITOR_ACTION_TYPE, HexEditorAction, document_local_operations, register,
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
        // Open/save/search/goto/undo/redo — local Views and Effects. Legacy
        // HexEditorAction::apply refuses collaborative replay; palette clients
        // use HexOperation. Cursor-targeted byte edits ride live extras.
        document_local_operations()
            .into_iter()
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
    /// Bytes are device-local and never projected from Automerge. An empty
    /// `HexState` must not wipe an unpublished local buffer.
    fn present(&mut self, _state: &Rc<HexState>) {}

    fn attach(&mut self, _cx: SurfaceCx<HexEditor>) {
        self.mark_framework_hosted();
    }
}

/// Live launch through the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<HexEditorShell>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::live_buffer_extras;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    fn publish_owns_document_door_and_hosted_keeps_only_live_extras() {
        let published = ui::tilo_resource::publish::<HexEditor>(
            "hex_editor.document",
            "Hex document",
            &HexState {
                label: "Untitled".into(),
            },
        );
        assert_eq!(published.kind, HexEditor::KIND);
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            "open",
            "save",
            "undo",
            "redo",
            "search",
            "goto-offset",
            ui::tilo_resource::APPLY,
            ui::tilo_resource::GROUP,
        ] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride publish via local_operations: {ids:?}"
            );
        }
        for id in [
            "insert-byte",
            "overwrite-byte",
            "delete-byte",
            "delete-selection",
        ] {
            assert!(
                !ids.contains(&id),
                "cursor byte edits stay on live extras, not publish: {ids:?}"
            );
        }
        let search = published
            .actions
            .iter()
            .find(|action| action.id == "search")
            .expect("search");
        assert_eq!(search.kind, OperationKind::View);
        assert!(
            search.contract.availability.is_available(),
            "publish door must not mark search unavailable from an empty projection"
        );

        let mut view = HexView::new(vec![0x00, 0x11, 0x22, 0x33], "buf.bin");
        view.model_mut().move_to(1, false);
        let mut state = ui::State::new();
        Widget::sync(&mut view, &mut state.doc, &state.root);
        assert!(
            !view.is_framework_hosted(),
            "standalone publishes the document door"
        );

        let standalone = view.resources(&state.doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, HexEditor::KIND);
        let cursor = standalone[0]
            .children
            .iter()
            .find(|child| child.kind == "hex_editor.cursor")
            .expect("cursor edits stay a live child");
        assert!(
            cursor
                .actions
                .iter()
                .any(|action| action.id == "insert-byte"),
            "standalone keeps cursor edits beside the document door"
        );

        view.mark_hosted_for_test();
        assert!(view.is_framework_hosted());
        assert!(ui::resources::resource(&view, &state.doc, "hex_editor.document").is_none());
        assert!(
            ui::resources::resource(&view, &state.doc, "hex_editor.cursor").is_some_and(|cursor| {
                cursor
                    .actions
                    .iter()
                    .any(|action| action.id == "insert-byte")
            }),
            "a hosted cursor stays the published row"
        );
        let extras = view.resources(&state.doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != HexEditor::KIND
                    && resource.id.as_str() != "hex_editor.document"),
            "hosted must not republish hex_editor.document beside ResourceRoot: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            extras.len(),
            1,
            "hosted returns the cursor / selection slice alone"
        );
        assert_eq!(extras[0].kind, "hex_editor.cursor");
        for id in ["insert-byte", "overwrite-byte", "delete-byte"] {
            assert!(
                extras[0].actions.iter().any(|action| action.id == id),
                "hosted keeps live byte edit `{id}`: {:?}",
                extras[0]
                    .actions
                    .iter()
                    .map(|a| a.id.as_str())
                    .collect::<Vec<_>>()
            );
        }
        assert!(
            !extras[0].actions.iter().any(|action| matches!(
                action.id.as_str(),
                "open" | "save" | "search" | "goto-offset" | "undo" | "redo"
            )),
            "hosted cursor must not copy document door ops already on publish"
        );

        view.model_mut().move_to(2, false);
        view.model_mut().move_to(3, true);
        let with_sel = view.resources(&state.doc);
        assert_eq!(with_sel.len(), 1);
        assert!(
            with_sel[0]
                .actions
                .iter()
                .any(|action| action.id == "delete-selection"),
            "hosted selection binds delete-selection"
        );
        let from_helper = live_buffer_extras(&view.hex_resource_ctx());
        assert_eq!(from_helper.len(), 1);
        assert_eq!(from_helper[0].kind, "hex_editor.cursor");
    }

    #[test]
    fn present_empty_does_not_wipe_unpublished_local_bytes() {
        let mut view = HexView::new(vec![0xCA, 0xFE, 0xBA, 0xBE], "local.bin");
        assert_eq!(view.model().bytes(), &[0xCA, 0xFE, 0xBA, 0xBE]);
        assert!(view.has_file());

        ResourceSurface::<HexEditor>::present(&mut view, &Rc::new(HexState::default()));
        assert_eq!(
            view.model().bytes(),
            &[0xCA, 0xFE, 0xBA, 0xBE],
            "empty present must not wipe unpublished local bytes"
        );
        assert!(
            view.has_file(),
            "empty present must keep the local file projection"
        );
    }
}
