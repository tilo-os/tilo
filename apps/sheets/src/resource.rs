//! Sheets as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path. Mutations are [`SheetsAction`] applied through the framework
//! dispatcher; file interchange and views are the surface's [`SheetsEffect`]s.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use automerge::ObjId;
use tilo_actions::{ActionParam, ParamType};
use ui::framework::{
    EffectAnswer, EffectCx, Fixture, ResourceApp, ResourceSurface, SurfaceCx, ViewCx,
};
use ui::tilo_resource::{ActionDecl, Delta, Doc, Invoke, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    SHEETS_ACTION_TYPE, SearchableCell, SheetsAction, SheetsEffect, add_sheet_action,
    document_children, set_workbook_title_action,
};
use crate::app::SheetsApp;
use crate::doc::{ChartSnapshot, CommentThread, SheetSnapshot, SheetsDoc};
use crate::model::{CellFormat, format_value};
use tilo_locale::Currency;

/// The currency a stored currency format without its own reads as.
const READ_CURRENCY: Currency = Currency::USD;

/// The workbook as stored: its sheets in order, each with its cells, charts
/// and comments. A change re-reads only the sheets it touched, and of those
/// only the cells it touched ([`Resource::refresh`]).
#[derive(Clone, Debug, Default)]
pub struct SheetsState {
    pub sheets: Vec<Arc<SheetInfo>>,
    /// The workbook's title, which a file it was imported from gave it.
    pub title: Option<String>,
}

/// One sheet. What its formulas compute is worked out the first time
/// something asks ([`Self::cells`]) and kept until the sheet changes, so
/// reading the workbook never recalculates it.
#[derive(Debug)]
pub struct SheetInfo {
    pub id: String,
    pub name: String,
    /// The file the sheet was imported from, which opening again reveals.
    pub source: Option<String>,
    /// Whether the sheet is hidden: no tab shows it, but its cells are kept
    /// and the formulas reading them work.
    pub hidden: bool,
    /// The stored cells: raw inputs, formats and bold.
    pub snapshot: SheetSnapshot,
    pub charts: Vec<ChartSnapshot>,
    pub comments: Vec<CommentThread>,
    /// The sheet's map, which a change names when it touches the sheet.
    object: ObjId,
    /// Whether a formula of the sheet reads another sheet, so what it
    /// computes is worked out with the whole workbook.
    crosses: bool,
    computed: OnceLock<Vec<SearchableCell>>,
}

impl SheetInfo {
    fn read(doc: &Doc, idx: usize, object: ObjId) -> Self {
        let snapshot = SheetsDoc::snapshot_sheet(doc, idx, READ_CURRENCY);
        Self::with_snapshot(doc, idx, object, snapshot)
    }

    fn with_snapshot(doc: &Doc, idx: usize, object: ObjId, snapshot: SheetSnapshot) -> Self {
        let id = SheetsDoc::sheet_resource_id(doc, idx).unwrap_or_default();
        let name = if snapshot.name.trim().is_empty() {
            format!("Sheet{}", idx.saturating_add(1))
        } else {
            snapshot.name.clone()
        };
        Self {
            id,
            name,
            source: SheetsDoc::sheet_source(doc, idx),
            hidden: SheetsDoc::sheet_hidden(doc, idx),
            charts: SheetsDoc::charts(doc, idx),
            comments: SheetsDoc::comments(doc, idx),
            crosses: snapshot
                .raw
                .values()
                .any(|raw| crate::formula::references_other_sheets(raw)),
            snapshot,
            object,
            computed: OnceLock::new(),
        }
    }

    /// This sheet with its cells showing `values`, what it computes in its
    /// workbook.
    fn in_book(&self, values: &HashMap<crate::model::Addr, crate::model::CellValue>) -> Self {
        let computed = OnceLock::new();
        let _ = computed.set(self.cells_from(values));
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            source: self.source.clone(),
            hidden: self.hidden,
            snapshot: self.snapshot.clone(),
            charts: self.charts.clone(),
            comments: self.comments.clone(),
            object: self.object.clone(),
            crosses: self.crosses,
            computed,
        }
    }

    /// Re-read what `delta` changed in this sheet, now at `idx`: its cells
    /// key by key, its name, charts and comments.
    fn refreshed(&self, doc: &Doc, idx: usize, delta: &Delta) -> Self {
        let snapshot = match SheetsDoc::cells_object(doc, &self.object) {
            Some(cells) if !delta.under(&self.object).is_all() => {
                let touched = delta.under(&cells);
                if touched.is_all() {
                    SheetsDoc::snapshot_sheet(doc, idx, READ_CURRENCY)
                } else {
                    let mut snapshot = self.snapshot.clone();
                    snapshot.name = SheetsDoc::sheet_names(doc)
                        .get(idx)
                        .cloned()
                        .unwrap_or_default();
                    for key in touched.keys() {
                        SheetsDoc::read_cell_into(doc, &cells, key, &mut snapshot, READ_CURRENCY);
                    }
                    snapshot
                }
            }
            _ => SheetsDoc::snapshot_sheet(doc, idx, READ_CURRENCY),
        };
        Self::with_snapshot(doc, idx, self.object.clone(), snapshot)
    }

    /// Every populated cell with what it shows, row by row: formulas are
    /// calculated on the first call and kept until the sheet changes.
    pub fn cells(&self) -> &[SearchableCell] {
        self.computed
            .get_or_init(|| self.cells_from(&crate::formula::recalculate(&self.snapshot.raw)))
    }

    /// Every populated cell, row by row, showing `values`.
    fn cells_from(
        &self,
        values: &HashMap<crate::model::Addr, crate::model::CellValue>,
    ) -> Vec<SearchableCell> {
        {
            let mut cells = self
                .snapshot
                .raw
                .iter()
                .map(|(&(col, row), raw)| {
                    // A formula without a format of its own shows in its
                    // operands' (a sum of amounts is an amount).
                    let fmt = self
                        .snapshot
                        .fmt
                        .get(&(col, row))
                        .copied()
                        .unwrap_or_else(|| {
                            raw.strip_prefix('=')
                                .and_then(|body| {
                                    crate::formula::infer_result_format(body, &self.snapshot.fmt)
                                })
                                .unwrap_or(CellFormat::General)
                        });
                    let value = values.get(&(col, row));
                    let display = value.map_or_else(String::new, |value| format_value(value, fmt));
                    let value = value.cloned().unwrap_or(crate::model::CellValue::Empty);
                    SearchableCell {
                        col,
                        row,
                        raw: raw.clone(),
                        display,
                        value,
                    }
                })
                .collect::<Vec<_>>();
            cells.sort_by_key(|cell| (cell.row, cell.col));
            cells
        }
    }
}

impl SheetsState {
    /// Work out the sheets whose formulas read other sheets with the whole
    /// workbook, so what they show follows those sheets; the others are
    /// worked out alone when first asked.
    fn link_book(&mut self) {
        if !self.sheets.iter().any(|sheet| sheet.crosses) {
            return;
        }
        let linked: Vec<(usize, SheetInfo)> = {
            let book: Vec<crate::formula::BookSheet<'_, _>> = self
                .sheets
                .iter()
                .map(|sheet| (sheet.name.as_str(), &sheet.snapshot.raw))
                .collect();
            self.sheets
                .iter()
                .enumerate()
                .filter(|(_, sheet)| sheet.crosses)
                .map(|(idx, sheet)| {
                    let values = crate::formula::recalculate_in_book(book.clone(), idx);
                    (idx, sheet.in_book(&values))
                })
                .collect()
        };
        for (idx, sheet) in linked {
            if let Some(slot) = self.sheets.get_mut(idx) {
                *slot = Arc::new(sheet);
            }
        }
    }
}

/// The sheets workbook resource.
pub struct SheetsWorkbook;

/// The workbook's import of an `.xlsx` kept as content: an attachment, a
/// shared file, an export.
fn import_xlsx_content() -> ActionDecl<SheetsWorkbook> {
    ActionDecl::new(
        "import-xlsx-content",
        "Import Excel workbook from content",
        vec![
            ActionParam::required(
                "content",
                ParamType::Text,
                "The workbook's content address, `tilo.content:<hash>` (an attachment, a \
                 shared file, an export)",
            ),
            ActionParam::optional(
                "title",
                ParamType::Text,
                "The workbook's title, e.g. the file's name, when it has none yet",
            ),
        ],
        |args, _| {
            let address = args.text("content").map_or("", str::trim);
            let hash = tilo_actions::content_hash(address)
                .unwrap_or(address)
                .to_ascii_lowercase();
            if hash.len() != 64 || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!(
                    "`{address}` names no content: give `tilo.content:<hash>`"
                ));
            }
            let content = ui::tilo_resource::content::current()
                .ok_or("this window keeps no content to read the workbook from")?;
            let bytes = content
                .read(&hash)
                .ok_or_else(|| format!("content `{hash}` is not available on this machine"))?;
            let tables = crate::xlsx::parse_xlsx(&bytes)
                .map_err(|error| format!("content `{hash}` is not an Excel workbook: {error}"))?;
            let (mut action, _) = crate::app::imported_sheets(tables, Currency::USD)?;
            if let SheetsAction::ImportSheets {
                replace_blank,
                source,
                title,
                ..
            } = &mut action
            {
                *replace_blank = true;
                *source = Some(format!("tilo.content:{hash}"));
                *title = args
                    .text("title")
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .map(|title| title.trim_end_matches(".xlsx").to_owned());
            }
            Ok(action)
        },
    )
    .describe(
        "Add every sheet of an .xlsx workbook kept as content, formulas, formats and \
         hidden sheets included; a new workbook's blank sheet gives way to them.",
    )
}

/// The workbook's new sheet, appended, with its first content when given;
/// `count` is how many sheets the workbook has, `titled` whether it has a
/// title already.
fn new_sheet(count: usize, titled: bool) -> ActionDecl<SheetsWorkbook> {
    ActionDecl::new(
        "new-sheet",
        "New sheet",
        vec![
            ActionParam::optional(
                "name",
                ParamType::Text,
                "The new sheet's name; omit for the next free SheetN",
            ),
            ActionParam::optional(
                "rows",
                ParamType::Array,
                "Its first content: rows of values from A1, \"=…\" a formula, an ISO date such as \
                 \"2026-10-22\" a real date",
            ),
            ActionParam::optional(
                "formats",
                ParamType::Object,
                "Number formats of its ranges, e.g. {\"D2:D5\": \"percent\"}: general, number, \
                 currency, percent, date, time or text",
            ),
            ActionParam::optional(
                "workbook",
                ParamType::Text,
                "The spreadsheet's name, when the workbook has none yet",
            ),
        ],
        move |args, _| {
            let name = args.text("name").map_or("", str::trim);
            let name = if name.is_empty() {
                format!("Sheet{}", count + 1)
            } else {
                name.to_owned()
            };
            // A workbook already named keeps its name.
            let title = args
                .text("workbook")
                .map(str::trim)
                .filter(|title| !titled && !title.is_empty())
                .map(str::to_owned);
            let rows = args.array("rows");
            if rows.is_none() && title.is_none() {
                return Ok(add_sheet_action(&name));
            }
            // A sheet and its content in one change, as an import makes
            // them; a new workbook's blank first sheet gives way to it, and
            // the workbook takes the name it is given in the same change.
            let mut cells = match rows {
                Some(rows) => crate::cell_actions::cells_from_rows((0, 0), rows)?,
                None => Vec::new(),
            };
            if let Some(formats) = args.object("formats") {
                cells = crate::cell_actions::with_formats(cells, formats)?;
            }
            Ok(SheetsAction::ImportSheets {
                sheets: vec![crate::actions::ImportedSheet {
                    name,
                    sheet_id: uuid::Uuid::new_v4().to_string(),
                    cells,
                    hidden: false,
                }],
                replace_blank: true,
                source: None,
                title,
            })
        },
    )
    .describe(
        "Add a sheet at the end of the workbook, with its content when given; naming the \
         workbook too when it has no name yet.",
    )
}

/// The workbook's naming: the spreadsheet's name, as its label shows it.
fn set_title() -> ActionDecl<SheetsWorkbook> {
    ActionDecl::new(
        "set-title",
        "Name the workbook",
        vec![ActionParam::required(
            "title",
            ParamType::Text,
            "The spreadsheet's name, as people call it",
        )],
        |args, _| {
            let title = args.text("title").map_or("", str::trim);
            if title.is_empty() {
                return Err("give the workbook a name: `title` is empty".into());
            }
            Ok(set_workbook_title_action(title))
        },
    )
    .describe("Name the workbook, so it is found and listed by that name.")
}

/// The identity of a new workbook's first sheet. The genesis is the same
/// change on every device, so two people creating the room's workbook before
/// they sync converge on one first sheet rather than two.
const FIRST_SHEET_ID: &str = "00000000-0000-4000-8000-000000000001";

/// The sheet maps of the workbook, in order.
fn sheet_objects(doc: &Doc) -> Vec<ObjId> {
    (0..SheetsDoc::sheet_count(doc))
        .filter_map(|idx| SheetsDoc::sheet_object(doc, idx))
        .collect()
}

impl Resource for SheetsWorkbook {
    const KIND: &'static str = "sheets.workbook";
    const LABEL: &'static str = "Workbook";
    const ACTION_TYPE: &'static str = SHEETS_ACTION_TYPE;

    type State = SheetsState;
    type Action = SheetsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        crate::actions::register(registry);
    }

    fn least_permission() -> ui::tilo_resource::PermissionRequirement {
        ui::tilo_resource::PermissionRequirement::Comment
    }

    /// Commenting on a cell — a thread, a reply, an edit of one's comment,
    /// resolving it — needs only the right to comment.
    fn permission(action: &SheetsAction) -> ui::tilo_resource::PermissionRequirement {
        match action {
            SheetsAction::PutComment { .. }
            | SheetsAction::ReplyComment { .. }
            | SheetsAction::EditComment { .. }
            | SheetsAction::SetCommentResolved { .. } => {
                ui::tilo_resource::PermissionRequirement::Comment
            }
            _ => ui::tilo_resource::PermissionRequirement::Write,
        }
    }

    fn read(doc: &Doc) -> Result<SheetsState, ModelError> {
        let mut state = SheetsState {
            sheets: sheet_objects(doc)
                .into_iter()
                .enumerate()
                .map(|(idx, object)| Arc::new(SheetInfo::read(doc, idx, object)))
                .collect(),
            title: SheetsDoc::workbook_title(doc),
        };
        state.link_book();
        Ok(state)
    }

    fn refresh(state: &mut SheetsState, doc: &Doc, delta: &Delta) -> Result<(), ModelError> {
        if delta.is_empty() {
            return Ok(());
        }
        if delta.is_everything() {
            *state = Self::read(doc)?;
            return Ok(());
        }
        // A sheet nothing touched is kept as it was, wherever it moved; a
        // touched one re-reads what changed in it.
        let sheets = sheet_objects(doc)
            .into_iter()
            .enumerate()
            .map(
                |(idx, object)| match state.sheets.iter().find(|sheet| sheet.object == object) {
                    Some(sheet) if !delta.touches(&object) => Arc::clone(sheet),
                    Some(sheet) => Arc::new(sheet.refreshed(doc, idx, delta)),
                    None => Arc::new(SheetInfo::read(doc, idx, object)),
                },
            )
            .collect();
        state.sheets = sheets;
        state.title = SheetsDoc::workbook_title(doc);
        // A change to any sheet may change what another's formulas read.
        state.link_book();
        Ok(())
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        if SheetsDoc::is_empty(doc) {
            tilo_actions::Action::apply(
                &SheetsAction::AddSheet {
                    name: "Sheet1".into(),
                    new_sheet_id: FIRST_SHEET_ID.into(),
                },
                doc,
            )
            .map_err(|error| ModelError::invalid(error.to_string()))?;
        }
        Ok(())
    }

    fn bound_declarations(state: &SheetsState) -> Vec<ActionDecl<Self>> {
        vec![
            new_sheet(state.sheets.len(), state.title.is_some()),
            set_title(),
            import_xlsx_content(),
        ]
    }

    /// Each sheet's cell actions, naming the sheet by a `sheet` argument.
    fn bound_actions(state: &SheetsState) -> Vec<tilo_actions::ActionSpec> {
        crate::cell_actions::workbook_actions(state)
    }

    fn children(state: &SheetsState) -> Vec<tilo_actions::Resource> {
        document_children(state)
    }

    /// A cell is not published with its sheet (a sheet of a hundred thousand
    /// rows would make the tree as large); its address resolves here, while
    /// its sheet exists and it is inside the grid.
    fn child(state: &SheetsState, id: &str) -> Option<tilo_actions::Resource> {
        let (sheet_id, col, row) = crate::actions::parse_cell_resource_id(id)?;
        state
            .sheets
            .iter()
            .any(|sheet| sheet.id == sheet_id)
            .then(|| crate::actions::open_cell_resource(sheet_id, col, row))
    }

    /// Every sheet by name and every populated cell by reference, input and
    /// what it shows; a hit opens it.
    fn search(state: &SheetsState) -> Vec<SearchEntry> {
        let mut entries = Vec::new();
        for sheet in &state.sheets {
            if sheet.id.is_empty() {
                continue;
            }
            entries.push(
                SearchEntry::new(sheet.name.clone(), sheet.name.clone())
                    .anchored(sheet.id.clone())
                    .invoking(crate::actions::open_sheet_address(&sheet.id)),
            );
            for cell in sheet.cells() {
                let reference = crate::model::addr_to_a1(cell.col, cell.row);
                let title = format!("{}!{reference}", sheet.name);
                let text = format!("{reference} {title} {} {}", cell.raw, cell.display);
                entries.push(
                    SearchEntry::new(title, text)
                        .anchored(crate::actions::cell_resource_id(
                            &sheet.id, cell.col, cell.row,
                        ))
                        .invoking(crate::actions::open_cell_address(
                            &sheet.id, cell.col, cell.row,
                        )),
                );
            }
        }
        entries
    }
}

/// Framework app: the sheets grid is the surface over [`SheetsWorkbook`].
pub struct SheetsShell;

/// Every screenshot scenario: the sample workbook with one surface of the app
/// in view.
const SCENARIOS: &[&str] = &[
    "file",
    "format",
    "more",
    "docsettings",
    "editing",
    "pointmode",
    "summary",
    "multiselect",
    "tabmenu",
    "tabcolour",
    "tabrename",
    "fbarselect",
    "commandpalette",
    "fillseries",
    "scrollbars",
    "pasteformula",
    "error",
    "conflict",
    "reducedmotion",
    "charts",
    "comments",
    "bold",
];

/// When the sample workbook of the annotated scenarios was written.
const SAMPLE_WRITTEN_MS: i64 = 1_767_225_600_000;

/// Whether `scenario` shows the sample's charts and comments.
fn annotated(scenario: Option<&str>) -> bool {
    matches!(scenario, Some("charts" | "comments"))
}

impl ResourceApp for SheetsShell {
    /// A workbook is named by its title, else by its first shown sheet when
    /// that has a name of its own rather than `SheetN`.
    fn label(state: &SheetsState) -> String {
        let own = |name: &str| {
            let name = name.trim();
            !name.is_empty()
                && !name
                    .strip_prefix("Sheet")
                    .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
        };
        state
            .title
            .clone()
            .or_else(|| {
                state
                    .sheets
                    .iter()
                    .find(|sheet| !sheet.hidden)
                    .map(|sheet| sheet.name.clone())
                    .filter(|name| own(name))
            })
            .unwrap_or_else(|| <SheetsWorkbook as Resource>::LABEL.to_owned())
    }

    type Resource = SheetsWorkbook;
    const ID: &'static str = "so.tilo.sheets";
    const TITLE: &'static str = "Sheets";
    const ICON: Option<&'static str> = Some("sheets");

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    /// The annotated scenarios show a workbook other people wrote: Alex
    /// shared the budget and commented on it, Sam replied.
    fn fixture(fixture: &mut Fixture<SheetsWorkbook>) {
        if !annotated(fixture.scenario()) {
            return;
        }
        let versions =
            std::iter::once(("Alex", crate::sample::workbook())).chain(crate::sample::comments());
        for (at, (author, action)) in (0_i64..).zip(versions) {
            if let Ok(invoke) = Invoke::apply(&action) {
                fixture.version(author, SAMPLE_WRITTEN_MS + at * 60_000, invoke);
            }
        }
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<SheetsWorkbook>> {
        let actions = if annotated(name) {
            let mut actions = crate::sample::charts();
            actions.push(crate::sample::resolve_comment());
            actions
        } else {
            let mut actions = vec![crate::sample::workbook()];
            if name == Some("fillseries") {
                actions.push(crate::sample::fill_seeds());
            }
            actions
        };
        actions
            .iter()
            .filter_map(|action| Invoke::apply(action).ok())
            .collect()
    }

    fn view(cx: &mut ViewCx<SheetsWorkbook>) -> Element {
        cx.surface(SheetsApp::new())
    }
}

impl ResourceSurface<SheetsWorkbook, SheetsEffect> for SheetsApp {
    fn effect(
        &mut self,
        effect: SheetsEffect,
        cx: &mut EffectCx<SheetsWorkbook, Self>,
    ) -> EffectAnswer {
        self.answer_effect(effect, cx)
    }

    fn present(&mut self, state: &Rc<SheetsState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<SheetsWorkbook>) {
        self.attach_framework(cx.dispatcher());
        self.follow_locale(cx.settings::<tilo_locale::Locale>());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn bound_declarations(&self) -> Vec<ActionDecl<SheetsWorkbook>> {
        self.selection_declarations()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::framework::testing::TestApp;

    /// A new workbook is one empty first sheet; nothing is fabricated.
    #[test]
    fn a_new_workbook_opens_one_blank_sheet() {
        let app = TestApp::<SheetsShell>::new().unwrap();
        let state = app.state();
        let names: Vec<&str> = state
            .sheets
            .iter()
            .map(|sheet| sheet.name.as_str())
            .collect();
        assert_eq!(names, ["Sheet1"]);
        assert!(state.sheets[0].snapshot.raw.is_empty());
        assert_eq!(state.sheets[0].id, FIRST_SHEET_ID);
    }

    /// A cell edit on the workbook names its sheet and answers with the
    /// range it changed, as the sheet's own edit does.
    #[test]
    fn a_workbook_cell_edit_answers_the_range_it_changed() {
        let mut app = TestApp::<SheetsShell>::new().unwrap();
        let answer = app
            .call(
                "set-cells",
                serde_json::json!({"sheet": "sheet1", "start": "B2", "rows": [[2, "=B2*3"]]}),
            )
            .unwrap()
            .expect("the edit answers");
        assert_eq!(answer["range"], "B2:C2");
        assert_eq!(answer["rows"], serde_json::json!([[2.0, 6.0]]));
        let error = app
            .call(
                "set-cells",
                serde_json::json!({"sheet": "Budget", "start": "A1", "rows": [[1]]}),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("the sheets are `Sheet1`"), "{error}");
    }

    /// Screenshots show the sample workbook, staged through its actions.
    #[test]
    fn a_capture_stages_the_sample_workbook() {
        let plain = TestApp::<SheetsShell>::capture(None).unwrap();
        let state = plain.state();
        let names: Vec<&str> = state
            .sheets
            .iter()
            .map(|sheet| sheet.name.as_str())
            .collect();
        assert_eq!(names, ["Budget", "Sales"]);
        assert!(state.sheets[0].charts.is_empty());

        let charts = TestApp::<SheetsShell>::capture(Some("charts")).unwrap();
        let budget = &charts.state().sheets[0];
        assert_eq!(budget.charts.len(), 2);
        assert_eq!(budget.comments.len(), 1);
        assert!(budget.comments[0].resolved);
        assert_eq!(budget.comments[0].author, "Alex", "signed by who wrote it");
        assert_eq!(budget.comments[0].replies[0].author, "Sam");
    }

    /// A comment is signed by whoever invokes it, never by a name its
    /// payload carries.
    #[test]
    fn a_comment_is_signed_by_its_invoker() {
        let mut app = TestApp::<SheetsShell>::new().unwrap();
        let sheet_id = app.state().sheets[0].id.clone();
        app.invoke(
            Invoke::apply(&crate::actions::put_comment_action(
                0,
                sheet_id,
                (0, 0),
                (0, 0),
                "Looks right",
            ))
            .unwrap(),
        )
        .unwrap();
        let comments = &app.state().sheets[0].comments;
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "test-author");
    }

    /// The fill-series scenario extends its seeds through the real fill
    /// handle, and the paste scenario adjusts the copied formula: both are
    /// edits the capture's document holds.
    #[test]
    fn document_scenarios_edit_through_the_dispatcher() {
        let fill = TestApp::<SheetsShell>::capture(Some("fillseries")).unwrap();
        let raw = |app: &TestApp<SheetsShell>, a1: &str| {
            let addr = crate::model::a1_to_addr(a1).unwrap();
            app.state().sheets[0].snapshot.raw.get(&addr).cloned()
        };
        assert_eq!(raw(&fill, "B21").as_deref(), Some("6"));
        assert_eq!(raw(&fill, "C21").as_deref(), Some("Sat"));
        assert_eq!(raw(&fill, "D21").as_deref(), Some("Task 6"));

        let _clipboard = crate::app::tests::CLIP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let paste = TestApp::<SheetsShell>::capture(Some("pasteformula")).unwrap();
        // E2's `=SUM(B2:D2)` pasted one row down reads its own row.
        assert_eq!(raw(&paste, "E3").as_deref(), Some("=SUM(B3:D3)"));
    }

    /// A change re-reads what it touched: an edit to one cell keeps every
    /// other sheet as it was read, and refreshing arrives where reading the
    /// whole workbook again would.
    #[test]
    fn a_change_refreshes_only_what_it_touched() {
        use tilo_actions::Action as _;

        let mut doc = Doc::new();
        crate::sample::write(&mut doc);
        let mut state = SheetsWorkbook::read(&doc).unwrap();
        let sales = Arc::clone(&state.sheets[1]);
        let _ = state.sheets[0].cells();

        let mut tracker = ui::tilo_resource::Tracker::new();
        tracker.start(&mut doc);
        SheetsAction::SetCell {
            sheet: 0,
            sheet_id: crate::sample::BUDGET_SHEET_ID.into(),
            col: 1,
            row: 1,
            raw: "1300".into(),
            fmt: None,
        }
        .apply(&mut doc)
        .unwrap();
        SheetsAction::RenameSheet {
            idx: 0,
            sheet_id: crate::sample::BUDGET_SHEET_ID.into(),
            name: "Plan".into(),
        }
        .apply(&mut doc)
        .unwrap();
        let delta = tracker.delta(&mut doc);
        SheetsWorkbook::refresh(&mut state, &doc, &delta).unwrap();

        assert!(
            Arc::ptr_eq(&state.sheets[1], &sales),
            "Sales was not touched"
        );
        let budget = &state.sheets[0];
        assert_eq!(budget.name, "Plan");
        let total = budget
            .cells()
            .iter()
            .find(|cell| (cell.col, cell.row) == (4, 1))
            .unwrap();
        assert_eq!(total.display, "$3,700.00", "the edit is recalculated");

        let fresh = SheetsWorkbook::read(&doc).unwrap();
        for (refreshed, read) in state.sheets.iter().zip(&fresh.sheets) {
            assert_eq!(refreshed.id, read.id);
            assert_eq!(refreshed.name, read.name);
            assert_eq!(refreshed.snapshot.raw, read.snapshot.raw);
            assert_eq!(refreshed.snapshot.fmt, read.snapshot.fmt);
        }
    }

    /// Search finds sheets and cells by what they hold and what they show,
    /// and a hit opens the cell through the workbook's own tree.
    #[test]
    fn search_hits_open_cells_the_workbook_resolves() {
        let mut doc = Doc::new();
        crate::sample::write(&mut doc);
        let state = SheetsWorkbook::read(&doc).unwrap();
        let entries = SheetsWorkbook::search(&state);
        let rent = entries
            .iter()
            .find(|entry| entry.title == "Budget!A2")
            .expect("a cell hit");
        assert!(rent.text.contains("Rent"));
        let total = entries
            .iter()
            .find(|entry| entry.title == "Budget!E2")
            .expect("a formula hit");
        assert!(total.text.contains("=SUM(B2:D2)") && total.text.contains("$3,600.00"));
        let open = rent.action.clone().expect("a hit opens its cell");
        let cell = SheetsWorkbook::child(&state, &open.resource).expect("the cell resolves");
        assert!(tilo_actions::door::resolve(&[cell], &open).is_some());
        assert!(entries.iter().any(|entry| entry.title == "Sales"));
    }

    /// A formula reads another sheet of the workbook by name, and what an
    /// agent's edit answers follows that sheet.
    #[test]
    fn a_formula_reads_another_sheet() {
        let mut app = TestApp::<SheetsShell>::new().unwrap();
        // The first sheet with content takes the blank sheet's place.
        app.call(
            "new-sheet",
            serde_json::json!({"name": "Summary", "rows": [[0]]}),
        )
        .unwrap();
        app.call(
            "new-sheet",
            serde_json::json!({"name": "Rates", "rows": [["EUR", 1.08], ["GBP", 1.27]]}),
        )
        .unwrap();
        let answer = app
            .call(
                "set-cells",
                serde_json::json!({"sheet": "Summary", "start": "A1", "rows": [
                    ["=Rates!B2*100", "=VLOOKUP(\"EUR\",Rates!A1:B2,2,0)", "=SUM('Rates'!B1:B2)"],
                ]}),
            )
            .unwrap()
            .expect("the edit answers");
        assert_eq!(answer["rows"], serde_json::json!([[127.0, 1.08, 2.35]]));
        let state = app.state();
        let first = state
            .sheets
            .iter()
            .find(|sheet| sheet.name == "Summary")
            .unwrap();
        assert_eq!(first.cells()[0].display, "127");
    }

    /// A hidden sheet stays in the workbook and in its formulas; the last
    /// shown sheet cannot be hidden, and the workbook is named by its first
    /// shown sheet when that has a name of its own.
    #[test]
    fn a_sheet_hides_and_shows_again() {
        let mut app = TestApp::<SheetsShell>::new().unwrap();
        app.call(
            "new-sheet",
            serde_json::json!({"name": "Lookups", "rows": [[3]]}),
        )
        .unwrap();
        app.call("new-sheet", serde_json::json!({"name": "Summary"}))
            .unwrap();
        app.call(
            "set-cells",
            serde_json::json!({"sheet": "Summary", "start": "A1", "rows": [["=Lookups!A1*2"]]}),
        )
        .unwrap();
        app.call(
            "set-hidden",
            serde_json::json!({"sheet": "Lookups", "hidden": true}),
        )
        .unwrap();
        let state = app.state();
        assert!(
            state
                .sheets
                .iter()
                .any(|sheet| sheet.name == "Lookups" && sheet.hidden)
        );
        assert_eq!(
            SheetsShell::label(&state),
            "Summary",
            "the first shown sheet names it"
        );
        let error = app
            .call(
                "set-hidden",
                serde_json::json!({"sheet": "Summary", "hidden": true}),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("at least one sheet shown"), "{error}");
        app.call(
            "set-hidden",
            serde_json::json!({"sheet": "Lookups", "hidden": false}),
        )
        .unwrap();
        assert!(app.state().sheets.iter().all(|sheet| !sheet.hidden));
    }

    /// An agent names the workbook, and it is called by that name; an empty
    /// name is refused, by the declaration and by the action itself.
    #[test]
    fn a_workbook_is_named() {
        use tilo_actions::Action as _;

        let mut app = TestApp::<SheetsShell>::new().unwrap();
        app.call("set-title", serde_json::json!({"title": "Lumo pricing"}))
            .unwrap();
        assert_eq!(SheetsShell::label(&app.state()), "Lumo pricing");
        app.call("set-title", serde_json::json!({"title": "  Lumo 2027  "}))
            .unwrap();
        assert_eq!(SheetsShell::label(&app.state()), "Lumo 2027", "renamed");
        let error = app
            .call("set-title", serde_json::json!({"title": "   "}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("empty"), "{error}");
        assert_eq!(SheetsShell::label(&app.state()), "Lumo 2027");

        let mut doc = Doc::new();
        SheetsWorkbook::initialize(&mut doc).unwrap();
        for title in [" ", &"x".repeat(5 * 1024)] {
            assert!(
                crate::actions::set_workbook_title_action(title)
                    .apply(&mut doc)
                    .is_err()
            );
        }
        assert_eq!(SheetsDoc::workbook_title(&doc), None);
    }

    /// A new sheet can name a workbook that has no name yet, taking the
    /// blank first sheet's place, with or without content; a named workbook
    /// keeps its name.
    #[test]
    fn a_new_sheet_names_its_workbook() {
        let mut app = TestApp::<SheetsShell>::new().unwrap();
        app.call(
            "new-sheet",
            serde_json::json!({"name": "Tiers", "workbook": "Lumo pricing"}),
        )
        .unwrap();
        let state = app.state();
        let names: Vec<&str> = state
            .sheets
            .iter()
            .map(|sheet| sheet.name.as_str())
            .collect();
        assert_eq!(names, ["Tiers"], "the blank sheet gave way");
        assert_eq!(SheetsShell::label(&state), "Lumo pricing");
        app.call(
            "new-sheet",
            serde_json::json!({"name": "Plans", "workbook": "Other"}),
        )
        .unwrap();
        let state = app.state();
        let names: Vec<&str> = state
            .sheets
            .iter()
            .map(|sheet| sheet.name.as_str())
            .collect();
        assert_eq!(names, ["Tiers", "Plans"], "the named sheet stays");
        assert_eq!(SheetsShell::label(&state), "Lumo pricing", "kept its name");

        let mut app = TestApp::<SheetsShell>::new().unwrap();
        app.call(
            "new-sheet",
            serde_json::json!({"name": "Tiers", "rows": [["Basic", 5]], "workbook": "Lumo pricing"}),
        )
        .unwrap();
        let state = app.state();
        assert_eq!(state.sheets.len(), 1);
        assert_eq!(state.sheets[0].cells()[0].raw, "Basic");
        assert_eq!(SheetsShell::label(&state), "Lumo pricing");
    }

    /// A workbook kept as content imports with its formulas, its hidden
    /// sheet and its title, into the new workbook's place; exported again,
    /// the sheet is still hidden and the cross-sheet values are cached.
    #[test]
    fn an_xlsx_kept_as_content_imports_whole() {
        use crate::xlsx::{ImportedCell, SheetTable};
        let cell = |raw: &str| {
            Some(ImportedCell {
                raw: raw.into(),
                format: CellFormat::General,
                formula: raw.starts_with('=').then(|| raw.into()),
                bold: false,
            })
        };
        let tables = vec![
            SheetTable {
                name: "Summary".into(),
                hidden: false,
                rows: vec![vec![cell("=Rates!B1*10")]],
            },
            SheetTable {
                name: "Rates".into(),
                hidden: true,
                rows: vec![vec![cell("EUR"), cell("1.5")]],
            },
        ];
        let bytes = crate::xlsx::xlsx_bytes(&tables).unwrap();
        let mut app = TestApp::<SheetsShell>::new().unwrap();
        let hash = app.content().put(&bytes).unwrap();
        let window = ui::WakeScope::new();
        ui::tilo_resource::content::register(&window.handle(), app.content());
        let _entered = window.enter();
        for (content, says) in [
            ("q3.xlsx", "names no content"),
            (&*"0".repeat(64), "not available"),
        ] {
            let error = app
                .call(
                    "import-xlsx-content",
                    serde_json::json!({"content": content}),
                )
                .unwrap_err()
                .to_string();
            assert!(error.contains(says), "{content}: {error}");
        }
        app.call(
            "import-xlsx-content",
            serde_json::json!({"content": format!("tilo.content:{hash}"), "title": "Q3 forecast.xlsx"}),
        )
        .unwrap();
        let state = app.state();
        let names: Vec<(&str, bool)> = state
            .sheets
            .iter()
            .map(|sheet| (sheet.name.as_str(), sheet.hidden))
            .collect();
        assert_eq!(
            names,
            [("Summary", false), ("Rates", true)],
            "the blank sheet gave way"
        );
        assert_eq!(state.sheets[0].cells()[0].display, "15");
        assert_eq!(SheetsShell::label(&state), "Q3 forecast");

        let exported = crate::xlsx::parse_xlsx(
            &SheetsApp::encode_tables(crate::app::Interchange::Xlsx, &state.sheets).unwrap(),
        )
        .unwrap();
        assert!(exported[1].hidden && !exported[0].hidden);
        let summary = exported[0].rows[0][0].as_ref().unwrap();
        assert_eq!(summary.formula.as_deref(), Some("=Rates!B1*10"));
        assert_eq!(summary.raw, "15", "the cached value reads the other sheet");
    }

    /// Reading a sheet the workbook does not have is refused naming the
    /// sheets it has, as an edit's refusal does.
    #[test]
    fn reading_an_unknown_sheet_names_the_sheets() {
        let mut app = TestApp::<SheetsShell>::new().unwrap();
        let error = app
            .call("read-cells", serde_json::json!({"sheet": "Q3 actuals"}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("the sheets are `Sheet1`"), "{error}");
    }
}
