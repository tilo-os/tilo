//! [`Chart`] as a described component, and [`ChartLine`] as the series it plots.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::widget::drawing::Ink;
use tilo_ui_base::{
    component::Component, component::Described, component::PropType, component::Type,
    component::Value, component::case_field, component::case_field_description,
};

use super::{Chart, Series};

/// One line of a chart: what it is of, its readings oldest first, and the ink of the theme it is
/// drawn in.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChartLine {
    pub label: String,
    pub values: Vec<f64>,
    pub ink: Ink,
}

impl PropType for ChartLine {
    fn ty() -> Type {
        Type::Cases {
            name: "Series".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(tilo_ui_base::component::wrong(&Self::ty(), value));
        };
        if case != "series" {
            return Err(format!("a chart's line is a `series`, not `{case}`"));
        }
        Ok(Self {
            label: case_field(fields, "label")?.unwrap_or_default(),
            values: case_field(fields, "values")?
                .ok_or("`series` needs `values`, a list of numbers")?,
            ink: case_field(fields, "ink")?.unwrap_or_default(),
        })
    }
}

/// The description of [`ChartLine`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn series_description() -> EnumDescription {
    EnumDescription {
        name: "Series".to_owned(),
        doc: "One line of a chart: its readings, oldest first.".to_owned(),
        cases: vec![CaseDescription {
            name: "series".to_owned(),
            doc: "A line through `values`, named `label` in the chart's legend.".to_owned(),
            fields: vec![
                case_field_description(
                    "values",
                    Type::List(Box::new(Type::Float)),
                    None,
                    "The readings, oldest first.",
                ),
                case_field_description(
                    "label",
                    Type::Text,
                    Some(Value::Text(String::new())),
                    "What it is of, in the legend.",
                ),
                case_field_description(
                    "ink",
                    Ink::ty(),
                    Some(Value::Choice("accent".to_owned())),
                    "The theme's ink it is drawn in.",
                ),
            ],
        }],
    }
}

/// A titled chart of one or more lines over a shared scale, with a grid and the labels of its scale:
/// a history worth reading closely, where a sparkline only hints at a trend.
#[derive(Component, Default)]
#[component(name = "Chart", role = "Image", label = "title")]
pub struct ChartProps {
    /// The lines, each over the ones before it: `.series(values: cpu, label: "CPU")`.
    #[component(required, positional)]
    pub series: Vec<ChartLine>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
    /// The top of its scale, which may be more than the readings reach; the highest reading
    /// unless said.
    pub max: Option<f64>,
    /// The area under each line washed in its ink.
    pub area: bool,
    /// The label at the top of its scale: "100%".
    pub top_label: Option<String>,
    /// The label at the foot of its scale: "0%".
    pub bottom_label: Option<String>,
    /// What each reading is at, oldest first ("9:51", "Monday"): the position a person pointing
    /// at the chart reads the lines at.
    pub labels: Vec<String>,
    /// What a reading counts, written after each value read ("ms", "/s").
    pub unit: Option<String>,
    /// Its height in logical pixels: 160 unless said.
    pub height: Option<f64>,
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "readings and sizes, well inside f32"
)]
fn lines(lines: &[ChartLine]) -> Vec<Series> {
    lines
        .iter()
        .map(|line| {
            Series::new(
                line.values.iter().map(|value| *value as f32).collect(),
                vello::peniko::Color::TRANSPARENT,
            )
            .with_legend(line.label.clone())
        })
        .collect()
}

impl Described for Chart {
    type Props = ChartProps;

    fn from_props(_key: &str, props: ChartProps) -> Self {
        let mut chart = Self::new(Vec::new());
        chart.set_props(props);
        chart
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "readings and sizes, well inside f32"
    )]
    fn set_props(&mut self, props: ChartProps) -> bool {
        self.set_series(lines(&props.series));
        let marks = &mut self.marks;
        marks.inks = props.series.iter().map(|line| line.ink).collect();
        marks.max = props.max.map(|max| max as f32);
        marks.area = props.area;
        marks.frame.title = props.title.unwrap_or_default();
        marks.frame.empty = props.empty.unwrap_or_default();
        marks.axis_hi = props.top_label.unwrap_or_default();
        marks.axis_lo = props.bottom_label.unwrap_or_default();
        marks.labels = props.labels;
        marks.unit = props.unit.unwrap_or_default();
        marks.frame.height = Some(props.height.unwrap_or(160.0) as f32);
        true
    }
}
