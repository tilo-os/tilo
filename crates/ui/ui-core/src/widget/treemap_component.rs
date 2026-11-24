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

    fn props(rows: &[(&str, &str, &str, f64)]) -> TreemapProps {
        TreemapProps {
            parts: rows
                .iter()
                .map(|(id, parent, label, value)| {
                    ChartPart::from_value(&part(id, parent, label, *value)).unwrap()
                })
                .collect(),
            ..TreemapProps::default()
        }
    }

    fn written(map: &Treemap) -> Vec<(String, f64)> {
        map.marks
            .items
            .iter()
            .map(|item| (item.label.clone(), item.value))
            .collect()
    }

    #[test]
    fn each_first_level_part_is_a_rectangle_as_large_as_everything_under_it() {
        let map = Treemap::from_props(
            "k",
            TreemapProps {
                title: Some("Hours a week".to_owned()),
                ..props(&[
                    ("build", "ci", "Build", 12.0),
                    ("ci", "", "CI", 0.0),
                    ("design", "", "Design", 8.0),
                ])
            },
        );
        assert_eq!(
            written(&map),
            [("CI".to_owned(), 12.0), ("Design".to_owned(), 8.0)],
            "a part with parts is as large as they are, and its parts are not rectangles of \
             their own"
        );
        assert_eq!(map.marks.frame.title, "Hours a week");
    }

    #[test]
    fn a_part_given_its_own_value_is_that_large_even_with_parts_under_it() {
        let map = Treemap::from_props(
            "k",
            props(&[("ci", "", "CI", 20.0), ("build", "ci", "Build", 12.0)]),
        );
        assert_eq!(written(&map), [("CI".to_owned(), 20.0)]);
    }

    #[test]
    fn changed_parts_change_the_rectangles_where_they_stand() {
        let mut map = Treemap::from_props("k", props(&[("ci", "", "CI", 4.0)]));
        assert_eq!(written(&map), [("CI".to_owned(), 4.0)]);
        assert!(
            map.set_props(TreemapProps {
                parts: props(&[("ci", "", "CI", 5.0), ("design", "", "Design", 2.0)]).parts,
                title: Some("Hours".to_owned()),
            }),
            "taken in place, never built again"
        );
        assert_eq!(
            written(&map),
            [("CI".to_owned(), 5.0), ("Design".to_owned(), 2.0)]
        );
        assert_eq!(map.marks.frame.title, "Hours");
    }
}
