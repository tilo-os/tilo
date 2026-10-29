//! What the reader window shares with the view it stands in: the rows and
//! search entries it last published, the system search's request for the
//! document's text, a screenshot scenario to stage, and the host's answers
//! to opening a PDF here that arrive after the effect that asked returned.

use std::cell::{RefCell, RefMut};
use std::path::PathBuf;
use std::rc::Rc;

use ui::framework::{EffectAnswer, EffectCx, Effected, Live, LiveRow, OpenHere};
use ui::tilo_resource::SearchEntry;

use super::PdfApp;
use crate::actions::{PdfEffect, PdfResourceCtx};
use crate::markup::PdfMarkup;

/// What the view's publications, stage hook and later effect answers
/// share with the window.
#[derive(Default)]
pub(crate) struct ReaderStore {
    /// The open document's outline and find hits, as the window last
    /// published them; `None` without a document.
    rows: Option<PdfResourceCtx>,
    /// What the open document says, as the window last published it.
    search_entries: Rc<Vec<SearchEntry>>,
    /// Whether the text the system's search asked for is still being read.
    search_pending: bool,
    /// The system's search asked for every page's text since the last sync.
    search_asked: bool,
    /// A screenshot scenario to stage on the next sync.
    #[cfg(feature = "samples")]
    staged: Option<Staged>,
    /// The window asked its host to show a PDF here since the last sync.
    opening: bool,
    /// The host's answers to showing a PDF here, taken up on the next sync.
    opened: Vec<(PathBuf, Result<(), String>)>,
}

impl ReaderStore {
    pub(crate) fn live_rows(&self) -> Vec<LiveRow> {
        self.rows
            .as_ref()
            .map(crate::actions::live_rows)
            .unwrap_or_default()
    }

    pub(crate) fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.rows
            .as_ref()
            .and_then(|ctx| crate::actions::live_row(ctx, id))
    }

    pub(crate) fn search_entries(&self) -> Vec<SearchEntry> {
        self.search_entries.as_ref().clone()
    }

    /// The system's search is looking: the window reads every page's text
    /// on its next sync.
    pub(crate) fn ask_search(&mut self) {
        self.search_asked = true;
    }

    pub(crate) fn search_pending(&self) -> bool {
        self.search_asked || self.search_pending
    }

    /// Stage the screenshot `scenario` on the window's next sync.
    #[cfg(feature = "samples")]
    pub(crate) fn stage(&mut self, scenario: Option<&str>) {
        self.staged = Some(Staged(scenario.map(str::to_owned)));
    }
}

/// A screenshot scenario to stage, `None` for the plain window.
#[cfg(feature = "samples")]
struct Staged(Option<String>);

/// What showing a PDF here needs of the window, kept past the effect that
/// asks so a chooser's answer can still ask the host.
pub(super) struct AskHere {
    /// The file the window shows now.
    shown: Option<PathBuf>,
    open_here: Option<OpenHere>,
    store: Rc<RefCell<ReaderStore>>,
}

impl AskHere {
    /// Ask the host to show the PDF at `path` on its own markup record in
    /// this window, and answer with whether it does.
    pub(super) fn open(self, path: PathBuf, cx: &mut EffectCx<PdfMarkup>) -> EffectAnswer {
        if self.shown.as_deref() == Some(path.as_path()) {
            return Ok(Effected::done());
        }
        let Some(open_here) = self.open_here else {
            return Err("this window shows the document it was opened on".into());
        };
        self.store.borrow_mut().opening = true;
        cx.waker().wake();
        let arguments = [path.display().to_string()];
        let store = self.store;
        Ok(cx.open_here(&open_here, &arguments, move |(), answer, cx| {
            let result = answer
                .as_ref()
                .map(|()| Effected::done())
                .map_err(|reason| super::open_error(&path, reason));
            store.borrow_mut().opened.push((path, answer));
            cx.waker().wake();
            result
        }))
    }
}

impl PdfApp {
    /// What the window shares with the view's publications and stage hook.
    pub(crate) fn shared(&self) -> Rc<RefCell<ReaderStore>> {
        Rc::clone(&self.store)
    }

    pub(super) fn store(&self) -> RefMut<'_, ReaderStore> {
        self.store.borrow_mut()
    }

    /// What showing a PDF here needs, as the window is now.
    pub(super) fn ask_here(&self) -> AskHere {
        AskHere {
            shown: self.source_path.clone(),
            open_here: self.open_here.clone(),
            store: Rc::clone(&self.store),
        }
    }

    /// Follow the document and the participant's permission to edit it
    /// from now on.
    pub(crate) fn follow(&mut self, live: Live<PdfMarkup>, read_only: ui::LocalState<bool>) {
        self.live = Some(live);
        self.read_only_state = Some(read_only);
        self.follow_document();
    }

    /// Take up the document and the permission to edit it when either
    /// changed since; whether one did.
    pub(super) fn follow_document(&mut self) -> bool {
        let mut changed = false;
        if let Some(state) = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented))
        {
            self.adopt_resource_state(&state);
            changed = true;
        }
        if let Some(read_only) = self.read_only_state.as_ref().map(ui::LocalState::get)
            && read_only != self.read_only
        {
            self.set_read_only(read_only);
            changed = true;
        }
        changed
    }

    /// Whether the view asked for something the next sync takes up.
    pub(super) fn asked(&self) -> bool {
        let store = self.store.borrow();
        #[cfg(feature = "samples")]
        if store.staged.is_some() {
            return true;
        }
        store.search_asked || store.opening || !store.opened.is_empty()
    }

    /// Take up what the view asked for since the last sync: a scenario to
    /// stage, the system search's request for every page's text, and the
    /// host's answers to showing a PDF here.
    pub(super) fn take_up_asked(&mut self) {
        #[cfg(feature = "samples")]
        {
            let staged = self.store().staged.take();
            if let Some(Staged(scenario)) = staged {
                self.stage(scenario.as_deref());
            }
        }
        let asked = std::mem::take(&mut self.store().search_asked);
        if asked {
            self.read_text_for_search();
        }
        if std::mem::take(&mut self.store().opening) {
            self.status_note.show(crate::t("pdf-note-opening"));
            self.dirty = true;
        }
        let opened = std::mem::take(&mut self.store().opened);
        for (path, answer) in opened {
            // The answer's caller already heard why; the person is told here.
            let _ = self.opened_here(&path, answer);
        }
    }

    /// Keep what the window publishes now for the view's publications.
    pub(crate) fn publish(&self) {
        let rows = self.pdf_resource_ctx();
        let entries = if self.has_document {
            Rc::clone(&self.search_entries)
        } else {
            Rc::default()
        };
        let pending = self.search_text_pending();
        let mut store = self.store.borrow_mut();
        store.rows = rows;
        store.search_entries = entries;
        store.search_pending = pending;
    }

    /// Every effect acts on the window: what it shows, the document it
    /// opens, the text it reads.
    pub(crate) fn effect(
        &mut self,
        effect: PdfEffect,
        cx: &mut EffectCx<PdfMarkup>,
    ) -> EffectAnswer {
        let answer = match effect {
            PdfEffect::OpenPdf { path } => self.open_effect(path, cx),
            PdfEffect::ReadText { from_page, to_page } => {
                self.read_text(from_page, to_page).map(Effected::value)
            }
            PdfEffect::LookPage { page, size } => self.look_page(page, size).map(Effected::Done),
            PdfEffect::ImportPdfContent { content, name } => self
                .import_pdf_content(&content, name.as_deref(), cx)
                .map(Effected::value),
            PdfEffect::SelectText { .. } | PdfEffect::CopySelection => {
                self.selection_effect(&effect, || cx.reply())
            }
            effect => self.answer_effect(&effect, || cx.reply()),
        };
        // What the effect asked shows now, and publications read what it
        // changed before the next sync.
        self.take_up_asked();
        self.publish();
        answer
    }
}
