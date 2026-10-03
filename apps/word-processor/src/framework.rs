//! The word processor as a framework app: the document resource is rich text,
//! and the editor — its own page layout, input and chrome — is the surface.
//! Images, tables and headings are children of that resource, so the framework
//! publishes them on the same door as the document.
//!
//! What the framework adds without the editor writing it: record storage and
//! sync, permission-checked edits (a viewer's keystrokes are refused before
//! they reach the document), per-author undo with typing grouped by word,
//! attributed history and a History mode drawn by the editor's real page
//! layout, presence keyed to paragraphs, and search over the outline.
//!
//! The editor reads the document through the framework's incrementally
//! maintained [`RichTextState`]: a keystroke refreshes its paragraph, and
//! only the paragraphs around the viewport are ever read.

use std::rc::Rc;

use automerge::AutoCommit;
use tilo_actions::{ActionContext, ActionError};
use tilo_actions::{ActionParam, ParamType};
use tilo_document::{RichText, RichTextAction, RichTextState};
use ui::framework::{Dispatcher, Live, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_resource::{ActionDecl, Demand, Doc, ModelError, Reader, Resource};
use ui::widget::Element;

use crate::WordProcessorWidget;

/// Rich text plus the images, tables and headings that document already holds.
///
/// The record kind stays `tilo.rich-text`. The editor's chrome (find, open,
/// save) stays on [`WordProcessorApp::local_operations`]. Structural objects,
/// notes, review, paragraph styles, and the page header and footer are
/// [`Resource::children`], so a hosted palette does not rebuild them beside
/// the document.
pub struct Document;

impl Resource for Document {
    const KIND: &'static str = <RichText as Resource>::KIND;
    const LABEL: &'static str = <RichText as Resource>::LABEL;
    const ACTION_TYPE: &'static str = <RichText as Resource>::ACTION_TYPE;

    type State = RichTextState;
    type Action = RichTextAction;

    fn read(doc: &Doc) -> Result<Self::State, ModelError> {
        <RichText as Resource>::read(doc)
    }

    fn locate(state: &Self::State, obj: &automerge::ObjId) -> Option<ui::tilo_resource::Path> {
        <RichText as Resource>::locate(state, obj)
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        <RichText as Resource>::initialize(doc)
    }

    fn refresh(
        state: &mut Self::State,
        doc: &Doc,
        delta: &ui::tilo_resource::Delta,
    ) -> Result<(), ModelError> {
        <RichText as Resource>::refresh(state, doc, delta)
    }

    fn load(state: &mut Self::State, doc: &Doc, demand: &Demand) -> Result<(), ModelError> {
        <RichText as Resource>::load(state, doc, demand)
    }

    fn coalesces(previous: &Self::Action, next: &Self::Action) -> bool {
        <RichText as Resource>::coalesces(previous, next)
    }

    fn annotates(action: &Self::Action) -> bool {
        <RichText as Resource>::annotates(action)
    }

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        <RichText as Resource>::register(registry);
    }

    fn action_schema() -> Option<serde_json::Value> {
        <RichText as Resource>::action_schema()
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        let mut decls: Vec<ActionDecl<Self>> = <RichText as Resource>::declarations()
            .into_iter()
            .map(|decl| ActionDecl {
                id: decl.id,
                label: decl.label,
                description: decl.description,
                params: decl.params,
                permission: decl.permission,
                destructive: decl.destructive,
                build: decl.build,
            })
            .collect();
        decls.push(
            ActionDecl::new(
                "set-page-margins",
                "Set page margins",
                vec![
                    ActionParam::required_owned("top", ParamType::Number, "Top margin in px"),
                    ActionParam::required_owned("right", ParamType::Number, "Right margin in px"),
                    ActionParam::required_owned("bottom", ParamType::Number, "Bottom margin in px"),
                    ActionParam::required_owned("left", ParamType::Number, "Left margin in px"),
                ],
                |args, _| {
                    let margin = |name: &str| {
                        args.number(name)
                            .ok_or_else(|| format!("missing {name} margin"))
                    };
                    Ok(RichTextAction::SetPageMargins {
                        top: margin("top")?,
                        right: margin("right")?,
                        bottom: margin("bottom")?,
                        left: margin("left")?,
                    })
                },
            )
            .describe("Set the four page margins, in pixels."),
        );
        decls
    }

    fn views(state: &Self::State) -> Vec<tilo_actions::ResourceViewSpec> {
        <RichText as Resource>::views(state)
    }

    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        let mut specs = <RichText as Resource>::local_operations();
        specs.push(crate::semantic_read::action::spec());
        specs
    }

    fn search(state: &Self::State) -> Vec<ui::tilo_resource::SearchEntry> {
        <RichText as Resource>::search(state)
    }

    fn children(state: &Self::State) -> Vec<tilo_actions::Resource> {
        crate::actions::structural_children(state)
    }

    fn bound_actions(state: &Self::State) -> Vec<tilo_actions::ActionSpec> {
        crate::actions::toc_action(state).into_iter().collect()
    }
}

/// The word processor on the app framework.
pub struct WordProcessorApp;

impl ResourceApp for WordProcessorApp {
    type Resource = Document;
    const ID: &'static str = "so.tilo.word-processor";
    const TITLE: &'static str = "Word Processor";
    const ICON: Option<&'static str> = Some("word-processor");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1000, 700)
    }

    fn view(cx: &mut ViewCx<Document>) -> Element {
        cx.surface(WordProcessorWidget::new())
    }

    /// The document's first heading, as the palette and history name it.
    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        // History is the framework's own surface. The editor's version strip
        // is not offered beside it. Open, save, and save-as are Effects on
        // this door; a live file continuation is overlaid by the standalone tree.
        let mut specs = crate::actions::view_operations(false);
        specs.extend(crate::actions::file_operation_specs(None));
        specs
    }

    fn label(state: &RichTextState) -> String {
        state
            .headings()
            .into_iter()
            .map(|heading| heading.text.trim().to_owned())
            .find(|text| !text.is_empty())
            .unwrap_or_else(|| "Document".to_owned())
    }
}

/// Where the editor reads the document from.
pub(crate) enum StateSource {
    /// Standalone (its own tests, file-based launches): the editor follows
    /// the document itself, with the same incremental reader the framework
    /// uses.
    Own(Option<Reader<RichText>>),
    /// Hosted: the framework's live state, shared with History mode, search
    /// and the palette.
    Hosted(Live<Document>),
}

#[cfg(test)]
mod tests {
    use super::WordProcessorApp;
    use ui::framework::ResourceApp;

    #[test]
    fn framework_palette_includes_file_effects_and_views() {
        let specs = WordProcessorApp::local_operations();
        let ids: Vec<&str> = specs.iter().map(|spec| spec.id.as_str()).collect();
        for id in ["find", "open", "save", "save-as"] {
            assert!(ids.contains(&id), "{ids:?}");
        }
        assert!(
            !ids.contains(&"history"),
            "framework history is the host surface"
        );
    }

    #[test]
    fn page_margins_are_a_published_document_action() {
        use ui::tilo_resource::Resource;
        let decl = super::Document::declarations()
            .into_iter()
            .find(|decl| decl.id == "set-page-margins")
            .expect("page margins are a document action");
        let supplied = serde_json::json!({
            "top": 72.0,
            "right": 54.0,
            "bottom": 72.0,
            "left": 54.0
        });
        let supplied = supplied.as_object().expect("margin object");
        let args = tilo_actions::ActionArgs::bind_json(decl.params.as_slice(), supplied)
            .expect("margin arguments");
        let action = (decl.build)(&args, 0).expect("the declaration builds an action");
        let mut doc = automerge::AutoCommit::new();
        <super::Document as Resource>::initialize(&mut doc).expect("document initializes");
        ui::tilo_actions::Action::apply(&action, &mut doc).expect("margins apply");
        let containers = tilo_doc::model::open(&doc, &automerge::ROOT).expect("document opens");
        assert_eq!(
            tilo_doc::model::meta_str(&doc, &containers, "page_margin_top").as_deref(),
            Some("72")
        );
        assert_eq!(
            tilo_doc::model::meta_str(&doc, &containers, "page_margin_right").as_deref(),
            Some("54")
        );
    }

    #[test]
    fn read_text_is_published_with_the_document() {
        use ui::tilo_resource::Resource;
        let mut doc = automerge::AutoCommit::new();
        <super::Document as Resource>::initialize(&mut doc).expect("document initializes");
        let state = <super::Document as Resource>::read(&doc).expect("document reads");
        let published =
            ui::tilo_resource::publish::<super::Document>("tilo.rich-text", "Document", &state);
        let read = published
            .actions
            .iter()
            .find(|action| action.id == "read-text")
            .expect("read-text is on the published document");
        assert_eq!(read.kind, tilo_actions::OperationKind::Effect);
    }
}

impl Default for StateSource {
    fn default() -> Self {
        Self::Own(None)
    }
}

impl StateSource {
    /// Bring the state up to date with `doc`. Returns whether it changed.
    pub(crate) fn sync(&mut self, doc: &mut AutoCommit) -> bool {
        match self {
            Self::Own(reader) => match reader {
                Some(reader) => reader.sync(doc).unwrap_or_else(|error| {
                    eprintln!("word-processor: document unreadable: {error}");
                    false
                }),
                None => match Reader::open(doc) {
                    Ok(opened) => {
                        *reader = Some(opened);
                        true
                    }
                    Err(_) => false,
                },
            },
            Self::Hosted(live) => live.sync(doc).unwrap_or_else(|error| {
                eprintln!("word-processor: document unreadable: {error}");
                false
            }),
        }
    }

    /// Show the paragraphs `demand` names. Returns whether the state changed.
    pub(crate) fn demand(&mut self, doc: &AutoCommit, demand: Demand) -> bool {
        let result = match self {
            Self::Own(Some(reader)) => reader.demand(doc, demand),
            Self::Own(None) => Ok(false),
            Self::Hosted(live) => live.demand(doc, demand),
        };
        result.unwrap_or_else(|error| {
            eprintln!("word-processor: paragraphs unreadable: {error}");
            false
        })
    }

    /// Read the state, once there is one.
    pub(crate) fn with<T>(&self, read: impl FnOnce(&RichTextState) -> T) -> Option<T> {
        match self {
            Self::Own(Some(reader)) => Some(read(reader.state())),
            Self::Own(None) => None,
            Self::Hosted(live) => Some(live.with(read)),
        }
    }

    pub(crate) fn hosted(&self) -> bool {
        matches!(self, Self::Hosted(_))
    }
}

/// Where the editor's edits go.
#[derive(Default)]
pub(crate) struct ActionSink {
    /// Standalone: the toolkit's local action host.
    local: ui::ActionHost,
    /// Hosted: the framework's dispatcher.
    framework: Option<Dispatcher<Document>>,
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Document>) {
        self.framework = Some(dispatcher);
    }

    /// Whether this participant may only view the document.
    pub(crate) fn read_only(&self) -> bool {
        self.framework.as_ref().is_some_and(Dispatcher::read_only)
    }

    /// Apply one edit now. Hosted, the framework checks the participant's
    /// permission, attributes the edit and groups typing into undo steps;
    /// standalone it applies through the local executor.
    pub(crate) fn dispatch(
        &mut self,
        doc: &mut AutoCommit,
        action: &RichTextAction,
        context: ActionContext,
    ) -> Result<(), ActionError> {
        match &self.framework {
            Some(dispatcher) => dispatcher.apply_now_with(doc, action, context),
            None => self.local.try_dispatch_with_context(
                doc,
                crate::actions::WORD_PROCESSOR_ACTION_TYPE,
                action,
                context,
            ),
        }
    }

    /// The standalone undo boundary; hosted edits are bracketed by the host.
    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.local.last_dispatched()
    }
}

impl ResourceSurface<Document> for WordProcessorWidget {
    /// The editor reads the live state where it needs it; a new state only
    /// asks for a redraw.
    fn present(&mut self, _state: &Rc<RichTextState>) {
        self.mark_document_changed();
    }

    fn attach(&mut self, cx: SurfaceCx<Document>) {
        self.attach_framework(cx.live(), cx.dispatcher());
    }
}
