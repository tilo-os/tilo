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

    fn part(id: &str, parent: &str, label: &str, value: f64) -> Value {
        Value::Case {
            case: "part".to_owned(),
            fields: vec![
                ("id".to_owned(), Value::Text(id.to_owned())),
                ("parent".to_owned(), Value::Text(parent.to_owned())),
                ("label".to_owned(), Value::Text(label.to_owned())),
                ("value".to_owned(), Value::Float(value)),
            ],
        }
    }

    fn parts(rows: &[(&str, &str, &str, f64)]) -> Vec<ChartPart> {
        rows.iter()
            .map(|(id, parent, label, value)| {
                ChartPart::from_value(&part(id, parent, label, *value)).unwrap()
            })
            .collect()
    }

    /// Each ring's id, its depth and its children's, as the chart lays them out.
    fn rings(node: &SunburstNode, depth: usize, into: &mut Vec<(String, usize)>) {
        for child in &node.children {
            into.push((child.id.clone(), depth));
            rings(child, depth + 1, into);
        }
    }

    #[test]
    fn a_part_reads_from_its_case_and_needs_what_it_is_and_how_much() {
        let read = ChartPart::from_value(&part("ci", "root", "CI", 4.0)).unwrap();
        assert_eq!(
            (read.id.as_str(), read.parent.as_str(), read.label.as_str()),
            ("ci", "root", "CI")
        );
        assert_eq!(read.value, 4.0);
        let bare = Value::Case {
            case: "part".to_owned(),
            fields: vec![
                ("label".to_owned(), Value::Text("CI".to_owned())),
                ("value".to_owned(), Value::Float(1.0)),
            ],
        };
        let read = ChartPart::from_value(&bare).unwrap();
        assert_eq!((read.id.as_str(), read.parent.as_str()), ("", ""));
        assert_eq!(read.known_as(), "CI", "a part without an id is its label");
        let slice = Value::Case {
            case: "slice".to_owned(),
            fields: Vec::new(),
        };
        assert!(ChartPart::from_value(&slice).is_err());
        let unvalued = Value::Case {
            case: "part".to_owned(),
            fields: vec![("label".to_owned(), Value::Text("CI".to_owned()))],
        };
        assert!(ChartPart::from_value(&unvalued).is_err());
    }

    #[test]
    fn a_part_hangs_under_the_part_it_names_and_one_with_no_top_stands_at_the_centre() {
        let props = SunburstChartProps {
            parts: parts(&[
                ("build", "ci", "Build", 12.0),
                ("ci", "", "CI", 0.0),
                ("design", "", "Design", 8.0),
                ("orphan", "gone", "Orphan", 1.0),
            ]),
            title: Some("Hours a week".to_owned()),
            total: Some("21 hours".to_owned()),
        };
        let chart = SunburstChart::from_props("k", props);
        let mut laid = Vec::new();
        rings(&chart.marks.root, 1, &mut laid);
        assert_eq!(
            laid,
            [
                ("ci".to_owned(), 1),
                ("build".to_owned(), 2),
                ("design".to_owned(), 1),
                ("orphan".to_owned(), 1),
            ],
            "a parent stands before its parts, and a part whose parent is missing stands at the \
             top level"
        );
        assert_eq!(chart.marks.frame.title, "Hours a week");
        assert_eq!(chart.marks.center_title, "21 hours");
    }

    #[test]
    fn a_loop_of_parts_has_no_top_and_its_rings_are_still_drawn() {
        let root = root(&parts(&[
            ("a", "b", "A", 1.0),
            ("b", "a", "B", 2.0),
            ("c", "a", "C", 1.0),
        ]));
        let mut laid = Vec::new();
        rings(&root, 1, &mut laid);
        assert_eq!(
            laid.len(),
            3,
            "each part is drawn once, whatever its parents: {laid:?}"
        );
    }

    #[test]
    fn changed_parts_change_the_rings_where_they_stand() {
        let mut chart = SunburstChart::from_props(
            "k",
            SunburstChartProps {
                parts: parts(&[("ci", "", "CI", 4.0)]),
                ..SunburstChartProps::default()
            },
        );
        let next = SunburstChartProps {
            parts: parts(&[("ci", "", "CI", 5.0), ("web", "ci", "Web", 2.0)]),
            title: Some("Hours".to_owned()),
            ..SunburstChartProps::default()
        };
        assert!(chart.set_props(next), "taken in place, never built again");
        let mut laid = Vec::new();
        rings(&chart.marks.root, 1, &mut laid);
        assert_eq!(
            laid,
            [("ci".to_owned(), 1), ("web".to_owned(), 2)],
            "a part added under another is drawn under it"
        );
        assert_eq!(chart.marks.frame.title, "Hours");
        assert_eq!(chart.marks.center_title, "");
    }
}
