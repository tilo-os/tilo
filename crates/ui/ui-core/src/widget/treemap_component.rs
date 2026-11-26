//! [`Treemap`] as a described component: a hierarchy as rectangles, and [`ChartPart`] as one of
//! its parts.

use crate::component::{Component, Described};
use crate::widget::{ChartItem, ChartPart, hierarchy};

use super::Treemap;

/// A hierarchy as rectangles: each part of the first level a rectangle as large as its share of
/// the whole, its label written on it where it fits, in a colour from the theme. Use it where
/// labels have to be read, rather than followed round a ring.
#[derive(Component, Default)]
#[component(name = "Treemap", role = "Image", label = "title")]
pub struct TreemapProps {
    /// The parts, each naming the part it belongs to: `.part(id: "build", parent: "ci",
    /// label: "Build", value: 12.0)`. A part nothing names as its parent is a rectangle of its
    /// own, as large as everything under it.
    #[component(required, positional)]
    pub parts: Vec<ChartPart>,
    /// What it shows, over it and as assistive technology announces it.
    pub title: Option<String>,
    /// What it says while it has nothing to show, and why: "No request has been answered yet".
    /// Without it the chart says only that it has nothing to show.
    pub empty: Option<String>,
}

/// The rectangles `parts` are drawn as: each first-level part, sized by everything under it.
fn tiles(parts: &[ChartPart]) -> Vec<ChartItem> {
    hierarchy(parts)
        .into_iter()
        .map(|branch| {
            ChartItem::new(
                branch.part.known_as(),
                branch.part.label.clone(),
                branch.value(),
            )
        })
        .collect()
}

impl Described for Treemap {
    type Props = TreemapProps;

    fn from_props(_key: &str, props: TreemapProps) -> Self {
        let mut map = Self::new(Vec::new());
        map.set_props(props);
        map
    }

    /// Changed parts change the rectangles where they stand: the widget stays, with what the
    /// person is inspecting.
    fn set_props(&mut self, props: TreemapProps) -> bool {
        self.set_items(tiles(&props.parts));
        self.marks.frame.title = props.title.unwrap_or_default();
        self.marks.frame.empty = props.empty.unwrap_or_default();
        self.mark_dirty();
        true
    }
}
