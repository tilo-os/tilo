//! [`PolarAreaChart`] as a described component: [`PolarAreaValue`] as one direction's magnitude.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::component::{
    Component, Described, PropType, Type, Value, case_field, case_field_description,
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
            return Err(crate::component::wrong(&Self::ty(), value));
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
        self.mark_dirty();
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "in a test these are the assertion"
    )]

    use super::*;
    use crate::theme::Theme;

    fn area(label: &str, value: f64) -> Value {
        Value::Case {
            case: "area".to_owned(),
            fields: vec![
                ("label".to_owned(), Value::Text(label.to_owned())),
                ("value".to_owned(), Value::Float(value)),
            ],
        }
    }

    fn props(areas: &[(&str, f64)]) -> PolarAreaChartProps {
        PolarAreaChartProps {
            areas: areas
                .iter()
                .map(|(label, value)| PolarAreaValue::from_value(&area(label, *value)).unwrap())
                .collect(),
            ..PolarAreaChartProps::default()
        }
    }

    fn values(chart: &PolarAreaChart) -> Vec<(String, f64)> {
        chart
            .marks
            .data
            .iter()
            .map(|area| (area.label.clone(), area.value))
            .collect()
    }

    #[test]
    fn an_area_reads_from_its_case_and_needs_its_value() {
        let read = PolarAreaValue::from_value(&area("March", 7.0)).unwrap();
        assert_eq!((read.label.as_str(), read.value), ("March", 7.0));
        let unvalued = Value::Case {
            case: "area".to_owned(),
            fields: vec![("label".to_owned(), Value::Text("March".to_owned()))],
        };
        assert!(PolarAreaValue::from_value(&unvalued).is_err());
        let slice = Value::Case {
            case: "slice".to_owned(),
            fields: Vec::new(),
        };
        assert!(PolarAreaValue::from_value(&slice).is_err());
    }

    #[test]
    fn changed_magnitudes_change_the_sectors_where_they_stand() {
        let mut chart = PolarAreaChart::from_props("k", props(&[("Jan", 3.0), ("Feb", 5.0)]));
        assert_eq!(
            values(&chart),
            [("Jan".to_owned(), 3.0), ("Feb".to_owned(), 5.0)]
        );

        let next = PolarAreaChartProps {
            title: Some("Signups by month".to_owned()),
            ..props(&[("Jan", 4.0), ("Feb", 2.0), ("Mar", 6.0)])
        };
        assert!(chart.set_props(next), "taken in place, never built again");
        assert_eq!(
            values(&chart),
            [
                ("Jan".to_owned(), 4.0),
                ("Feb".to_owned(), 2.0),
                ("Mar".to_owned(), 6.0)
            ]
        );
        assert_eq!(chart.marks.frame.title, "Signups by month");
    }

    #[test]
    fn a_polar_area_of_no_directions_holds_none() {
        let chart = PolarAreaChart::from_props("k", props(&[]));
        assert!(chart.marks.data.is_empty());
    }

    #[test]
    fn each_direction_takes_the_theme_s_next_colour_by_its_place() {
        let chart = PolarAreaChart::from_props("k", props(&[("Jan", 3.0), ("Feb", 5.0)]));
        for theme in [Theme::light(), Theme::dark()] {
            for (at, area) in chart.marks.data.iter().enumerate() {
                assert_eq!(
                    crate::widget::polar_area_chart::PolarAreaMarks::color(area, at, &theme),
                    theme.tokens.colors.presence.series(at)
                );
            }
        }
    }
}
