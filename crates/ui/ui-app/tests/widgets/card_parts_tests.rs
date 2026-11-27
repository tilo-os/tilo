//! Cards: a selected card's edge, the actions scrolling reaches, a card's
//! parts in their places, and a pressable card's own actions.

use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::motion::*;
use tilo_ui_base::offscreen::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::peniko::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_render::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

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
                            .with_icon(tilo_ui_base::icons::Icon::Search)
                            .role(ButtonRole::Primary),
                        )
                        .secondary(Button::new(&format!("secondary-{index}"), "View details")),
                    ),
                ) as Element
            })
            .collect(),
    ))
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
            tilo_ui_app::State::new(),
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
        let width = tilo_ui_base::surface::selected_edge_width(&theme);
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

#[test]
fn scrolling_reaches_the_last_scaled_action() {
    let root = Scrollable::new_vertical().with_child(long_fixture());
    let mut harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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
            tilo_ui_app::State::new(),
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
            tilo_ui_app::State::new(),
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
            tilo_ui_app::State::new(),
            Box::new(
                Card::new(Box::new(Text::new("body")))
                    .with_section_header(tilo_ui_app::widget::SectionHeader::group("Saturday").count(10)),
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

#[test]
fn a_pressable_card_activates_but_its_own_actions_do_not_open_it() {
    let opened = Rc::new(Cell::new(0));
    let counter = opened.clone();
    let card = Card::new(Box::new(Text::new("Quarterly report")))
        .with_title("Report.pdf")
        .with_footer(Actions::new(
            Button::new("share", "Share").on_click(tilo_ui_app::testing::record_click("shared")),
        ))
        .on_activate("open-report", move || counter.set(counter.get() + 1));
    let mut harness = Driver::new_with_theme(
        tilo_ui_app::State::new(),
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
        .perform_named("open-report", &tilo_ui_base::a11y::Action::Activate)
        .unwrap();
    assert_eq!(opened.get(), 2);
}
