//! PDF Viewer as a [`tilo_resource::Resource`]: markup on the open document is
//! the shared state; page/zoom presentation is View; annotations are document
//! actions. Live find-in-page hits stay on the surface — hosted
//! [`PublishesResources`](ui::resources::PublishesResources) returns only that
//! slice beside the framework door.

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionSpec, EncodedAction};
use ui::tilo_resource::{Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{PDF_VIEWER_ACTION_TYPE, PdfViewerAction, register};
use crate::app::PdfApp;
use crate::doc::{PagePlanEntry, PdfDoc};
use crate::model::{Annotation, Comment, Signature};

/// Collaborative markup projected from the annotation-set record.
#[derive(Clone, Debug, Default)]
pub struct PdfMarkup {
    pub pages: Vec<PagePlanEntry>,
    pub annotations: Vec<Annotation>,
    pub comments: Vec<Comment>,
    pub signatures: Vec<Signature>,
}

/// The PDF markup resource.
pub struct PdfViewer;

impl Resource for PdfViewer {
    /// Matches the action-surface kind tests and search addresses use.
    /// Persistent checkout historically also used `annotation-set` /
    /// `pdf-markup` — see blockers.
    const KIND: &'static str = "pdf.document";
    const LABEL: &'static str = "PDF";
    const ACTION_TYPE: &'static str = PDF_VIEWER_ACTION_TYPE;

    type State = PdfMarkup;
    type Action = PdfViewerAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<PdfMarkup, ModelError> {
        Ok(PdfMarkup {
            pages: PdfDoc::list_page_plan(doc),
            annotations: PdfDoc::list_annotations(doc),
            comments: PdfDoc::list_comments(doc),
            signatures: PdfDoc::list_signatures(doc),
        })
    }

    fn children(state: &PdfMarkup) -> Vec<tilo_actions::Resource> {
        crate::actions::markup_children(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        crate::actions::document_local_operations()
    }

    fn search(state: &PdfMarkup) -> Vec<SearchEntry> {
        state
            .annotations
            .iter()
            .map(|anno| SearchEntry {
                anchor: Some(format!("pdf.annotation:{}", anno.id)),
                title: format!("Annotation on page {}", anno.page.saturating_add(1)),
                text: anno.author.clone(),
            })
            .chain(state.comments.iter().map(|comment| SearchEntry {
                anchor: Some(format!("pdf.comment:{}", comment.id)),
                title: "Comment".into(),
                text: comment.author.clone(),
            }))
            .collect()
    }
}

/// Framework app: the PDF widget is the surface over [`PdfViewer`].
pub struct PdfViewerShell;

impl ResourceApp for PdfViewerShell {
    type Resource = PdfViewer;
    const ID: &'static str = "so.tilo.pdf-viewer";
    const TITLE: &'static str = "PDF Viewer";
    const ICON: Option<&'static str> = Some("pdf-viewer");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn view(cx: &mut ViewCx<PdfViewer>) -> Element {
        cx.surface(PdfApp::new())
    }

    fn label(state: &PdfMarkup) -> String {
        if state.pages.is_empty() {
            "PDF Viewer".into()
        } else {
            format!("{} pages", state.pages.len())
        }
    }
}

impl ResourceSurface<PdfViewer> for PdfApp {
    fn present(&mut self, state: &Rc<PdfMarkup>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<PdfViewer>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<PdfViewer>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    hosted: bool,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<PdfViewer>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &PdfViewerAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

pub fn launch_spec() -> ui::launch::LaunchSpec {
    ui::framework::launch_spec::<PdfViewerShell>()
}

pub fn standalone_launch() -> bool {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--data-dir" => {
                let _ = args.next();
            }
            "--shots" | "--responsive-matrix" | "--native-audit" | "--headless" | "--sample" => {
                return true;
            }
            file if !file.starts_with('-') => return true,
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::resources::PublishesResources;
    use ui::tilo_actions::OperationKind;
    use ui::widget::Widget;

    #[test]
    fn published_pages_carry_duplicate_and_delete() {
        let markup = PdfMarkup {
            pages: vec![
                crate::doc::PagePlanEntry {
                    id: "a".into(),
                    source_page: 1,
                    imported: None,
                    rotation: 0,
                },
                crate::doc::PagePlanEntry {
                    id: "b".into(),
                    source_page: 2,
                    imported: None,
                    rotation: 0,
                },
            ],
            ..PdfMarkup::default()
        };
        let published =
            ui::tilo_resource::publish::<PdfViewer>("pdf.document:sample", "PDF", &markup);
        let page = published
            .children
            .iter()
            .find(|child| child.kind == "pdf.page")
            .expect("published page");
        let ids: Vec<&str> = page
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for id in ["open", "duplicate", "delete", "move-down", "rotate-cw"] {
            assert!(ids.contains(&id), "{ids:?}");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn hosted_resources_keep_find_hits_without_document_root() {
        let published = ui::tilo_resource::publish::<PdfViewer>(
            "pdf.document:sample",
            "PDF",
            &PdfMarkup::default(),
        );
        assert_eq!(published.kind, "pdf.document");
        let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        for expected in [
            "previous-page",
            "next-page",
            "zoom-in",
            "zoom-out",
            "fit-page",
            "find",
            "open",
            "save-as",
        ] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride local_operations via publish: {ids:?}"
            );
        }
        let find = published
            .actions
            .iter()
            .find(|action| action.id == "find")
            .expect("find");
        assert_eq!(find.kind, OperationKind::View);

        let mut app = PdfApp::sample();
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        assert!(
            !app.is_hosted(),
            "standalone stays on the local action host"
        );

        app.stage_find_for_test("sandbox");
        assert!(
            app.find_match_count_for_test() > 0,
            "sample must yield find hits for the hosted extras contract"
        );

        let standalone = app.resources(&state.doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "pdf.document");
        let standalone_find = standalone[0]
            .children
            .iter()
            .find(|child| child.kind == "pdf.search")
            .expect("standalone nests find hits under the document");
        assert!(
            !standalone_find.children.is_empty(),
            "standalone find projection carries match rows"
        );
        assert!(
            standalone_find
                .children
                .iter()
                .all(|hit| hit.kind == "pdf.search-result"
                    && hit.actions.iter().any(|action| action.id == "open")),
            "each find hit is a navigable search-result row"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let extras = app.resources(&state.doc);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "pdf.document"),
            "hosted must not republish pdf.document beside ResourceRoot: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(extras.len(), 1, "hosted returns the find slice alone");
        assert_eq!(extras[0].kind, "pdf.search");
        let directory = app
            .action_directory_resource_page(&state.doc, &[], 32)
            .expect("directory page");
        assert!(
            directory
                .resources
                .iter()
                .all(|resource| resource.kind != "pdf.document"),
            "hosted directory must not republish pdf.document: {:?}",
            directory
                .resources
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(directory.resources.len(), 1);
        assert_eq!(directory.resources[0].id, extras[0].id);
        assert_eq!(
            extras[0].id, standalone_find.id,
            "hosted find tree matches the standalone nested slice"
        );
        assert_eq!(
            extras[0].children.len(),
            standalone_find.children.len(),
            "hosted keeps the same bounded match rows"
        );
        assert!(
            extras[0]
                .children
                .iter()
                .all(|hit| hit.kind == "pdf.search-result"),
            "hosted extras are find hits only, not pages/markup: {:?}",
            extras[0]
                .children
                .iter()
                .map(|child| child.kind.as_str())
                .collect::<Vec<_>>()
        );

        app.stage_find_for_test("");
        assert!(
            app.resources(&state.doc).is_empty(),
            "hosted publishes no extras when find has no query"
        );
        let directory = app
            .action_directory_resource_page(&state.doc, &[], 32)
            .expect("directory page");
        assert!(
            directory.resources.is_empty(),
            "hosted directory publishes no document when find has no query"
        );
    }
}
