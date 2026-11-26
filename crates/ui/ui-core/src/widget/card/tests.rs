use super::*;
use crate::app::driver::Driver;
use crate::headless::InspectionNode;
use crate::widget::ButtonRole;
use crate::widget::{Column, Container, Scrollable};
use std::cell::Cell;
use std::rc::Rc;

fn rect(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
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
    let mut bindings = crate::widget::bound_nodes::BoundNodeMap::new();
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

#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "pixel coordinates inside the rendered frame"
)]
fn a_selected_card_replaces_its_edge_with_one_accent_outline() {
    for theme in [Theme::dark(), Theme::light()] {
        let grid = CardGrid::new(vec![
            Box::new(Card::new(Box::new(Text::new("Chosen"))).selected(true)),
            Box::new(Card::new(Box::new(Text::new("Other")))),
        ]);
        let harness = Driver::new_with_theme(
            crate::State::new(),
            Box::new(Container::new().with_child(Box::new(grid))),
            (600, 300),
            theme.clone(),
        );
        let inspection = harness.inspection();
        let mut cards = Vec::new();
        collect_named(&inspection.root, "Card", &mut cards);
        assert_eq!(cards.len(), 2);
        let rgba = harness.render_rgba();
        let pixel = |x: f64, y: f64| {
            let (x, y) = (x.floor() as usize, y.floor() as usize);
            let at = (y * 600 + x) * 4;
            [rgba[at], rgba[at + 1], rgba[at + 2]]
        };
        let accent = theme.tokens.colors.intent.accent.to_rgba8();
        let accent = [accent.r, accent.g, accent.b];
        let ground = theme.tokens.colors.surfaces.content.to_rgba8();
        let ground = [ground.r, ground.g, ground.b];
        let near = |a: [u8; 3], b: [u8; 3]| a.iter().zip(b).all(|(a, b)| a.abs_diff(b) <= 12);
        let width = crate::surface::selected_edge_width(&theme);
        let (chosen, other) = (rect(cards[0]), rect(cards[1]));
        let mid = chosen.center().y;
        assert!(
            near(pixel(chosen.x0 + width / 2.0, mid), accent),
            "{}: the edge is in the accent",
            theme.name
        );
        assert!(
            near(pixel(chosen.x0 - 2.0, mid), ground),
            "{}: nothing is drawn outside the card",
            theme.name
        );
        assert!(
            !near(pixel(chosen.x0 + width + 2.0, mid), accent),
            "{}: one outline, not two",
            theme.name
        );
        assert!(
            !near(pixel(other.x0 + width / 2.0, other.center().y), accent),
            "{}: an unselected card keeps its rest edge",
            theme.name
        );
    }
}

fn collect_named<'a>(node: &'a InspectionNode, name: &str, out: &mut Vec<&'a InspectionNode>) {
    if node.widget == name {
        out.push(node);
    }
    for child in &node.children {
        collect_named(child, name, out);
    }
}

#[test]
fn scrolling_reaches_the_last_scaled_action() {
    let root = Scrollable::new_vertical().with_child(long_fixture());
    let mut harness = Driver::new_with_theme(
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

tilo_test_env::scenario! {
    /// Card columns, grids and heights.
    /// Each phase was a test of its own and builds its own fixture.
    fn cards_reflow_and_keep_their_content_height;


    phase card_grid_resolves_columns_from_semantic_width_and_content_rows;
    {
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
        let line = f64::from(theme.text(TextRole::Body).line_height_px());
        assert!(g.cards[0].get_rect().height() < line * 4.0);

        let narrow = lay_out_grid(
            CardGrid::new((0..4).map(|i| text_card(&format!("card {i}"))).collect()),
            min - 10.0,
            2000.0,
        );
        assert_eq!(narrow.columns_for_width(min - 10.0), 1);
        assert!((narrow.cards[0].get_rect().width() - (min - 10.0)).abs() < 0.5);

        let wide = CardGrid::new((0..6).map(|i| text_card(&format!("{i}"))).collect())
            .with_card_width(CardWidth::Wide);
        assert!(CardWidth::Wide.min_width(&theme) > min);
        assert!(CardWidth::Compact.min_width(&theme) < min);
        assert!(CardWidth::Regular.min_width(&Theme::dark().with_text_scale(2.0)) > min * 1.5);
        let wide = lay_out_grid(wide, width, 2000.0);
        assert!(wide.columns_for_width(width) < 3);
    }

    phase a_card_led_by_its_art_shows_the_art_first_and_its_caption_under_it;
    {
        let theme = Theme::dark();
        let art = || Box::new(Container::new().with_no_padding().with_height(120.0)) as Element;
        let actions = || {
            Box::new(
                Actions::new(Button::new("use", "Use").role(ButtonRole::Primary))
                    .secondary(Button::new("customize", "Customize")),
            ) as Element
        };
        let card = Card::with_media(art())
            .with_title("Placeholder")
            .with_title_action("Dark graphite", actions());
        let g = lay_out_grid(CardGrid::new(vec![Box::new(card)]), 400.0, 1000.0);
        let card = g.cards[0].get_rect();
        let parts = g.cards[0].children();
        assert_eq!(
            parts.len(),
            2,
            "one header, replaced in place, under the art"
        );
        let media = parts[0].get_rect();
        let header = parts[1].get_rect();
        let pad = f64::from(theme.tokens.spacing.rhythm.section);
        assert!((media.height() - 120.0).abs() < 0.5, "{media:?}");
        assert!(
            (media.y0 - (card.y0 + pad)).abs() < 0.5,
            "the art leads the card"
        );
        assert!(
            (media.width() - (card.width() - 2.0 * pad)).abs() < 0.5,
            "the art spans the inner width: {media:?} in {card:?}"
        );
        assert!(header.y0 >= media.y1, "the caption sits under the art");
        let caption = parts[1].children();
        assert!(
            caption[1].get_rect().x1 <= header.x1 + 0.5
                && caption[1].get_rect().x0 > caption[0].get_rect().x0,
            "the actions trail the name on the same row"
        );
    }

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
    let mut harness = Driver::new_with_theme(
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

tilo_test_env::scenario! {
    /// Card titles, faces, actions and presses.
    /// Each phase was a test of its own and builds its own fixture.
    fn a_cards_title_face_and_actions_stand_where_they_should;

    phase actions_trail_and_keep_manual_order;
    {
        let root = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(Actions::new(Button::new("save", "Save changes"))));
        let harness = Driver::new_with_theme(
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
        let harness = Driver::new_with_theme(
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

    phase a_card_headed_by_a_section_header_shows_its_title_above_the_body;
    {
        let harness = Driver::new_with_theme(
            crate::State::new(),
            Box::new(
                Card::new(Box::new(Text::new("body")))
                    .with_section_header(crate::widget::SectionHeader::group("Saturday").count(10)),
            ),
            (400, 300),
            Theme::dark(),
        );
        let root = harness.inspection().root;
        fn named<'a>(node: &'a InspectionNode, widget: &str) -> Option<&'a InspectionNode> {
            if node.widget == widget {
                return Some(node);
            }
            node.children.iter().find_map(|child| named(child, widget))
        }
        fn texts<'a>(node: &'a InspectionNode, out: &mut Vec<&'a InspectionNode>) {
            if node.widget == "Text" {
                out.push(node);
            }
            for child in &node.children {
                texts(child, out);
            }
        }
        let header =
            rect(named(&root, "SectionHeader").expect("the card is headed by a section header"));
        let mut all = Vec::new();
        texts(&root, &mut all);
        let body = rect(all.last().expect("the body"));
        assert!(
            header.y1 <= body.y0 + 0.5,
            "the header stands above the body: {header:?} {body:?}"
        );
    }


}
