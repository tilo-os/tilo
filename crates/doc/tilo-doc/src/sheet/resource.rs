//! Workbook as a [`tilo_resource::Resource`]: the only public write path is
//! [`WorkbookAction`].

use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, ActionParam, Doc, ParamType};
use tilo_resource::{ActionDecl, JsonSchema, ModelError, Resource, SearchEntry};

use super::model::{SheetContainers, init, open, sheet_ids_in_order};
use super::schema::{
    CELL_FORMULA, CELL_STYLE, CELL_TYPE, CELL_VALUE, SHEET_CELLS, SHEET_NAME, SHEET_ORDER,
};
use super::snapshot::{CellType, WorkbookSnapshot};

/// The workbook resource. State is a snapshot; mutations are [`WorkbookAction`].
pub struct WorkbookDoc;

/// Replayable workbook edits. The owned [`super::Workbook`] mutators are not an
/// app API — only this enum's [`Action::apply`] writes the document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub enum WorkbookAction {
    /// Ensure the schema exists (empty workbook).
    Initialize,
    /// Add a sheet named `name` after `after` (or at the end when `None`).
    AddSheet { name: String, after: Option<String> },
    /// Rename a sheet.
    RenameSheet { id: String, name: String },
    /// Set one cell on a sheet.
    SetCell {
        sheet: String,
        reference: String,
        value: String,
        cell_type: String,
        formula: Option<String>,
        style: Option<String>,
    },
}

impl Action for WorkbookAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::Initialize => {
                if open(doc, &ROOT).is_err() {
                    let _ = init(doc, &ROOT);
                }
                Ok(())
            }
            Self::AddSheet { name, after } => {
                let c = open_or_init(doc);
                add_sheet(doc, &c, name, after.as_deref())?;
                Ok(())
            }
            Self::RenameSheet { id, name } => {
                let c =
                    open(doc, &ROOT).map_err(|error| ActionError::Refused(error.to_string()))?;
                let Some(obj) = sheet_obj(doc, &c, id) else {
                    return Err(ActionError::Refused("unknown sheet".into()));
                };
                let _ = doc.put(&obj, SHEET_NAME, name.as_str());
                Ok(())
            }
            Self::SetCell {
                sheet,
                reference,
                value,
                cell_type,
                formula,
                style,
            } => {
                if super::a1::parse_ref(reference).is_none() {
                    return Err(ActionError::Refused("bad A1 reference".into()));
                }
                let c =
                    open(doc, &ROOT).map_err(|error| ActionError::Refused(error.to_string()))?;
                let Some(cells) = cells_container(doc, &c, sheet) else {
                    return Err(ActionError::Refused("unknown sheet".into()));
                };
                let cell_obj = match doc.get(&cells, reference.as_str()) {
                    Ok(Some((Value::Object(ObjType::Map), o))) => o,
                    _ => doc
                        .put_object(&cells, reference.as_str(), ObjType::Map)
                        .map_err(ActionError::Automerge)?,
                };
                let ty = CellType::from_str(cell_type);
                let _ = doc.put(&cell_obj, CELL_VALUE, value.as_str());
                let _ = doc.put(&cell_obj, CELL_TYPE, ty.as_str());
                match formula {
                    Some(src) => {
                        let _ = doc.put(&cell_obj, CELL_FORMULA, src.as_str());
                    }
                    None => {
                        let _ = doc.delete(&cell_obj, CELL_FORMULA);
                    }
                }
                match style {
                    Some(src) => {
                        let _ = doc.put(&cell_obj, CELL_STYLE, src.as_str());
                    }
                    None => {
                        let _ = doc.delete(&cell_obj, CELL_STYLE);
                    }
                }
                Ok(())
            }
        }
    }
}

fn open_or_init(doc: &mut Doc) -> SheetContainers {
    match open(doc, &ROOT) {
        Ok(containers) => containers,
        Err(_) => init(doc, &ROOT),
    }
}

fn sheet_obj(doc: &Doc, c: &SheetContainers, id: &str) -> Option<automerge::ObjId> {
    match doc.get(&c.sheets, id) {
        Ok(Some((Value::Object(ObjType::Map), obj))) => Some(obj),
        _ => None,
    }
}

fn cells_container(doc: &mut Doc, c: &SheetContainers, sheet_id: &str) -> Option<automerge::ObjId> {
    let obj = sheet_obj(doc, c, sheet_id)?;
    match doc.get(&obj, SHEET_CELLS) {
        Ok(Some((Value::Object(ObjType::Map), cid))) => Some(cid),
        _ => doc.put_object(&obj, SHEET_CELLS, ObjType::Map).ok(),
    }
}

fn add_sheet(
    doc: &mut Doc,
    c: &SheetContainers,
    name: &str,
    after: Option<&str>,
) -> Result<String, ActionError> {
    let ordered = sheet_ids_in_order(doc, c);
    let pos = match after {
        None => ordered.len(),
        Some(id) => ordered
            .iter()
            .position(|candidate| candidate == id)
            .map_or(ordered.len(), |i| i.saturating_add(1)),
    };
    let lo = pos
        .checked_sub(1)
        .and_then(|i| ordered.get(i))
        .and_then(|id| sheet_order(doc, c, id));
    let hi = ordered.get(pos).and_then(|id| sheet_order(doc, c, id));
    let key = tilo_resource::order::bytes::between(lo.as_deref(), hi.as_deref());
    let mut ids = crate::model::IdGen::new(doc.get_actor().to_hex_string());
    let id = ids.mint();
    let obj = doc
        .put_object(&c.sheets, id.as_str(), ObjType::Map)
        .map_err(ActionError::Automerge)?;
    doc.put(&obj, SHEET_ORDER, ScalarValue::Bytes(key))
        .map_err(ActionError::Automerge)?;
    doc.put(&obj, SHEET_NAME, name)
        .map_err(ActionError::Automerge)?;
    doc.put_object(&obj, SHEET_CELLS, ObjType::Map)
        .map_err(ActionError::Automerge)?;
    Ok(id)
}

fn sheet_order(doc: &Doc, c: &SheetContainers, id: &str) -> Option<Vec<u8>> {
    let obj = sheet_obj(doc, c, id)?;
    match doc.get(&obj, SHEET_ORDER) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(bytes) => Some(bytes.clone()),
            _ => None,
        },
        _ => None,
    }
}

impl Resource for WorkbookDoc {
    const KIND: &'static str = "tilo.workbook";
    const LABEL: &'static str = "Workbook";

    type State = WorkbookSnapshot;
    type Action = WorkbookAction;

    fn read(doc: &Doc) -> Result<WorkbookSnapshot, ModelError> {
        let c = open(doc, &ROOT).map_err(|error| ModelError::invalid(error.to_string()))?;
        Ok(super::model::materialize(doc, &c))
    }

    fn initialize(doc: &mut Doc) -> Result<(), ModelError> {
        if open(doc, &ROOT).is_err() {
            let _ = init(doc, &ROOT);
        }
        Ok(())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "add-sheet",
                "Add sheet",
                vec![ActionParam::required_owned(
                    "name",
                    ParamType::Text,
                    "Sheet name",
                )],
                |args, _| {
                    let name = args.text("name").ok_or("missing name")?.to_owned();
                    Ok(WorkbookAction::AddSheet { name, after: None })
                },
            )
            .describe("Add a sheet at the end of the workbook."),
            ActionDecl::new(
                "set-cell",
                "Set cell",
                vec![
                    ActionParam::required_owned("sheet", ParamType::Text, "Sheet id"),
                    ActionParam::required_owned("reference", ParamType::Text, "A1 reference"),
                    ActionParam::required_owned("value", ParamType::Text, "Cell value"),
                    ActionParam::optional_owned("type", ParamType::Text, "text, number, …"),
                ],
                |args, _| {
                    Ok(WorkbookAction::SetCell {
                        sheet: args.text("sheet").ok_or("missing sheet")?.to_owned(),
                        reference: args
                            .text("reference")
                            .ok_or("missing reference")?
                            .to_owned(),
                        value: args.text("value").ok_or("missing value")?.to_owned(),
                        cell_type: args.text("type").unwrap_or("text").to_owned(),
                        formula: None,
                        style: None,
                    })
                },
            )
            .describe("Set one cell's value."),
        ]
    }

    fn search(state: &WorkbookSnapshot) -> Vec<SearchEntry> {
        state
            .sheets
            .iter()
            .map(|sheet| SearchEntry {
                anchor: Some(sheet.id.clone()),
                title: sheet.name.clone(),
                text: sheet.name.clone(),
                action: None,
            })
            .collect()
    }
}

/// Containers for callers that already hold a live document.
///
/// # Errors
///
/// Returns [`ModelError`] when the sheet containers cannot be opened on `doc`.
pub fn containers(doc: &Doc) -> Result<SheetContainers, ModelError> {
    open(doc, &ROOT).map_err(|error| ModelError::invalid(error.to_string()))
}
