//! The room layout offers a box: the constraint it is laid out under, as a CSS container query
//! reads its container, never the size of what the box drew.
//!
//! A box whose size its parent decides (a stretched column child, a growing row item, a grid
//! cell, a fixed or placed size) is offered the size it was given. A box that takes the size of
//! its content (a part in a bar, an item packed in a row) is offered the room its parent has for
//! content, whatever it drew: a view that reads its room to decide what to draw then never feeds
//! back on itself, drawing less because it measured less because it drew less.

use taffy::{AlignItems, Display, FlexDirection, NodeId, Position, Size, TaffyTree};

/// The room layout offered `node` in the solved `taffy`, in the tree's units: per axis, the size
/// it was given where its parent or its own style decided it, and otherwise the room its parent
/// has for content, less the node's margins. It is never less than the node's own size.
#[must_use]
pub fn offered_room(taffy: &TaffyTree<()>, node: NodeId) -> Size<f32> {
    Size {
        width: room(taffy, node, Axis::Width).0,
        height: room(taffy, node, Axis::Height).0,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Width,
    Height,
}

/// The room `node` is offered along `axis`, and whether that is the size it was given (so its
/// content cannot have decided it).
fn room(taffy: &TaffyTree<()>, node: NodeId, axis: Axis) -> (f32, bool) {
    let Ok(layout) = taffy.layout(node) else {
        return (0.0, true);
    };
    let along = |size: Size<f32>| match axis {
        Axis::Width => size.width,
        Axis::Height => size.height,
    };
    let own = along(layout.size);
    let Ok(style) = taffy.style(node) else {
        return (own, true);
    };
    let fixed = match axis {
        Axis::Width => !style.size.width.is_auto(),
        Axis::Height => !style.size.height.is_auto(),
    };
    let Some(parent) = taffy.parent(node) else {
        return (own, true);
    };
    if fixed || style.position == Position::Absolute {
        return (own, true);
    }
    let Ok(parent_style) = taffy.style(parent) else {
        return (own, true);
    };
    let given = given_by_parent(style, parent_style, axis);
    let (parent_room, parent_given) = room(taffy, parent, axis);
    if given && parent_given {
        return (own, true);
    }
    let Ok(parent_layout) = taffy.layout(parent) else {
        return (own, true);
    };
    let (insets, margins) = match axis {
        Axis::Width => (
            parent_layout.padding.left
                + parent_layout.padding.right
                + parent_layout.border.left
                + parent_layout.border.right,
            layout.margin.left + layout.margin.right,
        ),
        Axis::Height => (
            parent_layout.padding.top
                + parent_layout.padding.bottom
                + parent_layout.border.top
                + parent_layout.border.bottom,
            layout.margin.top + layout.margin.bottom,
        ),
    };
    let mut offered = parent_room - insets - margins;
    let max = match axis {
        Axis::Width => style.max_size.width,
        Axis::Height => style.max_size.height,
    };
    if let Some(max) = max.into_option() {
        offered = offered.min(max);
    }
    (offered.max(own), false)
}

/// Whether `parent` decides the size of a child styled `style` along `axis`, rather than the
/// child's content: a flex item that grows along the main axis or stretches across it, a block's
/// child across its width, a grid item filling its cell.
fn given_by_parent(style: &taffy::Style, parent: &taffy::Style, axis: Axis) -> bool {
    match parent.display {
        Display::Flex => {
            let main = match parent.flex_direction {
                FlexDirection::Row | FlexDirection::RowReverse => Axis::Width,
                FlexDirection::Column | FlexDirection::ColumnReverse => Axis::Height,
            };
            if axis == main {
                style.flex_grow > 0.0 || !style.flex_basis.is_auto()
            } else {
                stretched(style.align_self, parent.align_items)
            }
        }
        Display::Block => axis == Axis::Width,
        Display::Grid => match axis {
            Axis::Width => stretched(style.justify_self, parent.justify_items),
            Axis::Height => stretched(style.align_self, parent.align_items),
        },
        Display::None => true,
    }
}

/// Whether an item aligned `own` in a container aligning its items `items` stretches across it:
/// the default where neither says otherwise.
fn stretched(own: Option<AlignItems>, items: Option<AlignItems>) -> bool {
    own.or(items)
        .is_none_or(|align| align == AlignItems::Stretch)
}

#[cfg(test)]
mod tests {
    use super::offered_room;
    use taffy::prelude::*;

    fn solve(tree: &mut TaffyTree<()>, root: NodeId, width: f32, height: f32) {
        tree.compute_layout(
            root,
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::Definite(height),
            },
        )
        .unwrap();
    }

    /// A part packed by its content into a bar's row is offered the row's room, however little
    /// it drew; an item that grows, or stretches across, is offered what it was given.
    #[test]
    fn a_content_sized_item_is_offered_its_parents_room() {
        let mut tree: TaffyTree<()> = TaffyTree::new();
        let content = tree
            .new_leaf(Style {
                size: Size {
                    width: length(80.0_f32),
                    height: length(20.0_f32),
                },
                ..Style::default()
            })
            .unwrap();
        // The part: a row as wide as what it holds.
        let part = tree
            .new_with_children(Style::default(), &[content])
            .unwrap();
        let growing = tree
            .new_leaf(Style {
                flex_grow: 1.0,
                ..Style::default()
            })
            .unwrap();
        let bar = tree
            .new_with_children(
                Style {
                    size: Size {
                        width: length(1000.0_f32),
                        height: length(40.0_f32),
                    },
                    padding: Rect {
                        left: length(10.0_f32),
                        right: length(10.0_f32),
                        top: length(0.0_f32),
                        bottom: length(0.0_f32),
                    },
                    align_items: Some(AlignItems::Center),
                    ..Style::default()
                },
                &[part, growing],
            )
            .unwrap();
        solve(&mut tree, bar, 1000.0, 40.0);
        assert_eq!(
            tree.layout(part).unwrap().size.width,
            80.0,
            "the part hugs its content"
        );
        let room = offered_room(&tree, part);
        assert_eq!(
            room.width, 980.0,
            "the part is offered the bar's room, not what it drew"
        );
        assert_eq!(
            room.height, 40.0,
            "centred, the part is offered the bar's height"
        );
        // What the part holds is packed by its content too, so it reads the same room.
        assert_eq!(
            offered_room(&tree, content).width,
            80.0,
            "a fixed size is its own room"
        );
        // The growing item was given its width: its room is that, not the whole bar.
        let grown = tree.layout(growing).unwrap().size.width;
        assert_eq!(offered_room(&tree, growing).width, grown);
        assert_eq!(offered_room(&tree, growing).height, 40.0);
        // Narrowing the bar narrows the room of the part that kept its size.
        tree.set_style(
            bar,
            Style {
                size: Size {
                    width: length(400.0_f32),
                    height: length(40.0_f32),
                },
                align_items: Some(AlignItems::Center),
                ..Style::default()
            },
        )
        .unwrap();
        solve(&mut tree, bar, 400.0, 40.0);
        assert_eq!(tree.layout(part).unwrap().size.width, 80.0);
        assert_eq!(offered_room(&tree, part).width, 400.0);
    }
}
