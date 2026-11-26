//! [`SunburstChart`] as a described component: a hierarchy as rings, and [`ChartPart`] as one of
//! its parts.

use crate::widget::{ChartBranch, ChartPart, hierarchy};
use tilo_ui_base::{component::Component, component::Described};

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
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
    /// What the whole is, read at its centre ("128 hours"); without it the centre reads the sum
    /// of the parts, so the hole always says how large the whole is.
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
        let rings = root(&props.parts);
        let whole = rings.effective_value();
        self.set_root(rings);
        let marks = &mut self.marks;
        marks.frame.title = props.title.unwrap_or_default();
        marks.frame.empty = props.empty.unwrap_or_default();
        marks.center_title = props.total.unwrap_or_else(|| {
            if whole > 0.0 {
                super::format_compact(whole)
            } else {
                String::new()
            }
        });
        marks.center_subtitle = String::new();
        self.mark_dirty();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(id: &str, parent: &str, value: f64) -> ChartPart {
        ChartPart {
            id: id.to_owned(),
            parent: parent.to_owned(),
            label: id.to_owned(),
            value,
        }
    }

    /// A sunburst whose app names no total still says how large the whole is at its centre: the
    /// sum of its parts, nested ones counted once. One that names a total reads that, and one
    /// with nothing in it leaves the hole empty.
    #[test]
    fn the_centre_reads_the_whole_when_no_total_is_given() {
        let parts = vec![
            part("integration", "platform", 22.0),
            part("migration", "platform", 14.0),
            part("platform", "", 0.0),
            part("interviews", "", 7.0),
        ];
        let chart = SunburstChart::from_props(
            "",
            SunburstChartProps {
                parts: parts.clone(),
                ..SunburstChartProps::default()
            },
        );
        assert_eq!(chart.marks.center_title, "43");
        let named = SunburstChart::from_props(
            "",
            SunburstChartProps {
                parts,
                total: Some("43 hours".to_owned()),
                ..SunburstChartProps::default()
            },
        );
        assert_eq!(named.marks.center_title, "43 hours");
        let empty = SunburstChart::from_props("", SunburstChartProps::default());
        assert_eq!(empty.marks.center_title, "");
    }
}
