//! [`SankeyChart`] as a described component: nodes in stages, and the weighted links between them.

use crate::component::{Component, Described};
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

    fn props(stages: &[(&str, &str)], flows: &[(&str, &str, f64)]) -> SankeyChartProps {
        SankeyChartProps {
            nodes: stages
                .iter()
                .map(|(id, label)| ChartNode::from_value(&node(id, label)).unwrap())
                .collect(),
            links: flows
                .iter()
                .map(|(from, to, value)| ChartLink::from_value(&link(from, to, *value)).unwrap())
                .collect(),
            ..SankeyChartProps::default()
        }
    }

    #[test]
    fn a_node_and_a_link_read_from_their_cases() {
        let read = ChartNode::from_value(&node("visits", "Visits")).unwrap();
        assert_eq!(
            (read.id.as_str(), read.label.as_str()),
            ("visits", "Visits")
        );
        let unnamed = Value::Case {
            case: "node".to_owned(),
            fields: vec![("label".to_owned(), Value::Text("Visits".to_owned()))],
        };
        let read = ChartNode::from_value(&unnamed).unwrap();
        assert_eq!(
            read.id, "Visits",
            "a node without an id is called by its label"
        );
        let read = ChartLink::from_value(&link("visits", "signup", 72.0)).unwrap();
        assert_eq!(
            (read.source.as_str(), read.target.as_str(), read.value),
            ("visits", "signup", 72.0)
        );
        assert_eq!(read.id, "visits\u{2192}signup");
        let connection = Value::Case {
            case: "connection".to_owned(),
            fields: Vec::new(),
        };
        assert!(ChartLink::from_value(&connection).is_err());
        let unweighted = Value::Case {
            case: "link".to_owned(),
            fields: vec![
                ("from".to_owned(), Value::Text("a".to_owned())),
                ("to".to_owned(), Value::Text("b".to_owned())),
            ],
        };
        assert!(ChartLink::from_value(&unweighted).is_err());
    }

    #[test]
    fn the_flow_carries_the_nodes_and_links_it_is_given() {
        let chart = SankeyChart::from_props(
            "k",
            SankeyChartProps {
                title: Some("Sign-ups".to_owned()),
                ..props(
                    &[("visits", "Visits"), ("signup", "Sign-ups")],
                    &[("visits", "signup", 72.0)],
                )
            },
        );
        assert_eq!(chart.marks.nodes.len(), 2);
        assert_eq!(chart.marks.links.len(), 1);
        assert_eq!(chart.marks.frame.title, "Sign-ups");
        assert!(
            chart.marks.nodes.iter().all(|node| node.color.is_none()),
            "a node takes its colour from the theme, not from the app"
        );
    }

    #[test]
    fn changed_nodes_and_links_change_the_flow_where_it_stands() {
        let mut chart = SankeyChart::from_props(
            "k",
            props(&[("visits", "Visits")], &[("visits", "visits", 1.0)]),
        );
        let next = props(
            &[
                ("visits", "Visits"),
                ("signup", "Sign-ups"),
                ("paid", "Paid"),
            ],
            &[("visits", "signup", 72.0), ("signup", "paid", 31.0)],
        );
        assert!(chart.set_props(next), "taken in place, never built again");
        assert_eq!(chart.marks.nodes.len(), 3);
        assert_eq!(chart.marks.links.len(), 2);
        assert_eq!(chart.marks.links[1].value, 31.0);
    }

    #[test]
    fn a_link_between_nodes_that_are_not_there_flows_nothing() {
        let chart = SankeyChart::from_props(
            "k",
            props(&[("visits", "Visits")], &[("visits", "gone", 4.0)]),
        );
        assert!(chart.marks.resolved_links().is_empty());
    }
}
