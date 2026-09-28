//! PDF Viewer as a [`tilo_resource::Resource`]: markup on the open document is
//! the shared state; page/zoom presentation is View; annotations are document
//! actions.

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
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

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
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<PdfViewer>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
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

    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
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
