//! [`RangeChart`] as a described component: what takes a span of something, and [`RangeSpan`] as
//! one of those spans.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::widget::chart_labels::format_compact;
use tilo_ui_base::{
    component::Component, component::Described, component::PropType, component::Type,
    component::Value, component::case_field, component::case_field_description,
};

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
            return Err(tilo_ui_base::component::wrong(&Self::ty(), value));
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

tilo_ui_base::choice!(RangeKind, "RangeKind", {
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
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
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
        marks.frame.empty = props.empty.unwrap_or_default();
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
