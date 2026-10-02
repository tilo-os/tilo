//! Typst Editor as a [`tilo_resource::Resource`]: source text stays on the
//! CRDT-native text path; compile / open / save / zoom are View and Effect
//! operations published from the resource. Live diagnostics and preview-page
//! hits stay on the surface — hosted
//! [`PublishesResources`](ui::resources::PublishesResources) returns only that
//! slice beside the framework door.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions;
use crate::app::TypstApp;

/// Document actions for the shared source text. Keystrokes stay on the CRDT
/// text object the editor is bound to; creating that object and replacing the
/// whole source are actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypstDocAction {
    /// Create `source` when it is missing and seed it once.
    EnsureSource { seed: String },
    /// Replace the shared source, diffing the unchanged prefix and suffix.
    ReplaceSource { text: String },
}

impl Action for TypstDocAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        use automerge::transaction::Transactable;
        use automerge::{ObjType, ReadDoc, Value};
        let (id, created) = match doc.get(automerge::ROOT, "source")? {
            Some((Value::Object(ObjType::Text), id)) => (id, false),
            Some(_) => {
                return Err(ActionError::Refused("typst source is not text".into()));
            }
            None => (
                doc.put_object(automerge::ROOT, "source", ObjType::Text)?,
                true,
            ),
        };
        match self {
            Self::EnsureSource { seed } => {
                if created && !seed.is_empty() {
                    doc.splice_text(&id, 0, 0, seed)?;
                }
            }
            Self::ReplaceSource { text } => {
                ui::doc::splice_text_diffed(doc, &id, text);
            }
        }
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
        // Static document / preview / export door only — live diagnostics and
        // preview pages ride [`PublishesResources`] via [`actions::live_extras`].
        actions::workspace_resource().children
    }

    fn local_operations() -> Vec<ActionSpec> {
        let root = actions::workspace_resource();
        let nested = root.children.into_iter().flat_map(|child| {
            child
                .actions
                .into_iter()
                .chain(child.children.into_iter().flat_map(|g| g.actions))
        });
        root.actions
            .into_iter()
            .chain(nested)
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
    fn present(&mut self, _state: &Rc<()>) {
        // Source lives on the CRDT text object and local document list — an
        // empty projection must not wipe unpublished local source.
    }

    fn attach(&mut self, cx: SurfaceCx<TypstEditor>) {
        self.attach_document_actions(cx.dispatcher());
        self.set_framework_hosted(true);
    }
}

/// Live launch on the framework host.
pub fn launch_spec() -> ui::launch::LaunchSpec {
    // Window title / capture protection live in main.rs via configure_app
    // (native App only). The browser mounts the definition without that path.
    ui::framework::launch_spec::<TypstEditorShell>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile::{DiagKind, Diagnostic, PageImage};
    use std::sync::Arc;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    fn blank_page() -> PageImage {
        PageImage {
            rgba: Arc::from([255u8, 255, 255, 255].as_slice()),
            width: 1,
            height: 1,
            width_pt: 100.0,
            height_pt: 100.0,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one hosted extras check covers problems, preview pages, and overlay"
    )]
    #[test]
    fn hosted_resources_keep_live_extras_without_workspace_root() {
        let published =
            ui::tilo_resource::publish::<TypstEditor>("typst.workspace", "Typst workspace", &());
        assert_eq!(published.kind, "typst.workspace");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            "open",
            "save",
            "compile",
            "export-pdf",
            "zoom-in",
            "problems",
            "apply",
            "group",
        ] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride local_operations via publish: {ids:?}"
            );
        }
        let compile = published
            .actions
            .iter()
            .find(|action| action.id == "compile")
            .expect("compile");
        assert_eq!(compile.kind, OperationKind::Effect);
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "typst.document"),
            "document door must ride Resource children: {:?}",
            published
                .children
                .iter()
                .map(|child| child.id.as_str())
                .collect::<Vec<_>>()
        );

        let mut app = TypstApp::with_demo_documents();
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        assert!(
            !app.is_framework_hosted(),
            "standalone stays on the full action tree"
        );

        let pages = [blank_page(), blank_page()];
        app.stage_live_extras_for_test(
            vec![
                Diagnostic {
                    kind: DiagKind::Error,
                    message: "unknown variable".into(),
                    line: Some(4),
                    col: Some(28),
                    end_line: Some(4),
                    end_col: Some(43),
                    hints: Vec::new(),
                },
                Diagnostic {
                    kind: DiagKind::Warning,
                    message: "unknown font family".into(),
                    line: Some(1),
                    col: Some(12),
                    end_line: Some(1),
                    end_col: Some(26),
                    hints: Vec::new(),
                },
            ],
            &pages,
        );
        assert!(
            app.live_diagnostic_count_for_test() > 0,
            "staged diagnostics required for the hosted extras contract"
        );
        assert!(
            app.live_page_count_for_test() > 1,
            "staged pages required for the hosted extras contract"
        );

        let standalone = app.resources(&state.doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "typst.workspace");
        let document = standalone[0]
            .children
            .iter()
            .find(|child| child.kind == "typst.document")
            .expect("standalone nests the document under the workspace");
        let standalone_problems = document
            .children
            .iter()
            .find(|child| child.kind == "typst.problems")
            .expect("standalone nests diagnostics under the document");
        assert!(
            !standalone_problems.children.is_empty(),
            "standalone problems projection carries diagnostic rows"
        );
        assert!(
            standalone_problems.children.iter().all(|hit| {
                hit.kind == "typst.diagnostic"
                    && hit.actions.iter().any(|action| action.id == "open")
            }),
            "each diagnostic is a navigable typst.diagnostic row"
        );
        let standalone_pages = document
            .children
            .iter()
            .find(|child| child.kind == "typst.preview-pages")
            .expect("standalone nests preview pages under the document");
        assert_eq!(
            standalone_pages.children.len(),
            app.live_page_count_for_test(),
            "standalone keeps one row per compiled page"
        );

        let document_id = document.id.as_str().to_owned();
        app.set_framework_hosted(true);
        assert!(app.is_framework_hosted());
        assert!(ui::resources::resource(&app, &state.doc, "typst.workspace").is_none());
        assert!(
            ui::resources::resource(&app, &state.doc, &document_id).is_some(),
            "a hosted document stays the published row"
        );
        let extras = app.resources(&state.doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "typst.workspace"
                    && resource.kind != "typst.document"),
            "hosted must not republish typst.workspace beside ResourceRoot: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(extras.len(), 2, "hosted returns problems + pages slices");
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "typst.problems"),
            "hosted keeps the problems slice"
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "typst.preview-pages"),
            "hosted keeps the preview-pages slice"
        );
        let hosted_problems = extras
            .iter()
            .find(|resource| resource.kind == "typst.problems")
            .expect("hosted returns the problems slice");
        assert_eq!(
            hosted_problems.id, standalone_problems.id,
            "hosted problems tree matches the standalone nested slice"
        );
        assert_eq!(
            hosted_problems.children.len(),
            standalone_problems.children.len(),
            "hosted keeps the same bounded diagnostic rows"
        );

        app.clear_live_extras_for_test();
        assert!(
            app.resources(&state.doc).is_empty(),
            "hosted publishes no live extras when compile reported none"
        );
    }

    #[test]
    fn present_empty_does_not_wipe_unpublished_local_source() {
        let mut app = TypstApp::new();
        app.seed_unpublished_source_for_test("local-only.typ", "= Not published yet\n");
        let empty = Rc::new(());
        ResourceSurface::<TypstEditor>::present(&mut app, &empty);
        assert_eq!(
            app.local_document_names_for_test(),
            vec!["local-only.typ".to_owned()],
            "empty present must not wipe unpublished local source"
        );
        assert_eq!(
            app.local_source_for_test(),
            "= Not published yet\n",
            "empty present must keep the local source body"
        );
    }

    #[test]
    fn a_standalone_source_is_created_through_the_local_host() {
        use automerge::ReadDoc;
        let mut app = TypstApp::with_demo_documents();
        assert!(!app.is_framework_hosted());
        let mut doc = automerge::AutoCommit::new();
        let id = app.ensure_source_obj(&mut doc);
        assert!(
            !doc.text(&id).unwrap_or_default().is_empty(),
            "standalone source setup seeds the sample"
        );
        assert!(
            app.take_source_action_for_test().is_some(),
            "standalone source setup is recorded on the local action host"
        );
    }

    #[test]
    fn hosted_source_setup_is_submitted_as_an_action() {
        use automerge::ReadDoc;
        let mut app = TypstApp::with_demo_documents();
        app.set_framework_hosted(true);
        let mut doc = automerge::AutoCommit::new();
        let id = app.ensure_source_obj(&mut doc);
        let text = doc.text(&id).expect("seeded source");
        assert!(!text.is_empty(), "the first source action seeds the sample");
        assert!(
            app.take_source_action_for_test().is_some(),
            "hosted source setup is an action the host records"
        );
        app.ensure_source_obj(&mut doc);
        assert_eq!(
            doc.text(&id).unwrap(),
            text,
            "a second setup leaves the source in place"
        );
    }
}
