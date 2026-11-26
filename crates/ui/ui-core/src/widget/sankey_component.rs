//! [`SankeyChart`] as a described component: nodes in stages, and the weighted links between them.

use tilo_ui_base::{component::Component, component::Described};
use crate::widget::{ChartLink, ChartNode};

use super::SankeyChart;

/// Where a quantity moves between stages: visits that become trials, hours that become work, money
/// that becomes cost. Each stage is a column of nodes as tall as what passes through them, each
/// link a ribbon as thick as its share, its nodes and links named where they fit; a person
/// pointing at one reads what it carries.
#[derive(Component, Default)]
#[component(name = "SankeyChart", role = "Image", label = "title")]
pub struct SankeyChartProps {
    /// The stages and sources, in order: `.node(id: "visits", label: "Visits")`. Its stage is the
    /// longest chain of links that leads to it.
    #[component(required, positional)]
    pub nodes: Vec<ChartNode>,
    /// What moves between them: `.link(from: "visits", to: "signup", value: 72.0)`, `from` and
    /// `to` naming nodes by their ids.
    #[component(required)]
    pub links: Vec<ChartLink>,
    /// What it shows, over it and as assistive technology announces it.
    pub title: Option<String>,
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
}

impl Described for SankeyChart {
    type Props = SankeyChartProps;

    fn from_props(_key: &str, props: SankeyChartProps) -> Self {
        let mut chart = Self::new(Vec::new(), Vec::new());
        chart.set_props(props);
        chart
    }

    /// Changed nodes and links change the flow where it stands: the widget stays, with what the
    /// person is inspecting.
    fn set_props(&mut self, props: SankeyChartProps) -> bool {
        self.set_data(props.nodes, props.links);
        self.marks.frame.title = props.title.unwrap_or_default();
        self.marks.frame.empty = props.empty.unwrap_or_default();
        self.mark_dirty();
        true
    }
}
