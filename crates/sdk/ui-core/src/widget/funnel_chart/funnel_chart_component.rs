//! [`FunnelChart`] as a described component: [`FunnelStage`] as one stage and how many reach it.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use crate::component::{
    Component, Described, PropType, Type, Value, case_field, case_field_description,
};

use super::{FunnelChart, FunnelDirection};
use crate::widget::chart_data::ChartItem;

crate::component::choice!(FunnelDirection, "FunnelDirection", {
    Funnel => "funnel", Pyramid => "pyramid",
});

/// One stage of a sequence: what it is, and how many reach it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FunnelStage {
    pub label: String,
    pub value: f64,
}

impl PropType for FunnelStage {
    fn ty() -> Type {
        Type::Cases {
            name: "Stages".to_owned(),
        }
    }

    fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Case { case, fields } = value else {
            return Err(crate::component::wrong(&Self::ty(), value));
        };
        if case != "stage" {
            return Err(format!("a funnel chart's stages are `stage`, not `{case}`"));
        }
        Ok(Self {
            label: case_field(fields, "label")?.unwrap_or_default(),
            value: case_field(fields, "value")?.ok_or("`stage` needs `value`, a number")?,
        })
    }
}

/// The description of [`FunnelStage`], as the catalogue lists it in its `enums`.
#[must_use]
pub fn stages_description() -> EnumDescription {
    EnumDescription {
        name: "Stages".to_owned(),
        doc: "One stage of a sequence, and how many reach it.".to_owned(),
        cases: vec![CaseDescription {
            name: "stage".to_owned(),
            doc: "A stage as wide as `value` is of the widest, named `label`.".to_owned(),
            fields: vec![
                case_field_description("label", Type::Text, None, "What the stage is."),
                case_field_description(
                    "value",
                    Type::Float,
                    None,
                    "How many reach it; nothing at or below zero is drawn.",
                ),
            ],
        }],
    }
}

/// How many reach each stage of a sequence: the steps of a sign-up, a checkout, a build. Stages
/// narrow as fewer arrive, each with the share of the one before it, or widen as a pyramid; each
/// takes the next colour of the theme's palette.
#[derive(Component, Default)]
#[component(name = "FunnelChart", role = "Image", label = "title")]
pub struct FunnelChartProps {
    /// The stages from the first to the last:
    /// `.stage(label: "Visited", value: 1200.0)`.
    #[component(required, positional)]
    pub stages: Vec<FunnelStage>,
    /// What it shows, at its head and as assistive technology announces it.
    pub title: Option<String>,
    /// How the stages stand: `funnel` narrowing by how many reach each unless said, `pyramid`
    /// widening.
    pub direction: Option<FunnelDirection>,
}

/// The stages `props` draws, in the order each reaches the next.
fn plotted(props: &FunnelChartProps) -> Vec<ChartItem> {
    props
        .stages
        .iter()
        .enumerate()
        .map(|(at, stage)| {
            let id = if stage.label.is_empty() {
                at.to_string()
            } else {
                stage.label.clone()
            };
            ChartItem::new(id, stage.label.clone(), stage.value)
        })
        .collect()
}

impl Described for FunnelChart {
    type Props = FunnelChartProps;

    fn from_props(_key: &str, props: FunnelChartProps) -> Self {
        let mut chart = Self::new(Vec::new());
        chart.set_props(props);
        chart
    }

    /// Changed counts change the stages where they stand: the widget stays, with what the person
    /// is inspecting.
    fn set_props(&mut self, props: FunnelChartProps) -> bool {
        self.set_stages(plotted(&props));
        let marks = &mut self.marks;
        marks.frame.title = props.title.unwrap_or_default();
        marks.style.direction = props.direction.unwrap_or(FunnelDirection::Funnel);
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

    fn stage(label: &str, value: f64) -> Value {
        Value::Case {
            case: "stage".to_owned(),
            fields: vec![
                ("label".to_owned(), Value::Text(label.to_owned())),
                ("value".to_owned(), Value::Float(value)),
            ],
        }
    }

    fn props(stages: &[(&str, f64)]) -> FunnelChartProps {
        FunnelChartProps {
            stages: stages
                .iter()
                .map(|(label, value)| FunnelStage::from_value(&stage(label, *value)).unwrap())
                .collect(),
            ..FunnelChartProps::default()
        }
    }

    fn reached(chart: &FunnelChart) -> Vec<(String, f64)> {
        chart
            .marks
            .stages
            .iter()
            .map(|stage| (stage.label.clone(), stage.value))
            .collect()
    }

    #[test]
    fn a_stage_reads_from_its_case_and_needs_its_count() {
        let read = FunnelStage::from_value(&stage("Visited", 1200.0)).unwrap();
        assert_eq!((read.label.as_str(), read.value), ("Visited", 1200.0));
        let uncounted = Value::Case {
            case: "stage".to_owned(),
            fields: vec![("label".to_owned(), Value::Text("Visited".to_owned()))],
        };
        assert!(FunnelStage::from_value(&uncounted).is_err());
        let area = Value::Case {
            case: "area".to_owned(),
            fields: Vec::new(),
        };
        assert!(FunnelStage::from_value(&area).is_err());
    }

    #[test]
    fn changed_counts_change_the_stages_where_they_stand() {
        let mut chart =
            FunnelChart::from_props("k", props(&[("Visited", 1200.0), ("Signed up", 300.0)]));
        assert_eq!(
            reached(&chart),
            [
                ("Visited".to_owned(), 1200.0),
                ("Signed up".to_owned(), 300.0)
            ]
        );
        assert_eq!(chart.marks.style.direction, FunnelDirection::Funnel);

        let next = FunnelChartProps {
            title: Some("Sign-ups".to_owned()),
            direction: Some(FunnelDirection::Pyramid),
            ..props(&[
                ("Visited", 1500.0),
                ("Signed up", 400.0),
                ("Invited a team", 90.0),
            ])
        };
        assert!(chart.set_props(next), "taken in place, never built again");
        assert_eq!(chart.marks.stages.len(), 3);
        assert_eq!(reached(&chart)[2], ("Invited a team".to_owned(), 90.0));
        assert_eq!(chart.marks.frame.title, "Sign-ups");
        assert_eq!(chart.marks.style.direction, FunnelDirection::Pyramid);
    }

    #[test]
    fn each_stage_takes_the_palette_colour_of_its_place_in_the_theme_that_draws_it() {
        let chart =
            FunnelChart::from_props("k", props(&[("Visited", 1200.0), ("Signed up", 300.0)]));
        for theme in [Theme::light(), Theme::dark()] {
            let palette = ChartPalette::from_theme(&theme);
            for (at, stage) in chart.marks.stages.iter().enumerate() {
                assert!(
                    stage.color.is_none(),
                    "a view gives readings, never a colour"
                );
                assert_eq!(
                    crate::widget::funnel_chart::FunnelMarks::palette(&theme, at),
                    palette.categorical(at)
                );
            }
        }
    }

    #[test]
    fn a_funnel_of_no_stages_holds_none() {
        let chart = FunnelChart::from_props("k", props(&[]));
        assert!(chart.marks.stages.is_empty());
        assert!(chart.marks.valid().is_empty());
    }
}
