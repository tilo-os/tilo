//! [`DistributionChart`] as a described component: raw observations per series, and
//! [`DistributionSamples`] as one measured thing's observations.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::component::{
    Component, Described, PropType, Type, Value, case_field, case_field_description,
};

use super::{DistributionChart, DistributionKind, DistributionSeries};

crate::component::choice!(DistributionKind, "DistributionKind", {
    Box => "box", Violin => "violin", BoxAndViolin => "both",
});

/// The observations of one measured thing: what it is, and each reading taken.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DistributionSamples {
    pub label: String,
    pub values: Vec<f64>,
}

impl PropType for DistributionSamples {
    fn ty() -> Type {
        Type::Cases {
            name: "Samples".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(crate::component::wrong(&Self::ty(), value));
        };
        if case != "samples" {
            return Err(format!(
                "a distribution chart's readings are `samples`, not `{case}`"
            ));
        }
        Ok(Self {
            label: case_field(fields, "label")?.unwrap_or_default(),
            values: case_field(fields, "values")?
                .ok_or("`samples` needs `values`, a list of numbers")?,
        })
    }
}

/// The description of [`DistributionSamples`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn samples_description() -> EnumDescription {
    EnumDescription {
        name: "Samples".to_owned(),
        doc: "One measured thing's observations, from which its spread is calculated.".to_owned(),
        cases: vec![CaseDescription {
            name: "samples".to_owned(),
            doc: "Every reading taken of `label`, in any order.".to_owned(),
            fields: vec![
                case_field_description(
                    "values",
                    Type::List(Box::new(Type::Float)),
                    None,
                    "The observations, one reading each.",
                ),
                case_field_description(
                    "label",
                    Type::Text,
                    Some(Value::Text(String::new())),
                    "What was measured.",
                ),
            ],
        }],
    }
}

/// The spread of each measured thing: where its readings centre and how far they run, as a box,
/// a violin or both. Callers give the readings themselves, never quartiles or bins; each series
/// takes the next colour of the theme's palette and is named under the chart.
#[derive(Component, Default)]
#[component(name = "DistributionChart", role = "Image", label = "title")]
pub struct DistributionChartProps {
    /// What was measured, each with the readings taken: `.samples(values: hours, label: "Before")`.
    #[component(required, positional)]
    pub samples: Vec<DistributionSamples>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
    /// How each distribution is drawn: `box` for its quartiles, `violin` for its shape, `both` for
    /// the two together unless said.
    pub kind: Option<DistributionKind>,
}

/// The series `props` plot, each in the theme's palette colour of its place, so the colours
/// follow the theme the chart is drawn with.
fn plotted(props: &DistributionChartProps) -> Vec<DistributionSeries> {
    props
        .samples
        .iter()
        .enumerate()
        .map(|(at, samples)| {
            // A legend entry a person switched off stays off while its series keeps its name.
            let id = if samples.label.is_empty() {
                at.to_string()
            } else {
                samples.label.clone()
            };
            DistributionSeries::from_palette(
                id,
                samples.label.clone(),
                at,
                samples.values.iter().copied(),
            )
        })
        .collect()
}

impl Described for DistributionChart {
    type Props = DistributionChartProps;

    fn from_props(_key: &str, props: DistributionChartProps) -> Self {
        let mut chart = Self::new(Vec::new());
        chart.set_props(props);
        chart
    }

    /// Changed readings change the boxes where they stand: the widget stays, with what the person
    /// is inspecting.
    fn set_props(&mut self, props: DistributionChartProps) -> bool {
        self.set_series(plotted(&props));
        let marks = &mut self.marks;
        marks.frame.title = props.title.unwrap_or_default();
        marks.style.kind = props.kind.unwrap_or(DistributionKind::BoxAndViolin);
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

    fn samples(label: &str, values: &[f64]) -> Value {
        Value::Case {
            case: "samples".to_owned(),
            fields: vec![
                (
                    "values".to_owned(),
                    Value::List(values.iter().copied().map(Value::Float).collect()),
                ),
                ("label".to_owned(), Value::Text(label.to_owned())),
            ],
        }
    }

    fn props(readings: &[(&str, &[f64])]) -> DistributionChartProps {
        DistributionChartProps {
            samples: readings
                .iter()
                .map(|(label, values)| {
                    DistributionSamples::from_value(&samples(label, values)).unwrap()
                })
                .collect(),
            ..DistributionChartProps::default()
        }
    }

    fn observations(chart: &DistributionChart, series: usize) -> Vec<f64> {
        chart.marks.series[series].samples.clone()
    }

    #[test]
    fn samples_read_from_their_case_and_no_other_case_is_one() {
        let read = DistributionSamples::from_value(&samples("Before", &[4.0, 6.0])).unwrap();
        assert_eq!(read.label, "Before");
        assert_eq!(read.values, [4.0, 6.0]);
        let slice = Value::Case {
            case: "slice".to_owned(),
            fields: Vec::new(),
        };
        assert!(DistributionSamples::from_value(&slice).is_err());
        let nameless = Value::Case {
            case: "samples".to_owned(),
            fields: vec![("values".to_owned(), Value::List(vec![Value::Float(1.0)]))],
        };
        assert_eq!(
            DistributionSamples::from_value(&nameless).unwrap().label,
            ""
        );
        let empty = Value::Case {
            case: "samples".to_owned(),
            fields: Vec::new(),
        };
        assert!(
            DistributionSamples::from_value(&empty).is_err(),
            "samples without readings are refused"
        );
    }

    #[test]
    fn changed_readings_change_the_boxes_where_they_stand() {
        let mut chart = DistributionChart::from_props(
            "k",
            props(&[("Before", &[4.0, 6.0]), ("After", &[2.0, 1.0])]),
        );
        assert_eq!(observations(&chart, 0), [4.0, 6.0]);
        assert_eq!(observations(&chart, 1), [2.0, 1.0]);
        assert_eq!(chart.marks.style.kind, DistributionKind::BoxAndViolin);

        let next = DistributionChartProps {
            title: Some("Build times".to_owned()),
            kind: Some(DistributionKind::Violin),
            ..props(&[("Before", &[4.0, 6.0, 9.0]), ("After", &[2.0, 1.0, 0.0])])
        };
        assert!(chart.set_props(next), "taken in place, never built again");
        assert_eq!(observations(&chart, 0), [4.0, 6.0, 9.0]);
        assert_eq!(observations(&chart, 1), [2.0, 1.0, 0.0]);
        assert_eq!(chart.marks.frame.title, "Build times");
        assert_eq!(chart.marks.style.kind, DistributionKind::Violin);
    }

    #[test]
    fn a_distribution_of_no_samples_holds_none() {
        let chart = DistributionChart::from_props("k", props(&[]));
        assert!(chart.marks.series.is_empty());
    }

    #[test]
    fn each_measured_thing_takes_the_palette_colour_of_its_place_in_the_theme_that_draws_it() {
        let chart =
            DistributionChart::from_props("k", props(&[("Before", &[4.0]), ("After", &[2.0])]));
        for theme in [Theme::light(), Theme::dark()] {
            let palette = ChartPalette::from_theme(&theme);
            for (at, series) in chart.marks.series.iter().enumerate() {
                assert_eq!(series.color_in(&theme), palette.categorical(at));
            }
        }
    }
}
