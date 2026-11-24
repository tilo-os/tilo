//! [`CartesianPlot`] as a described component: readings on numeric axes, and [`PlotPoints`] as one
//! series of them.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::component::{
    Component, Described, PropType, Type, Value, case_field, case_field_description,
};

use super::{CartesianPlot, MarkerShape, PlotDatum, PlotSeries, SeriesKind};

/// One series of a plot: its readings at each x, what it is of, and how its points are drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlotPoints {
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub label: String,
    pub kind: Curve,
    pub marker: Marker,
}

impl PropType for PlotPoints {
    fn ty() -> Type {
        Type::Cases {
            name: "Points".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(crate::component::wrong(&Self::ty(), value));
        };
        if case != "points" {
            return Err(format!("a plot's series is `points`, not `{case}`"));
        }
        Ok(Self {
            x: case_field(fields, "x")?.ok_or("`points` needs `x`, a list of positions")?,
            y: case_field(fields, "y")?.ok_or("`points` needs `y`, a list of readings")?,
            label: case_field(fields, "label")?.unwrap_or_default(),
            kind: case_field(fields, "kind")?.unwrap_or_default(),
            marker: case_field(fields, "marker")?.unwrap_or_default(),
        })
    }
}

/// The description of [`PlotPoints`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn points_description() -> EnumDescription {
    EnumDescription {
        name: "Points".to_owned(),
        doc: "One series of a plot: its readings at each x, and how they are drawn.".to_owned(),
        cases: vec![CaseDescription {
            name: "points".to_owned(),
            doc: "A series through the reading at each x, named `label` in the legend.".to_owned(),
            fields: vec![
                case_field_description(
                    "x",
                    Type::List(Box::new(Type::Float)),
                    None,
                    "Where each reading stands on the x axis, in order.",
                ),
                case_field_description(
                    "y",
                    Type::List(Box::new(Type::Float)),
                    None,
                    "The reading at each x, in the same order.",
                ),
                case_field_description(
                    "label",
                    Type::Text,
                    Some(Value::Text(String::new())),
                    "What is measured, in the legend.",
                ),
                case_field_description(
                    "kind",
                    Curve::ty(),
                    Some(Value::Choice("line".to_owned())),
                    "How the readings are drawn: a line unless said.",
                ),
                case_field_description(
                    "marker",
                    Marker::ty(),
                    Some(Value::Choice("circle".to_owned())),
                    "The shape of the dot at each point, where the kind draws one.",
                ),
            ],
        }],
    }
}

/// How a series' readings are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Curve {
    /// A straight line through the points.
    #[default]
    Line,
    /// A line through the points, bent smoothly.
    Smooth,
    /// A line that steps from each reading to the next.
    Step,
    /// The area between the line and the axis, washed in its colour.
    Area,
    /// The area under a smooth line.
    SmoothArea,
    /// The area under a stepped line.
    StepArea,
    /// Only the points, as dots.
    Scatter,
    /// The points as dots sized by their reading.
    Bubble,
    /// A bar at each point.
    Bar,
    /// A stem from the axis to each point.
    Lollipop,
}

impl Curve {
    fn kind(self) -> SeriesKind {
        match self {
            Self::Line => SeriesKind::Line,
            Self::Smooth => SeriesKind::Smooth,
            Self::Step => SeriesKind::Step,
            Self::Area => SeriesKind::Area,
            Self::SmoothArea => SeriesKind::SmoothArea,
            Self::StepArea => SeriesKind::StepArea,
            Self::Scatter => SeriesKind::Scatter,
            Self::Bubble => SeriesKind::Bubble,
            Self::Bar => SeriesKind::Bar {
                width: f64::from(crate::tokens::Spacing::default().s4),
            },
            Self::Lollipop => SeriesKind::Lollipop,
        }
    }
}

crate::component::choice!(Curve, "Curve", {
    Line => "line",
    Smooth => "smooth",
    Step => "step",
    Area => "area",
    SmoothArea => "smooth_area",
    StepArea => "step_area",
    Scatter => "scatter",
    Bubble => "bubble",
    Bar => "bar",
    Lollipop => "lollipop",
});

/// The shape of the dot a series draws at each point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Marker {
    #[default]
    Circle,
    Square,
    Diamond,
}

impl Marker {
    fn shape(self) -> MarkerShape {
        match self {
            Self::Circle => MarkerShape::Circle,
            Self::Square => MarkerShape::Square,
            Self::Diamond => MarkerShape::Diamond,
        }
    }
}

crate::component::choice!(Marker, "Marker", {
    Circle => "circle", Square => "square", Diamond => "diamond",
});

/// Readings on numeric axes: a trend over time, a relationship between two measures, a
/// comparison. Each series takes the next colour of the theme's palette and is named in a legend;
/// it draws as a line, an area, steps, points, bars or stems.
#[derive(Component, Default)]
#[component(name = "CartesianPlot", role = "Image", label = "title")]
pub struct CartesianPlotProps {
    /// What is measured, each with its readings: `.points(x: [1.0, 2.0], y: [4.0, 6.0], label: "Latency")`.
    #[component(required, positional)]
    pub points: Vec<PlotPoints>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
    /// What the x axis measures, under it ("Minute", "Distance").
    pub x_title: Option<String>,
    /// What the y axis measures, beside it ("Milliseconds").
    pub y_title: Option<String>,
}

/// What `props` plot: each series' readings in the theme's palette colour of its place, so the
/// colours follow the theme the plot is drawn with.
fn plotted(points: &[PlotPoints]) -> Vec<PlotSeries> {
    points
        .iter()
        .enumerate()
        .map(|(at, line)| {
            let data = line
                .x
                .iter()
                .zip(line.y.iter())
                .enumerate()
                .map(|(index, (x, y))| PlotDatum::new(format!("{at}-{index}"), *x, *y))
                .collect();
            // A legend entry a person switched off stays off while its series keeps its name.
            let id = if line.label.is_empty() {
                at.to_string()
            } else {
                line.label.clone()
            };
            PlotSeries::from_palette(id, line.label.clone(), at, data)
                .kind(line.kind.kind())
                .marker_shape(line.marker.shape())
        })
        .collect()
}

impl CartesianPlot {
    /// Everything of `props` but its readings.
    fn wear(&mut self, props: CartesianPlotProps) {
        let marks = &mut self.marks;
        marks.frame.title = props.title.unwrap_or_default();
        marks.style.show_legend = props.points.iter().any(|line| !line.label.is_empty());
        marks.style.x_axis.title = props.x_title.unwrap_or_default();
        marks.style.y_axis.title = props.y_title.unwrap_or_default();
        self.mark_dirty();
    }
}

impl Described for CartesianPlot {
    type Props = CartesianPlotProps;

    /// The first readings stand at once: nothing was there for them to move from.
    fn from_props(_key: &str, props: CartesianPlotProps) -> Self {
        let mut chart = Self::new(Vec::new());
        chart.set_series_immediate(plotted(&props.points));
        chart.wear(props);
        chart
    }

    /// Changed readings move to their new places on the toolkit's own list change: the widget
    /// stays, with the series the person switched off in its legend.
    fn set_props(&mut self, props: CartesianPlotProps) -> bool {
        self.set_series(plotted(&props.points));
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
    use crate::chart_palette::ChartPalette;
    use crate::theme::Theme;

    fn points(label: &str, x: &[f64], y: &[f64]) -> Value {
        Value::Case {
            case: "points".to_owned(),
            fields: vec![
                (
                    "x".to_owned(),
                    Value::List(x.iter().copied().map(Value::Float).collect()),
                ),
                (
                    "y".to_owned(),
                    Value::List(y.iter().copied().map(Value::Float).collect()),
                ),
                ("label".to_owned(), Value::Text(label.to_owned())),
            ],
        }
    }

    fn props(lines: &[(&str, &[f64], &[f64])]) -> CartesianPlotProps {
        CartesianPlotProps {
            points: lines
                .iter()
                .map(|(label, x, y)| PlotPoints::from_value(&points(label, x, y)).unwrap())
                .collect(),
            ..CartesianPlotProps::default()
        }
    }

    fn readings(chart: &CartesianPlot, series: usize) -> Vec<(f64, f64)> {
        chart.marks.series[series]
            .data
            .iter()
            .map(|datum| (datum.x, datum.y))
            .collect()
    }

    #[test]
    fn a_series_reads_from_its_case_and_no_other_case_is_one() {
        let read = PlotPoints::from_value(&points("Latency", &[1.0, 2.0], &[4.0, 6.0])).unwrap();
        assert_eq!(read.label, "Latency");
        assert_eq!(read.x, [1.0, 2.0]);
        assert_eq!(read.y, [4.0, 6.0]);
        assert_eq!(read.kind, Curve::Line);
        assert_eq!(read.marker, Marker::Circle);
        let bars = Value::Case {
            case: "bars".to_owned(),
            fields: Vec::new(),
        };
        assert!(PlotPoints::from_value(&bars).is_err());
        let no_y = Value::Case {
            case: "points".to_owned(),
            fields: vec![("x".to_owned(), Value::List(vec![Value::Float(1.0)]))],
        };
        assert!(
            PlotPoints::from_value(&no_y).is_err(),
            "points need readings on both axes"
        );
    }

    #[test]
    fn changed_readings_move_where_they_stand() {
        let mut chart =
            CartesianPlot::from_props("k", props(&[("Latency", &[1.0, 2.0], &[4.0, 6.0])]));
        assert_eq!(readings(&chart, 0), [(1.0, 4.0), (2.0, 6.0)]);
        assert!(chart.marks.style.show_legend);

        let next = CartesianPlotProps {
            title: Some("Request latency".to_owned()),
            x_title: Some("Minute".to_owned()),
            y_title: Some("Milliseconds".to_owned()),
            ..props(&[
                ("Reads", &[1.0, 2.0, 3.0], &[18.0, 12.0, 9.0]),
                ("Writes", &[1.0, 2.0, 3.0], &[6.0, 8.0, 7.0]),
            ])
        };
        assert!(chart.set_props(next), "taken in place, never built again");
        assert_eq!(readings(&chart, 0), [(1.0, 18.0), (2.0, 12.0), (3.0, 9.0)]);
        assert_eq!(readings(&chart, 1), [(1.0, 6.0), (2.0, 8.0), (3.0, 7.0)]);
        assert_eq!(chart.marks.frame.title, "Request latency");
        assert_eq!(chart.marks.style.x_axis.title, "Minute");
        assert_eq!(chart.marks.style.y_axis.title, "Milliseconds");
    }

    #[test]
    fn a_plot_of_one_unnamed_series_shows_no_legend() {
        let chart = CartesianPlot::from_props("k", props(&[("", &[1.0], &[2.0])]));
        assert!(!chart.marks.style.show_legend);
    }

    #[test]
    fn each_series_takes_the_palette_colour_of_its_place_in_the_theme_that_draws_it() {
        let chart = CartesianPlot::from_props(
            "k",
            props(&[("Reads", &[1.0], &[4.0]), ("Writes", &[1.0], &[2.0])]),
        );
        for theme in [Theme::light(), Theme::dark()] {
            let palette = ChartPalette::from_theme(&theme);
            for (at, series) in chart.marks.series.iter().enumerate() {
                assert_eq!(series.color_in(&theme), palette.categorical(at));
            }
            assert_ne!(
                chart.marks.series[0].color_in(&theme),
                chart.marks.series[1].color_in(&theme),
                "two series are told apart"
            );
        }
    }
}
