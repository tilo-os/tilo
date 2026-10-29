//! The sheets grid as a bound view: the [`Store`] the grid shares with the
//! view's effect handler, and [`window`], which takes the grid into the
//! view.
//!
//! The effects answer here, from the workbook the document holds and what
//! the grid last showed of it (the active sheet, the locale's currency);
//! what they ask of the grid itself (a cell to go to, a sheet to show, the
//! commands, a status) waits in [`Store::asks`] until the grid next syncs.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_locale::{Currency, Locale};
use ui::framework::content::ContentStore;
use ui::framework::{EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_resource::{ActionDecl, Id};
use ui::widget::Element;

use super::{
    Interchange, MAX_IMPORT_FILE_BYTES, PendingReveal, SelectionKey, SheetsApp, file_label,
    import_source, sheet_name_from_path, spreadsheet_export_request, spreadsheet_import_request,
};
use crate::actions::SheetsEffect;
use crate::workbook::{SheetSnapshot, Workbook};

/// What an effect asked of the grid, done when it next syncs.
pub(super) enum Ask {
    /// Show what a view effect names: a cell, the formulas, a sheet.
    Show(SheetsEffect),
    /// Open the command palette on the workbook's actions.
    Commands,
    /// Say what happened in the status.
    Status(String),
    /// Show a sheet with one of its cells selected.
    Reveal(PendingReveal),
    /// Stage a capture's scenario (`None` for a plain capture).
    Stage(Option<String>),
}

/// What the grid and the view's effect handler share.
pub(crate) struct Store {
    /// The store the host lends the window: workbooks imported from and
    /// exported as content are read and kept there.
    content: Option<ContentStore>,
    /// The sheet the grid shows, which a CSV export writes.
    pub(super) active_sheet: Option<Id>,
    /// The currency typed amounts are read in, as the grid shows them.
    pub(super) currency: Currency,
    /// The edits last bound to the selection, with what they were built
    /// from: every publication asks for them, and they change only with the
    /// workbook, the selection or the locale.
    pub(super) selection: Option<(SelectionKey, Vec<ActionDecl<Workbook>>)>,
    /// What effects asked of the grid since it last synced.
    pub(super) asks: Vec<Ask>,
}

/// The [`Store`] as the grid and the view share it.
pub(crate) type Shared = Rc<RefCell<Store>>;

impl Default for Store {
    fn default() -> Self {
        Self {
            content: None,
            active_sheet: None,
            currency: Locale::default().currency,
            selection: None,
            asks: Vec::new(),
        }
    }
}

impl Store {
    fn ask(&mut self, ask: Ask) {
        self.asks.push(ask);
    }

    /// Read and keep content in `content`, the store the host lends the
    /// window; without one, content is kept in memory.
    pub(super) fn attach_content(&mut self, content: Option<ContentStore>) {
        self.content = content;
    }

    fn content_store(&mut self) -> ContentStore {
        self.content
            .get_or_insert_with(ContentStore::memory)
            .clone()
    }

    /// The edits bound to the selection now.
    pub(super) fn selection_declarations(&self) -> Vec<ActionDecl<Workbook>> {
        self.selection
            .as_ref()
            .map(|(_, decls)| decls.clone())
            .unwrap_or_default()
    }

    /// Answer one of the workbook's effects: reading or writing a file is a
    /// job, a view changes what is shown once the grid syncs.
    fn answer(
        &mut self,
        effect: SheetsEffect,
        me: &Shared,
        cx: &mut EffectCx<Workbook>,
    ) -> EffectAnswer {
        use SheetsEffect as E;
        match effect {
            E::ImportCsv { path } => Ok(self.import(Interchange::Csv, path, me, cx)),
            E::ImportXlsx { path } => Ok(self.import(Interchange::Xlsx, path, me, cx)),
            E::ImportOds { path } => Ok(self.import(Interchange::Ods, path, me, cx)),
            E::ImportNotion { path } => Ok(self.import(Interchange::Notion, path, me, cx)),
            E::ExportCsv { path } => self.export(Interchange::Csv, path, me, cx),
            E::ExportXlsx { path } => self.export(Interchange::Xlsx, path, me, cx),
            E::ExportOds { path } => self.export(Interchange::Ods, path, me, cx),
            E::ReadCells { sheet, range } => {
                let workbook = cx.live().get();
                let id = workbook
                    .sheet_key(&Id::from_raw(sheet))
                    .map_err(|refusal| refusal.0)?;
                let snapshot = workbook
                    .snapshot_of(&id)
                    .ok_or_else(|| "no such sheet".to_owned())?;
                crate::cell_actions::read(&snapshot, range.as_deref()).map(|mut read| {
                    if let Some(fields) = read.as_object_mut() {
                        fields.insert("item".into(), id.as_str().into());
                    }
                    Effected::value(read)
                })
            }
            E::ImportXlsxContent { content, title } => {
                self.import_xlsx_content(&content, title, cx)
            }
            E::ExportXlsxContent => self.export_xlsx_content(&cx.live().get()),
            E::Commands => {
                self.ask(Ask::Commands);
                Ok(Effected::done())
            }
            view => {
                if let E::Open { sheet, .. } = &view {
                    let sheet = Id::from_raw(sheet.clone());
                    let shown = cx
                        .live()
                        .get()
                        .sheets
                        .iter()
                        .any(|(id, it)| *id == sheet && !it.hidden);
                    if !shown {
                        return Err("the sheet no longer exists".into());
                    }
                }
                self.ask(Ask::Show(view));
                Ok(Effected::done())
            }
        }
    }

    /// Import a spreadsheet file into new sheets: the one at `path`, or the
    /// one the person chooses. A file the workbook already holds is shown
    /// instead of imported again.
    fn import(
        &mut self,
        format: Interchange,
        path: Option<ui::tilo_resource::AbsolutePath>,
        me: &Shared,
        cx: &mut EffectCx<Workbook>,
    ) -> Effected {
        let me = Rc::clone(me);
        let Some(path) = path else {
            self.ask(Ask::Status(format!(
                "Choose a .{} file to import…",
                format.ext()
            )));
            return cx.choose_file(spreadsheet_import_request(format), move |(), chosen, cx| {
                let mut store = me.borrow_mut();
                let Some(file) = chosen else {
                    store.ask(Ask::Status("Import cancelled".into()));
                    return Ok(Effected::done());
                };
                if u64::try_from(file.bytes.len()).unwrap_or(u64::MAX) > MAX_IMPORT_FILE_BYTES {
                    return store.failed(format!(
                        "{} is larger than the {} import limit",
                        file.name,
                        ui::format::memory(MAX_IMPORT_FILE_BYTES)
                    ));
                }
                // The same file chosen again is shown, not imported twice.
                let source = file.identity.as_ref().map(ToString::to_string);
                if let Some(source) = &source
                    && store.show_imported(&cx.live().get(), source, &file.name)
                {
                    return Ok(Effected::done());
                }
                store.ask(Ask::Status(format!("Importing .{}…", format.ext())));
                drop(store);
                let name = file.name;
                let sheet = sheet_name_from_path(std::path::Path::new(&name));
                Ok(cx.thread(
                    move |_| SheetsApp::read_tables(format, &file.bytes, &sheet),
                    move |(), tables, cx| me.borrow_mut().finish_import(tables, &name, source, cx),
                ))
            });
        };
        let path = path.into_path_buf();
        let source = import_source(&path);
        if self.show_imported(&cx.live().get(), &source, &file_label(&path)) {
            return Effected::done();
        }
        self.ask(Ask::Status(format!("Importing .{}…", format.ext())));
        cx.thread(
            move |_| SheetsApp::read_tables_at(format, &path).map(|tables| (tables, path)),
            move |(), read, cx| {
                let mut store = me.borrow_mut();
                match read {
                    Ok((tables, path)) => {
                        store.finish_import(Ok(tables), &file_label(&path), Some(source), cx)
                    }
                    Err(error) => store.failed(error),
                }
            },
        )
    }

    /// Show the sheet the file `source` was already imported as, if any:
    /// whether there was one.
    fn show_imported(&mut self, workbook: &Workbook, source: &str, label: &str) -> bool {
        let open = workbook
            .sheets
            .iter()
            .find(|(_, sheet)| {
                sheet.source.as_ref().map(|source| source.0.as_str()) == Some(source)
            })
            .map(|(id, sheet)| (id.clone(), sheet.name.0.clone()));
        let Some((sheet_id, name)) = open else {
            return false;
        };
        self.ask(Ask::Reveal(PendingReveal {
            sheet_id: Some(sheet_id),
            addr: (0, 0),
            status: Some(format!("{label} is already open as {name}")),
            arriving: false,
        }));
        true
    }

    /// Add the sheets a file was read into as one action, and show the last
    /// of them once it is applied.
    fn finish_import(
        &mut self,
        tables: Result<Vec<crate::xlsx::SheetTable>, String>,
        label: &str,
        source: Option<String>,
        cx: &EffectCx<Workbook>,
    ) -> EffectAnswer {
        let built =
            tables.and_then(|tables| SheetsApp::import_action(tables, source, None, self.currency));
        let (invoke, total) = match built {
            Ok(built) => built,
            Err(error) => return self.failed(error),
        };
        cx.submit(invoke);
        let status = format!("Imported {total} cells from {label}");
        self.ask(Ask::Status(status.clone()));
        self.ask(Ask::Reveal(PendingReveal {
            sheet_id: None,
            addr: (0, 0),
            status: Some(status),
            arriving: true,
        }));
        Ok(Effected::done())
    }

    /// Write the workbook as a spreadsheet file (a CSV holds the active sheet
    /// only): to `path`, or where the person chooses. Encoding runs off the UI
    /// thread; the file is written only when the job was not cancelled.
    fn export(
        &mut self,
        format: Interchange,
        path: Option<ui::tilo_resource::AbsolutePath>,
        me: &Shared,
        cx: &mut EffectCx<Workbook>,
    ) -> EffectAnswer {
        let workbook = cx.live().get();
        let active = self
            .active_sheet
            .clone()
            .filter(|id| workbook.sheet(id).is_some());
        let sheets: Vec<SheetSnapshot> = match format {
            // A workbook exports as itself; there is no file to write a
            // Notion export into.
            Interchange::Notion => Vec::new(),
            Interchange::Csv => active
                .as_ref()
                .and_then(|id| workbook.snapshot_of(id))
                .into_iter()
                .collect(),
            Interchange::Xlsx | Interchange::Ods => workbook_snapshots(&workbook),
        };
        let Some(name) = active
            .as_ref()
            .and_then(|id| workbook.sheet(id))
            .map(|sheet| sheet.name.0.clone())
            .or_else(|| sheets.first().map(|sheet| sheet.name.clone()))
        else {
            return self.failed("there is no sheet to export".into());
        };
        self.ask(Ask::Status(format!("Exporting .{}…", format.ext())));
        let encode = move |_| SheetsApp::encode_tables(format, &sheets);
        let me = Rc::clone(me);
        let Some(path) = path else {
            let suggested = format!("{name}.{}", format.ext());
            return Ok(cx.thread(encode, move |(), bytes, cx| {
                let bytes = match bytes {
                    Ok(bytes) => bytes,
                    Err(error) => return me.borrow_mut().failed(error),
                };
                me.borrow_mut()
                    .ask(Ask::Status("Choose where to save…".into()));
                Ok(cx.save_file(
                    spreadsheet_export_request(format, &suggested),
                    bytes,
                    move |(), saved, _| {
                        me.borrow_mut().ask(Ask::Status(if saved {
                            format!("Exported .{}", format.ext())
                        } else {
                            "Export cancelled".into()
                        }));
                        Ok(Effected::done())
                    },
                ))
            }));
        };
        let path = path.into_path_buf();
        Ok(cx.thread(encode, move |(), bytes, _| {
            let written = bytes.and_then(|bytes| {
                crate::xlsx::write_file(&path, &bytes).map_err(|error| error.to_string())
            });
            let mut store = me.borrow_mut();
            match written {
                Ok(()) => {
                    store.ask(Ask::Status(format!(
                        "Exported .{} to {}",
                        format.ext(),
                        file_label(&path)
                    )));
                    Ok(Effected::done())
                }
                Err(error) => store.failed(error),
            }
        }))
    }

    /// Report a failed import or export, and fail its effect.
    pub(super) fn failed(&mut self, error: String) -> EffectAnswer {
        self.ask(Ask::Status(format!(
            "Spreadsheet operation failed: {error}"
        )));
        Err(error)
    }

    /// Add the sheets of the `.xlsx` workbook at `content` (a
    /// `tilo.content:<hash>` address) as one step, naming the workbook
    /// `title` when it has no name yet. Content imported before is not added
    /// again: the answer names the sheets it became.
    fn import_xlsx_content(
        &mut self,
        content: &str,
        title: Option<String>,
        cx: &EffectCx<Workbook>,
    ) -> EffectAnswer {
        let hash = ui::tilo_actions::content_hash(content.trim())
            // A plan that carried the content gives its bare hash.
            .or_else(|| {
                let bare = content.trim();
                (bare.len() == 64 && bare.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then_some(bare)
            })
            .ok_or_else(|| format!("`{content}` names no content: give `tilo.content:<hash>`"))?
            .to_ascii_lowercase();
        let source = format!("tilo.content:{hash}");
        let already: Vec<String> = cx
            .live()
            .get()
            .sheets
            .values()
            .filter(|sheet| sheet.source.as_ref().is_some_and(|it| it.0 == source))
            .map(|sheet| sheet.name.0.clone())
            .collect();
        if !already.is_empty() {
            return Ok(Effected::value(serde_json::json!({
                "sheets": already,
                "imported": false,
                "content": source,
            })));
        }
        let bytes = self
            .content_store()
            .read(&hash)
            .ok_or_else(|| format!("content `{hash}` is not on this device"))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_IMPORT_FILE_BYTES {
            return self.failed(format!(
                "the workbook is larger than the {} import limit",
                ui::format::memory(MAX_IMPORT_FILE_BYTES)
            ));
        }
        let tables = SheetsApp::read_tables(Interchange::Xlsx, &bytes, "")
            .map_err(|error| format!("content `{hash}` is not an Excel workbook: {error}"))?;
        let sheets: Vec<serde_json::Value> = tables
            .iter()
            .map(|table| serde_json::json!({ "name": table.name, "hidden": table.hidden }))
            .collect();
        let title = title
            .map(|title| title.trim().trim_end_matches(".xlsx").trim().to_owned())
            .filter(|title| !title.is_empty());
        let (invoke, total) =
            SheetsApp::import_action(tables, Some(source.clone()), title, self.currency)?;
        cx.submit(invoke);
        self.ask(Ask::Status(format!("Imported {total} cells")));
        Ok(Effected::value(serde_json::json!({
            "sheets": sheets,
            "cells": total,
            "imported": true,
            "content": source,
        })))
    }

    /// Keep every sheet as an `.xlsx` workbook in the content store and
    /// answer its address.
    fn export_xlsx_content(&mut self, workbook: &Workbook) -> EffectAnswer {
        let sheets = workbook_snapshots(workbook);
        let bytes = SheetsApp::encode_tables(Interchange::Xlsx, &sheets)?;
        let hash = self
            .content_store()
            .put(&bytes)
            .map_err(|error| format!("the workbook could not be kept: {error}"))?;
        Effected::content(&hash)
    }
}

/// Every sheet as the file formats take it, a sheet whose formulas read
/// others evaluated with the workbook.
fn workbook_snapshots(workbook: &Workbook) -> Vec<SheetSnapshot> {
    workbook
        .sheets
        .ids()
        .filter_map(|id| workbook.snapshot_of(id))
        .collect()
}

/// The view of the workbook: `app` follows the document, its [`Store`]
/// answers the effects, and the view binds the edits of the selection
/// beside it.
pub(crate) fn window(cx: &ViewCx<Workbook>, mut app: SheetsApp) -> Element {
    app.store.borrow_mut().attach_content(cx.content());
    if let Some(path) = cx.launch_arguments().first() {
        app.show_file_of(std::path::PathBuf::from(path));
    }
    app.attach_framework(cx.dispatcher(), cx.live().clone());
    app.attach_people(cx.people());
    app.follow_locale(cx.settings::<Locale>());
    // A search hit, or an agent's `open`, shows the sheet or cell it names.
    cx.on_open(app.opener());
    let store = Rc::clone(&app.store);
    {
        let answer = Rc::clone(&store);
        cx.effects(move |effect: SheetsEffect, cx| answer.borrow_mut().answer(effect, &answer, cx));
    }
    {
        let store = Rc::clone(&store);
        cx.on_stage(move |scenario| {
            store
                .borrow_mut()
                .ask(Ask::Stage(scenario.map(str::to_owned)));
        });
    }
    cx.bind_declarations(move |_| store.borrow().selection_declarations());
    Box::new(app)
}
