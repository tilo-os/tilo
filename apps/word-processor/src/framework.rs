//! The word processor as a framework app: qdoc's rich text is the resource,
//! and the editor — its own page layout, input and chrome — is the surface.
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
use tilo_document::{RichText, RichTextAction, RichTextState};
use ui::framework::{Dispatcher, Live, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_resource::{Demand, Reader};
use ui::widget::Element;

use crate::WordProcessorWidget;

/// The word processor on the app framework.
pub struct WordProcessorApp;

impl ResourceApp for WordProcessorApp {
    type Resource = RichText;
    const ID: &'static str = "so.tilo.word-processor";
    const TITLE: &'static str = "Word Processor";
    const ICON: Option<&'static str> = Some("word-processor");
    const SHOT_VIEWPORT: (u32, u32) = (1000, 700);

    fn view(cx: &mut ViewCx<RichText>) -> Element {
        cx.surface(WordProcessorWidget::new())
    }

    /// The document's first heading, as the palette and history name it.
    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        // History is the framework's own surface. The editor's version strip
        // is not offered beside it.
        crate::actions::view_operations(false)
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
    Hosted(Live<RichText>),
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
    framework: Option<Dispatcher<RichText>>,
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<RichText>) {
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
    pub(crate) fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.local.last_dispatched()
    }
}

impl ResourceSurface<RichText> for WordProcessorWidget {
    /// The editor reads the live state where it needs it; a new state only
    /// asks for a redraw.
    fn present(&mut self, _state: &Rc<RichTextState>) {
        self.mark_document_changed();
    }

    fn attach(&mut self, cx: SurfaceCx<RichText>) {
        self.attach_framework(cx.live(), cx.dispatcher());
    }
}
