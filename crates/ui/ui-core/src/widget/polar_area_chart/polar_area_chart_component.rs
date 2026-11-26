//! [`PolarAreaChart`] as a described component: [`PolarAreaValue`] as one direction's magnitude.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use tilo_ui_base::{
    component::Component, component::Described, component::PropType, component::Type,
    component::Value, component::case_field, component::case_field_description,
};

use super::PolarAreaChart;
use crate::widget::chart_data::ChartItem;

/// One direction of a cycle — a month, an hour, a weekday — and how much it holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PolarAreaValue {
    pub label: String,
    pub value: f64,
}

impl PropType for PolarAreaValue {
    fn ty() -> Type {
        Type::Cases {
            name: "Areas".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(tilo_ui_base::component::wrong(&Self::ty(), value));
        };
        if case != "area" {
            return Err(format!(
                "a polar area chart's directions are `area`, not `{case}`"
            ));
        }
        Ok(Self {
            label: case_field(fields, "label")?.unwrap_or_default(),
            value: case_field(fields, "value")?.ok_or("`area` needs `value`, a number")?,
        })
    }
}

/// The description of [`PolarAreaValue`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn areas_description() -> EnumDescription {
    EnumDescription {
        name: "Areas".to_owned(),
        doc: "One direction of a cycle, and its magnitude.".to_owned(),
        cases: vec![CaseDescription {
            name: "area".to_owned(),
            doc: "A sector as long as `value` is of the largest, named `label`.".to_owned(),
            fields: vec![
                case_field_description("label", Type::Text, None, "Which direction it is."),
                case_field_description(
                    "value",
                    Type::Float,
                    None,
                    "Its magnitude; nothing below zero is drawn.",
                ),
            ],
        }],
    }
}

/// A cycle's magnitudes around a circle: months of the year, hours of the day, weekdays. Every
/// direction takes the same angle and its length carries the value, so seasonal shape is read at
/// a glance; each takes the next colour of the theme's palette.
#[derive(Component, Default)]
#[component(name = "PolarAreaChart", role = "Image", label = "title")]
pub struct PolarAreaChartProps {
    /// The directions in order, clockwise from the top: `.area(label: "January", value: 12.0)`.
    #[component(required, positional)]
    pub areas: Vec<PolarAreaValue>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
}

/// The sectors `props` draws, in the order each takes around the circle.
fn plotted(props: &PolarAreaChartProps) -> Vec<ChartItem> {
    props
        .areas
        .iter()
        .enumerate()
        .map(|(at, area)| {
            let id = if area.label.is_empty() {
                at.to_string()
            } else {
                area.label.clone()
            };
            ChartItem::new(id, area.label.clone(), area.value)
        })
        .collect()
}

impl Described for PolarAreaChart {
    type Props = PolarAreaChartProps;

    fn from_props(_key: &str, props: PolarAreaChartProps) -> Self {
        let mut chart = Self::new(Vec::new());
        chart.set_props(props);
        chart
    }

    /// Changed magnitudes grow and shrink where they stand: the widget stays, with what the
    /// person is inspecting.
    fn set_props(&mut self, props: PolarAreaChartProps) -> bool {
        self.set_data(plotted(&props));
        self.marks.frame.title = props.title.unwrap_or_default();
        self.marks.frame.empty = props.empty.unwrap_or_default();
        self.mark_dirty();
        true
    }
}
