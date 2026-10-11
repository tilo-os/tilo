//! PDF Viewer as a [`tilo_resource::Resource`]: markup on the open document is
//! the shared state; page/zoom presentation is View; annotations are document
//! actions. The document's outline and live find-in-page hits stay on the
//! surface, published beside the resource with view operations only.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, Fixture, LaunchInput, LiveRow, ResourceApp, ResourceSurface, RunMode,
    SurfaceCx, ViewCx,
};
use ui::record::RecordId;
use ui::tilo_actions::{ActionAddress, PermissionRequirement};
use ui::tilo_resource::{ActionDecl, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{PDF_VIEWER_ACTION_TYPE, PdfEffect, PdfViewerAction, register};
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

    fn declarations() -> Vec<ActionDecl<Self>> {
        crate::actions::declarations()
    }

    fn least_permission() -> PermissionRequirement {
        PermissionRequirement::Comment
    }

    /// Replying and commenting need only the right to comment.
    fn permission(action: &PdfViewerAction) -> PermissionRequirement {
        match action.required_verb() {
            ui::permissions::Verb::Comment => PermissionRequirement::Comment,
            _ => PermissionRequirement::Write,
        }
    }

    fn children(state: &PdfMarkup) -> Vec<tilo_actions::Resource> {
        crate::actions::markup_children(state)
    }

    fn child(state: &PdfMarkup, id: &str) -> Option<tilo_actions::Resource> {
        crate::actions::markup_child(state, id)
    }

    /// The markup people made, each entry opening the child it names.
    fn search(state: &PdfMarkup) -> Vec<SearchEntry> {
        let entry = |resource: String, title: String, text: String| SearchEntry {
            anchor: Some(resource.clone()),
            title,
            text,
            action: Some(ActionAddress::new(&resource, "go-to")),
            ..Default::default()
        };
        let mut entries: Vec<SearchEntry> = state
            .annotations
            .iter()
            .map(|anno| {
                entry(
                    format!("pdf.annotation:{}", anno.id),
                    format!("{} annotation", anno.kind.type_key()),
                    format!("Page {} annotated by {}", anno.page, anno.author),
                )
            })
            .collect();
        for comment in &state.comments {
            entries.push(entry(
                format!("pdf.comment:{}", comment.id),
                format!("Comment — {}", comment.author),
                comment.body.clone(),
            ));
            for reply in &comment.replies {
                entries.push(entry(
                    format!("pdf.reply:{}:{}", comment.id, reply.id),
                    format!("Reply — {}", reply.author),
                    reply.body.clone(),
                ));
            }
        }
        for signature in &state.signatures {
            entries.push(entry(
                format!("pdf.signature:{}", signature.id),
                format!("Signature by {}", signature.signer),
                format!("Page {} signed on {}", signature.page, signature.date),
            ));
        }
        entries
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

    fn fixture(fixture: &mut Fixture<PdfViewer>) {
        crate::scenarios::fixture(fixture);
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<PdfViewer>> {
        crate::scenarios::scenario(name)
    }
}

/// The role of a document's markup record beside the document's own.
const MARKUP_ROLE: &str = "annotations";

impl ResourceSurface<PdfViewer, PdfEffect> for PdfApp {
    fn present(&mut self, state: &Rc<PdfMarkup>) {
        self.adopt_resource_state(state);
    }

    /// What the open document says — its pages' text, its outline, the
    /// current find hits — beside the markup the resource holds.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.shown_search_entries()
    }

    fn attach(&mut self, cx: SurfaceCx<PdfViewer>) {
        self.attach_framework(cx.dispatcher());
        self.attach_people(cx.people());
        self.attach_open_here(cx.opener());
        // A capture shows the scenario's signature, never the person's.
        if cx.mode().is_live()
            && let Some(signature) = cx.person::<crate::signature::SignatureProfile>()
        {
            self.attach_signature(signature);
        }
    }

    fn read_only_changed(&mut self, read_only: bool) {
        self.set_read_only(read_only);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        PdfApp::stage(self, scenario);
    }

    fn effect(&mut self, effect: PdfEffect, cx: &mut EffectCx<PdfViewer, Self>) -> EffectAnswer {
        match effect {
            PdfEffect::Open { path } => self.open_effect(path, cx),
            effect => self.answer_effect(&effect, || cx.reply()),
        }
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.document_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.document_row(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::OperationKind;
    use ui::tilo_resource::Effect;

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
        for id in ["go-to", "duplicate", "delete", "move-down", "rotate-cw"] {
            assert!(ids.contains(&id), "{ids:?}");
        }
    }

    #[test]
    fn the_surface_publishes_find_hits_beside_the_resource_never_a_document_root() {
        let specs = PdfEffect::specs();
        let ids: Vec<&str> = specs.iter().map(|a| a.id.as_str()).collect();
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
                "palette `{expected}` is a typed effect of the reader: {ids:?}"
            );
        }
        let kind = |id: &str| {
            specs
                .iter()
                .find(|action| action.id == id)
                .map(|action| action.kind)
        };
        assert_eq!(kind("find"), Some(OperationKind::View));
        assert_eq!(kind("open"), Some(OperationKind::Effect));
        assert!(
            !ids.contains(&"go-to") && !ids.contains(&"go-to-match"),
            "navigation to a place is offered only bound to its row: {ids:?}"
        );

        let mut app = crate::scenarios::sample_reader();
        let mut state = crate::scenarios::sample_state();
        app.sync_settled(&mut state.doc, &state.root);
        app.stage_find_for_test("sandbox");
        assert!(
            app.find_match_count_for_test() > 0,
            "sample must yield find hits"
        );
        let rows: Vec<tilo_actions::Resource> = app
            .document_rows()
            .into_iter()
            .map(LiveRow::into_resource)
            .collect();
        assert!(
            rows.iter().all(|resource| resource.kind != "pdf.document"),
            "the surface never republishes pdf.document beside the resource"
        );
        let hits = rows
            .iter()
            .find(|resource| resource.kind == "pdf.search")
            .expect("the find slice");
        assert!(
            !hits.children.is_empty()
                && hits
                    .children
                    .iter()
                    .all(|hit| hit.kind == "pdf.search-result"
                        && hit
                            .actions
                            .iter()
                            .all(|action| action.kind == OperationKind::View
                                && action.id == "go-to-match")),
            "find hits are navigable rows with view operations only"
        );
        let hit = &hits.children[0];
        assert_eq!(
            app.document_row(hit.id.as_str())
                .map(|resolved| resolved.id().to_owned()),
            Some(hit.id.as_str().to_owned()),
            "a hit beyond the listed page resolves by id"
        );

        app.stage_find_for_test("");
        assert!(
            app.document_rows()
                .iter()
                .all(|row| !row.id().starts_with("pdf.search")),
            "no find slice without a query"
        );
    }
}
