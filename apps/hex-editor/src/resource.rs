//! Hex editor as a [`tilo_resource::Resource`]: bytes stay device-local;
//! open/save/goto/search are Effects and Views in [`local_operations`].
//! Hosted [`PublishesResources`](ui::resources::PublishesResources) must not
//! republish a second `hex_editor.document` beside ResourceRoot — only live
//! cursor / selection byte extras.

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    HEX_EDITOR_ACTION_TYPE, HexEditorAction, document_local_operations,
    refuses_collaborative_replay, register,
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

    fn attach(&mut self, cx: SurfaceCx<HexEditor>) {
        self.actions.attach(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
///
/// Raw hex bytes never take the dispatcher: [`HexEditorAction::apply`] refuses
/// collaborative replay. A document action that `apply` accepts uses
/// [`Dispatcher::apply_now_with`] while hosted, and the local host otherwise.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<HexEditor>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    hosted: bool,
    /// How many times a hosted replayable action reached `apply_now_with`.
    /// Raw byte edits must leave this at zero.
    #[cfg(test)]
    framework_applies: u32,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            #[cfg(test)]
            framework_applies: 0,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<HexEditor>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
        #[cfg(test)]
        remember_attached(self);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted || self.framework.is_some()
    }

    /// Submit one document action. Hosted replayable edits go through the
    /// framework dispatcher. Raw hex bytes return before that call.
    pub(crate) fn dispatch_document(
        &mut self,
        doc: &mut AutoCommit,
        action: &HexEditorAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        if refuses_collaborative_replay(action) {
            return;
        }
        match &self.framework {
            Some(dispatcher) => {
                #[cfg(test)]
                {
                    self.framework_applies = self.framework_applies.saturating_add(1);
                }
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, HEX_EDITOR_ACTION_TYPE, action, context),
        }
    }

    /// Test-only: mark hosted without constructing a framework dispatcher.
    /// Standalone resource tests stay on the local host.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

#[cfg(test)]
struct AttachedDispatcher {
    hosted: bool,
    framework: Option<Dispatcher<HexEditor>>,
}

#[cfg(test)]
thread_local! {
    static ATTACHED: std::cell::RefCell<Option<AttachedDispatcher>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn remember_attached(sink: &ActionSink) {
    let shot = AttachedDispatcher {
        hosted: sink.is_hosted(),
        framework: sink.framework.clone(),
    };
    ATTACHED.with(|slot| *slot.borrow_mut() = Some(shot));
}

#[cfg(test)]
fn take_attached_dispatcher() -> Option<AttachedDispatcher> {
    ATTACHED.with(|slot| slot.borrow_mut().take())
}

/// Live launch through the framework host.
#[cfg(not(target_arch = "wasm32"))]
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

    #[test]
    fn hosted_attach_retains_dispatcher_without_raw_byte_replay() {
        let mut view = HexView::new(vec![0x00, 0x11, 0x22], "buf.bin");
        view.model_mut().move_to(1, false);
        let mut state = ui::State::new();
        Widget::sync(&mut view, &mut state.doc, &state.root);
        assert!(
            !view.is_framework_hosted() && view.actions.framework.is_none(),
            "standalone tests stay on the local host"
        );

        view.mark_hosted_for_test();
        let extras = view.resources(&state.doc);
        assert!(
            extras.iter().all(|resource| {
                resource.kind != HexEditor::KIND && resource.id.as_str() != "hex_editor.document"
            }),
            "hosted resources() must not include a second document root: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "hex_editor.cursor"),
            "selection / cursor extras stay on the widget"
        );

        // The framework constructs the only Dispatcher a surface can hold.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _host = ui::framework::testing::HeadlessApp::<HexEditorShell>::new()
                .expect("hex editor framework host opens");
            let attached = take_attached_dispatcher().expect("attach retains the dispatcher");
            assert!(attached.hosted, "is_hosted");
            let dispatcher = attached.framework.expect("framework.is_some()");

            let mut sink = ActionSink::default();
            sink.attach(dispatcher);
            assert!(sink.is_hosted());
            assert!(sink.framework.is_some());

            let mut doc = automerge::AutoCommit::new();
            let context = ui::tilo_actions::ActionContext::default();
            for action in [
                crate::actions::insert_bytes_action(0, vec![0xAB]),
                crate::actions::overwrite_bytes_action(0, vec![0xCD]),
                crate::actions::delete_bytes_action(0, 1),
            ] {
                sink.dispatch_document(&mut doc, &action, context.clone());
            }
            assert_eq!(
                sink.framework_applies, 0,
                "raw hex bytes must not be applied through the framework dispatcher"
            );
            assert!(
                doc.get_heads().is_empty(),
                "raw bytes must not be written into the shared document"
            );
        }
    }
}
