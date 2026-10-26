use super::*;
use crate::a11y::Role;
use crate::synthetic_input::{mouse_press, mouse_release, named_key};
use crate::testing::dispatch;
use taffy::prelude::TaffyMaxContent;

fn lay_out(tile: &mut QuickTile, theme: &Theme, width: f32) {
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = tile.build_layout(&mut taffy, theme, &mut nodes);
    let root = taffy
        .new_with_children(
            Style {
                size: taffy::Size {
                    width: length(width),
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            },
            &[node],
        )
        .unwrap();
    taffy
        .compute_layout(root, taffy::Size::MAX_CONTENT)
        .unwrap();
    tile.apply_layout(&taffy, node, Point::ZERO);
}

fn draw(tile: &mut QuickTile, theme: &Theme) -> Scene {
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    tile.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        &PresenceCtx::default(),
    );
    scene
}

fn wifi() -> QuickTile {
    QuickTile::new("quick_settings_network", Icon::Wifi, "Wi-Fi")
        .detail("Home")
        .on(true)
        .with_details("Network details")
}

fn cursor(point: Point) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(point.x, point.y),
    }
}

#[test]
fn a_tile_fills_its_width_at_the_two_line_height() {
    for width in [180.0, 420.0] {
        let theme = Theme::dark();
        let mut tile = wifi();
        lay_out(&mut tile, &theme, width);
        assert_eq!(tile.get_rect().width(), f64::from(width));
        assert_eq!(tile.get_rect().height(), QuickTile::height(&theme));
        assert!(QuickTile::height(&theme) >= f64::from(theme.tokens.metrics.row_double));
    }
}

#[test]
fn larger_text_grows_the_tile_to_fit_both_lines() {
    let normal = QuickTile::height(&Theme::dark());
    let large = QuickTile::height(&Theme::dark().with_text_scale(2.0));
    assert!(large > normal, "{large} > {normal}");
    let theme = Theme::dark().with_text_scale(2.0);
    let label = theme.text(TextRole::Label).line_height_px();
    let detail = theme.text(TextRole::Caption).line_height_px();
    assert!(large >= f64::from(label + detail));
}

#[test]
fn the_chevron_half_is_one_pointer_target_at_the_trailing_edge() {
    let theme = Theme::dark();
    let mut tile = wifi();
    lay_out(&mut tile, &theme, 200.0);
    draw(&mut tile, &theme);
    let (switch, details) = tile.parts();
    let details = details.expect("a tile with a section has a chevron half");
    assert_eq!(details.width(), QuickTile::details_width(&theme));
    assert_eq!(details.x1, tile.get_rect().x1);
    assert_eq!(switch.x1, details.x0);
    let plain = QuickTile::new("quick_settings_night_light", Icon::Moon, "Night light");
    assert_eq!(plain.parts().1, None, "no section, no chevron");
}

#[test]
fn a_narrow_tile_keeps_its_chevron_inside_itself() {
    let (switch, details) = QuickTile::split(Rect::new(10.0, 0.0, 30.0, 56.0), 44.0);
    assert_eq!(details.x0, 10.0);
    assert_eq!(switch.width(), 0.0);
}

#[test]
fn a_press_on_the_switch_half_switches_and_one_on_the_chevron_opens_details() {
    let theme = Theme::dark();
    let mut tile = wifi();
    lay_out(&mut tile, &theme, 240.0);
    draw(&mut tile, &theme);
    let (switch, details) = tile.parts();
    dispatch(&mut tile, &cursor(switch.center()));
    assert!(dispatch(&mut tile, &mouse_press()));
    assert_eq!(tile.take_outcome(), None, "nothing happens until release");
    dispatch(&mut tile, &mouse_release());
    assert_eq!(tile.take_outcome(), Some(QuickTileOutcome::Switch));
    assert_eq!(tile.take_outcome(), None, "the outcome drains");

    dispatch(&mut tile, &cursor(details.unwrap().center()));
    dispatch(&mut tile, &mouse_press());
    dispatch(&mut tile, &mouse_release());
    assert_eq!(tile.take_outcome(), Some(QuickTileOutcome::Details));
}

#[test]
fn a_press_dragged_off_the_part_does_nothing() {
    let theme = Theme::dark();
    let mut tile = wifi();
    lay_out(&mut tile, &theme, 240.0);
    let (switch, details) = tile.parts();
    dispatch(&mut tile, &cursor(switch.center()));
    dispatch(&mut tile, &mouse_press());
    dispatch(&mut tile, &cursor(details.unwrap().center()));
    dispatch(&mut tile, &mouse_release());
    assert_eq!(tile.take_outcome(), None);
}

#[test]
fn a_tile_that_is_not_enabled_ignores_presses_and_says_so() {
    let theme = Theme::dark();
    let mut tile = wifi().enabled(false);
    lay_out(&mut tile, &theme, 240.0);
    let switch = tile.parts().0.center();
    dispatch(&mut tile, &cursor(switch));
    assert!(!dispatch(&mut tile, &mouse_press()));
    dispatch(&mut tile, &mouse_release());
    assert_eq!(tile.take_outcome(), None);
    let node = tile.accessibility().unwrap();
    assert!(node.children.iter().all(|part| part.state.disabled));
}

#[test]
fn the_keyboard_switches_moves_to_the_chevron_and_opens_details() {
    let theme = Theme::dark();
    let mut tile = wifi();
    lay_out(&mut tile, &theme, 240.0);
    tile.set_focus(true);
    assert!(dispatch(&mut tile, &named_key(NamedKey::Space)));
    assert_eq!(tile.take_outcome(), Some(QuickTileOutcome::Switch));
    assert!(dispatch(&mut tile, &named_key(NamedKey::ArrowRight)));
    assert!(dispatch(&mut tile, &named_key(NamedKey::Enter)));
    assert_eq!(tile.take_outcome(), Some(QuickTileOutcome::Details));
    assert!(dispatch(&mut tile, &named_key(NamedKey::ArrowLeft)));
    dispatch(&mut tile, &named_key(NamedKey::Enter));
    assert_eq!(tile.take_outcome(), Some(QuickTileOutcome::Switch));
}

#[test]
fn a_tile_is_a_switch_beside_a_details_button_for_assistive_technology() {
    let theme = Theme::dark();
    let mut tile = wifi();
    lay_out(&mut tile, &theme, 240.0);
    tile.set_details_open(1.0);
    let node = tile.accessibility().unwrap();
    assert_eq!(node.role, Role::Group);
    let [switch, details] = node.children.as_slice() else {
        panic!("a switch and a details button");
    };
    assert_eq!(switch.role, Role::Switch);
    assert_eq!(switch.name.as_deref(), Some("Wi-Fi"));
    assert_eq!(switch.description.as_deref(), Some("Home"));
    assert_eq!(switch.state.checked, Some(true));
    assert_eq!(details.role, Role::Button);
    assert_eq!(details.name.as_deref(), Some("Network details"));
    assert_eq!(details.state.expanded, Some(true));
    assert!(switch.action_target.is_some() && details.action_target.is_some());

    let plain = QuickTile::new("quick_settings_night_light", Icon::Moon, "Night light");
    let node = plain.accessibility().unwrap();
    assert_eq!(node.role, Role::Switch);
    assert_eq!(node.state.checked, Some(false));
}

#[test]
fn assistive_technology_presses_each_part() {
    let mut tile = wifi();
    let mut doc = automerge::AutoCommit::new();
    let mut edits = crate::widget::Edits::lend(&mut doc);
    let path = automerge::ROOT;
    let details = [crate::a11y::hash_key(&"details")];
    assert!(tile.perform_action(&crate::a11y::Action::Activate, &details, &mut edits, &path));
    assert_eq!(tile.take_outcome(), Some(QuickTileOutcome::Details));
    let switch = [crate::a11y::hash_key(&"switch")];
    assert!(tile.perform_action(&crate::a11y::Action::Activate, &switch, &mut edits, &path));
    assert_eq!(tile.take_outcome(), Some(QuickTileOutcome::Switch));
}

#[test]
fn every_part_is_a_named_target() {
    let theme = Theme::dark();
    let mut tile = wifi();
    lay_out(&mut tile, &theme, 240.0);
    let names: Vec<String> = tile
        .test_rects()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    assert_eq!(
        names,
        ["quick_settings_network", "quick_settings_network_details"]
    );
}

#[test]
fn hover_and_focus_ask_for_a_frame_and_leaving_clears_hover() {
    let theme = Theme::dark();
    let mut tile = wifi();
    lay_out(&mut tile, &theme, 240.0);
    draw(&mut tile, &theme);
    assert!(!tile.needs_redraw());
    tile.set_pointer(Some(tile.parts().0.center()));
    assert!(tile.needs_redraw(), "hovering a half repaints");
    draw(&mut tile, &theme);
    tile.set_pointer(Some(tile.parts().0.center()));
    assert!(!tile.needs_redraw(), "the same half again does not");
    tile.set_focused_part(Some(QuickTilePart::Details));
    assert!(tile.needs_redraw());
    tile.set_pointer(None);
    assert_eq!(tile.hovered(), None);
}

#[test]
fn a_tile_without_a_section_cannot_focus_or_press_a_chevron() {
    let mut tile = QuickTile::new("quick_settings_night_light", Icon::Moon, "Night light");
    tile.set_focused_part(Some(QuickTilePart::Details));
    assert!(!tile.is_focused());
    assert!(!tile.activate(QuickTilePart::Details));
}

#[test]
fn the_tile_paints_from_the_theme_it_is_given() {
    for on in [false, true] {
        let mut tile = wifi().on(on);
        lay_out(&mut tile, &Theme::dark(), 240.0);
        let dark = draw(&mut tile, &Theme::dark());
        let light = draw(&mut tile, &Theme::light());
        assert_ne!(
            dark.encoding().draw_data,
            light.encoding().draw_data,
            "on={on}: dark and light differ"
        );
    }
    let on = draw_at(&mut wifi().on(true));
    let off = draw_at(&mut wifi().on(false));
    assert_ne!(on, off, "a tile that is on is filled");
}

fn draw_at(tile: &mut QuickTile) -> Vec<u32> {
    let theme = Theme::dark();
    lay_out(tile, &theme, 240.0);
    draw(tile, &theme).encoding().draw_data.clone()
}
