//! A dock item that magnifies under the pointer: the item under it is larger than its
//! neighbours, those than the items further off, and leaving settles every one back.

use super::{Button, Container, Magnify, MagnifyProps, Row};
use crate::app::driver::Driver;
use crate::time::Duration;
use tilo_ui_base::component::Described;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::Rhythm;

/// A row of `count` tile buttons, each in a `Magnify` of `zoom` and a container between them, as
/// the dock draws them: a view call around the button is a wrapper, and the scale reaches the
/// button through it.
fn dock(zoom: f64, count: usize) -> Row {
    let mut row = Row::new().with_rhythm(Rhythm::Sibling).with_id("dock");
    for index in 0..count {
        let tile = Button::icon_only(
            &format!("t{index}"),
            tilo_ui_base::icons::Icon::X,
            &format!("Tile {index}"),
        );
        let mut wrapper = Container::new().hug().with_id(format!("p{index}"));
        wrapper.set_child(Box::new(tile));
        row = row.push(Box::new(Magnify::from_props(
            &format!("m{index}"),
            MagnifyProps {
                zoom,
                body: Some(Box::new(wrapper)),
            },
        )));
    }
    row
}

fn driver_with(zoom: f64, theme: Theme) -> Driver {
    let mut driver = Driver::new_with_theme(
        crate::State::new(),
        Box::new(dock(zoom, 5)),
        (600, 140),
        theme,
    );
    driver.relayout();
    driver
}

/// Run frames until the motion stops, recording each size the tile under the pointer took.
fn settle(driver: &mut Driver, along: &mut Vec<f64>) -> bool {
    driver.wait_until(Duration::from_secs(3), |driver| {
        if let Some(rect) = driver.targets().get("t2") {
            let height = rect.height();
            if along.last().is_none_or(|last| (last - height).abs() > 0.01) {
                along.push(height);
            }
        }
        driver.next_frame().is_none()
    })
}

#[test]
fn the_item_under_the_pointer_grows_more_than_its_neighbours() {
    for theme in [Theme::dark(), Theme::light()] {
        let mut driver = driver_with(0.6, theme);
        let rest = driver.targets().clone();
        for name in ["t0", "t1", "t2", "t3", "t4"] {
            assert!(
                (rest[&name.to_owned()].height() - rest["t2"].height()).abs() < 0.5,
                "{name} stands at the dock's rest size: {rest:?}"
            );
        }
        let centre = rest["t2"].center();
        driver.hover_at(centre.x, centre.y);
        let mut along = Vec::new();
        assert!(settle(&mut driver, &mut along), "the growth settles");
        let grown = driver.targets();
        let height = |name: &str| grown.get(name).map_or(0.0, vello::kurbo::Rect::height);
        let (near, neighbour, far) = (height("t2"), height("t1"), height("t0"));
        assert!(
            near > neighbour + 2.0 && neighbour > far + 1.0,
            "the item grows most, its neighbour less, the one beyond least: {grown:?}"
        );
        assert!(
            along
                .iter()
                .any(|step| *step > rest["t2"].height() + 1.0 && *step < near - 1.0),
            "it grows through the frames between, not in one jump: {along:?}"
        );
    }
}

#[test]
fn leaving_settles_every_item_back_to_its_size() {
    let mut driver = driver_with(0.6, Theme::dark());
    let rest = driver.targets()["t2"].height();
    let centre = driver.targets()["t2"].center();
    driver.hover_at(centre.x, centre.y);
    assert!(settle(&mut driver, &mut Vec::new()));
    assert!(
        driver.targets()["t2"].height() > rest + 2.0,
        "the item grew under the pointer"
    );
    driver.hover_at(560.0, 8.0);
    assert!(settle(&mut driver, &mut Vec::new()));
    for name in ["t0", "t1", "t2", "t3", "t4"] {
        let height = driver.targets()[name].height();
        assert!(
            (height - rest).abs() < 0.5,
            "{name} is back at rest: {height} against {rest}"
        );
    }
}

#[test]
fn with_zoom_off_no_item_grows() {
    let mut driver = driver_with(0.0, Theme::dark());
    let rest = driver.targets()["t2"].height();
    let centre = driver.targets()["t2"].center();
    driver.hover_at(centre.x, centre.y);
    assert!(settle(&mut driver, &mut Vec::new()));
    for name in ["t0", "t1", "t2", "t3", "t4"] {
        let height = driver.targets()[name].height();
        assert!(
            (height - rest).abs() < 0.5,
            "{name} keeps its size with zoom off: {height} against {rest}"
        );
    }
}

#[test]
fn the_dock_grows_itself_so_the_items_never_overlap() {
    let mut driver = driver_with(0.6, Theme::dark());
    let rest: Vec<_> = (0..5)
        .map(|index| driver.targets()[&format!("t{index}")])
        .collect();
    let centre = rest[2].center();
    driver.hover_at(centre.x, centre.y);
    assert!(settle(&mut driver, &mut Vec::new()));
    let grown: Vec<_> = (0..5)
        .map(|index| driver.targets()[&format!("t{index}")])
        .collect();
    for pair in grown.windows(2) {
        assert!(
            pair[0].x1 <= pair[1].x0 + 0.01,
            "items stand apart rather than over each other: {grown:?}"
        );
    }
    assert!(
        grown[4].x1 > rest[4].x1 + 1.0,
        "the dock itself is wider: {:?} against {:?}",
        grown[4],
        rest[4]
    );
}

/// A tile's props at `size`, as the dock builds its buttons.
fn tile_props(size: super::ControlSize) -> crate::widget::ButtonProps {
    crate::widget::ButtonProps {
        label: "Tile".to_owned(),
        icon: Some(tilo_ui_base::icons::Icon::X),
        icon_only: true,
        size,
        ..crate::widget::ButtonProps::default()
    }
}

#[test]
fn a_size_setting_change_reaches_its_new_height_through_the_frames_between() {
    let mut driver = Driver::for_root(
        crate::State::new(),
        <Button as Described>::from_props("tile", tile_props(super::ControlSize::Regular)),
        (400, 200),
        Theme::dark(),
    );
    driver.relayout();
    let regular = driver.targets()["tile"].height();
    let taken = driver
        .with_widget_mut(|button: &mut Button| {
            button.set_props(tile_props(super::ControlSize::Large))
        })
        .is_some_and(|taken| taken);
    assert!(taken, "the new size is taken in place, keeping the face");
    let mut along: Vec<f64> = Vec::new();
    driver.wait_until(Duration::from_secs(3), |driver| {
        if let Some(rect) = driver.targets().get("tile") {
            let height = rect.height();
            if along.last().is_none_or(|last| (last - height).abs() > 0.01) {
                along.push(height);
            }
        }
        driver.next_frame().is_none()
    });
    let large = driver.targets()["tile"].height();
    assert!(
        large > regular + 2.0,
        "the larger size is taken: {large} against {regular}"
    );
    assert!(
        along
            .iter()
            .any(|step| *step > regular + 0.5 && *step < large - 0.5),
        "the face passes through the sizes between rather than jumping: {along:?}"
    );
}

/// A dock body that keeps its band: a Chrome that reserves its rested height, holding one
/// magnifying item, keeps its own height and its foot edge while the item grows over the band.
#[test]
fn a_dock_body_keeps_its_band_while_an_item_grows_over_it() {
    use super::chrome_body::{ChromeBody, ChromeEdge, ChromeProps, ChromeShape};
    use tilo_ui_base::widget::Widget;
    let tile = Button::icon_only("tile", tilo_ui_base::icons::Icon::X, "Tile");
    let magnify = Magnify::from_props(
        "m",
        MagnifyProps {
            zoom: 0.6,
            body: Some(Box::new(tile)),
        },
    );
    let chrome = ChromeBody::from_props(
        "dock",
        ChromeProps {
            name: Some("Dock".to_owned()),
            shape: Some(ChromeShape::Rounded),
            edge: Some(ChromeEdge::Bottom),
            material: None,
            reserve: true,
            body: Some(Box::new(magnify)),
        },
    );
    let mut driver = Driver::new_with_theme(
        crate::State::new(),
        Box::new(chrome),
        (300, 200),
        Theme::dark(),
    );
    driver.relayout();
    let band = |driver: &mut Driver| {
        driver
            .with_widget_mut(|dock: &mut ChromeBody| dock.get_rect())
            .expect("the dock is the root")
    };
    let before = band(&mut driver);
    let tile_rest = driver.targets()["tile"];
    let centre = tile_rest.center();
    driver.hover_at(centre.x, centre.y);
    assert!(settle(&mut driver, &mut Vec::new()));
    let grown = driver.targets()["tile"];
    assert!(
        grown.height() > tile_rest.height() + 4.0,
        "the item grew: {grown:?} against {tile_rest:?}"
    );
    let after = band(&mut driver);
    assert!(
        (after.height() - before.height()).abs() < 0.5 && (after.y1 - before.y1).abs() < 0.5,
        "the dock keeps the band it rests in, foot edge and height: {after:?} against {before:?}"
    );
}

/// An icon button's hover plate is seen on the ground it stands on: the theme's plain role
/// clears the non-text contrast floor against a chrome body's ground in both palettes.
#[test]
fn a_plain_hover_plate_clears_the_non_text_floor_on_chrome() {
    use crate::widget::{ButtonRole, Interaction, face_colors};
    use tilo_ui_base::a11y::contrast_ratio;
    use tilo_ui_base::tokens::colors::NON_TEXT_MIN_CONTRAST;
    for theme in [Theme::dark(), Theme::light()] {
        let inside = theme.on_ground(tilo_ui_base::chrome::panel_ground(&theme));
        let ground = inside.tokens.colors.surfaces.content;
        let (fill, _, _) = face_colors(
            &inside,
            ButtonRole::Plain,
            Interaction {
                hovered: true,
                ..Interaction::default()
            },
        );
        let ratio = contrast_ratio(fill, ground);
        assert!(
            ratio >= NON_TEXT_MIN_CONTRAST - 0.05,
            "the hover plate is a {ratio:.2}:1 step on the chrome ground"
        );
    }
}
