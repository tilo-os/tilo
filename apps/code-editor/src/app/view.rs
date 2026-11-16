//! The editor as a bound view: [`window`] lays the [`CodeApp`] out over the project the document holds
//! and answers [`CodeEditorEffect`]s on it.

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

pub(crate) type Shared = Rc<RefCell<Store>>;

#[derive(Default)]
pub(crate) struct Store {
    pub(super) shown: Shown,
    pub(super) staged: Vec<Option<String>>,
}

#[derive(Default)]
pub(crate) struct Shown {
    pub(super) files: Rc<Vec<(String, LiveRow)>>,
    pub(super) symbols: Rc<Vec<SymbolRef>>,
    pub(super) results: Rc<Vec<SearchResultRef>>,
    pub(super) entries: Rc<Vec<SearchEntry>>,
    pub(super) open: Option<(String, bool)>,
    pub(super) statuses: Vec<EffectStatus>,
}

impl Shown {
    fn file_row(&self, id: &str, row: &LiveRow) -> LiveRow {
        match &self.open {
            Some((open, true)) if open == id => row.clone().effect(&CodeEditorEffect::Save {
                file: Some(id.to_owned()),
                path: None,
            }),
            _ => row.clone(),
        }
    }

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
            .search_entries(move || entries.borrow().shown.entries.to_vec()),
        Box::new(window),
    )
}
