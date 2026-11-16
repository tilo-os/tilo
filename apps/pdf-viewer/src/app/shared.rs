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

#[derive(Default)]
pub(crate) struct ReaderStore {
    rows: Option<PdfResourceCtx>,
    search_entries: Rc<Vec<SearchEntry>>,
    search_pending: bool,
    search_asked: bool,
    #[cfg(feature = "samples")]
    staged: Option<Staged>,
    opening: bool,
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

    pub(crate) fn ask_search(&mut self) {
        self.search_asked = true;
    }

    pub(crate) fn search_pending(&self) -> bool {
        self.search_asked || self.search_pending
    }

    #[cfg(feature = "samples")]
    pub(crate) fn stage(&mut self, scenario: Option<&str>) {
        self.staged = Some(Staged(scenario.map(str::to_owned)));
    }
}

#[cfg(feature = "samples")]
struct Staged(Option<String>);

pub(super) struct AskHere {
    shown: Option<PathBuf>,
    open_here: Option<OpenHere>,
    store: Rc<RefCell<ReaderStore>>,
}

impl AskHere {
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
    pub(crate) fn shared(&self) -> Rc<RefCell<ReaderStore>> {
        Rc::clone(&self.store)
    }

    pub(super) fn store(&self) -> RefMut<'_, ReaderStore> {
        self.store.borrow_mut()
    }

    pub(super) fn ask_here(&self) -> AskHere {
        AskHere {
            shown: self.source_path.clone(),
            open_here: self.open_here.clone(),
            store: Rc::clone(&self.store),
        }
    }

    pub(crate) fn follow(&mut self, live: Live<PdfMarkup>, read_only: ui::LocalState<bool>) {
        self.live = Some(live);
        self.read_only_state = Some(read_only);
        self.follow_document();
    }

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

    pub(super) fn asked(&self) -> bool {
        let store = self.store.borrow();
        #[cfg(feature = "samples")]
        if store.staged.is_some() {
            return true;
        }
        store.search_asked || store.opening || !store.opened.is_empty()
    }

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
            let _ = self.opened_here(&path, answer);
        }
    }

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
            PdfEffect::ReadFormFields => {
                self.read_form_fields(&cx.live().get()).map(Effected::value)
            }
            PdfEffect::InsertPages { path, at, pages } => {
                self.insert_pages(&path, at, pages, || cx.reply())
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
        self.take_up_asked();
        self.publish();
        answer
    }
}
