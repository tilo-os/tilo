//! Sheets as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. Mutations are [`SheetsAction`] applied through the framework
//! dispatcher; View/Effect ops come from [`Resource::local_operations`].

use std::rc::Rc;

use tilo_actions::{Action, ActionParam, ActionSpec, OperationKind, ParamType};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{SHEETS_ACTION_TYPE, SHEETS_OPERATION_TYPE, SheetsAction, SheetsOperation};
use crate::app::SheetsApp;
use crate::doc::SheetsDoc;

/// Lightweight workbook projection for search and the published tree.
#[derive(Clone, Debug, Default)]
pub struct SheetsState {
    pub sheets: Vec<SheetInfo>,
}

#[derive(Clone, Debug)]
pub struct SheetInfo {
    pub id: String,
    pub name: String,
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
            sheets.push(SheetInfo { id, name });
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
        // `new-sheet` stays a prompted ActionSpec (param `value`, default
        // name from the open sheet count). A stub declaration here would
        // shadow that prompt.
        Vec::new()
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

/// Import/export Effects and the command-palette View — no document delta.
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
    ] {
        if variant == 6 {
            specs.push(
                ActionSpec::operation(
                    id,
                    label,
                    kind,
                    SHEETS_OPERATION_TYPE,
                    &SheetsOperation::OpenCommandPalette,
                )
                .expect("SheetsOperation serializes"),
            );
            continue;
        }
        specs.push(
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
        );
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
