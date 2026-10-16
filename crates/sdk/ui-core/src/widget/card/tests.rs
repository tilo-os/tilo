use super::*;
use crate::headless::{FrameHarness, InspectionNode};
use crate::widget::ButtonRole;
use crate::widget::{Column, Container, Scrollable};
use std::cell::Cell;
use std::rc::Rc;

// ---- pure reflow geometry ----

#[test]
fn card_columns_reflow_fill_and_align_by_width() {
    assert_eq!(card_columns(200.0, 260.0, 16.0), 1, "too narrow → 1 col");
    assert_eq!(card_columns(260.0, 260.0, 16.0), 1);
    assert_eq!(card_columns(536.0, 260.0, 16.0), 2);
    assert_eq!(card_columns(820.0, 260.0, 16.0), 3);
    assert_eq!(card_columns(0.0, 260.0, 16.0), 1);
    assert_eq!(card_columns(-50.0, 260.0, 16.0), 1);
    let w = card_column_width(820.0, 3, 16.0);
    assert!((w - 262.6667).abs() < 0.01, "got {w}");
    assert!((3.0 * w + 2.0 * 16.0 - 820.0).abs() < 0.01);
    assert!((card_column_width(500.0, 1, 16.0) - 500.0).abs() < 0.01);
}

// ---- fixtures ----

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
    find_optional(node, id).unwrap_or_else(|| panic!("missing inspection node {id:?}"))
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

fn text_card(text: &str) -> Element {
    Box::new(Card::new(Box::new(Text::new(text))))
}

fn fixture() -> Element {
    Box::new(CardGrid::new(vec![
        Box::new(
            Card::new(Box::new(Text::new("Short description")))
                .with_title("Terminal")
                .with_footer(Actions::new(
                    Button::new("open", "Open")
                        .on_click(crate::testing::record_click("opened"))
                        .role(ButtonRole::Primary),
                )),
        ),
        Box::new(
            Card::new(Box::new(Text::new(
                &"A longer description with useful details. ".repeat(10),
            )))
            .with_title("Workspace")
            .with_footer(Actions::new(
                Button::new("install", "Install").role(ButtonRole::Primary),
            )),
        ),
    ]))
}

fn long_fixture() -> Element {
    Box::new(CardGrid::new(
        (0_usize..3)
            .map(|index| {
                Box::new(
                    Card::new(Box::new(Text::new(
                        &"Manage your documents and shared workspace. "
                            .repeat(index.checked_add(1).unwrap()),
                    )))
                    .with_title("Workspace connections and documents")
                    .with_footer(
                        Actions::new(
                            Button::new(
                                &format!("primary-{index}"),
                                "Refresh all remote connections",
                            )
                            .with_icon(crate::icons::Icon::Search)
                            .role(ButtonRole::Primary),
                        )
                        .secondary(Button::new(&format!("secondary-{index}"), "View details")),
                    ),
                ) as Element
            })
            .collect(),
    ))
}

fn lay_out_grid(mut g: CardGrid, w: f64, h: f64) -> CardGrid {
    let theme = Theme::dark();
    let mut taffy = TaffyTree::<()>::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut tr = TextRenderer::new();
    let node = g.build_layout(&mut taffy, &theme, &mut map);
    let mut bindings = crate::bound_dispatch::BoundNodeMap::new();
    unsafe {
        bindings.rebuild(&g, &taffy, node, &map);
    }
    unsafe {
        crate::widget::compute_widget_layout(
            &mut taffy,
            node,
            Size {
                width: taffy::AvailableSpace::Definite(crate::num::f32_of_f64(w)),
                height: taffy::AvailableSpace::Definite(crate::num::f32_of_f64(h)),
            },
            &map,
            &bindings,
            &mut tr,
        )
    }
    .unwrap();
    g.apply_layout(&taffy, node, Point::ZERO);
    g
}

// ---- CardGrid ----

#[test]
fn card_grid_resolves_columns_from_semantic_width_and_content_rows() {
    let theme = Theme::dark();
    let min = CardWidth::Regular.min_width(&theme);
    let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
    let width = 3.0 * min + 2.0 * gap + 20.0;
    let g = lay_out_grid(
        CardGrid::new((0..6).map(|i| text_card(&format!("card {i}"))).collect()),
        width,
        2000.0,
    );
    let first_row: Vec<_> = g
        .cards
        .iter()
        .filter(|card| card.get_rect().y0 == g.rect.y0)
        .collect();
    assert_eq!(first_row.len(), 3);
    let gap_between = g.cards[1].get_rect().x0 - g.cards[0].get_rect().x1;
    assert!((gap_between - gap).abs() <= 1.0);
    for card in &g.cards {
        assert!(
            (card.get_rect().width() - g.cards[0].get_rect().width()).abs() <= 1.0,
            "{:?}",
            g.cards.iter().map(|c| c.get_rect()).collect::<Vec<_>>()
        );
    }
    // A row of short cards is short: no fixed height stretches them.
    let line = f64::from(theme.text(TextRole::Body).line_height_px());
    assert!(g.cards[0].get_rect().height() < line * 4.0);

    // Narrow: one full-width column.
    let narrow = lay_out_grid(
        CardGrid::new((0..4).map(|i| text_card(&format!("card {i}"))).collect()),
        min - 10.0,
        2000.0,
    );
    assert_eq!(narrow.columns_for_width(min - 10.0), 1);
    assert!((narrow.cards[0].get_rect().width() - (min - 10.0)).abs() < 0.5);

    // Wider cards take fewer columns at the same width; larger text widens them.
    let wide = CardGrid::new((0..6).map(|i| text_card(&format!("{i}"))).collect())
        .with_card_width(CardWidth::Wide);
    assert!(CardWidth::Wide.min_width(&theme) > min);
    assert!(CardWidth::Compact.min_width(&theme) < min);
    assert!(CardWidth::Regular.min_width(&Theme::dark().with_text_scale(2.0)) > min * 1.5);
    let wide = lay_out_grid(wide, width, 2000.0);
    assert!(wide.columns_for_width(width) < 3);
}

#[test]
fn content_rows_use_each_rows_largest_card_and_align_footers() {
    let mut harness =
        FrameHarness::new_with_theme(crate::State::new(), fixture(), (700, 800), Theme::dark());
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
    assert_eq!(
        crate::testing::clicks("opened"),
        1,
        "the visible card footer invokes its published callback"
    );
}

#[test]
fn scaled_card_regions_contain_content_and_match_fresh_geometry() {
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
        let fresh =
            FrameHarness::new_with_theme(crate::State::new(), long_fixture(), (240, 1800), theme);
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
        assert_eq!(
            resized.focused_access_node().unwrap().name.as_deref(),
            Some("View details")
        );
    }
}

#[test]
fn scrolling_reaches_the_last_scaled_action() {
    let root = Scrollable::new_vertical().with_child(long_fixture());
    let mut harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(root),
        (240, 400),
        Theme::dark().with_text_scale(2.0),
    );
    let before = harness.inspection();
    assert!(rect(&before.root.children[0]).height() > 400.0);
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
}

#[test]
fn actions_trail_and_keep_manual_order() {
    let root = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Actions::new(Button::new("save", "Save changes"))));
    let harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(root),
        (600, 300),
        Theme::dark(),
    );
    let snapshot = harness.inspection();
    let action = &snapshot.root.children[0].children[0];
    assert!((rect(action).x1 - 600.0).abs() < 1.0);

    let actions = Actions::new(Button::new("primary", "Primary"))
        .secondary(Button::new("secondary", "Secondary"));
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
}

#[test]
fn cards_in_a_scroll_stack_keep_their_content_height() {
    let invocations = Rc::new(Cell::new(0));
    let counter = invocations.clone();
    let mut content = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(8.0);
    for index in 0..8 {
        content = content.push(Box::new(
            Card::new(Box::new(Text::new(
                &"A useful multi-line description that must remain inside its own card. ".repeat(3),
            )))
            .with_title(format!("Application {index}"))
            .with_footer(Actions::new(Button::new(
                &format!("details-{index}"),
                "View details",
            ))),
        ));
    }
    content = content
        .push(Box::new(
            Text::new("All applications").with_id("trailing-heading"),
        ))
        .push(Box::new(
            Button::new("final-action", "Open all applications").on_click(move || {
                counter.set(counter.get() + 1);
            }),
        ));
    let mut harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(Scrollable::new_vertical().with_child(Box::new(content))),
        (480, 360),
        Theme::dark(),
    );
    let before = harness.inspection();
    let content = &before.root.children[0];
    let cards = &content.children[..8];
    for (index, card) in cards.iter().enumerate() {
        assert_contained(card, rect(card));
        if let Some(next) = cards.get(index + 1) {
            assert!(rect(next).y0 >= rect(card).y1 + 7.0);
        }
    }
    let trailing = rect(find(&before.root, "trailing-heading"));
    assert!(trailing.y0 >= rect(cards.last().unwrap()).y1 + 7.0);
    assert!(rect(content).height() > 360.0);
    harness.click_at(8.0, 8.0);
    harness.scroll(0.0, -10_000.0);
    harness.click_named("final-action").unwrap();
    assert_eq!(invocations.get(), 1);
}

#[test]
fn a_card_keeps_intrinsic_height_in_a_short_flex_parent() {
    let mut parent = Column::new()
        .with_no_padding()
        .push(Box::new(Card::new(Box::new(
            Container::new().with_no_padding().with_height(80.0),
        ))));
    let mut tree = TaffyTree::new();
    let mut map = HashMap::new();
    let node = parent.build_layout(&mut tree, &Theme::dark(), &mut map);
    tree.compute_layout(
        node,
        Size {
            width: taffy::AvailableSpace::Definite(300.0),
            height: taffy::AvailableSpace::Definite(40.0),
        },
    )
    .unwrap();
    parent.apply_layout(&tree, node, Point::ZERO);
    assert!(parent.children()[0].get_rect().height() >= 80.0);
}

#[test]
fn a_titled_card_title_is_a_subheading_that_follows_the_text_scale() {
    fn first_text(node: &InspectionNode) -> Option<[f64; 4]> {
        if node.widget == "Text" {
            return Some(node.rect);
        }
        node.children.iter().find_map(first_text)
    }
    let title_height = |scale: f32| {
        let harness = FrameHarness::new_with_theme(
            crate::State::new(),
            Box::new(Card::new(Box::new(Text::new("body"))).with_title("Group")),
            (400, 200),
            Theme::dark().with_text_scale(scale),
        );
        first_text(&harness.inspection().root).expect("card title text")[3]
    };
    let theme = Theme::dark();
    let expected = f64::from(theme.text(TextRole::Subheading).line_height_px());
    let base = title_height(1.0);
    assert!((base - expected).abs() < 1.0, "{base} vs {expected}");
    assert!((title_height(2.0) - base * 2.0).abs() < 1.5);
}

#[test]
fn a_pressable_card_activates_but_its_own_actions_do_not_open_it() {
    let opened = Rc::new(Cell::new(0));
    let counter = opened.clone();
    let card = Card::new(Box::new(Text::new("Quarterly report")))
        .with_title("Report.pdf")
        .with_footer(Actions::new(
            Button::new("share", "Share").on_click(crate::testing::record_click("shared")),
        ))
        .on_activate("open-report", move || counter.set(counter.get() + 1));
    let mut harness = FrameHarness::new_with_theme(
        crate::State::new(),
        Box::new(Column::new().with_no_padding().push(Box::new(card))),
        (400, 300),
        Theme::dark(),
    );
    let inspection = harness.inspection();
    let card = find(&inspection.root, "open-report");
    let body = rect(&card.children[1]);
    let share = rect(&card.children[2].children[0]);
    harness.click_at(body.center().x, body.center().y);
    assert_eq!(opened.get(), 1);
    harness.click_at(share.center().x, share.center().y);
    assert_eq!(opened.get(), 1, "the footer's action is its own");
    harness
        .perform_named("open-report", &crate::a11y::Action::Activate)
        .unwrap();
    assert_eq!(opened.get(), 2);
}

#[test]
fn card_face_reserves_its_title_and_paints_a_stripe_inside_the_shape() {
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let face = CardFace::new()
        .title("Comment from Ada")
        .leading_stripe(Tone::of_identity("ada"));
    let rect = Rect::new(0.0, 0.0, 320.0, 160.0);
    let body = face.body_rect(&mut text, &theme, rect);
    let m = CardMetrics::new(&theme);
    assert!(
        body.y0 > rect.y0 + m.padding,
        "the title sits above the body"
    );
    assert!((body.x0 - m.padding).abs() < 0.01);
    let chrome = face.height_for_body(&mut text, &theme, 320.0, 40.0);
    assert!((chrome - (body.y0 + 40.0 + m.padding)).abs() < 1.0);
    let mut scene = Scene::new();
    face.paint(
        &mut PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme),
        rect,
    );
    assert!(!scene.encoding().draw_tags.is_empty());
}
