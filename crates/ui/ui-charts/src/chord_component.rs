//! [`ChordChart`] as a described component: weighted links between nodes around a ring.

use crate::{ChartLink, ChartNode};
use tilo_ui_base::{component::Component, component::Described};

use super::ChordChart;

/// How much moves between a set of things: which teams hand work to which, which services call
/// which, which people write to whom. Each thing takes an arc as wide as everything that touches
/// it, a ribbon as wide as its weight joins each pair, and a person pointing at an arc or a ribbon
/// reads what it carries.
#[derive(Component, Default)]
#[component(name = "ChordChart", role = "Image", label = "title")]
pub struct ChordChartProps {
    /// The things it relates, around the ring, in order: `.node(id: "design", label: "Design")`.
    #[component(required, positional)]
    pub nodes: Vec<ChartNode>,
    /// How much moves between them, in either direction:
    /// `.link(from: "design", to: "engineering", value: 32.0)`, `from` and `to` naming nodes by
    /// their ids.
    #[component(required)]
    pub links: Vec<ChartLink>,
    /// What it shows, over it and as assistive technology announces it.
    pub title: Option<String>,
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
}

impl Described for ChordChart {
    type Props = ChordChartProps;

    fn from_props(_key: &str, props: ChordChartProps) -> Self {
        let mut chart = Self::new(Vec::new(), Vec::new());
        chart.set_props(props);
        chart
    }

    /// Changed nodes and links change the ring where it stands: the widget stays, with what the
    /// person is inspecting.
    fn set_props(&mut self, props: ChordChartProps) -> bool {
        self.set_data(props.nodes, props.links);
        self.marks.frame.title = props.title.unwrap_or_default();
        self.marks.frame.empty = props.empty.unwrap_or_default();
        self.mark_dirty();
        true
    }
}
