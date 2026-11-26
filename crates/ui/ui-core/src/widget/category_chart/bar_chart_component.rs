//! [`CategoryChart`] as a described component: bars over named categories, and [`BarSeries`] as the
//! bars one measured thing has across them.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::component::{
    Component, Described, PropType, Type, Value, case_field, case_field_description,
};

use super::{Category, CategoryChart, CategoryLayout, CategorySeries, CategoryValue};

/// The bars of one measured thing: what it is, and its reading in each category, in the
/// categories' order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarSeries {
    pub label: String,
    pub values: Vec<f64>,
}

impl PropType for BarSeries {
    fn ty() -> Type {
        Type::Cases {
            name: "Bars".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(crate::component::wrong(&Self::ty(), value));
        };
        if case != "bars" {
            return Err(format!("a bar chart's readings are `bars`, not `{case}`"));
        }
        Ok(Self {
            label: case_field(fields, "label")?.unwrap_or_default(),
            values: case_field(fields, "values")?
                .ok_or("`bars` needs `values`, a list of numbers")?,
        })
    }
}

/// The description of [`BarSeries`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn bars_description() -> EnumDescription {
    EnumDescription {
        name: "Bars".to_owned(),
        doc: "The bars of one measured thing: its reading in each category.".to_owned(),
        cases: vec![CaseDescription {
            name: "bars".to_owned(),
            doc: "A bar in each category for `values`, named `label` in the chart's legend."
                .to_owned(),
            fields: vec![
                case_field_description(
                    "values",
                    Type::List(Box::new(Type::Float)),
                    None,
                    "The readings, one for each category, in the categories' order.",
                ),
                case_field_description(
                    "label",
                    Type::Text,
                    Some(Value::Text(String::new())),
                    "What is measured, in the legend.",
                ),
            ],
        }],
    }
}

crate::choice!(CategoryLayout, "BarLayout", {
    Grouped => "grouped", Stacked => "stacked", Percent => "share", Waterfall => "waterfall",
});

/// Bars over named categories: how much there is of each, to compare. Several measured things
/// stand beside each other in a category, stack on each other, or stack as shares of the category's
/// whole; each takes the next colour of the theme's palette and is named in a legend.
#[derive(Component, Default)]
#[component(name = "BarChart", role = "Image", label = "title")]
pub struct BarChartProps {
    /// What is measured, each with its reading in every category:
    /// `.bars(values: done, label: "Done")`.
    #[component(required, positional)]
    pub bars: Vec<BarSeries>,
    /// What each group of bars is of, in order, under it: a week, a team, a stage.
    #[component(required)]
    pub categories: Vec<String>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
    /// How several measured things share a category: `grouped` beside each other unless said,
    /// `stacked` on each other, `share` stacked as parts of the category's whole, or `waterfall`
    /// for the first measured thing's rises and falls, each standing on the one before.
    pub layout: Option<CategoryLayout>,
    /// Each bar's reading written at it.
    pub readings: bool,
    /// What a reading counts, written after each value a person pointing at a category reads
    /// ("GB", "ms", "/s").
    pub unit: Option<String>,
}

/// What `props` plot: a category for each name, and for each measured thing its bars in the
/// theme's palette colour of its place, so the colours follow the theme the chart is drawn with.
fn plotted(props: &BarChartProps) -> (Vec<Category>, Vec<CategorySeries>) {
    let categories = props
        .categories
        .iter()
        .enumerate()
        .map(|(at, label)| Category::new(at.to_string(), label.clone()))
        .collect();
    let series = props
        .bars
        .iter()
        .enumerate()
        .map(|(at, bars)| {
            // A legend entry a person switched off stays off while its bars keep their name.
            let id = if bars.label.is_empty() {
                at.to_string()
            } else {
                bars.label.clone()
            };
            CategorySeries::from_palette(
                id,
                bars.label.clone(),
                at,
                bars.values
                    .iter()
                    .copied()
                    .map(CategoryValue::new)
                    .collect(),
            )
        })
        .collect();
    (categories, series)
}

impl Described for CategoryChart {
    type Props = BarChartProps;

    fn from_props(_key: &str, props: BarChartProps) -> Self {
        let mut chart = Self::new(Vec::new(), Vec::new());
        chart.set_props(props);
        chart
    }

    /// Changed readings change the bars where they stand: the widget stays, with what the person
    /// is inspecting and what they switched off in its legend.
    fn set_props(&mut self, props: BarChartProps) -> bool {
        let (categories, series) = plotted(&props);
        self.set_data(categories, series);
        let marks = &mut self.marks;
        marks.frame.title = props.title.unwrap_or_default();
        marks.frame.empty = props.empty.unwrap_or_default();
        marks.style.layout = props.layout.unwrap_or(CategoryLayout::Grouped);
        marks.style.show_values = props.readings;
        marks.style.unit = props.unit.unwrap_or_default();
        marks.style.show_legend = props.bars.iter().any(|bars| !bars.label.is_empty());
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

    fn bars(label: &str, values: &[f64]) -> Value {
        Value::Case {
            case: "bars".to_owned(),
            fields: vec![
                (
                    "values".to_owned(),
                    Value::List(values.iter().copied().map(Value::Float).collect()),
                ),
                ("label".to_owned(), Value::Text(label.to_owned())),
            ],
        }
    }

    fn props(readings: &[(&str, &[f64])], categories: &[&str]) -> BarChartProps {
        BarChartProps {
            bars: readings
                .iter()
                .map(|(label, values)| BarSeries::from_value(&bars(label, values)).unwrap())
                .collect(),
            categories: categories.iter().map(|name| (*name).to_owned()).collect(),
            ..BarChartProps::default()
        }
    }

    fn readings(chart: &CategoryChart, series: usize) -> Vec<f64> {
        chart.marks.series[series]
            .values
            .iter()
            .map(|value| value.value)
            .collect()
    }

    #[test]
    fn bars_read_from_their_case_and_no_other_case_is_one() {
        let read = BarSeries::from_value(&bars("Closed", &[4.0, 6.0])).unwrap();
        assert_eq!(read.label, "Closed");
        assert_eq!(read.values, [4.0, 6.0]);
        let line = Value::Case {
            case: "series".to_owned(),
            fields: Vec::new(),
        };
        assert!(BarSeries::from_value(&line).is_err());
        let nameless = Value::Case {
            case: "bars".to_owned(),
            fields: vec![("values".to_owned(), Value::List(vec![Value::Float(1.0)]))],
        };
        assert_eq!(BarSeries::from_value(&nameless).unwrap().label, "");
        let empty = Value::Case {
            case: "bars".to_owned(),
            fields: Vec::new(),
        };
        assert!(
            BarSeries::from_value(&empty).is_err(),
            "bars without readings are refused"
        );
    }

    #[test]
    fn changed_readings_change_the_bars_where_they_stand() {
        let mut chart = CategoryChart::from_props(
            "k",
            props(
                &[("Closed", &[4.0, 6.0]), ("Opened", &[2.0, 1.0])],
                &["Week 1", "Week 2"],
            ),
        );
        assert_eq!(readings(&chart, 0), [4.0, 6.0]);
        assert_eq!(readings(&chart, 1), [2.0, 1.0]);
        assert_eq!(chart.marks.categories[1].label, "Week 2");
        assert_eq!(chart.marks.style.layout, CategoryLayout::Grouped);
        assert!(chart.marks.style.show_legend);

        let next = BarChartProps {
            title: Some("Issues each week".to_owned()),
            layout: Some(CategoryLayout::Stacked),
            readings: true,
            ..props(
                &[("Closed", &[4.0, 6.0, 9.0]), ("Opened", &[2.0, 1.0, 0.0])],
                &["Week 1", "Week 2", "Week 3"],
            )
        };
        assert!(chart.set_props(next), "taken in place, never built again");
        assert_eq!(readings(&chart, 0), [4.0, 6.0, 9.0]);
        assert_eq!(readings(&chart, 1), [2.0, 1.0, 0.0]);
        assert_eq!(chart.marks.categories.len(), 3);
        assert_eq!(chart.marks.frame.title, "Issues each week");
        assert_eq!(chart.marks.style.layout, CategoryLayout::Stacked);
        assert!(chart.marks.style.show_values);
    }

    #[test]
    fn a_chart_of_one_unnamed_thing_shows_no_legend() {
        let chart = CategoryChart::from_props("k", props(&[("", &[1.0, 2.0])], &["a", "b"]));
        assert!(!chart.marks.style.show_legend);
    }

    #[test]
    fn each_measured_thing_takes_the_palette_colour_of_its_place_in_the_theme_that_draws_it() {
        let chart = CategoryChart::from_props(
            "k",
            props(&[("Closed", &[4.0]), ("Opened", &[2.0])], &["Week 1"]),
        );
        for theme in [Theme::light(), Theme::dark()] {
            let palette = ChartPalette::from_theme(&theme);
            for (at, series) in chart.marks.series.iter().enumerate() {
                assert_eq!(series.color_in(&theme), palette.categorical(at));
            }
            assert_ne!(
                chart.marks.series[0].color_in(&theme),
                chart.marks.series[1].color_in(&theme),
                "two measured things are told apart"
            );
        }
    }
}
