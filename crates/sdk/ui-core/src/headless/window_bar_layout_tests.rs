//! The window bar at every width from a phone's to a wide display's: every
//! control has a slot of its own, and the location stays inside the room
//! the controls leave it.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::FrameHarness;
use super::tests::{HostedReset, NamedApp};
use crate::widget::{TabBar, Widget as _};
use crate::window_toolbar::{
    LocationSegment, ToolbarAction, ToolbarLocation, ToolbarNavigation, WindowToolbar,
};

/// From the narrowest window (`window_min_width`) to a wide display.
const WIDTHS: [f64; 12] = [
    320.0, 360.0, 390.0, 420.0, 480.0, 560.0, 640.0, 720.0, 840.0, 1024.0, 1280.0, 1440.0,
];

/// A folder deep in the home, as Files names it: its levels a breadcrumb,
/// its path editable, and Files' actions.
fn deep_folder() -> WindowToolbar {
    let levels = [
        "Home",
        "Documents",
        "Projects",
        "Tilo",
        "designs",
        "app-language",
    ];
    WindowToolbar {
        sidebar: Some(true),
        navigation: Some(ToolbarNavigation {
            back: true,
            forward: false,
        }),
        location: Some(ToolbarLocation {
            label: "app-language".into(),
            value: "~/Documents/Projects/Tilo/designs/app-language".into(),
            editable: true,
            segments: levels
                .iter()
                .map(|label| LocationSegment {
                    label: (*label).into(),
                    value: (*label).into(),
                })
                .collect(),
            ..ToolbarLocation::default()
        }),
        actions: vec![
            ToolbarAction::new("view", "layout-grid", "Show as grid"),
            ToolbarAction::new("new-folder", "folder-plus", "New folder"),
            ToolbarAction::new("up", "arrow-up", "Up").in_menu(),
            ToolbarAction::new("new-file", "file-plus", "New file").in_menu(),
            ToolbarAction::new("terminal", "square-terminal", "Open in Terminal").in_menu(),
            ToolbarAction::new("hidden", "eye", "Show hidden files").in_menu(),
        ],
        ..WindowToolbar::default()
    }
}

/// The bar of a window `width` wide showing `toolbar`, with the window's
/// History, laid out and painted twice, as a window's first frames
/// measure and then lay out its labels.
fn bar_at(harness: &mut FrameHarness, width: f64) -> TabBar {
    let mut bar = harness.window_bar(width);
    bar.set_history_control(super::TAB, Some(crate::widget::HistoryControl::of(false)));
    for _ in 0..2 {
        super::lay_out_bar(&mut bar, width, &harness.theme);
        bar.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut harness.text,
            &harness.theme,
            &harness.presence,
        );
    }
    bar
}

/// Every control the bar shows, by name: the app's icon, the app's slots,
/// the window's search and its own controls. The location is not one: it
/// takes what they leave.
fn controls(bar: &TabBar) -> Vec<(String, Rect)> {
    let mut out: Vec<(String, Rect)> = bar
        .test_rects()
        .into_iter()
        .filter(|(name, rect)| {
            rect.area() > 0.0
                && name != "window_location"
                && !name.starts_with("tab:")
                && name != "tabbar_new_tab"
        })
        .collect();
    if let Some((row, _)) = bar.toolbar_search_slot() {
        let px = crate::widget::window_controls::CONTROL_PX;
        let x0 = row.x1 + crate::widget::window_controls::CONTROL_GAP;
        let y0 = row.center().y - px / 2.0;
        out.push(("window_search".into(), Rect::new(x0, y0, x0 + px, y0 + px)));
    }
    out
}

fn overlap(a: Rect, b: Rect) -> bool {
    let i = a.intersect(b);
    i.width() > 0.01 && i.height() > 0.01
}

/// At every width, no two of the bar's controls overlap, every one stands
/// inside the window, the location stands clear of all of them, and its
/// breadcrumb paints inside it: its middle levels fold into the `…`, then
/// the folder's own name is cut short, and nothing runs past its end.
#[test]
fn a_deep_folders_bar_gives_every_control_its_slot_at_every_width() {
    let _hosted = HostedReset;
    let mut harness = FrameHarness::for_app(
        crate::State::new(),
        NamedApp,
        (1440, 800),
        crate::theme::Theme::dark(),
    );
    harness.present_in_window_bar();
    crate::window_toolbar::declare(Some(deep_folder()));
    let style = harness.theme.text(crate::tokens::TextRole::Body);
    for width in WIDTHS {
        let bar = bar_at(&mut harness, width);
        let shown = controls(&bar);
        for (i, (name, rect)) in shown.iter().enumerate() {
            assert!(
                rect.x0 >= -0.01 && rect.x1 <= width + 0.01,
                "{width} px: {name} stands inside the window: {rect:?}"
            );
            for (other, other_rect) in shown.iter().skip(i + 1) {
                assert!(
                    !overlap(*rect, *other_rect),
                    "{width} px: {name} {rect:?} overlaps {other} {other_rect:?}: {shown:?}"
                );
            }
        }
        let location = bar.location_rect();
        assert!(
            location.area() > 0.0,
            "{width} px: the folder is named in the bar: {shown:?}"
        );
        for (name, rect) in &shown {
            assert!(
                !overlap(location, *rect),
                "{width} px: the location {location:?} runs under {name} {rect:?}"
            );
        }
        let (painted, trail) = bar
            .toolbar_row()
            .crumbs_painted()
            .expect("the folder's levels are a breadcrumb");
        assert!(
            trail.x0 >= location.x0 - 0.01 && trail.x1 <= location.x1 + 0.01,
            "{width} px: the breadcrumb {trail:?} stands in the location {location:?}"
        );
        let last = painted
            .last()
            .and_then(|(label, _)| label.as_deref())
            .map(|label| {
                label
                    .trim_end_matches(crate::text_render::ELLIPSIS)
                    .to_owned()
            });
        assert!(
            last.is_some_and(|label| !label.is_empty() && "app-language".starts_with(&label)),
            "{width} px: the folder's own name always shows, cut short at the narrowest: {painted:?}"
        );
        for (label, rect) in &painted {
            let ink = label.as_deref().map_or(rect.width(), |label| {
                f64::from(harness.text.measure_style(label, None, &style).0)
            });
            assert!(
                rect.x0 >= trail.x0 - 0.01 && rect.x0 + ink <= trail.x1 + 0.5,
                "{width} px: {label:?} ({ink} px at {rect:?}) paints inside the breadcrumb \
                 {trail:?}: {painted:?}"
            );
        }
        let whole = painted.iter().all(|(label, _)| label.is_some()) && painted.len() == 6;
        if width >= 1024.0 {
            assert!(
                whole,
                "{width} px: a wide window shows every level: {painted:?}"
            );
        }
    }
    crate::window_toolbar::declare(None);
}

/// The window's own controls are a group of their own at the trailing
/// edge, a group's gap from the app's controls before them, and they stand
/// in the same place however many controls the app has.
#[test]
fn the_windows_controls_stand_apart_at_a_fixed_trailing_edge() {
    let _hosted = HostedReset;
    let mut harness = FrameHarness::for_app(
        crate::State::new(),
        NamedApp,
        (1440, 800),
        crate::theme::Theme::dark(),
    );
    harness.present_in_window_bar();
    let group = f64::from(crate::tokens::Spacing::BASE.rhythm.group);
    for width in [720.0, 1024.0, 1440.0] {
        let mut placed = Vec::new();
        for toolbar in [
            deep_folder(),
            WindowToolbar {
                actions: Vec::new(),
                ..deep_folder()
            },
        ] {
            crate::window_toolbar::declare(Some(toolbar));
            let bar = bar_at(&mut harness, width);
            let controls = *bar.window_controls();
            let shown = self::controls(&bar);
            let history = controls.history;
            assert!(
                history.area() > 0.0,
                "{width} px: History stands: {shown:?}"
            );
            let app_end = shown
                .iter()
                .filter(|(_, rect)| rect.x1 <= history.x0)
                .map(|(_, rect)| rect.x1)
                .fold(f64::MIN, f64::max);
            assert!(
                (history.x0 - app_end - group).abs() < 0.5,
                "{width} px: a group's gap ({group} px) parts the app's controls from the \
                 window's: {app_end} to {history:?}"
            );
            placed.push(controls);
        }
        assert_eq!(
            placed[0], placed[1],
            "{width} px: the window's controls never move with the app's"
        );
    }
    crate::window_toolbar::declare(None);
}
