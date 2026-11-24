//! [`RangeChart`] as a described component: what takes a span of something, and [`RangeSpan`] as
//! one of those spans.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::component::{
    Component, Described, PropType, Type, Value, case_field, case_field_description,
};
use crate::widget::chart_labels::format_compact;

use super::{RangeBar, RangeChart, RangeStyle};

/// One row of a range chart: what takes the span, from where to where, and how far it has come.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RangeSpan {
    pub label: String,
    pub start: f64,
    pub end: f64,
    /// How far it has come, from zero to one; drawn as a fill in the bar when the kind shows it.
    pub progress: Option<f64>,
}

impl PropType for RangeSpan {
    fn ty() -> Type {
        Type::Cases {
            name: "Range".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(crate::component::wrong(&Self::ty(), value));
        };
        if case != "range" {
            return Err(format!("a row of a range chart is a `range`, not `{case}`"));
        }
        Ok(Self {
            label: case_field(fields, "label")?.ok_or("`range` needs `label`, what it is")?,
            start: case_field(fields, "start")?.ok_or("`range` needs `start`, where it begins")?,
            end: case_field(fields, "end")?.ok_or("`range` needs `end`, where it ends")?,
            progress: case_field(fields, "progress")?.flatten(),
        })
    }
}

/// The description of [`RangeSpan`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn range_description() -> EnumDescription {
    EnumDescription {
        name: "Range".to_owned(),
        doc: "One row of a range chart: what takes the span, from where to where.".to_owned(),
        cases: vec![CaseDescription {
            name: "range".to_owned(),
            doc: "What `label` is from `start` to `end`; a single point when the two are the same."
                .to_owned(),
            fields: vec![
                case_field_description("label", Type::Text, None, "What takes the span."),
                case_field_description("start", Type::Float, None, "Where the span begins."),
                case_field_description(
                    "end",
                    Type::Float,
                    None,
                    "Where the span ends; the same as `start` draws a milestone.",
                ),
                case_field_description(
                    "progress",
                    Type::Option(Box::new(Type::Float)),
                    Some(Value::None),
                    "How far it has come, from zero to one, filled in the bar by the `plan` kind.",
                ),
            ],
        }],
    }
}

/// What a range chart shows: spans over time, a plan's progress, numeric intervals, or points in
/// time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RangeKind {
    /// Spans over time: when each thing happens, one row each.
    #[default]
    Timeline,
    /// A plan's rows, each filled to how far its work has come.
    Plan,
    /// Numeric intervals, such as the hours a room is booked.
    Intervals,
    /// Points in time, each drawn as a diamond, for when something happened.
    Milestones,
}

crate::component::choice!(RangeKind, "RangeKind", {
    Timeline => "timeline",
    Plan => "plan",
    Intervals => "intervals",
    Milestones => "milestones",
});

impl RangeKind {
    /// Whether a row's bar is filled to its progress.
    fn shows_progress(self) -> bool {
        self == Self::Plan
    }

    /// Whether the rows alternate their band.
    fn bands_rows(self) -> bool {
        matches!(self, Self::Timeline | Self::Plan)
    }
}

/// What takes a span of something, one row each: when things happen on a timeline, a plan's rows
/// and how far each has come, the hours a room is booked, or the moments something happened. Each
/// row is named before its bar and read out with its interval.
#[derive(Component, Default)]
#[component(name = "RangeChart", role = "Image", label = "title")]
pub struct RangeChartProps {
    /// The rows, top to bottom: `.range(label: "Design review", start: 2.0, end: 5.0, progress: 0.6)`.
    #[component(required, positional)]
    pub ranges: Vec<RangeSpan>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
    /// What it shows: `timeline` spans unless said, a `plan` whose rows fill to their progress,
    /// `intervals` between numbers, or `milestones` at points.
    pub kind: Option<RangeKind>,
    /// What the axis under the rows measures ("October", "Hour").
    pub axis: Option<String>,
    /// What an axis value counts, written after it ("min", "°C").
    pub unit: Option<String>,
}

/// The rows `spans` are drawn as.
fn spans(spans: &[RangeSpan]) -> Vec<RangeBar> {
    spans
        .iter()
        .enumerate()
        .map(|(at, span)| {
            let id = if span.label.is_empty() {
                at.to_string()
            } else {
                span.label.clone()
            };
            let bar = RangeBar::new(id, span.label.clone(), span.start, span.end);
            match span.progress {
                Some(progress) => bar.with_progress(progress),
                None => bar,
            }
        })
        .collect()
}

impl RangeChart {
    /// Everything of `props` but its rows.
    fn wear(&mut self, props: RangeChartProps) {
        let kind = props.kind.unwrap_or_default();
        let marks = &mut self.marks;
        marks.frame.title = props.title.unwrap_or_default();
        marks.axis_title = props.axis.unwrap_or_default();
        marks.style = RangeStyle {
            show_progress: kind.shows_progress(),
            show_row_bands: kind.bands_rows(),
            ..RangeStyle::default()
        };
        marks.formatter = props.unit.map(|unit| {
            let format = move |value: f64| format!("{} {unit}", format_compact(value));
            Box::new(format) as Box<dyn Fn(f64) -> String>
        });
        self.mark_dirty();
    }
}

impl Described for RangeChart {
    type Props = RangeChartProps;

    fn from_props(_key: &str, props: RangeChartProps) -> Self {
        let mut chart = Self::new(Vec::new());
        chart.set_props(props);
        chart
    }

    /// Changed rows stand where the old ones stood: the widget stays, with what the person is
    /// inspecting.
    fn set_props(&mut self, props: RangeChartProps) -> bool {
        self.marks.bars = spans(&props.ranges);
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

    fn range(label: &str, start: f64, end: f64) -> Value {
        Value::Case {
            case: "range".to_owned(),
            fields: vec![
                ("label".to_owned(), Value::Text(label.to_owned())),
                ("start".to_owned(), Value::Float(start)),
                ("end".to_owned(), Value::Float(end)),
            ],
        }
    }

    fn props(rows: &[(&str, f64, f64)]) -> RangeChartProps {
        RangeChartProps {
            ranges: rows
                .iter()
                .map(|(label, start, end)| {
                    RangeSpan::from_value(&range(label, *start, *end)).unwrap()
                })
                .collect(),
            ..RangeChartProps::default()
        }
    }

    fn rows(chart: &RangeChart) -> Vec<(String, f64, f64)> {
        chart
            .marks
            .bars
            .iter()
            .map(|bar| (bar.label.clone(), bar.start, bar.end))
            .collect()
    }

    #[test]
    fn a_row_reads_from_its_case_and_no_other_case_is_one() {
        let read = RangeSpan::from_value(&range("Design review", 2.0, 5.0)).unwrap();
        assert_eq!(
            (read.label.as_str(), read.start, read.end, read.progress),
            ("Design review", 2.0, 5.0, None)
        );
        let candle = Value::Case {
            case: "candle".to_owned(),
            fields: Vec::new(),
        };
        assert!(RangeSpan::from_value(&candle).is_err());
        let no_end = Value::Case {
            case: "range".to_owned(),
            fields: vec![
                ("label".to_owned(), Value::Text("Review".to_owned())),
                ("start".to_owned(), Value::Float(2.0)),
            ],
        };
        assert!(
            RangeSpan::from_value(&no_end).is_err(),
            "a span needs its end"
        );
    }

    #[test]
    fn changed_rows_stand_where_they_stood() {
        let mut chart = RangeChart::from_props("k", props(&[("Design review", 2.0, 5.0)]));
        assert_eq!(rows(&chart), [("Design review".to_owned(), 2.0, 5.0)]);
        assert!(
            !chart.marks.style.show_progress,
            "a timeline fills no bar to its progress"
        );

        let next = RangeChartProps {
            title: Some("Release plan".to_owned()),
            kind: Some(RangeKind::Plan),
            axis: Some("Week".to_owned()),
            unit: Some("w".to_owned()),
            ..props(&[("Design review", 2.0, 5.0), ("Build", 5.0, 7.0)])
        };
        assert!(chart.set_props(next), "taken in place, never built again");
        assert_eq!(chart.marks.bars.len(), 2);
        assert_eq!(chart.marks.bars[1].label, "Build");
        assert_eq!(chart.marks.frame.title, "Release plan");
        assert_eq!(chart.marks.axis_title, "Week");
        assert!(chart.marks.style.show_progress);
        assert_eq!(chart.marks.format(6.0), "6 w");
    }

    #[test]
    fn milestones_are_rows_without_a_band_and_a_point_reads_as_a_milestone() {
        let chart = RangeChart::from_props(
            "k",
            RangeChartProps {
                kind: Some(RangeKind::Milestones),
                ..props(&[("Shipped", 3.0, 3.0)])
            },
        );
        assert!(!chart.marks.style.show_row_bands);
        assert_eq!(rows(&chart), [("Shipped".to_owned(), 3.0, 3.0)]);
    }
}
