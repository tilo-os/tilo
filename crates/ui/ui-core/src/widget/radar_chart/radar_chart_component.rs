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
