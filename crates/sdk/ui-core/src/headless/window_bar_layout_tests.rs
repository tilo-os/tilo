//! The window bar at every width from a phone's to a wide display's: every control has a part of its
//! own, and the location stays inside the width the controls leave it.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::FrameHarness;
use super::tests::{HostedReset, NamedApp};
use crate::widget::{TabBar, Widget as _};
use crate::window_toolbar::{
    LocationSegment, ToolbarAction, ToolbarLocation, ToolbarNavigation, WindowToolbar,
};

const WIDTHS: [f64; 12] = [
    320.0, 360.0, 390.0, 420.0, 480.0, 560.0, 640.0, 720.0, 840.0, 1024.0, 1280.0, 1440.0,
];

/// A folder deep in the home, as Files names it: its levels a breadcrumb, its path editable, and Files'
/// actions.
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

/// The bar of a window `width` wide showing the declared toolbar, laid out and painted twice.
fn bar_at(harness: &mut FrameHarness, width: f64, history: bool) -> TabBar {
    let mut bar = harness.window_bar(width);
    if history {
        bar.set_history_control(super::TAB, Some(crate::widget::HistoryControl::of(false)));
    }
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

/// The bar's controls from the layout the shell draws: its editable location, and the rest.
fn controls(layout: &crate::window_toolbar::BarLayout) -> (Option<Rect>, Vec<(String, Rect)>) {
    let mut location = None;
    let mut out = Vec::new();
    for control in &layout.controls {
        let [x, y, width, height] = control.rect;
        if width <= 0.0 || height <= 0.0 {
            continue;
        }
        let rect = Rect::new(x, y, x + width, y + height);
        if control.part == "location" {
            location = Some(rect);
        } else {
            out.push((control.name.clone(), rect));
        }
    }
    (location, out)
}

fn overlap(a: Rect, b: Rect) -> bool {
    let i = a.intersect(b);
    i.width() > 0.01 && i.height() > 0.01
}

/// At every width no two controls overlap, the location stands clear, and its words paint inside
/// it: cut short, never under a button.
#[test]
fn every_bar_gives_every_part_its_own_place_at_every_width() {
    let _hosted = HostedReset;
    let mut harness = FrameHarness::for_app(
        crate::State::new(),
        NamedApp,
        (1440, 800),
        crate::theme::Theme::dark(),
    );
    harness.present_in_window_bar();
    let mut files = deep_folder();
    files.location = Some(ToolbarLocation {
        label: "Workspace".into(),
        value: "Workspace".into(),
        editable: true,
        ..ToolbarLocation::default()
    });
    files.actions[0].primary = true;
    for toolbar in [deep_folder(), files] {
        let crumbs = toolbar
            .location
            .as_ref()
            .is_some_and(|location| !location.segments.is_empty());
        crate::window_toolbar::declare(Some(toolbar));
        for width in WIDTHS {
            let bar = bar_at(&mut harness, width, false);
            let layout = harness.window_bar_layout(width);
            let (place, shown) = controls(&layout);
            let location = place.unwrap_or_else(|| bar.location_rect());
            assert!(
                location.area() > 0.0,
                "{width} px: the location stands: {shown:?}"
            );
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
                assert!(
                    !overlap(location, *rect),
                    "{width} px: the location {location:?} runs under {name} {rect:?}: {shown:?}"
                );
            }
            if crumbs {
                let (painted, trail) = bar
                    .toolbar_row()
                    .crumbs_painted()
                    .expect("the folder's levels are a breadcrumb");
                assert!(
                    trail.x0 >= location.x0 - 0.01 && trail.x1 <= location.x1 + 0.01,
                    "{width} px: the breadcrumb {trail:?} stands in the location {location:?}"
                );
                let name = painted.last().and_then(|(label, _)| label.as_deref());
                let cut = name.map_or("", |name| {
                    name.trim_end_matches(crate::text_render::ELLIPSIS)
                });
                assert!(
                    !cut.is_empty() && "app-language".starts_with(cut),
                    "{width} px: the folder's own name shows, cut short at the narrowest: {painted:?}"
                );
            } else {
                for label in layout.labels.iter().filter(|l| l.widget == "BarLocation") {
                    assert!(
                        label.rect[0] >= location.x0 - 0.01
                            && label.rect[0] + label.rect[2] <= location.x1 + 0.5,
                        "{width} px: {:?} paints outside {location:?}",
                        label.text
                    );
                }
            }
        }
    }
    crate::window_toolbar::declare(None);
}

/// The window's own controls are a group of their own at the trailing edge, a group's gap from the
/// app's controls before them, and they stand in the same place however many controls the app has.
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
    let apart = crate::widget::window_controls::LEAD_GAP;
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
            let bar = bar_at(&mut harness, width, true);
            let controls = *bar.window_controls();
            let shown = self::controls(&harness.window_bar_layout(width)).1;
            let history = bar.history_action_rect();
            assert!(
                history.area() > 0.0,
                "{width} px: History stands: {shown:?}"
            );
            assert!(
                history.x1 <= controls.pill.x0,
                "{width} px: the app's History stands before the window's pill: {history:?} \
                 and {:?}",
                controls.pill
            );
            assert!(
                (controls.pill.x0 - history.x1 - apart).abs() < 0.5,
                "{width} px: {apart} px part the app's action from the window's controls, \
                 {gap} px from one app control to the next: {history:?} to {:?}",
                controls.pill,
                gap = crate::widget::window_controls::HISTORY_GAP
            );
            let app_end = shown
                .iter()
                .filter(|(_, rect)| rect.x1 <= history.x0)
                .map(|(_, rect)| rect.x1)
                .fold(f64::MIN, f64::max);
            assert!(
                app_end <= history.x0,
                "{width} px: the app's parts stop before its own History action: {app_end} \
                 to {history:?}"
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
