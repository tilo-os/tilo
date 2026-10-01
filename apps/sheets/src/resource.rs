//! Sheets as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. Mutations are [`SheetsAction`] applied through the framework
//! dispatcher; View/Effect ops come from [`Resource::local_operations`].

use std::rc::Rc;

use tilo_actions::{Action, ActionParam, ActionSpec, OperationKind, ParamType};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    SHEETS_ACTION_TYPE, SHEETS_OPERATION_TYPE, SearchableCell, SheetsAction, SheetsOperation,
    SheetsResourceCtx, add_sheet_action, document_children, encode,
};
use crate::app::SheetsApp;
use crate::doc::{ChartSnapshot, CommentThread, SheetsDoc};
use crate::model::{CellFormat, Currency, format_value};

/// Lightweight workbook projection for search and the published tree.
#[derive(Clone, Debug, Default)]
pub struct SheetsState {
    pub sheets: Vec<SheetInfo>,
}

#[derive(Clone, Debug)]
pub struct SheetInfo {
    pub id: String,
    pub name: String,
    /// Populated cells used by sheet-level actions (sort) at publish time.
    pub cells: Vec<SearchableCell>,
    pub charts: Vec<ChartSnapshot>,
    pub comments: Vec<CommentThread>,
}

impl SheetsState {
    /// Build from the live app / test presentation context when the document
    /// projection is empty (standalone fixtures that author sheets in `ctx`).
    pub(crate) fn from_ctx(ctx: &SheetsResourceCtx) -> Self {
        Self {
            sheets: ctx
                .searchable_sheets
                .iter()
                .map(|sheet| SheetInfo {
                    id: sheet.id.clone(),
                    name: sheet.name.clone(),
                    cells: sheet.cells.clone(),
                    charts: Vec::new(),
                    comments: Vec::new(),
                })
                .collect(),
        }
    }

    /// Prefer live searchable cells from `ctx` when the ids still match.
    pub(crate) fn with_ctx_cells(mut self, ctx: &SheetsResourceCtx) -> Self {
        for sheet in &mut self.sheets {
            if let Some(live) = ctx
                .searchable_sheets
                .iter()
                .find(|candidate| candidate.id == sheet.id)
            {
                sheet.cells = live.cells.clone();
                if !live.name.is_empty() {
                    sheet.name = live.name.clone();
                }
            }
        }
        self
    }
}

/// The sheets workbook resource.
pub struct SheetsWorkbook;

impl Resource for SheetsWorkbook {
    const KIND: &'static str = "sheets.workbook";
    const LABEL: &'static str = "Workbook";
    const ACTION_TYPE: &'static str = SHEETS_ACTION_TYPE;

    type State = SheetsState;
    type Action = SheetsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        crate::actions::register(registry);
    }

    fn read(doc: &Doc) -> Result<SheetsState, ModelError> {
        let mut projection = doc.clone();
        let count = SheetsDoc::sheet_count_readonly(doc);
        let names = SheetsDoc::sheet_names_readonly(doc);
        let mut sheets = Vec::with_capacity(count);
        for idx in 0..count {
            let id = SheetsDoc::sheet_resource_id(doc, idx)
                .unwrap_or_else(|| format!("sheets.sheet:{idx}"));
            let name = names
                .get(idx)
                .cloned()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| format!("Sheet{}", idx.saturating_add(1)));
            let snap = SheetsDoc::snapshot_sheet(&mut projection, idx, Currency::USD);
            let values = crate::formula::recalculate(&snap.raw);
            let mut cells = snap
                .raw
                .iter()
                .map(|(&(col, row), raw)| {
                    let fmt = snap
                        .fmt
                        .get(&(col, row))
                        .copied()
                        .unwrap_or(CellFormat::General);
                    let display = values
                        .get(&(col, row))
                        .map_or_else(String::new, |value| format_value(value, fmt));
                    SearchableCell {
                        col,
                        row,
                        raw: raw.clone(),
                        display,
                    }
                })
                .collect::<Vec<_>>();
            cells.sort_by_key(|cell| (cell.row, cell.col));
            sheets.push(SheetInfo {
                id,
                name,
                cells,
                charts: SheetsDoc::charts(&mut projection, idx),
                comments: SheetsDoc::comments(&mut projection, idx),
            });
        }
        Ok(SheetsState { sheets })
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        if SheetsDoc::is_empty(doc) {
            SheetsAction::AddSheet {
                name: "Sheet1".into(),
                new_sheet_id: uuid::Uuid::new_v4().to_string(),
            }
            .apply(doc)
            .map_err(|error| ModelError::invalid(error.to_string()))?;
        }
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // `new-sheet` needs the open sheet count for its default name — see
        // [`bound_actions`].
        Vec::new()
    }

    fn bound_actions(state: &SheetsState) -> Vec<ActionSpec> {
        let count = state.sheets.len();
        vec![
            ActionSpec::prompted(
                "new-sheet",
                "New sheet",
                "Name",
                "sheet name…",
                tilo_actions::ExecutionLocus::ClientLocal,
                move |name: &str| {
                    let name = name.trim();
                    let name = if name.is_empty() {
                        format!("Sheet{}", count + 1)
                    } else {
                        name.to_string()
                    };
                    encode(&add_sheet_action(&name))
                },
            )
            .reversible(),
        ]
    }

    fn children(state: &SheetsState) -> Vec<tilo_actions::Resource> {
        document_children(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        workbook_local_operations()
    }

    fn search(state: &SheetsState) -> Vec<SearchEntry> {
        state
            .sheets
            .iter()
            .map(|sheet| SearchEntry {
                anchor: Some(sheet.id.clone()),
                title: sheet.name.clone(),
                text: sheet.name.clone(),
            })
            .collect()
    }
}

/// Import/export Effects and workbook Views — no document delta.
fn workbook_local_operations() -> Vec<ActionSpec> {
    use tilo_actions::{EncodedAction, ExecutionMode, ResultSpec};

    let mut specs = Vec::new();
    for (id, label, kind, variant) in [
        ("import-csv", "Import CSV", OperationKind::Effect, 0_u8),
        (
            "import-xlsx",
            "Import Excel workbook",
            OperationKind::Effect,
            1,
        ),
        (
            "import-ods",
            "Import OpenDocument spreadsheet",
            OperationKind::Effect,
            2,
        ),
        ("export-csv", "Export CSV", OperationKind::Effect, 3),
        (
            "export-xlsx",
            "Export Excel workbook",
            OperationKind::Effect,
            4,
        ),
        (
            "export-ods",
            "Export OpenDocument spreadsheet",
            OperationKind::Effect,
            5,
        ),
        ("commands", "Open command palette", OperationKind::View, 6),
        ("go-to-cell", "Go to cell…", OperationKind::View, 7),
        (
            "toggle-formula-view",
            "Toggle formula view",
            OperationKind::View,
            8,
        ),
    ] {
        match variant {
            6 => specs.push(
                ActionSpec::operation(
                    id,
                    label,
                    kind,
                    SHEETS_OPERATION_TYPE,
                    &SheetsOperation::OpenCommandPalette,
                )
                .expect("SheetsOperation serializes"),
            ),
            7 => specs.push(ActionSpec::prompted_operation(
                id,
                label,
                "Cell reference",
                "e.g. B12",
                kind,
                SHEETS_OPERATION_TYPE,
                |reference| {
                    EncodedAction::encode_operation(
                        SHEETS_OPERATION_TYPE,
                        &SheetsOperation::GoToCell {
                            reference: reference.to_string(),
                        },
                    )
                    .expect("SheetsOperation serializes")
                },
            )),
            8 => specs.push(
                ActionSpec::operation(
                    id,
                    label,
                    kind,
                    SHEETS_OPERATION_TYPE,
                    &SheetsOperation::ToggleFormulaView,
                )
                .expect("SheetsOperation serializes"),
            ),
            _ => specs.push(
                ActionSpec::typed_operation(
                    id,
                    label,
                    "path (omit to use the system file picker)",
                    kind,
                    SHEETS_OPERATION_TYPE,
                    vec![ActionParam::optional_owned(
                        "path",
                        ParamType::Text,
                        "Direct spreadsheet path",
                    )],
                    move |args: &tilo_actions::ActionArgs| {
                        let path = args
                            .text("path")
                            .filter(|p| !p.trim().is_empty())
                            .map(str::to_string);
                        let value = match variant {
                            0 => SheetsOperation::ImportCsv { path },
                            1 => SheetsOperation::ImportXlsx { path },
                            2 => SheetsOperation::ImportOds { path },
                            3 => SheetsOperation::ExportCsv { path },
                            4 => SheetsOperation::ExportXlsx { path },
                            _ => SheetsOperation::ExportOds { path },
                        };
                        EncodedAction::encode_operation(SHEETS_OPERATION_TYPE, &value)
                            .expect("sheet operation serializes")
                    },
                )
                .execution(ExecutionMode::Background { cancellable: false })
                .returns(ResultSpec::Job),
            ),
        }
    }
    specs
}

/// Framework app: the sheets grid is the surface over [`SheetsWorkbook`].
pub struct SheetsShell;

impl ResourceApp for SheetsShell {
    type Resource = SheetsWorkbook;
    const ID: &'static str = "so.tilo.sheets";
    const TITLE: &'static str = "Sheets";
    const ICON: Option<&'static str> = Some("sheets");
    const SHOT_VIEWPORT: (u32, u32) = (1200, 800);

    fn view(cx: &mut ViewCx<SheetsWorkbook>) -> Element {
        cx.surface(SheetsApp::new())
    }
}

impl ResourceSurface<SheetsWorkbook> for SheetsApp {
    fn present(&mut self, state: &Rc<SheetsState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<SheetsWorkbook>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<ui::framework::Dispatcher<SheetsWorkbook>>,
    #[cfg(test)]
    pub(crate) last: Option<tilo_actions::EncodedAction>,
    /// Test-only: hosted lookup without a framework dispatcher.
    #[cfg(test)]
    hosted: bool,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            #[cfg(test)]
            last: None,
            #[cfg(test)]
            hosted: false,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: ui::framework::Dispatcher<SheetsWorkbook>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        #[cfg(test)]
        if self.hosted {
            return true;
        }
        self.framework.is_some()
    }

    /// Test-only: mark hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }

    pub(crate) fn try_dispatch_with_context(
        &mut self,
        doc: &mut automerge::AutoCommit,
        action: &SheetsAction,
        context: tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        match &self.framework {
            Some(dispatcher) => {
                #[cfg(test)]
                {
                    self.last = Some(encode(action));
                }
                dispatcher.apply_now_with(doc, action, context)
            }
            None => self
                .local
                .try_dispatch_with_context(doc, SHEETS_ACTION_TYPE, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    pub(crate) fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        #[cfg(test)]
        if self.is_hosted() {
            return self.last.as_ref();
        }
        self.local.last_dispatched()
    }
}
