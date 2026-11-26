//! [`SunburstChart`] as a described component: a hierarchy as rings, and [`ChartPart`] as one of
//! its parts.

use crate::component::{Component, Described};
use crate::widget::{ChartBranch, ChartPart, hierarchy};

use super::{SunburstChart, SunburstNode};

/// A hierarchy as rings: how a whole divides, and how each part divides, down to the smallest
/// ones. Each ring is a level, a part as wide as its share of the one it belongs to, and each part
/// takes its colour from the theme; a person pointing at one reads its path and its value.
#[derive(Component, Default)]
#[component(name = "SunburstChart", role = "Image", label = "title")]
pub struct SunburstChartProps {
    /// The parts, each naming the part it belongs to: `.part(id: "build", parent: "ci",
    /// label: "Build", value: 12.0)`. A part nothing names as its parent stands at the centre
    /// ring.
    #[component(required, positional)]
    pub parts: Vec<ChartPart>,
    /// What it shows, over it and as assistive technology announces it.
    pub title: Option<String>,
    /// What the whole is, read at its centre ("128 hours").
    pub total: Option<String>,
}

/// The rings of `branches`, each with the rings under it.
fn subtree(branch: &ChartBranch<'_>) -> SunburstNode {
    let children = branch.children.iter().map(subtree).collect();
    SunburstNode::new(
        branch.part.known_as(),
        branch.part.label.clone(),
        branch.part.value,
    )
    .with_children(children)
}

/// The node the rings hang from: it draws no ring of its own, and is as large as everything under
/// it.
fn root(parts: &[ChartPart]) -> SunburstNode {
    SunburstNode::new("", "", 0.0).with_children(hierarchy(parts).iter().map(subtree).collect())
}

impl Described for SunburstChart {
    type Props = SunburstChartProps;

    fn from_props(_key: &str, props: SunburstChartProps) -> Self {
        let mut chart = Self::new(SunburstNode::new("", "", 0.0));
        chart.set_props(props);
        chart
    }

    /// Changed parts change the rings where they stand: the widget stays, with what the person is
    /// inspecting.
    fn set_props(&mut self, props: SunburstChartProps) -> bool {
        self.set_root(root(&props.parts));
        let marks = &mut self.marks;
        marks.frame.title = props.title.unwrap_or_default();
        marks.center_title = props.total.unwrap_or_default();
        marks.center_subtitle = String::new();
        self.mark_dirty();
        true
    }
}
