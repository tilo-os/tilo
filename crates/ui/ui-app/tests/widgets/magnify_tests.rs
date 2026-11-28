//! A dock item that magnifies under the pointer: the item under it is larger than its
//! neighbours, those than the items further off, and leaving settles every one back.

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
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;
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
        tilo_ui_app::State::new(),
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
        let height = |name: &str| {
            grown
                .get(name)
                .map_or(0.0, tilo_ui_app::kurbo::Rect::height)
        };
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
    let centre = rest[3].center();
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

/// Tiles the shell draws straight in a row — each a `Magnify` of its button, no wrapper between —
/// under a finger: the row places each face by the target it stands at, so no face is drawn over
/// its neighbour, and a grown tile pushes the row wider rather than over it.
#[test]
fn a_row_of_bare_tiles_at_touch_keeps_them_apart() {
    // The dock's running tiles: a row on the edge's floor, its faces hugging at the sibling gap.
    // The row a layout makes, not the legacy row widget the older tests here use.
    let mut row = tilo_ui_app::widget::arrange::Row::row()
        .fill_width()
        .gap(Rhythm::Sibling)
        .align(tilo_ui_app::widget::arrange::Align::End)
        .with_id("dock");
    for index in 0..6 {
        row = row.push(Box::new(Magnify::from_props(
            &format!("m{index}"),
            MagnifyProps {
                zoom: 0.6,
                // Each tile as the shell's view builds it: a component's props, an icon-only
                // plain button at the dock's size, with its badge.
                body: Some(Box::new(<Button as Described>::from_props(
                    &format!("t{index}"),
                    tilo_ui_app::widget::ButtonProps {
                        label: format!("Tile {index}"),
                        icon: Some(tilo_ui_base::icons::Icon::X),
                        icon_only: true,
                        role: Some(tilo_ui_app::widget::ButtonRole::Plain),
                        size: tilo_ui_app::widget::ControlSize::Regular,
                        count: Some(0),
                        ..Default::default()
                    },
                ))),
            },
        )));
    }
    // The shell's dock scrolls along the edge: the row stands in a horizontal scroll's content,
    // as the dock's does.
    let mut driver = Driver::new_with_theme(
        tilo_ui_app::State::new(),
        Box::new(Scrollable::new_horizontal().with_child(Box::new(row))),
        (320, 80),
        Theme::dark(),
    );
    // The window is laid once for a mouse, then reached by a finger, as a window that is told its
    // input after its first frame is.
    driver.relayout();
    driver.set_interaction_environment(InteractionEnvironment::TOUCH);
    driver.relayout();
    let tiles = |driver: &Driver| -> Vec<Rect> {
        let snapshot = driver.inspection();
        snapshot
            .root
            .nodes()
            .into_iter()
            .filter(|node| node.widget == "Button")
            .map(|node| {
                let [x, y, w, h] = node.rect;
                Rect::new(x, y, x + w, y + h)
            })
            .collect()
    };
    let rest = tiles(&driver);
    for pair in rest.windows(2) {
        assert!(
            pair[0].x1 <= pair[1].x0 + 0.01,
            "tiles stand apart at rest: {rest:?}"
        );
    }
    let centre = rest[2].center();
    driver.hover_at(centre.x, centre.y);
    assert!(settle(&mut driver, &mut Vec::new()));
    let grown = tiles(&driver);
    for group in [&rest, &grown] {
        for pair in group.windows(2) {
            assert!(
                pair[0].x1 <= pair[1].x0 + 0.01,
                "no face is drawn over its neighbour: {grown:?} against {rest:?}"
            );
        }
    }
    for face in &rest {
        assert!(
            face.width() >= 47.5,
            "a face under a finger is a target: {face:?}"
        );
    }
    // The same row under a pointer: the item under it grows and the row makes room, so the
    // neighbour it pushes stands further along rather than under it.
    driver.set_interaction_environment(InteractionEnvironment::POINTER);
    driver.hover_at(560.0, 8.0);
    assert!(settle(&mut driver, &mut Vec::new()));
    let pointer_rest = tiles(&driver);
    let centre = pointer_rest[3].center();
    driver.hover_at(centre.x, centre.y);
    assert!(settle(&mut driver, &mut Vec::new()));
    let magnified = tiles(&driver);
    for pair in magnified.windows(2) {
        assert!(
            pair[0].x1 <= pair[1].x0 + 0.01,
            "a grown face pushes its neighbour apart rather than over it: {magnified:?}"
        );
    }
    assert!(
        magnified[3].width() > pointer_rest[3].width() + 1.0,
        "the face under the pointer grows: {magnified:?} against {pointer_rest:?}"
    );
    assert!(
        magnified[4].x0 > pointer_rest[4].x0 + 1.0,
        "the row makes room for it: {magnified:?} against {pointer_rest:?}"
    );
}

/// A tile's props at `size`, as the dock builds its buttons.
fn tile_props(size: tilo_ui_app::widget::ControlSize) -> tilo_ui_app::widget::ButtonProps {
    tilo_ui_app::widget::ButtonProps {
        label: "Tile".to_owned(),
        icon: Some(tilo_ui_base::icons::Icon::X),
        icon_only: true,
        size,
        ..tilo_ui_app::widget::ButtonProps::default()
    }
}

#[test]
fn a_size_setting_change_reaches_its_new_height_through_the_frames_between() {
    let mut driver = Driver::for_root(
        tilo_ui_app::State::new(),
        <Button as Described>::from_props(
            "tile",
            tile_props(tilo_ui_app::widget::ControlSize::Regular),
        ),
        (400, 200),
        Theme::dark(),
    );
    driver.relayout();
    let regular = driver.targets()["tile"].height();
    let taken = driver
        .with_widget_mut(|button: &mut Button| {
            button.set_props(tile_props(tilo_ui_app::widget::ControlSize::Large))
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
    use tilo_ui_app::widget::chrome_body::{ChromeBody, ChromeEdge, ChromeProps, ChromeShape};
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
        tilo_ui_app::State::new(),
        Box::new(chrome),
        (300, 200),
        Theme::dark(),
    );
    driver.relayout();
    let band = |driver: &mut Driver| {
        driver
            .with_widget_mut(|dock: &mut ChromeBody| tilo_ui_base::widget::Widget::get_rect(dock))
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
    use tilo_ui_app::widget::{ButtonRole, Interaction, face_colors};
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

/// A row of a layout places each part after the one before it: an icon and the words beside it
/// stand a gap apart, never over each other.
#[test]
fn a_rows_parts_follow_each_others_widths() {
    let check = |width: u32| {
        let row = tilo_ui_app::widget::arrange::Row::row()
            .fill_width()
            .gap(Rhythm::Related)
            .push(Box::new(tilo_ui_app::widget::IconWidget::new(
                tilo_ui_base::icons::Icon::Bot,
            )))
            .push(Box::new(tilo_ui_app::widget::Text::new("Name")));
        let mut driver = Driver::new_with_theme(
            tilo_ui_app::State::new(),
            Box::new(row),
            (width, 60),
            Theme::dark(),
        );
        driver.relayout();
        let snapshot = driver.inspection();
        let rects: Vec<(&str, Rect)> = snapshot
            .root
            .nodes()
            .into_iter()
            .filter(|node| node.widget == "IconWidget" || node.widget == "Text")
            .map(|node| {
                let [x, y, w, h] = node.rect;
                (node.widget.as_str(), Rect::new(x, y, x + w, y + h))
            })
            .collect();
        let icon = rects
            .iter()
            .find(|(widget, _)| *widget == "IconWidget")
            .map(|(_, rect)| *rect)
            .expect("the icon is drawn");
        let text = rects
            .iter()
            .find(|(widget, _)| *widget == "Text")
            .map(|(_, rect)| *rect)
            .expect("the words are drawn");
        assert!(
            text.x0 >= icon.x1 + 8.0 - 0.5,
            "at {width} px the words stand a related gap after the icon, not over it: \
             {text:?} against {icon:?}"
        );
    };
    // Room to spare, and a row far too narrow for both parts: the parts never stand over each
    // other, the row goes on past the room it was given instead.
    check(320);
    check(60);
}
