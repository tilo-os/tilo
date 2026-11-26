use super::*;
use crate::a11y::Role;
use crate::synthetic_input::{mouse_press, mouse_release, named_key};
use crate::testing::dispatch;
use taffy::prelude::TaffyMaxContent;
use taffy::style_helpers::length;

fn lay_out(tile: &mut QuickTile, theme: &Theme, width: f32) {
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
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
        .compute_layout_with_measure(
            root,
            taffy::Size::MAX_CONTENT,
            |known, available, id, _, _| {
                nodes.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, &mut text)
                })
            },
        )
        .unwrap();
    tile.apply_layout(&taffy, node, Point::ZERO);
}

fn lay_out_row(left: &mut QuickTile, right: &mut QuickTile, theme: &Theme, width: f32) {
    let mut taffy = TaffyTree::<()>::new();
    let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    let a = left.build_layout(&mut taffy, theme, &mut nodes);
    let b = right.build_layout(&mut taffy, theme, &mut nodes);
    let row = taffy
        .new_with_children(
            Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            },
            &[a, b],
        )
        .unwrap();
    let root = taffy
        .new_with_children(
            Style {
                size: taffy::Size {
                    width: length(width),
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            },
            &[row],
        )
        .unwrap();
    taffy
        .compute_layout_with_measure(
            root,
            taffy::Size::MAX_CONTENT,
            |known, available, id, _, _| {
                nodes.get(&id).map_or(taffy::Size::ZERO, |&p| unsafe {
                    (*p).measure(known, available, &mut text)
                })
            },
        )
        .unwrap();
    left.apply_layout(&taffy, a, Point::ZERO);
    right.apply_layout(&taffy, b, Point::ZERO);
}

fn draw(tile: &mut QuickTile, theme: &Theme) -> Scene {
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    text.set_text_scale(theme.text_scale);
    tile.draw(&mut crate::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        theme,
        crate::presence::PresenceCtx::default().draw_env(),
    ));
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

tilo_test_env::scenario! {
    fn a_tile_sizes_itself_for_its_lines_and_keeps_its_chevron_inside;

    phase a_tile_sizes_itself_for_its_lines;
    {
        let theme = Theme::dark();
        let metrics = TileMetrics::of(&theme);
        let mut wide = wifi();
        lay_out(&mut wide, &theme, 420.0);
        assert_eq!(wide.get_rect().width(), 420.0);
        assert_eq!(
            wide.get_rect().height(),
            metrics.height_one,
            "a state line that fits leaves the tile one line tall"
        );
        assert!(metrics.height_one >= f64::from(theme.tokens.metrics.row_double));

        // Narrow, and sharing its row: the state line breaks, and the tile is
        // as tall as the two lines it now draws.
        let corridor = "The network at the far end of the corridor, by the kitchen api";
        let mut whole = wifi().detail(corridor);
        lay_out(&mut whole, &theme, 600.0);
        assert_eq!(whole.get_rect().height(), metrics.height_one);
        let (mut left, mut right) = (
            wifi().detail(corridor).share(0.5),
            wifi().detail(corridor).share(0.5),
        );
        lay_out_row(&mut left, &mut right, &theme, 600.0);
        assert_eq!(left.get_rect().width(), 300.0, "it draws in half the area");
        assert_eq!(
            left.get_rect().height(),
            metrics.height,
            "the tile grows for the second line"
        );
        assert_eq!(right.get_rect().height(), metrics.height);

        let (mut left, mut right) = (wifi().share(0.5), wifi().share(0.5));
        lay_out_row(&mut left, &mut right, &theme, 600.0);
        assert_eq!(left.get_rect().height(), metrics.height_one);
        assert_eq!(right.get_rect().height(), metrics.height_one);

        // The tile is tall enough for a state line on both lines, and larger text
        // grows it to fit them.
        let label = theme.text(TextRole::Label).line_height_px();
        let detail = theme.text(TextRole::Caption).line_height_px();
        assert!(
            QuickTile::height(&theme) >= f64::from(label + 2.0 * detail),
            "{} >= {}",
            QuickTile::height(&theme),
            f64::from(label + 2.0 * detail)
        );
        let large_theme = Theme::dark().with_text_scale(2.0);
        let normal = QuickTile::height(&theme);
        let large = QuickTile::height(&large_theme);
        assert!(large > normal, "{large} > {normal}");
        let label = large_theme.text(TextRole::Label).line_height_px();
        let detail = large_theme.text(TextRole::Caption).line_height_px();
        assert!(large >= f64::from(label + detail));

        // The state line breaks after the last word that fits and elides the rest.
        let style = theme.text(TextRole::Caption);
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        let spare = text.measure_style("The network at", None, &style).0 + 1.0;
        let (first, second) = detail_lines(
            &mut text,
            "The network at the far end of the corridor",
            spare,
            &style,
        );
        assert_eq!(first, "The network at");
        let second = second.expect("the rest goes on a second line");
        assert!(!second.is_empty(), "the second line says something");
        let (one, none) = detail_lines(&mut text, "The network", spare, &style);
        assert_eq!(one, "The network", "a state line that fits keeps one line");
        assert!(none.is_none());
    }

    phase the_tile_paints_from_the_theme_it_is_given;
    {
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
}

tilo_test_env::scenario! {
    fn a_tile_s_parts_are_pressed_by_pointer_keyboard_and_assistive_technology;

    phase a_press_on_a_part_acts_on_release_and_a_press_dragged_off_does_nothing;
    {
        let theme = Theme::dark();
        let mut tile = wifi();
        lay_out(&mut tile, &theme, 240.0);
        draw(&mut tile, &theme);
        let (switch, details) = tile.parts();
        let details = details.expect("a tile with a section has a chevron half");
        assert_eq!(details.width(), QuickTile::details_width(&theme));
        assert_eq!(details.x1, tile.get_rect().x1);
        assert_eq!(switch.x1, details.x0);
        dispatch(&mut tile, &cursor(switch.center()));
        assert!(dispatch(&mut tile, &mouse_press()));
        assert_eq!(tile.take_outcome(), None, "nothing happens until release");
        dispatch(&mut tile, &mouse_release());
        assert_eq!(tile.take_outcome(), Some(QuickTileOutcome::Switch));
        assert_eq!(tile.take_outcome(), None, "the outcome drains");

        dispatch(&mut tile, &cursor(details.center()));
        dispatch(&mut tile, &mouse_press());
        dispatch(&mut tile, &mouse_release());
        assert_eq!(tile.take_outcome(), Some(QuickTileOutcome::Details));

        // A press dragged off the part it started on does nothing.
        dispatch(&mut tile, &cursor(switch.center()));
        dispatch(&mut tile, &mouse_press());
        dispatch(&mut tile, &cursor(details.center()));
        dispatch(&mut tile, &mouse_release());
        assert_eq!(tile.take_outcome(), None);
    }

    phase a_tile_that_is_not_enabled_ignores_presses_and_says_so;
    {
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

    phase the_keyboard_switches_moves_to_the_chevron_and_opens_details;
    {
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

    phase a_tile_is_a_switch_beside_a_details_button_for_assistive_technology;
    {
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
        assert_eq!(plain.parts().1, None, "no section, no chevron");
        let node = plain.accessibility().unwrap();
        assert_eq!(node.role, Role::Switch);
        assert_eq!(node.state.checked, Some(false));

        // A tile without a section cannot focus or press a chevron.
        let mut plain = QuickTile::new("quick_settings_night_light", Icon::Moon, "Night light");
        plain.set_focused_part(Some(QuickTilePart::Details));
        assert!(!plain.is_focused());
        assert!(!plain.activate(QuickTilePart::Details));
    }

    phase assistive_technology_presses_each_part;
    {
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

}

fn draw_at(tile: &mut QuickTile) -> Vec<u32> {
    let theme = Theme::dark();
    lay_out(tile, &theme, 240.0);
    draw(tile, &theme).encoding().draw_data.clone()
}
