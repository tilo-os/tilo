//! [`ChordChart`] as a described component: weighted links between nodes around a ring.

use crate::component::{Component, Described};
use crate::widget::{ChartLink, ChartNode};

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
    use crate::component::PropType;
    use crate::component::Value;

    fn node(id: &str, label: &str) -> Value {
        Value::Case {
            case: "node".to_owned(),
            fields: vec![
                ("id".to_owned(), Value::Text(id.to_owned())),
                ("label".to_owned(), Value::Text(label.to_owned())),
            ],
        }
    }

    fn link(from: &str, to: &str, value: f64) -> Value {
        Value::Case {
            case: "link".to_owned(),
            fields: vec![
                ("from".to_owned(), Value::Text(from.to_owned())),
                ("to".to_owned(), Value::Text(to.to_owned())),
                ("value".to_owned(), Value::Float(value)),
            ],
        }
    }

    fn props(arcs: &[(&str, &str)], weights: &[(&str, &str, f64)]) -> ChordChartProps {
        ChordChartProps {
            nodes: arcs
                .iter()
                .map(|(id, label)| ChartNode::from_value(&node(id, label)).unwrap())
                .collect(),
            links: weights
                .iter()
                .map(|(from, to, value)| ChartLink::from_value(&link(from, to, *value)).unwrap())
                .collect(),
            ..ChordChartProps::default()
        }
    }

    #[test]
    fn the_ring_carries_the_nodes_and_links_it_is_given() {
        let chart = ChordChart::from_props(
            "k",
            ChordChartProps {
                title: Some("Hand-overs".to_owned()),
                ..props(
                    &[("design", "Design"), ("engineering", "Engineering")],
                    &[("design", "engineering", 32.0)],
                )
            },
        );
        assert_eq!(chart.marks.nodes.len(), 2);
        assert_eq!(chart.marks.links.len(), 1);
        assert_eq!(chart.marks.frame.title, "Hand-overs");
        assert!(
            chart.marks.nodes.iter().all(|node| node.color.is_none()),
            "a node takes its colour from the theme, not from the app"
        );
    }

    #[test]
    fn changed_weights_change_the_ring_where_it_stands() {
        let mut chart = ChordChart::from_props(
            "k",
            props(
                &[("design", "Design"), ("engineering", "Engineering")],
                &[("design", "engineering", 32.0)],
            ),
        );
        let next = props(
            &[
                ("design", "Design"),
                ("engineering", "Engineering"),
                ("research", "Research"),
            ],
            &[
                ("design", "engineering", 12.0),
                ("engineering", "research", 9.0),
            ],
        );
        assert!(chart.set_props(next), "taken in place, never built again");
        assert_eq!(chart.marks.nodes.len(), 3);
        assert_eq!(chart.marks.links[0].value, 12.0);
    }
}
