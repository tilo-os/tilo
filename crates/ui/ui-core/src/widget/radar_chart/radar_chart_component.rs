//! [`RadarChart`] as a described component: [`RadarAxisProps`] as one axis and its scale, and
//! [`RadarSeriesProps`] as one thing's readings across the axes.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::component::{
    Component, Described, PropType, Type, Value, case_field, case_field_description,
};

use super::{RadarAxis, RadarChart, RadarSeries};

/// One axis of a radar: what it measures and the reading that reaches its rim.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RadarAxisProps {
    pub label: String,
    pub max: f64,
}

impl PropType for RadarAxisProps {
    fn ty() -> Type {
        Type::Cases {
            name: "Axes".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(crate::component::wrong(&Self::ty(), value));
        };
        if case != "axis" {
            return Err(format!("a radar's axes are `axis`, not `{case}`"));
        }
        Ok(Self {
            label: case_field(fields, "label")?.ok_or("`axis` needs `label`, what it measures")?,
            max: case_field(fields, "max")?
                .ok_or("`axis` needs `max`, the reading that reaches its rim")?,
        })
    }
}

/// The description of [`RadarAxisProps`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn axes_description() -> EnumDescription {
    EnumDescription {
        name: "Axes".to_owned(),
        doc: "One measure a radar shows, with its own scale.".to_owned(),
        cases: vec![CaseDescription {
            name: "axis".to_owned(),
            doc: "An axis named `label`, whose rim stands at `max`.".to_owned(),
            fields: vec![
                case_field_description("label", Type::Text, None, "What it measures."),
                case_field_description(
                    "max",
                    Type::Float,
                    None,
                    "The reading that reaches the axis' rim; readings past it stop there.",
                ),
            ],
        }],
    }
}

/// One thing read across a radar's axes: what it is, and its reading on each axis.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RadarSeriesProps {
    pub label: String,
    pub values: Vec<f64>,
}

impl PropType for RadarSeriesProps {
    fn ty() -> Type {
        Type::Cases {
            name: "RadarSeries".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(crate::component::wrong(&Self::ty(), value));
        };
        if case != "series" {
            return Err(format!("a radar's readings are `series`, not `{case}`"));
        }
        Ok(Self {
            label: case_field(fields, "label")?.unwrap_or_default(),
            values: case_field(fields, "values")?
                .ok_or("`series` needs `values`, one number for each axis")?,
        })
    }
}

/// The description of [`RadarSeriesProps`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn radar_series_description() -> EnumDescription {
    EnumDescription {
        name: "RadarSeries".to_owned(),
        doc: "One thing read across a radar's axes.".to_owned(),
        cases: vec![CaseDescription {
            name: "series".to_owned(),
            doc: "A shape through `values`, one per axis in the axes' order, named `label` in \
                  the legend."
                .to_owned(),
            fields: vec![
                case_field_description(
                    "values",
                    Type::List(Box::new(Type::Float)),
                    None,
                    "The readings, one for each axis, in the axes' order.",
                ),
                case_field_description(
                    "label",
                    Type::Text,
                    Some(Value::Text(String::new())),
                    "What it is of, in the legend.",
                ),
            ],
        }],
    }
}

/// Several measures of the same things on axes around a circle, each axis with its own scale: a
/// thing's shape reads at a glance, and several things compare by overlay. Each series takes the
/// next colour of the theme's palette and is named in a legend.
#[derive(Component, Default)]
#[component(name = "RadarChart", role = "Image", label = "title")]
pub struct RadarChartProps {
    /// The axes in order, around the circle from the top:
    /// `.axis(label: "Speed", max: 100.0)`.
    #[component(required, positional)]
    pub axes: Vec<RadarAxisProps>,
    /// Each thing read across the axes: `.series(values: [12.0, 8.0], label: "This week")`.
    #[component(required)]
    pub series: Vec<RadarSeriesProps>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
}

/// The axes `props` names, in order.
fn axes(axes: &[RadarAxisProps]) -> Vec<RadarAxis> {
    axes.iter()
        .enumerate()
        .map(|(at, axis)| {
            let id = if axis.label.is_empty() {
                at.to_string()
            } else {
                axis.label.clone()
            };
            RadarAxis::new(id, axis.label.clone(), axis.max)
        })
        .collect()
}

/// The series `props` plots, each in the theme's palette colour of its place, so the colours
/// follow the theme the chart is drawn with.
fn plotted(series: &[RadarSeriesProps]) -> Vec<RadarSeries> {
    series
        .iter()
        .enumerate()
        .map(|(at, series)| {
            // A legend entry a person switched off stays off while its series keeps its name.
            let id = if series.label.is_empty() {
                at.to_string()
            } else {
                series.label.clone()
            };
            RadarSeries::from_palette(id, series.label.clone(), at, series.values.clone())
        })
        .collect()
}

impl Described for RadarChart {
    type Props = RadarChartProps;

    fn from_props(_key: &str, props: RadarChartProps) -> Self {
        let mut chart = Self::new(Vec::new(), Vec::new());
        chart.set_props(props);
        chart
    }

    /// Changed readings change the shapes where they stand: the widget stays, with what the
    /// person is inspecting and what they switched off in its legend.
    fn set_props(&mut self, props: RadarChartProps) -> bool {
        self.set_axes(axes(&props.axes));
        self.set_series(plotted(&props.series));
        let marks = &mut self.marks;
        marks.frame.title = props.title.unwrap_or_default();
        marks.style.legend.visible = props.series.iter().any(|series| !series.label.is_empty());
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
    use crate::chart_palette::ChartPalette;
    use crate::theme::Theme;

    fn axis(label: &str, max: f64) -> Value {
        Value::Case {
            case: "axis".to_owned(),
            fields: vec![
                ("label".to_owned(), Value::Text(label.to_owned())),
                ("max".to_owned(), Value::Float(max)),
            ],
        }
    }

    fn series(label: &str, values: &[f64]) -> Value {
        Value::Case {
            case: "series".to_owned(),
            fields: vec![
                (
                    "values".to_owned(),
                    Value::List(values.iter().copied().map(Value::Float).collect()),
                ),
                ("label".to_owned(), Value::Text(label.to_owned())),
            ],
        }
    }

    fn props(spokes: &[(&str, f64)], plots: &[(&str, &[f64])]) -> RadarChartProps {
        RadarChartProps {
            axes: spokes
                .iter()
                .map(|(label, max)| RadarAxisProps::from_value(&axis(label, *max)).unwrap())
                .collect(),
            series: plots
                .iter()
                .map(|(label, values)| {
                    RadarSeriesProps::from_value(&series(label, values)).unwrap()
                })
                .collect(),
            ..RadarChartProps::default()
        }
    }

    #[test]
    fn axes_and_series_read_from_their_cases() {
        let read = RadarAxisProps::from_value(&axis("Speed", 100.0)).unwrap();
        assert_eq!((read.label.as_str(), read.max), ("Speed", 100.0));
        let unscaled = Value::Case {
            case: "axis".to_owned(),
            fields: vec![("label".to_owned(), Value::Text("Speed".to_owned()))],
        };
        assert!(RadarAxisProps::from_value(&unscaled).is_err());
        let read = RadarSeriesProps::from_value(&series("This week", &[1.0, 2.0])).unwrap();
        assert_eq!(read.label, "This week");
        assert_eq!(read.values, [1.0, 2.0]);
        let bars = Value::Case {
            case: "bars".to_owned(),
            fields: Vec::new(),
        };
        assert!(RadarSeriesProps::from_value(&bars).is_err());
    }

    #[test]
    fn changed_readings_change_the_shapes_where_they_stand() {
        let mut chart = RadarChart::from_props(
            "k",
            props(
                &[("Speed", 10.0), ("Quality", 10.0), ("Cost", 10.0)],
                &[("Now", &[4.0, 6.0, 2.0])],
            ),
        );
        assert_eq!(chart.marks.axes[2].label, "Cost");
        assert_eq!(chart.marks.series[0].values, [4.0, 6.0, 2.0]);
        assert!(chart.marks.style.legend.visible);

        let next = RadarChartProps {
            title: Some("Service health".to_owned()),
            ..props(
                &[("Speed", 10.0), ("Quality", 10.0), ("Cost", 20.0)],
                &[("Now", &[5.0, 7.0, 3.0]), ("Last week", &[3.0, 5.0, 4.0])],
            )
        };
        assert!(chart.set_props(next), "taken in place, never built again");
        assert_eq!(chart.marks.axes.len(), 3);
        assert_eq!(chart.marks.axes[2].max, 20.0);
        assert_eq!(chart.marks.series.len(), 2);
        assert_eq!(chart.marks.series[1].values, [3.0, 5.0, 4.0]);
        assert_eq!(chart.marks.frame.title, "Service health");
    }

    #[test]
    fn a_radar_of_one_unnamed_shape_shows_no_legend() {
        let chart = RadarChart::from_props(
            "k",
            props(
                &[("Speed", 10.0), ("Quality", 10.0), ("Cost", 10.0)],
                &[("", &[4.0, 6.0, 2.0])],
            ),
        );
        assert!(!chart.marks.style.legend.visible);
    }

    #[test]
    fn a_radar_of_no_axes_or_shapes_holds_none() {
        let chart = RadarChart::from_props("k", RadarChartProps::default());
        assert!(chart.marks.axes.is_empty());
        assert!(chart.marks.series.is_empty());
        assert!(!chart.marks.style.legend.visible);
    }

    #[test]
    fn each_shape_takes_the_palette_colour_of_its_place_in_the_theme_that_draws_it() {
        let chart = RadarChart::from_props(
            "k",
            props(
                &[("Speed", 10.0), ("Quality", 10.0), ("Cost", 10.0)],
                &[("Now", &[4.0, 6.0, 2.0]), ("Before", &[2.0, 3.0, 1.0])],
            ),
        );
        for theme in [Theme::light(), Theme::dark()] {
            let palette = ChartPalette::from_theme(&theme);
            for (at, series) in chart.marks.series.iter().enumerate() {
                assert_eq!(series.color_in(&theme), palette.categorical(at));
            }
        }
    }
}
