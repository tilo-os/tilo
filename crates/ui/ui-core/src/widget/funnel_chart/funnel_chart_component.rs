//! [`FunnelChart`] as a described component: [`FunnelStage`] as one stage and how many reach it.

use tilo_ui_graph::component::{CaseDescription, EnumDescription};

use tilo_ui_base::{
    component::Component, component::Described, component::PropType, component::Type,
    component::Value, component::case_field, component::case_field_description,
};

use super::{FunnelChart, FunnelDirection};
use crate::widget::chart_data::ChartItem;

tilo_ui_base::choice!(FunnelDirection, "FunnelDirection", {
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
            return Err(tilo_ui_base::component::wrong(&Self::ty(), value));
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
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
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
        marks.frame.empty = props.empty.unwrap_or_default();
        marks.style.direction = props.direction.unwrap_or(FunnelDirection::Funnel);
        self.mark_dirty();
        true
    }
}
