//! [`Sheet`] as a described component: a workbook's active sheet, drawn as a grid of cells, with
//! what each cell shows and the cells the person chose.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::component::{
    Component, Described, Event, PropType, Type, Value, case_field, case_field_description, wrong,
};

use super::sheet::{CellKind, Peer, Sheet, SheetCell, SheetSize};

crate::choice!(CellKind, "SheetCellKind", {
    Text => "text",
    Number => "number",
    Bool => "bool",
    Error => "error",
});

impl PropType for SheetCell {
    fn ty() -> Type {
        Type::Cases {
            name: "SheetCell".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(wrong(&Self::ty(), value));
        };
        if case != "cell" {
            return Err(format!("a sheet's cell is a `cell`, not `{case}`"));
        }
        Ok(Self {
            cell: case_field(fields, "cell")?.ok_or("`cell` needs `cell`, its A1 reference")?,
            text: case_field(fields, "text")?.unwrap_or_default(),
            kind: case_field(fields, "kind")?.unwrap_or_default(),
            bold: case_field(fields, "bold")?.unwrap_or_default(),
        })
    }
}

/// The description of [`SheetCell`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn sheet_cell_description() -> EnumDescription {
    EnumDescription {
        name: "SheetCell".to_owned(),
        doc: "One cell of a sheet as it shows: its reference, the text it shows and how it reads."
            .to_owned(),
        cases: vec![CaseDescription {
            name: "cell".to_owned(),
            doc: "A cell at its A1 reference, showing `text`.".to_owned(),
            fields: vec![
                case_field_description("cell", Type::Text, None, "Its A1 reference, such as `A1`."),
                case_field_description(
                    "text",
                    Type::Text,
                    Some(Value::Text(String::new())),
                    "What it shows, already through its format.",
                ),
                case_field_description(
                    "kind",
                    CellKind::ty(),
                    Some(Value::Choice("text".to_owned())),
                    "How it reads: a number sits at its right edge and a word at its left.",
                ),
                case_field_description(
                    "bold",
                    Type::Bool,
                    Some(Value::Bool(false)),
                    "Set heavier than its neighbours.",
                ),
            ],
        }],
    }
}

impl PropType for Peer {
    fn ty() -> Type {
        Type::Cases {
            name: "SheetPeer".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(wrong(&Self::ty(), value));
        };
        if case != "peer" {
            return Err(format!("a sheet's peer is a `peer`, not `{case}`"));
        }
        let field = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };
        let Some(area) = field("area").and_then(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        }) else {
            return Err("`peer` needs `area`, the cells they chose as `A1` or `A1:B2`".to_owned());
        };
        let who = match field("who") {
            Some(Value::Actor { name, .. } | Value::Text(name)) => name.clone(),
            _ => return Err("`peer` needs `who`, the person they are".to_owned()),
        };
        Ok(Self { area, who })
    }
}

/// The description of [`Peer`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn sheet_peer_description() -> EnumDescription {
    EnumDescription {
        name: "SheetPeer".to_owned(),
        doc: "Another person at work in the sheet: the cells they chose and who they are."
            .to_owned(),
        cases: vec![CaseDescription {
            name: "peer".to_owned(),
            doc: "A person's selection, drawn in their colour.".to_owned(),
            fields: vec![
                case_field_description(
                    "area",
                    Type::Text,
                    None,
                    "Their selection, as `A1` or `A1:B2`.",
                ),
                case_field_description("who", Type::Actor, None, "Whose selection it is."),
            ],
        }],
    }
}

impl PropType for SheetSize {
    fn ty() -> Type {
        Type::Cases {
            name: "SheetSize".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(wrong(&Self::ty(), value));
        };
        let field = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };
        let number = |name: &str| match field(name) {
            Some(Value::Float(value)) => Ok(*value),
            Some(Value::Int(value)) => Ok(*value as f64),
            _ => Err(format!("`{case}` needs `{name}`, a number")),
        };
        match case.as_str() {
            "column" => {
                let Some(Value::Text(column)) = field("column") else {
                    return Err("`column` needs `column`, its letter".to_owned());
                };
                Ok(Self::Column {
                    column: column.clone(),
                    width: number("width")?,
                })
            }
            "row" => {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a row is a whole number of rows from 1"
                )]
                let row = number("row")?.max(1.0) as u32;
                Ok(Self::Row {
                    row,
                    height: number("height")?,
                })
            }
            other => Err(format!("a sheet size is `column` or `row`, not `{other}`")),
        }
    }
}

/// The description of [`SheetSize`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn sheet_size_description() -> EnumDescription {
    EnumDescription {
        name: "SheetSize".to_owned(),
        doc: "A column's width or a row's height, as the person set it.".to_owned(),
        cases: vec![
            CaseDescription {
                name: "column".to_owned(),
                doc: "A column made wider or narrower than the rest.".to_owned(),
                fields: vec![
                    case_field_description("column", Type::Text, None, "Its letter, such as `A`."),
                    case_field_description("width", Type::Float, None, "Its width in pixels."),
                ],
            },
            CaseDescription {
                name: "row".to_owned(),
                doc: "A row made taller or shorter than the rest.".to_owned(),
                fields: vec![
                    case_field_description(
                        "row",
                        Type::Float,
                        None,
                        "Its number, counting from 1.",
                    ),
                    case_field_description("height", Type::Float, None, "Its height in pixels."),
                ],
            },
        ],
    }
}

/// A sheet drawn as a grid: its column and row headers, its cells as they show, the cells the
/// person chose, the cells a search found and a comment's mark in the corner.
#[derive(Component, Default)]
#[component(name = "Sheet", role = "Table", label = "name")]
pub struct SheetProps {
    /// What the sheet is, for assistive technology.
    pub name: Option<String>,
    /// How many columns it shows.
    pub columns: i64,
    /// How many rows it shows.
    pub rows: i64,
    /// A column's width in pixels where the person did not size it.
    pub column_width: Option<f64>,
    /// A row's height in pixels where the person did not size it.
    pub row_height: Option<f64>,
    /// The columns and rows the person sized.
    pub sizes: Vec<SheetSize>,
    /// The cells that show anything; an empty cell takes no room here.
    pub cells: Vec<SheetCell>,
    /// The cells chosen, as `A1` or `A1:B2`.
    pub selection: Option<String>,
    /// The cells the window's search found.
    pub found: Vec<String>,
    /// The cells a comment is on.
    pub commented: Vec<String>,
    /// The cells a resolved comment is on.
    pub answered: Vec<String>,
    /// Who else is in the sheet, where they are.
    pub peers: Vec<Peer>,
    /// It takes edits; read-only when not.
    pub editable: bool,
    /// The cells chosen changed: carries them, as `A1` or `A1:B2`.
    #[component(event(value = String))]
    pub select: Event,
    /// Cells were written: carries pairs of reference and input, in order; an empty input clears
    /// its cell.
    #[component(event(value = Vec<String>))]
    pub edit: Event,
    /// A column was resized: carries its letter and its new width in pixels.
    #[component(event(value = Vec<String>))]
    pub resized_column: Event,
    /// A row was resized: carries its number and its new height in pixels.
    #[component(event(value = Vec<String>))]
    pub resized_row: Event,
    /// The fill handle was dragged: carries the cells it copies from and the cells it fills.
    #[component(event(value = Vec<String>))]
    pub fill: Event,
    /// The chosen cells were asked to be copied: carries them as a table, a line per row and a
    /// tab between columns. The app's action writes it to the person's clipboard.
    #[component(event(value = String))]
    pub copy: Event,
    /// The chosen cells were asked to be cut: carries them as `copy` does, and the app copies
    /// them and clears the cells.
    #[component(event(value = String))]
    pub cut: Event,
    /// The person asked to paste: the app reads the person's clipboard and writes the cells from
    /// the selection's top left.
    #[component(event)]
    pub paste: Event,
}

impl Described for Sheet {
    type Props = SheetProps;

    fn from_props(_key: &str, props: SheetProps) -> Self {
        let mut sheet = Self::new();
        sheet.set_props(props);
        sheet
    }

    fn set_props(&mut self, props: SheetProps) -> bool {
        // The props are taken where the sheet stands, so the person's selection and the cell
        // they edit stay: nothing about the grid is built again.
        self.take(props);
        true
    }
}
