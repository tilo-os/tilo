//! Settings laid out at the sizes people keep windows in: every pane fits its window, the
//! sidebar folds into a drawer at a phone's width and opens a place from it, a long name wraps
//! rather than pushing content past the edge, a tall pane scrolls, and a larger text scale keeps
//! it all inside.
//!
//! What the deleted Rust app's layout tests checked, read off the production window after a real
//! layout pass: no rasterising, no GPU.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

mod support;

use support::{accessible, open, open_pane, said, set_reading, settle, settle_motion, shows};
use tilolang_window::Window;
use tilo_ui_base::widget::Widget;

/// Every pane the sidebar opens, by the app's own name for it.
const PANES: &[&str] = &[
    "appearance",
    "themes",
    "shell",
    "accessibility",
    "notifications",
    "shortcuts",
    "region",
    "spelling",
    "default_apps",
    "time",
    "network",
    "bluetooth",
    "usage",
    "displays",
    "sound",
    "speakers",
    "power",
    "printers",
    "drives",
    "server",
    "input",
    "content",
    "accounts",
    "billing",
    "permissions",
    "agent_models",
    "devices",
    "continuity",
    "firmware",
    "compatibility",
    "disk_unlock",
    "storage",
    "updates",
    "users",
    "about",
];

/// The windows people use, and the text scale they read at.
const WINDOWS: [((u32, u32), f32); 5] = [
    ((320, 568), 1.0),
    ((390, 844), 1.0),
    ((768, 1024), 1.0),
    ((1280, 800), 1.0),
    ((1280, 800), 1.5),
];

/// A rectangle as `[x, y, width, height]`.
fn rect(node: &ui::headless::InspectionNode) -> [f64; 4] {
    node.rect
}

fn right(node: &ui::headless::InspectionNode) -> f64 {
    rect(node)[0] + rect(node)[2]
}

fn bottom(node: &ui::headless::InspectionNode) -> f64 {
    rect(node)[1] + rect(node)[3]
}

/// Everything a pane draws past its window's right edge: a leaf whose visible part stands beyond
/// it, named for the person reading the failure.
fn past_the_edge(window: &Window, (width, height): (u32, u32)) -> Vec<String> {
    let snapshot = window.harness.inspection();
    let mut problems = Vec::new();
    let mut pending: Vec<&ui::headless::InspectionNode> = vec![&snapshot.root];
    while let Some(node) = pending.pop() {
        let r = rect(node);
        let drawn = r[2] > 0.5 && r[3] > 0.5;
        // A node stands where it is clipped only by its ancestors: one that is clipped out of
        // sight entirely (a drawer behind what the window shows) is not what a person reads.
        if drawn
            && right(node) > f64::from(width) + 1.0
            && r[1] + r[3] > 0.0
            && r[1] < f64::from(height)
        {
            problems.push(format!(
                "{} `{}` reaches {} past the window's {width}: {:?}",
                node.widget,
                node.semantics
                    .as_ref()
                    .and_then(|access| access.name.clone())
                    .unwrap_or_default(),
                right(node).round(),
                r
            ));
        }
        if bottom(node) < 0.0 {
            continue;
        }
        pending.extend(node.children.iter());
    }
    problems
}

/// Every pane fits every window: none of what it draws reaches past the window's right edge, and
/// the pane in front is the one chosen, drawn with words of its own.
#[test]
fn every_pane_fits_the_window_it_is_shown_in() {
    for (size, text_scale) in WINDOWS {
        let mut window = open(size, tilo_ui_base::Theme::light().with_text_scale(text_scale));
        for pane in PANES {
            open_pane(&mut window, pane);
            assert!(
                !said(&window).is_empty(),
                "{pane} at {size:?} text {text_scale}: the window shows nothing"
            );
            let problems = past_the_edge(&window, size);
            assert!(
                problems.is_empty(),
                "{pane} at {size:?} text {text_scale} draws past its window: {problems:#?}"
            );
        }
    }
}

/// A phone's window folds the sidebar into a drawer and opens on the list of places: choosing one
/// shows it, and the window's way back to the list brings the list in front again.
#[test]
fn a_phone_folds_the_sidebar_into_a_drawer() {
    const PHONE: (u32, u32) = (390, 844);
    let mut window = open(PHONE, tilo_ui_base::Theme::light());

    assert!(
        !places_in_front(&window, PHONE).is_empty(),
        "a phone's window opens on the list of places: {:?}",
        said(&window)
    );
    assert!(
        !shows(&window, "Mode"),
        "and not on a pane: {:?}",
        said(&window)
    );

    press_place(&mut window, "Sound");
    settle_motion(&mut window, ui::time::Duration::from_secs(1));
    assert!(
        shows(&window, "Output"),
        "choosing Sound shows the Sound pane: {:?}",
        said(&window)
    );
    assert!(
        places_in_front(&window, PHONE).is_empty(),
        "the list goes behind what it opened: {:?}",
        said(&window)
    );

    press_window_control(&mut window, ui::widget::WINDOW_BACK);
    settle_motion(&mut window, ui::time::Duration::from_secs(1));
    assert!(
        !places_in_front(&window, PHONE).is_empty(),
        "the window's way back brings the list in front again: {:?}",
        said(&window)
    );
}

/// The sidebar's places the window shows now, inside its frame: a phone's window keeps the ones it
/// is not showing behind what it shows.
fn places_in_front(window: &Window, (width, height): (u32, u32)) -> Vec<String> {
    accessible(window)
        .into_iter()
        .filter(|node| node.role == tilo_ui_base::a11y::Role::TreeItem)
        .filter(|node| {
            node.bounds.is_some_and(|b| {
                b.x0 >= -0.5
                    && b.x1 <= f64::from(width) + 0.5
                    && b.y0 >= -0.5
                    && b.y1 <= f64::from(height) + 0.5
                    && b.x1 - b.x0 > 1.0
                    && b.y1 - b.y0 > 1.0
            })
        })
        .filter_map(|node| node.name)
        .collect()
}

/// Press the window's own control `id` in its bar: what a phone's window offers to go back to the
/// list of places it opened a pane from.
fn press_window_control(window: &mut Window, id: &str) {
    let node = window
        .harness
        .inspection()
        .root
        .nodes()
        .into_iter()
        .find(|node| node.test_id.as_deref() == Some(id))
        .map_or_else(
            || {
                let ids: Vec<String> = window
                    .harness
                    .inspection()
                    .root
                    .nodes()
                    .into_iter()
                    .filter_map(|node| node.test_id.clone())
                    .collect();
                panic!("the window's bar offers {id}: {ids:?}")
            },
            |node| (node.rect[0], node.rect[1], node.rect[2], node.rect[3]),
        );
    window
        .harness
        .click_at(node.0 + node.2 / 2.0, node.1 + node.3 / 2.0);
    settle(window);
}

/// Press the sidebar's row `label`, where it stands.
fn press_place(window: &mut Window, label: &str) {
    let bounds = accessible(window)
        .into_iter()
        .find(|node| {
            node.role == tilo_ui_base::a11y::Role::TreeItem
                && node.name.as_deref() == Some(label)
                && node.bounds.is_some()
        })
        .and_then(|node| node.bounds)
        .unwrap_or_else(|| panic!("the list offers {label}"));
    window.harness.click_at(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    );
    settle(window);
}

/// A pane holding a long name wraps its words instead of pushing them past the window's edge.
#[test]
fn a_long_name_stays_inside_a_narrow_window() {
    const PHONE: (u32, u32) = (320, 568);
    let mut window = open(PHONE, tilo_ui_base::Theme::light());
    let long = "Wilhelmina Alexandrina Featherstonehaugh-McAllister the Third of \
                the Northern Quarter's Household";
    let people = format!(
        "{{\"serving\":true,\"accounts\":[{{\"username\":\"wilmadefeatherstonehaugh\",\
          \"full_name\":\"{long}\",\"uid\":1000,\"admin\":true,\"current\":true,\
          \"automatic_login\":false,\"automatic_login_error\":null}}],\
          \"read_error\":null,\"pending\":[],\"problem\":null}}"
    );
    set_reading(&window, "users", "so.tilo.device#Users", &people);
    open_pane(&mut window, "users");
    assert!(
        shows(&window, "Featherstonehaugh"),
        "the pane names the person: {:?}",
        said(&window)
    );
    let problems = past_the_edge(&window, PHONE);
    assert!(
        problems.is_empty(),
        "a long name stays inside the window: {problems:#?}"
    );
}

/// A pane taller than a short window scrolls: a wheel over it moves what it shows, and the pane
/// does not scroll sideways.
#[test]
fn a_tall_pane_scrolls_in_a_short_window() {
    const SHORT: (u32, u32) = (1100, 360);
    let mut window = open(SHORT, tilo_ui_base::Theme::light());
    open_pane(&mut window, "power");

    let before = content_scroll(&window);
    assert!(
        before.2 > before.1,
        "Power is taller than a short window: content {} tall in {}",
        before.2,
        before.1
    );
    // The pointer rests over the pane: a wheel is the pointer's, sent where it stands.
    window.harness.dispatch_to_root_with(
        &tilo_ui_base::synthetic_input::cursor_moved(f64::from(SHORT.0) / 2.0, f64::from(SHORT.1) / 2.0),
        &ui::winit::event::Modifiers::default(),
    );
    window.harness.relayout();
    for _ in 0..3 {
        window.harness.dispatch_to_root_with(
            &tilo_ui_base::synthetic_input::mouse_wheel(0.0, -3.0),
            &ui::winit::event::Modifiers::default(),
        );
        window.harness.relayout();
    }
    settle(&mut window);
    let after = content_scroll(&window);
    assert!(
        after.0 > before.0,
        "a wheel moves the pane: {} then {}",
        before.0,
        after.0
    );
    let problems = past_the_edge(&window, SHORT);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// The content pane's scroll: how far it stands scrolled, how tall its viewport is, and how tall
/// what it holds is.
fn content_scroll(window: &Window) -> (f64, f64, f64) {
    fn collect<'a>(widget: &'a dyn Widget, out: &mut Vec<&'a dyn Widget>) {
        if widget.name() == "Scrollable" {
            out.push(widget);
        }
        for child in widget.children() {
            collect(child, out);
        }
    }
    let mut scrolls = Vec::new();
    collect(window.harness.root_widget(), &mut scrolls);
    let content = scrolls
        .into_iter()
        .max_by(|a, b| {
            a.get_rect()
                .x0
                .partial_cmp(&b.get_rect().x0)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("the window has a content scroll");
    let child = *content
        .children()
        .first()
        .expect("the scroll wraps the content it holds");
    let viewport = content.get_rect();
    (
        viewport.y0 - child.get_rect().y0,
        viewport.height(),
        child.get_rect().height(),
    )
}
