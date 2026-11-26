//! [`Heatmap`] as a described component: [`HeatmapPoint`] as one cell and its value.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use tilo_ui_base::{component::Component, component::Described, component::PropType, component::Type, component::Value, component::case_field, component::case_field_description};

use super::{Heatmap, HeatmapCell};

/// One cell of a matrix: the row and column it lies at, and its value.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeatmapPoint {
    pub row: String,
    pub column: String,
    pub value: f64,
}

impl PropType for HeatmapPoint {
    fn ty() -> Type {
        Type::Cases {
            name: "Cells".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(tilo_ui_base::component::wrong(&Self::ty(), value));
        };
        if case != "cell" {
            return Err(format!("a heatmap's values are `cell`, not `{case}`"));
        }
        Ok(Self {
            row: case_field(fields, "row")?.ok_or("`cell` needs `row`, which row it lies at")?,
            column: case_field(fields, "column")?
                .ok_or("`cell` needs `column`, which column it lies at")?,
            value: case_field(fields, "value")?.ok_or("`cell` needs `value`, a number")?,
        })
    }
}

/// The description of [`HeatmapPoint`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn cells_description() -> EnumDescription {
    EnumDescription {
        name: "Cells".to_owned(),
        doc: "One cell of a matrix, at the crossing of a row and a column.".to_owned(),
        cases: vec![CaseDescription {
            name: "cell".to_owned(),
            doc: "The value at `row` and `column`, coloured by where it stands among all cells."
                .to_owned(),
            fields: vec![
                case_field_description(
                    "row",
                    Type::Text,
                    None,
                    "Which row, by the name the rows give it.",
                ),
                case_field_description(
                    "column",
                    Type::Text,
                    None,
                    "Which column, by the name the columns give it.",
                ),
                case_field_description(
                    "value",
                    Type::Float,
                    None,
                    "What was measured there; a cell no number reaches stays empty.",
                ),
            ],
        }],
    }
}

/// How a value varies across two axes at once: a week by weekday and hour, a year by month and
/// region. Each cell's colour places it between the lowest and the highest value in the theme's
/// ramp; pointing at it reads its value.
#[derive(Component, Default)]
#[component(name = "Heatmap", role = "Image", label = "title")]
pub struct HeatmapProps {
    /// Each cell, by the row and column it lies at:
    /// `.cell(row: "Monday", column: "9h", value: 12.0)`.
    #[component(required, positional)]
    pub cells: Vec<HeatmapPoint>,
    /// The rows from top to bottom.
    #[component(required)]
    pub rows: Vec<String>,
    /// The columns from left to right.
    #[component(required)]
    pub columns: Vec<String>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
}

/// The cells `props` places: one for each crossing both a row and a column name.
fn placed(props: &HeatmapProps) -> Vec<HeatmapCell> {
    props
        .cells
        .iter()
        .filter_map(|cell| {
            let row = props.rows.iter().position(|name| name == &cell.row)?;
            let column = props.columns.iter().position(|name| name == &cell.column)?;
            Some(HeatmapCell::new(
                format!("{}/{}", cell.row, cell.column),
                row,
                column,
                cell.value,
            ))
        })
        .collect()
}

impl Described for Heatmap {
    type Props = HeatmapProps;

    fn from_props(_key: &str, props: HeatmapProps) -> Self {
        let mut heatmap = Self::new(Vec::new(), Vec::new(), Vec::new());
        heatmap.set_props(props);
        heatmap
    }

    /// Changed values recolour their cells where they stand: the widget stays, with what the
    /// person is inspecting.
    fn set_props(&mut self, props: HeatmapProps) -> bool {
        let cells = placed(&props);
        let HeatmapProps {
            rows,
            columns,
            title,
            ..
        } = props;
        self.set_cells(rows, columns, cells);
        self.marks.frame.title = title.unwrap_or_default();
        self.mark_dirty();
        true
    }
}
