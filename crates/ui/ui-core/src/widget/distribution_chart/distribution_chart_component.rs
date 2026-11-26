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
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
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
        marks.frame.empty = props.empty.unwrap_or_default();
        marks.style.kind = props.kind.unwrap_or(DistributionKind::BoxAndViolin);
        self.mark_dirty();
        true
    }
}
