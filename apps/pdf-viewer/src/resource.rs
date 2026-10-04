//! PDF Viewer as a [`tilo_resource::Resource`]: markup on the open document is
//! the shared state; page/zoom presentation is View; annotations are document
//! actions. Live find-in-page hits stay on the surface — hosted
//! [`PublishesResources`](ui::resources::PublishesResources) returns only that
//! slice beside the framework door.

use std::rc::Rc;

use automerge::AutoCommit;
use ui::framework::{
    Dispatcher, Fixture, LaunchInput, ResourceApp, ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::record::RecordId;
use ui::tilo_actions::{ActionSpec, EncodedAction};
use ui::tilo_resource::{Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{PDF_VIEWER_ACTION_TYPE, PdfViewerAction, register};
use crate::app::PdfApp;
use crate::doc::{PagePlanEntry, PdfDoc};
use crate::model::{Annotation, Comment, Signature};
use crate::scenarios::SampleDocument;

/// Collaborative markup projected from the annotation-set record.
#[derive(Clone, Debug, Default)]
pub struct PdfMarkup {
    pub pages: Vec<PagePlanEntry>,
    pub annotations: Vec<Annotation>,
    pub comments: Vec<Comment>,
    pub signatures: Vec<Signature>,
    /// The PDF bytes the markup is made against, once a document opened.
    pub source: Option<PinnedSource>,
}

/// The PDF bytes a record's markup is made against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedSource {
    pub hash: String,
    pub len: u64,
    /// The file name it was opened under; empty when it was not named.
    pub name: String,
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
            source: PdfDoc::source(doc).map(|(hash, len, _)| PinnedSource {
                hash,
                len,
                name: PdfDoc::source_name(doc).unwrap_or_default(),
            }),
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
                action: None,
            })
            .chain(state.comments.iter().map(|comment| SearchEntry {
                anchor: Some(format!("pdf.comment:{}", comment.id)),
                title: "Comment".into(),
                text: comment.author.clone(),
                action: None,
            }))
            .collect()
    }
}

/// The PDF reader: one record per document holds its markup, and the
/// reader is the surface over it.
pub struct PdfViewerShell;

impl ResourceApp for PdfViewerShell {
    type Resource = PdfViewer;
    const ID: &'static str = "so.tilo.pdf-viewer";
    const TITLE: &'static str = "PDF Viewer";
    const ICON: Option<&'static str> = Some("pdf-viewer");
    const OPENS: &'static [&'static str] = &["application/pdf"];
    /// The PDF to open (`pdf-viewer paper.pdf`).
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<PdfViewer>) -> Element {
        let content = cx.content().unwrap_or_else(crate::app::default_content);
        let app = if cx.mode() == RunMode::Past {
            // A past version shows its markup's record, never a document:
            // History builds a view for every version it previews.
            PdfApp::past()
        } else if let Some(path) = cx.launch_arguments().first() {
            PdfApp::open_with_content(path, content)
        } else if cx.fixture::<SampleDocument>().is_some() {
            PdfApp::sample_with_content(content)
        } else {
            PdfApp::empty_with_content(content)
        };
        cx.surface(app)
    }

    fn label(state: &PdfMarkup) -> String {
        match &state.source {
            Some(source) if !source.name.is_empty() => source.name.clone(),
            _ if state.pages.is_empty() => "PDF Viewer".into(),
            _ => format!("{} pages", state.pages.len()),
        }
    }

    /// A document's markup is a record of its own, derived from the file's
    /// durable identity, so renaming or moving the file finds the same
    /// annotations, a copy starts clean, and another app's record of the
    /// same file is left alone.
    fn launch_record(input: &LaunchInput<'_>) -> Option<RecordId> {
        input.argument().map(|path| {
            RecordId::derived(
                MARKUP_ROLE,
                &RecordId::identify_file(std::path::absolute(path).unwrap_or_else(|_| path.into())),
            )
        })
    }

    /// A handed file is kept in the content store and pinned as the record's
    /// source, so the record reopens it wherever the bytes are.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<PdfViewer>> {
        let (Some(file), Some(content)) = (input.file(), input.content()) else {
            return Vec::new();
        };
        let Ok(hash) = content.put(file.bytes()) else {
            return Vec::new();
        };
        let len = u64::try_from(file.bytes().len()).unwrap_or(u64::MAX);
        Invoke::apply(&crate::actions::pin_source_action(&hash, len, file.name()))
            .into_iter()
            .collect()
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn fixture(fixture: &mut Fixture<PdfViewer>) {
        crate::scenarios::fixture(fixture);
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<PdfViewer>> {
        crate::scenarios::scenario(name)
    }
}

/// The role of a document's markup record beside the document's own.
const MARKUP_ROLE: &str = "annotations";

impl ResourceSurface<PdfViewer> for PdfApp {
    fn present(&mut self, state: &Rc<PdfMarkup>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<PdfViewer>) {
        self.attach_framework(cx.dispatcher());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        PdfApp::stage(self, scenario);
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

        let mut app = crate::scenarios::sample_reader();
        let mut state = crate::scenarios::sample_state();
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
