//! Retained content-card regions and their small standard action group.
use super::composition_parts::{Parts, retained_parts};
use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{auto, fr, length, min_content, minmax, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{Button, Element, Text, Widget};

/// A themed card with retained header, body, and footer regions.
/// The body absorbs surplus row height; equal-height footers align fully.
/// Wrapping footers retain a common bottom edge, but may have different tops.
/// Construct with [`super::Card::composed`].
pub struct ContentCard {
    parts: Parts,
    has_header: bool,
    has_footer: bool,
}

impl ContentCard {
    pub(super) fn new(body: Element) -> Self {
        Self {
            parts: Parts::new(vec![body]),
            has_header: false,
            has_footer: false,
        }
    }

    /// Replace the header region, including any header installed by `with_title`.
    #[must_use]
    pub fn with_header(mut self, header: Element) -> Self {
        if self.has_header
            && let Some(slot) = self.parts.children.first_mut()
        {
            *slot = header;
        } else {
            self.parts.children.insert(0, header);
            self.has_header = true;
        }
        self
    }

    /// Replace the entire header with a semantic heading. Last header setter wins.
    #[must_use]
    pub fn with_title(self, title: impl Into<String>) -> Self {
        self.with_header(Box::new(
            Text::new(&title.into()).with_role(crate::tokens::typography::TextRole::Heading),
        ))
    }

    /// Replace the footer region. Standard [`Actions`] provide trailing placement.
    #[must_use]
    pub fn with_footer(mut self, footer: Element) -> Self {
        if self.has_footer {
            self.parts.children.pop();
        }
        self.parts.children.push(footer);
        self.has_footer = true;
        self
    }
}

/// Standard actions in declaration/focus order: secondaries, then primary.
/// Unspecified buttons use measured content sizing; explicit sizes remain manual.
/// Each wrapped line aligns to the trailing edge.
pub struct Actions {
    parts: Parts,
}

impl Actions {
    pub fn new(primary: Button) -> Self {
        Self {
            parts: Parts::new(vec![Box::new(primary.with_action_sizing())]),
        }
    }

    #[must_use]
    pub fn secondary(mut self, button: Button) -> Self {
        let index = self.parts.children.len().saturating_sub(1);
        self.parts
            .children
            .insert(index, Box::new(button.with_action_sizing()));
        self
    }
}

impl Widget for ContentCard {
    retained_parts!("ContentCard");

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut tracks = Vec::new();
        if self.has_header {
            tracks.push(min_content());
        }
        tracks.push(minmax(min_content(), fr(1.0_f32)));
        if self.has_footer {
            tracks.push(min_content());
        }
        let node = self.parts.build(
            Style {
                display: taffy::Display::Grid,
                grid_template_rows: tracks,
                grid_template_columns: vec![minmax(length(0.0_f32), fr(1.0_f32))],
                size: Size {
                    width: percent(1.0_f32),
                    height: auto(),
                },
                min_size: Size {
                    width: length(0.0_f32),
                    // Intrinsic Grid rows need this item's automatic block minimum;
                    // `flex_shrink` does not protect an item in Grid sizing.
                    height: auto(),
                },
                flex_shrink: 0.0,
                padding: taffy::Rect {
                    left: length(theme.padding_standard),
                    right: length(theme.padding_standard),
                    top: length(theme.padding_standard),
                    bottom: length(theme.padding_standard),
                },
                gap: Size {
                    width: length(theme.gap_standard),
                    height: length(theme.gap_standard),
                },
                ..Default::default()
            },
            taffy,
            theme,
            map,
        );
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The card's rest edge derives from its own fill (HIG §9.5); the
        // header, body and footer inside get space, never a second edge.
        crate::surface::card(
            scene,
            transform,
            self.parts.rect,
            theme.radius_standard,
            theme.bg_primary,
        );
        self.parts.draw(scene, transform, text, theme, presence);
    }
}

impl Widget for Actions {
    retained_parts!("Actions");

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.parts.build(
            Style {
                display: taffy::Display::Flex,
                flex_wrap: taffy::FlexWrap::Wrap,
                justify_content: Some(taffy::JustifyContent::End),
                align_items: Some(taffy::AlignItems::End),
                align_content: Some(taffy::AlignContent::Start),
                size: Size {
                    width: auto(),
                    height: auto(),
                },
                min_size: Size {
                    width: length(0.0_f32),
                    height: length(0.0_f32),
                },
                flex_shrink: 0.0,
                gap: Size {
                    width: length(theme.gap_standard),
                    height: length(theme.gap_standard),
                },
                ..Default::default()
            },
            taffy,
            theme,
            map,
        );
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.parts.draw(scene, transform, text, theme, presence);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::{FrameHarness, InspectionNode};
    use crate::widget::{Card, CardGrid, Column, Container, Scrollable};
    use std::cell::Cell;
    use std::rc::Rc;

    fn fixture() -> Element {
        Box::new(CardGrid::content_sized(vec![
            Box::new(
                Card::composed(Box::new(Text::new("Short description")))
                    .with_title("Terminal")
                    .with_footer(Box::new(Actions::new(
                        Button::new("open", "Open", Some("opened")).primary(),
                    ))),
            ),
            Box::new(
                Card::composed(Box::new(Text::new(
                    &"A longer description with useful details. ".repeat(10),
                )))
                .with_title("Workspace")
                .with_footer(Box::new(Actions::new(
                    Button::new("install", "Install", None).primary(),
                ))),
            ),
        ]))
    }

    fn rect(node: &InspectionNode) -> Rect {
        let [x, y, w, h] = node.rect;
        Rect::new(x, y, x + w, y + h)
    }

    fn assert_contained(node: &InspectionNode, bounds: Rect) {
        let child = rect(node);
        assert!(
            child.x0 >= bounds.x0 - 1.0
                && child.x1 <= bounds.x1 + 1.0
                && child.y0 >= bounds.y0 - 1.0
                && child.y1 <= bounds.y1 + 1.0,
            "{} {child:?} escapes {bounds:?}",
            node.widget
        );
        for descendant in &node.children {
            assert_contained(descendant, bounds);
        }
    }

    fn find<'a>(node: &'a InspectionNode, id: &str) -> &'a InspectionNode {
        if node.test_id.as_deref() == Some(id) {
            return node;
        }
        node.children
            .iter()
            .find_map(|child| find_optional(child, id))
            .unwrap_or_else(|| panic!("missing inspection node {id:?}"))
    }

    fn find_optional<'a>(node: &'a InspectionNode, id: &str) -> Option<&'a InspectionNode> {
        (node.test_id.as_deref() == Some(id))
            .then_some(node)
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| find_optional(child, id))
            })
    }

    fn automatic_card(index: usize) -> Element {
        Box::new(
            Card::composed(Box::new(Text::new(
                &"A useful multi-line description that must remain inside its own card. ".repeat(3),
            )))
            .with_header(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_height(40.0)
                    .with_child(Box::new(Text::new(&format!("Application {index}")))),
            ))
            .with_footer(Box::new(Actions::new(Button::new(
                &format!("details-{index}"),
                "View details",
                None,
            )))),
        )
    }

    fn standalone_stack_fixture(invocations: Rc<Cell<usize>>) -> Element {
        let mut content = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(8.0);
        for index in 0..8 {
            content = content.push(automatic_card(index));
        }
        content = content
            .push(Box::new(
                Text::new("All applications").with_id("trailing-heading"),
            ))
            .push(Box::new(
                Button::new("final-action", "Open all applications", None).on_click(move || {
                    invocations.set(invocations.get() + 1);
                }),
            ));
        Box::new(Scrollable::new_vertical().with_child(Box::new(content)))
    }

    fn section_grid(prefix: &str, count: usize) -> Element {
        Box::new(CardGrid::content_sized(
            (0..count)
                .map(|index| {
                    Box::new(Card::composed(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_height(72.0)
                            .with_child(Box::new(Text::new(&format!("{prefix} card {index}")))),
                    ))) as Element
                })
                .collect(),
        ))
    }

    fn section_fixture(wide: bool) -> Element {
        let content = if wide {
            let ready = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(8.0)
                .push(Box::new(Text::new("Ready").with_id("ready-heading")))
                .push(Box::new(Text::new("Installed applications")))
                .push(section_grid("ready", 6));
            let discover = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(8.0)
                .push(Box::new(Text::new("Discover").with_id("discover-heading")))
                .push(Box::new(Text::new("Available applications")))
                .push(section_grid("discover", 4));
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(0.0)
                .push(Box::new(ready))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(16.0),
                ))
                .push(Box::new(discover))
        } else {
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(8.0)
                .push(Box::new(Text::new("Ready").with_id("ready-heading")))
                .push(section_grid("ready", 6))
                .push(Box::new(
                    Container::new().with_no_padding().with_height(16.0),
                ))
                .push(Box::new(Text::new("Discover").with_id("discover-heading")))
                .push(section_grid("discover", 4))
        };
        Box::new(Scrollable::new_vertical().with_child(Box::new(content)))
    }

    fn long_fixture() -> Element {
        Box::new(CardGrid::content_sized(
            (0_usize..3)
                .map(|index| {
                    Box::new(
                        Card::composed(Box::new(Text::new(
                            &"Manage your documents and shared workspace. "
                                .repeat(index.checked_add(1).unwrap()),
                        )))
                        .with_title("Workspace connections and documents")
                        .with_footer(Box::new(
                            Actions::new(
                                Button::new(
                                    &format!("primary-{index}"),
                                    "Refresh all remote connections",
                                    None,
                                )
                                .with_icon(crate::icons::Icon::Search)
                                .primary(),
                            )
                            .secondary(Button::new(
                                &format!("secondary-{index}"),
                                "View details",
                                None,
                            )),
                        )),
                    ) as Element
                })
                .collect(),
        ))
    }

    #[test]
    fn scaled_card_regions_contain_content_and_match_fresh_geometry() {
        fn narrow_scaled_regions_contain_content_and_resize_matches_fresh() {
            fn geometry(node: &InspectionNode) -> Vec<[f64; 4]> {
                let mut all = vec![node.rect];
                for child in &node.children {
                    all.extend(geometry(child));
                }
                all
            }

            for scale in [1.0, 2.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut resized = FrameHarness::new_with_theme(
                    crate::State::new(),
                    long_fixture(),
                    (1000, 1800),
                    theme.clone(),
                );
                resized.tab(false);
                resized.resize((240, 1800)).unwrap();
                let fresh = FrameHarness::new_with_theme(
                    crate::State::new(),
                    long_fixture(),
                    (240, 1800),
                    theme,
                );
                let a = resized.inspection();
                let b = fresh.inspection();
                assert_eq!(geometry(&a.root), geometry(&b.root));
                for card in &a.root.children {
                    assert!(rect(card).width() <= 240.0);
                    for child in &card.children {
                        assert_contained(child, rect(card));
                    }
                    assert!(rect(&card.children[1]).y1 <= rect(&card.children[2]).y0);
                }
                let last = rect(a.root.children.last().unwrap());
                assert!((last.y1 - rect(&a.root).y1).abs() < 1.0);
                assert_eq!(
                    resized.focused_access_node().unwrap().name.as_deref(),
                    Some("View details")
                );
            }
        }

        fn short_card_stack_resize_and_text_scale_match_fresh_geometry() {
            fn geometry(node: &InspectionNode) -> Vec<[f64; 4]> {
                let mut all = vec![node.rect];
                for child in &node.children {
                    all.extend(geometry(child));
                }
                all
            }

            for scale in [1.0, 2.0] {
                let theme = Theme::dark().with_text_scale(scale);
                let mut resized = FrameHarness::new_with_theme(
                    crate::State::new(),
                    section_fixture(true),
                    (1000, 360),
                    theme.clone(),
                );
                resized.resize((480, 360)).unwrap();
                resized.resize((1000, 360)).unwrap();
                let fresh = FrameHarness::new_with_theme(
                    crate::State::new(),
                    section_fixture(true),
                    (1000, 360),
                    theme,
                );
                assert_eq!(
                    geometry(&resized.inspection().root),
                    geometry(&fresh.inspection().root)
                );
            }
        }

        fn retained_regions_align_footers_before_paint() {
            use automerge::ReadDoc;

            let mut harness = FrameHarness::new_with_theme(
                crate::State::new(),
                fixture(),
                (700, 800),
                Theme::dark(),
            );
            let inspection = harness.inspection();
            let cards = &inspection.root.children;
            assert_eq!(rect(&cards[0]).height(), rect(&cards[1]).height());
            assert_eq!(
                rect(&cards[0].children[2]).y1,
                rect(&cards[1].children[2]).y1
            );
            assert_eq!(
                harness
                    .tab_through()
                    .iter()
                    .map(|(_, name)| name.as_deref().unwrap_or(""))
                    .collect::<Vec<_>>(),
                ["Open", "Install"]
            );
            let action = rect(&cards[0].children[2].children[0]);
            harness.click_at(action.center().x, action.center().y);
            assert!(
                harness
                    .state_mut()
                    .doc
                    .get(automerge::ROOT, "opened")
                    .unwrap()
                    .is_some()
            );
        }

        narrow_scaled_regions_contain_content_and_resize_matches_fresh();
        short_card_stack_resize_and_text_scale_match_fresh_geometry();
        retained_regions_align_footers_before_paint();
    }

    #[test]
    fn card_actions_trail_scroll_and_keep_manual_order() {
        fn actions_trail_an_intrinsic_column_without_caller_widths() {
            let root = crate::widget::Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(Actions::new(Button::new(
                    "save",
                    "Save changes",
                    None,
                ))));
            let harness = FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(root),
                (600, 300),
                Theme::dark(),
            );
            let snapshot = harness.inspection();
            let action = &snapshot.root.children[0].children[0];
            assert!((rect(action).x1 - 600.0).abs() < 1.0);
        }

        fn scrolling_reaches_the_last_scaled_action() {
            let root = crate::widget::Scrollable::new_vertical().with_child(long_fixture());
            let mut harness = FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(root),
                (240, 400),
                Theme::dark().with_text_scale(2.0),
            );
            let before = harness.inspection();
            let grid = &before.root.children[0];
            assert!(rect(grid).height() > 400.0);
            harness.click_at(5.0, 5.0);
            harness.scroll(0.0, -1000.0);
            let after = harness.inspection();
            let last = after.root.children[0].children.last().unwrap();
            let action = last.children[2].children.last().unwrap();
            assert!(
                rect(action).y0 >= 0.0 && rect(action).y1 <= 400.0,
                "last action {:?}",
                rect(action)
            );
            assert!((rect(last).y1 - 400.0).abs() < 1.0);
        }

        fn actions_keep_manual_sizes_and_secondary_then_primary_order() {
            let actions =
                Actions::new(Button::new("primary", "Primary", None).with_size(97.0, 35.0))
                    .secondary(Button::new("secondary", "Secondary", None).fit_content(33.0));
            let harness = FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(actions),
                (600, 80),
                Theme::dark().with_text_scale(2.0),
            );
            let snapshot = harness.inspection();
            assert_eq!(
                snapshot.root.children[0].test_id.as_deref(),
                Some("secondary")
            );
            assert_eq!(
                snapshot.root.children[1].test_id.as_deref(),
                Some("primary")
            );
            assert_eq!(rect(&snapshot.root.children[0]).height(), 33.0);
            assert_eq!(
                rect(&snapshot.root.children[1]).size(),
                vello::kurbo::Size::new(97.0, 35.0)
            );
        }

        actions_trail_an_intrinsic_column_without_caller_widths();
        scrolling_reaches_the_last_scaled_action();
        actions_keep_manual_sizes_and_secondary_then_primary_order();
    }

    #[test]
    fn automatic_content_cards_expand_and_propagate_minimums() {
        fn automatic_content_cards_expand_a_short_scroll_stack() {
            let invocations = Rc::new(Cell::new(0));
            let mut harness = FrameHarness::new_with_theme(
                crate::State::new(),
                standalone_stack_fixture(invocations.clone()),
                (480, 360),
                Theme::dark(),
            );
            let before = harness.inspection();
            let content = &before.root.children[0];
            let cards = &content.children[..8];
            for (index, card) in cards.iter().enumerate() {
                assert_contained(card, rect(card));
                if let Some(next) = cards.get(index + 1) {
                    assert!(
                        rect(next).y0 >= rect(card).y1 + 7.0,
                        "card {index} {:?} overlaps next card {:?}",
                        rect(card),
                        rect(next)
                    );
                }
            }
            let trailing = rect(find(&before.root, "trailing-heading"));
            assert!(trailing.y0 >= rect(cards.last().unwrap()).y1 + 7.0);
            assert!(
                rect(content).height() > 360.0,
                "content must own scroll extent"
            );
            assert_eq!(rect(&before.root).height(), 360.0, "viewport stays fixed");

            harness.click_at(8.0, 8.0);
            harness.scroll(0.0, -10_000.0);
            let final_rect = rect(find(&harness.inspection().root, "final-action"));
            assert!(
                final_rect.y0 >= 0.0 && final_rect.y1 <= 360.0,
                "final action must be reachable after scrolling: {final_rect:?}"
            );
            harness.click_named("final-action").unwrap();
            assert_eq!(invocations.get(), 1, "pointer activation fires once");
            assert_eq!(
                harness
                    .tab_through()
                    .last()
                    .and_then(|(_, name)| name.as_deref()),
                Some("Open all applications")
            );
            harness
                .perform_named("final-action", &crate::a11y::Action::Activate)
                .unwrap();
            assert_eq!(invocations.get(), 2, "focused activation fires once");
        }

        fn automatic_legacy_card_expands_in_a_short_intrinsic_grid() {
            let mut rows = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(12.0);
            for index in 0..6 {
                rows = rows.push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_height(40.0)
                        .with_child(Box::new(
                            Text::new(&format!("Format control {index}"))
                                .with_id(format!("format-{index}")),
                        )),
                ));
            }
            let content = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(20.0)
                .push(Box::new(Card::titled("Region formats", Box::new(rows))))
                .push(Box::new(Text::new("Preview").with_id("preview-heading")));
            let harness = FrameHarness::new(
                crate::State::new(),
                Box::new(Scrollable::new_vertical().with_child(Box::new(content))),
                (480, 360),
            );
            let snapshot = harness.inspection();
            let content = &snapshot.root.children[0];
            let card = &content.children[0];
            let last_row = rect(find(card, "format-5"));
            let preview = rect(find(&snapshot.root, "preview-heading"));
            assert!(
                last_row.y1 <= rect(card).y1 + 1.0,
                "{last_row:?} escapes {:?}",
                rect(card)
            );
            assert!(
                preview.y0 >= rect(card).y1 + 19.0,
                "preview {preview:?} overlaps legacy card {:?}",
                rect(card)
            );
            assert!(rect(content).height() > 360.0);
            assert_eq!(rect(&snapshot.root).height(), 360.0);
        }

        fn nested_card_grids_propagate_content_minimum_to_following_sections() {
            for (wide, viewport) in [(false, (480, 360)), (true, (1000, 360))] {
                let harness = FrameHarness::new_with_theme(
                    crate::State::new(),
                    section_fixture(wide),
                    viewport,
                    Theme::dark(),
                );
                let snapshot = harness.inspection();
                let content = &snapshot.root.children[0];
                let (ready_grid, discover_grid) = if wide {
                    (
                        content.children[0].children.last().unwrap(),
                        content.children[2].children.last().unwrap(),
                    )
                } else {
                    (&content.children[1], &content.children[4])
                };
                let ready_last_bottom = ready_grid
                    .children
                    .iter()
                    .map(|card| rect(card).y1)
                    .fold(f64::NEG_INFINITY, f64::max);
                let discover_last_bottom = discover_grid
                    .children
                    .iter()
                    .map(|card| rect(card).y1)
                    .fold(f64::NEG_INFINITY, f64::max);
                assert!(ready_last_bottom <= rect(ready_grid).y1 + 1.0);
                assert!(discover_last_bottom <= rect(discover_grid).y1 + 1.0);
                assert!(
                    rect(find(content, "discover-heading")).y0 >= ready_last_bottom + 15.0,
                    "{viewport:?}: discover section begins before ready cards end"
                );
                assert!(
                    rect(&snapshot.root.children[0]).height() > f64::from(viewport.1),
                    "{viewport:?}: section content must exceed the fixed viewport"
                );
            }
        }

        fn content_cards_keep_explicit_fixed_rows() {
            let cards = vec![Box::new(
                Card::composed(Box::new(Text::new(&"Long description. ".repeat(50))))
                    .with_title("Fixed card"),
            ) as Element];
            let harness = FrameHarness::new_with_theme(
                crate::State::new(),
                Box::new(CardGrid::content_sized(cards).with_card_height(180.0)),
                (300, 600),
                Theme::dark(),
            );
            assert_eq!(harness.inspection().root.children[0].rect[3], 180.0);
        }

        automatic_content_cards_expand_a_short_scroll_stack();
        automatic_legacy_card_expands_in_a_short_intrinsic_grid();
        nested_card_grids_propagate_content_minimum_to_following_sections();
        content_cards_keep_explicit_fixed_rows();
    }
}
