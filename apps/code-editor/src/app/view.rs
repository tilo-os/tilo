//! The editor as a bound view: [`window`] lays the [`CodeApp`] out over the
//! project the document holds and answers [`CodeEditorEffect`]s on it. What
//! it publishes, and the scenario a capture stages, go through a [`Store`]
//! the editor shares with the view.

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{EffectStatus, LiveRow, Publishes, ViewCx};
use ui::tilo_resource::SearchEntry;
use ui::widget::Element;

use super::CodeApp;
use super::surface::{ROW_LIMIT, result_row, symbol_row};
use crate::actions::{self, SearchResultRef, SymbolRef};
use crate::effects::CodeEditorEffect;
use crate::project::Project;

/// The [`Store`] as the editor and the view share it.
pub(crate) type Shared = Rc<RefCell<Store>>;

/// What the editor and what the view publishes share.
#[derive(Default)]
pub(crate) struct Store {
    /// What the editor showed when it last looked.
    pub(super) shown: Shown,
    /// The scenarios a capture staged (`None` for a plain capture), staged
    /// when the editor next looks.
    pub(super) staged: Vec<Option<String>>,
}

/// What the editor showed when it last looked: what the view publishes.
#[derive(Default)]
pub(crate) struct Shown {
    /// Each file of the project by its resource id, with its row: open it
    /// and read it.
    pub(super) files: Rc<Vec<(String, LiveRow)>>,
    /// The declarations the symbol scan found.
    pub(super) symbols: Rc<Vec<SymbolRef>>,
    /// The hits of the project search.
    pub(super) results: Rc<Vec<SearchResultRef>>,
    /// The symbols and the project search's hits as search entries.
    pub(super) entries: Rc<Vec<SearchEntry>>,
    /// The file on screen, and whether it has a file on disk to save to.
    pub(super) open: Option<(String, bool)>,
    /// What the effects can do right now.
    pub(super) statuses: Vec<EffectStatus>,
}

impl Shown {
    /// The row of the file `id`: save it too when it is on screen with a
    /// file on disk to save to.
    fn file_row(&self, id: &str, row: &LiveRow) -> LiveRow {
        match &self.open {
            Some((open, true)) if open == id => row.clone().effect(&CodeEditorEffect::Save {
                file: Some(id.to_owned()),
                path: None,
            }),
            _ => row.clone(),
        }
    }

    /// The project's files as the editor can open and save them, the file on
    /// screen first, then the symbols it found and the hits of the project
    /// search. A file row shares its id with the file the project publishes,
    /// so its effects join that file.
    fn live_rows(&self) -> Vec<LiveRow> {
        let open = self.open.as_ref().map(|(open, _)| open.as_str());
        let files = self
            .files
            .iter()
            .filter(|(id, _)| Some(id.as_str()) == open)
            .chain(
                self.files
                    .iter()
                    .filter(|(id, _)| Some(id.as_str()) != open),
            )
            .map(|(id, row)| self.file_row(id, row))
            .take(ROW_LIMIT);
        let symbols = self.symbols.iter().take(ROW_LIMIT).map(symbol_row);
        let results = self.results.iter().take(ROW_LIMIT).map(result_row);
        files.chain(symbols).chain(results).collect()
    }

    /// The live row `id`, beyond the ones [`Self::live_rows`] publishes.
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        if let Some((_, file)) = ui::tilo_resource::items::item_of_resource::<Project>(id) {
            return self
                .files
                .iter()
                .find(|(id, _)| id == file.as_str())
                .map(|(id, row)| self.file_row(id, row));
        }
        if let Some(symbol) = self
            .symbols
            .iter()
            .find(|symbol| actions::symbol_resource_id(symbol) == id)
        {
            return Some(symbol_row(symbol));
        }
        self.results
            .iter()
            .find(|result| actions::result_resource_id(result) == id)
            .map(result_row)
    }
}

/// The editor as the view lays it out: it shows the project the document
/// holds, answers the effects on itself, and publishes the files it can open
/// and save, the symbols it found and the project search's hits.
pub(crate) fn window(cx: &ViewCx<Project>, mut window: CodeApp) -> Element {
    window.follow(cx);
    let store = window.store();
    {
        let status = Rc::clone(&store);
        cx.window_effects(|app: &mut CodeApp, effect: CodeEditorEffect, cx| {
            app.answer_effect(effect, cx)
        })
        .status(move || status.borrow().shown.statuses.clone());
    }
    {
        let store = Rc::clone(&store);
        cx.on_stage(move |scenario| {
            store.borrow_mut().staged.push(scenario.map(str::to_owned));
        });
    }
    let (rows, row, entries) = (Rc::clone(&store), Rc::clone(&store), store);
    cx.publishing(
        Publishes::new()
            .live_rows(move || rows.borrow().shown.live_rows())
            .live_row(move |id| row.borrow().shown.live_row(id))
            // The symbols declared in the buffers and the running project
            // search's hits; the files are the resource's own entries.
            .search_entries(move || entries.borrow().shown.entries.to_vec()),
        Box::new(window),
    )
}
