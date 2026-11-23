//! [`Sheet`] as a described component: a workbook's active sheet, drawn as a grid of cells, with
//! the format each shows in and the selection the person made.

use crate::component::{
    Component, Described, Event, FieldDescription, PropType, Type, Value, wrong,
};

use super::sheet::{CellKind, Peer, Sheet, SheetCell, SheetSize};

/// How a cell's text reads: a number sits at its cell's right edge and a word at its left.
crate::component::choice!(CellKind, "SheetCellKind", {
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
        let field = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
        let text = |name: &str| match field(name) {
            Some(Value::Text(text)) => Ok(text.clone()),
            _ => Err(format!("`{case}` needs `{name}`, a text")),
        };
        if case != "cell" {
            return Err(format!("a sheet cell is `cell`, not `{case}`"));
        }
        let kind = match field("kind") {
            None | Some(Value::None) => CellKind::Text,
            Some(value) => CellKind::from_value(value)?,
        };
        Ok(Self {
            cell: text("cell")?,
            text: text("text")?,
            kind,
            bold: matches!(field("bold"), Some(Value::Bool(true))),
        })
    }
}

/// What a sheet cell takes, as the catalogue describes it: a case with fields, so a view writes
/// `.cell(cell: "A1", text: "12", kind: .number, bold: true)`.
#[must_use]
pub fn sheet_cell_description() -> crate::component::EnumDescription {
    crate::component::EnumDescription {
        name: "SheetCell".to_owned(),
        doc: "One cell of a sheet as it shows: its reference, its text, how it reads and its weight."
            .to_owned(),
        cases: vec![crate::component::CaseDescription {
            name: "cell".to_owned(),
            doc: String::new(),
            fields: vec![
                FieldDescription {
                    name: "cell".to_owned(),
                    ty: Type::Text,
                    default: None,
                    doc: "Its A1 reference.".to_owned(),
                },
                FieldDescription {
                    name: "text".to_owned(),
                    ty: Type::Text,
                    default: None,
                    doc: "What it shows, already through its format.".to_owned(),
                },
                FieldDescription {
                    name: "kind".to_owned(),
                    ty: CellKind::ty(),
                    default: Some(Value::Choice("text".to_owned())),
                    doc: "How it reads: a number sits right, a word left.".to_owned(),
                },
                FieldDescription {
                    name: "bold".to_owned(),
                    ty: Type::Bool,
                    default: Some(Value::Bool(false)),
                    doc: "Set heavier than its neighbours.".to_owned(),
                },
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
            return Err(format!("a sheet's peer is `peer`, not `{case}`"));
        }
        let field = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
        let area = match field("area") {
            Some(Value::Text(text)) => text.clone(),
            _ => return Err("`peer` needs `area`, a text such as `A1:B2`".to_owned()),
        };
        let who = match field("who") {
            Some(Value::Actor { name, .. }) => name.clone(),
            _ => return Err("`peer` needs `who`, the person their name".to_owned()),
        };
        Ok(Self { area, who })
    }
}

/// The catalogue entry of a peer's selection.
#[must_use]
pub fn sheet_peer_description() -> crate::component::EnumDescription {
    crate::component::EnumDescription {
        name: "SheetPeer".to_owned(),
        doc: "Another person at work in the sheet: the cells they chose and what to call them."
            .to_owned(),
        cases: vec![crate::component::CaseDescription {
            name: "peer".to_owned(),
            doc: String::new(),
            fields: vec![
                FieldDescription {
                    name: "area".to_owned(),
                    ty: Type::Text,
                    default: None,
                    doc: "Their selection, as `A1` or `A1:B2`.".to_owned(),
                },
                FieldDescription {
                    name: "who".to_owned(),
                    ty: Type::Actor,
                    default: None,
                    doc: "Whose selection it is.".to_owned(),
                },
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
        let field = |name: &str| fields.iter().find(|(key, _)| key == name).map(|(_, value)| value);
        let number = |name: &str| match field(name) {
            Some(Value::Float(value)) => Ok(*value),
            Some(Value::Int(value)) => Ok(*value as f64),
            _ => Err(format!("`{case}` needs `{name}`, a number")),
        };
        match case.as_str() {
            "column" => {
                let at = match field("column") {
                    Some(Value::Text(text)) => text.clone(),
                    _ => return Err("`column` needs `column`, its letter".to_owned()),
                };
                Ok(Self::Column {
                    column: at,
                    width: number("width")?,
                })
            }
            "row" => Ok(Self::Row {
                row: number("row")? as u32,
                height: number("height")?,
            }),
            other => Err(format!("a sheet size is `column` or `row`, not `{other}`")),
        }
    }
}

/// The catalogue entry of a sized column or row.
#[must_use]
pub fn sheet_size_description() -> crate::component::EnumDescription {
    crate::component::EnumDescription {
        name: "SheetSize".to_owned(),
        doc: "A column's width or a row's height, as the person set it.".to_owned(),
        cases: vec![
            crate::component::CaseDescription {
                name: "column".to_owned(),
                doc: String::new(),
                fields: vec![
                    FieldDescription {
                        name: "column".to_owned(),
                        ty: Type::Text,
                        default: None,
                        doc: "The column, by its letter.".to_owned(),
                    },
                    FieldDescription {
                        name: "width".to_owned(),
                        ty: Type::Float,
                        default: None,
                        doc: "Its width in pixels.".to_owned(),
                    },
                ],
            },
            crate::component::CaseDescription {
                name: "row".to_owned(),
                doc: String::new(),
                fields: vec![
                    FieldDescription {
                        name: "row".to_owned(),
                        ty: Type::Float,
                        default: None,
                        doc: "The row's number, counting from 1.".to_owned(),
                    },
                    FieldDescription {
                        name: "height".to_owned(),
                        ty: Type::Float,
                        default: None,
                        doc: "Its height in pixels.".to_owned(),
                    },
                ],
            },
        ],
    }
}

/// A sheet drawn as a grid: its column and row headers, its cells as they show, the selection,
/// the cells a search found and a comment's corner mark.
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
    /// Who else is in the sheet, where.
    pub peers: Vec<Peer>,
    /// It takes edits; read-only when not.
    pub editable: bool,
    /// The selection changed: carries the chosen cells, as `A1` or `A1:B2`.
    #[component(event(value = String))]
    pub select: Event,
    /// Cells were written: carries pairs of reference and input, in order; an empty input clears
    /// the cell.
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
}

impl Described for Sheet {
    type Props = SheetProps;

    fn from_props(_key: &str, props: SheetProps) -> Self {
        let mut sheet = Self::new();
        sheet.set_props(props);
        sheet
    }

    fn set_props(&mut self, props: SheetProps) -> bool {
        let SheetProps {
            name,
            columns,
            rows,
            column_width,
            row_height,
            sizes,
            cells,
            selection,
            found,
            commented,
            answered,
            peers,
            editable,
            select,
            edit,
            resized_column,
            resized_row,
            fill,
        } = props;
        let changed = self.take(
            name,
            columns,
            rows,
            column_width,
            row_height,
            sizes,
            cells,
            selection,
            found,
            commented,
            answered,
            peers,
            editable,
        );
        self.events.select = select;
        self.events.edit = edit;
        self.events.resized_column = resized_column;
        self.events.resized_row = resized_row;
        self.events.fill = fill;
        changed
    }
}
