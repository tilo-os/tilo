//! [`PieChart`] as a described component: the parts of one whole, and [`PiePart`] as one of them.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use tilo_ui_base::{
    component::Component, component::Described, component::PropType, component::Type,
    component::Value, component::case_field, component::case_field_description,
};

use super::{PieChart, PieSlice, PieStyle};

/// One part of the whole a pie shows: what it is of, and how much.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PiePart {
    pub label: String,
    pub value: f64,
}

impl PropType for PiePart {
    fn ty() -> Type {
        Type::Cases {
            name: "Slice".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(tilo_ui_base::component::wrong(&Self::ty(), value));
        };
        if case != "slice" {
            return Err(format!("a pie's part is a `slice`, not `{case}`"));
        }
        Ok(Self {
            label: case_field(fields, "label")?.ok_or("`slice` needs `label`, what it is of")?,
            value: case_field(fields, "value")?.ok_or("`slice` needs `value`, a number")?,
        })
    }
}

/// The description of [`PiePart`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn slice_description() -> EnumDescription {
    EnumDescription {
        name: "Slice".to_owned(),
        doc: "One part of the whole a pie shows.".to_owned(),
        cases: vec![CaseDescription {
            name: "slice".to_owned(),
            doc: "A slice as large as `value` is of the sum, named `label` in the legend."
                .to_owned(),
            fields: vec![
                case_field_description("label", Type::Text, None, "What the part is of."),
                case_field_description(
                    "value",
                    Type::Float,
                    None,
                    "How much of it there is; nothing below zero is drawn.",
                ),
            ],
        }],
    }
}

/// The parts of one whole as the slices of a pie: how a total divides. Each slice takes the next
/// colour of the theme's palette and is named in a legend with its share.
#[derive(Component, Default)]
#[component(name = "PieChart", role = "Image", label = "title")]
pub struct PieChartProps {
    /// The parts, in order: `.slice(label: "Open", value: 12.0)`.
    #[component(required, positional)]
    pub slices: Vec<PiePart>,
    /// What it shows, as assistive technology announces it.
    pub title: Option<String>,
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
    /// A ring rather than a full pie, with `total` read at its centre.
    pub ring: bool,
    /// What a ring reads at its centre: the whole, as it reads ("128 issues").
    pub total: Option<String>,
    /// What a part's value counts, written after it where a person pointing at it reads it
    /// ("GB", "h").
    pub unit: Option<String>,
}

/// The slices `parts` are drawn as, each in the theme's palette colour of its place, so the
/// colours follow the theme the pie is drawn with.
fn slices(parts: &[PiePart]) -> Vec<PieSlice> {
    parts
        .iter()
        .enumerate()
        .map(|(at, part)| {
            let id = if part.label.is_empty() {
                at.to_string()
            } else {
                part.label.clone()
            };
            PieSlice::from_palette(id, part.label.clone(), part.value, at)
        })
        .collect()
}

impl PieChart {
    /// Everything of `props` but its slices.
    fn wear(&mut self, props: PieChartProps) {
        self.marks.style.inner_radius = if props.ring {
            PieStyle::default().inner_radius
        } else {
            0.0
        };
        self.marks.frame.title = props.title.unwrap_or_default();
        self.marks.frame.empty = props.empty.unwrap_or_default();
        self.marks.unit = props.unit.unwrap_or_default();
        let total = props.total.filter(|_| props.ring).unwrap_or_default();
        self.set_center(total, String::new());
    }
}

impl Described for PieChart {
    type Props = PieChartProps;

    /// The first slices stand at once: nothing was there for them to move from.
    fn from_props(_key: &str, props: PieChartProps) -> Self {
        let mut chart = Self::new(Vec::new()).with_legend();
        chart.set_slices_immediate(slices(&props.slices));
        chart.wear(props);
        chart
    }

    /// Changed parts move to their new shares on the toolkit's own list change: the widget stays,
    /// with what the person is inspecting.
    fn set_props(&mut self, props: PieChartProps) -> bool {
        self.set_slices(slices(&props.slices));
        self.wear(props);
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
    use tilo_ui_base::chart_palette::ChartPalette;
    use tilo_ui_base::theme::Theme;

    fn slice(label: &str, value: f64) -> Value {
        Value::Case {
            case: "slice".to_owned(),
            fields: vec![
                ("label".to_owned(), Value::Text(label.to_owned())),
                ("value".to_owned(), Value::Float(value)),
            ],
        }
    }

    fn props(parts: &[(&str, f64)]) -> PieChartProps {
        PieChartProps {
            slices: parts
                .iter()
                .map(|(label, value)| PiePart::from_value(&slice(label, *value)).unwrap())
                .collect(),
            ..PieChartProps::default()
        }
    }

    fn shares(chart: &PieChart) -> Vec<(String, f64)> {
        chart
            .marks
            .slices
            .iter()
            .map(|slice| (slice.label.clone(), slice.value))
            .collect()
    }

    #[test]
    fn a_slice_reads_from_its_case_and_needs_what_it_is_of_and_how_much() {
        let read = PiePart::from_value(&slice("Open", 3.0)).unwrap();
        assert_eq!((read.label.as_str(), read.value), ("Open", 3.0));
        let unnamed = Value::Case {
            case: "slice".to_owned(),
            fields: vec![("value".to_owned(), Value::Float(3.0))],
        };
        assert!(PiePart::from_value(&unnamed).is_err());
        let bars = Value::Case {
            case: "bars".to_owned(),
            fields: Vec::new(),
        };
        assert!(PiePart::from_value(&bars).is_err());
    }

    #[test]
    fn the_first_parts_stand_at_once_and_changed_parts_move_where_they_stand() {
        let mut chart = PieChart::from_props("k", props(&[("Open", 3.0), ("Closed", 5.0)]));
        assert_eq!(
            shares(&chart),
            [("Open".to_owned(), 3.0), ("Closed".to_owned(), 5.0)]
        );
        assert!(
            chart.marks.transition_from.is_none(),
            "nothing stood there for the first parts to move from"
        );
        assert!(chart.marks.legend, "every slice is named");
        assert!(
            chart.marks.style.inner_radius.abs() < f64::EPSILON,
            "a full pie"
        );

        let next = PieChartProps {
            ring: true,
            total: Some("9 issues".to_owned()),
            title: Some("Issues".to_owned()),
            ..props(&[("Open", 2.0), ("Closed", 7.0)])
        };
        assert!(chart.set_props(next), "taken in place, never built again");
        assert_eq!(
            shares(&chart),
            [("Open".to_owned(), 2.0), ("Closed".to_owned(), 7.0)]
        );
        assert!(chart.marks.style.inner_radius > 0.0, "a ring");
        assert_eq!(chart.marks.center_title, "9 issues");
        assert_eq!(chart.marks.frame.title, "Issues");
    }

    #[test]
    fn a_full_pie_reads_no_total_at_a_centre_it_does_not_have() {
        let chart = PieChart::from_props(
            "k",
            PieChartProps {
                total: Some("8".to_owned()),
                ..props(&[("Open", 3.0), ("Closed", 5.0)])
            },
        );
        assert_eq!(chart.marks.center_title, "");
    }

    #[test]
    fn each_slice_takes_the_palette_colour_of_its_place_in_the_theme_that_draws_it() {
        let chart = PieChart::from_props("k", props(&[("Open", 3.0), ("Closed", 5.0)]));
        for theme in [Theme::light(), Theme::dark()] {
            let palette = ChartPalette::from_theme(&theme);
            for (at, slice) in chart.marks.slices.iter().enumerate() {
                assert_eq!(slice.color_in(&theme), palette.categorical(at));
            }
        }
    }
}
