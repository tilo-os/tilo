//! The window bar at every width from a phone's to a wide display's: every control has a part of its
//! own, and the location stays inside the width the controls leave it.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use super::FrameHarness;
use super::tests::{HostedReset, NamedApp};
use crate::a11y::{AccessNode, Role};
use crate::widget::{TabBar, Widget as _};
use crate::window_toolbar::{
    LocationSegment, ToolbarAction, ToolbarLocation, ToolbarNavigation, ToolbarView, ToolbarViews,
    WindowToolbar,
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
fn controls(
    layout: &crate::window_toolbar::BarLayout,
) -> (Option<Rect>, Vec<(&'static str, String, Rect)>) {
    let mut location = None;
    let mut out = Vec::new();
    for control in &layout.controls {
        let [x, y, width, height] = control.rect;
        let rect = Rect::new(x, y, x + width, y + height);
        if control.part == "location" {
            location = Some(rect);
        } else {
            out.push((control.part, control.name.clone(), rect));
        }
    }
    (location, out)
}

/// The label the bar paints the location's words with: the views picker's where the views stand
/// in the location, the location's own otherwise.
fn location_label(
    layout: &crate::window_toolbar::BarLayout,
    picks: bool,
) -> Option<&crate::window_toolbar::BarLabel> {
    let widget = if picks { "BarPicker" } else { "BarLocation" };
    layout.labels.iter().find(|label| label.widget == widget)
}

fn overlap(a: Rect, b: Rect) -> bool {
    let i = a.intersect(b);
    i.width() > 0.01 && i.height() > 0.01
}

/// A week across a month, as Calendar names it: Monday, June 29 to Sunday, July 5, 2026.
fn week_across_a_month() -> crate::format::Period {
    let day = |m, d| jiff::civil::Date::new(2026, m, d).ok().unwrap_or_default();
    crate::format::Period::Days(day(6, 29), day(7, 5))
}

/// Calendar's bar: previous and next as a pair, Today, the week as its period, and New event.
fn calendar() -> WindowToolbar {
    WindowToolbar {
        sidebar: Some(true),
        leading: vec![
            ToolbarAction::new("previous", "chevron-left", "Previous week").navigating(),
            ToolbarAction::new("next", "chevron-right", "Next week").navigating(),
            ToolbarAction::new("today", "calendar", "Today"),
        ],
        location: Some(ToolbarLocation::period(week_across_a_month())),
        views: Some(ToolbarViews {
            name: "View".into(),
            choices: ["Month", "Week", "Day", "Agenda", "Tasks"]
                .into_iter()
                .map(|label| ToolbarView {
                    id: label.to_lowercase(),
                    label: label.into(),
                })
                .collect(),
            chosen: "week".into(),
        }),
        actions: vec![
            ToolbarAction::new("new-event", "plus", "New event").primary(),
            ToolbarAction::new("export", "upload", "Export calendar").in_menu(),
        ],
        ..WindowToolbar::default()
    }
}

/// The menu item `name` in the bar's accessibility tree.
fn find<'a>(node: &'a AccessNode, role: Role, name: &str) -> Option<&'a AccessNode> {
    if node.role == role && node.name.as_deref() == Some(name) {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| find(child, role, name))
}

/// Files' bar: the workspace as its location, Upload as its main action, New folder after it.
fn files() -> WindowToolbar {
    let mut files = deep_folder();
    files.location = Some(ToolbarLocation {
        label: "Workspace".into(),
        value: "Workspace".into(),
        editable: true,
        ..ToolbarLocation::default()
    });
    files.actions.insert(
        0,
        ToolbarAction::new("upload", "upload", "Upload files").primary(),
    );
    files
}

/// At every width no two controls overlap, the location stands clear, and its words paint
/// inside it: whole in its narrowest form while anything that yields to it still stands. The
/// leading actions that move through the content stand together or fold together.
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
    let bars: [(&str, fn() -> WindowToolbar); 4] = [
        ("en-US", deep_folder),
        ("en-US", files),
        ("en-US", calendar),
        ("de-DE", calendar),
    ];
    for (locale, make) in bars {
        crate::format::speak_locale(locale);
        let toolbar = make();
        {
            let place = toolbar.location.as_ref().expect("a bar names a place");
            let crumbs = !place.segments.is_empty();
            let current = place
                .segments
                .last()
                .map_or(&place.label, |place| &place.label);
            let declared: Vec<&str> = std::iter::once(place.label.as_str())
                .chain(place.shorter.iter().map(String::as_str))
                .collect();
            let pair: Vec<&str> = toolbar
                .leading
                .iter()
                .filter(|action| action.navigates)
                .map(|action| action.name.as_str())
                .collect();
            crate::window_toolbar::declare(Some(toolbar.clone()));
            for width in WIDTHS {
                let bar = bar_at(&mut harness, width, false);
                let layout = harness.window_bar_layout(width);
                let (place, shown) = controls(&layout);
                // Anything that yields to the location: the strip's own "+", the views, the
                // navigation's buttons and every action beside the window's main one.
                let yields = layout.controls.iter().any(|control| {
                    control.part == "new-tab"
                        || matches!(
                            control.part,
                            "leading" | "views" | "views-menu" | "sidebar" | "forward"
                        )
                        || (control.part == "action" && !control.primary)
                });
                let location = place.unwrap_or_else(|| bar.location_rect());
                assert!(location.area() > 0.0, "{locale} {width} px: {shown:?}");
                // The views picker is the location where the bar shows the view's name there:
                // that control and no other shares the location's place.
                let picks = bar.toolbar_row().parts().views_in_location();
                for (i, (part, name, rect)) in shown.iter().enumerate() {
                    assert!(
                        rect.x0 >= -0.01 && rect.x1 <= width + 0.01,
                        "{locale} {width} px: {name} stands inside the window: {rect:?}"
                    );
                    for (_, other, other_rect) in shown.iter().skip(i + 1) {
                        assert!(
                            !overlap(*rect, *other_rect),
                            "{locale} {width} px: {name} {rect:?} over {other} {other_rect:?}"
                        );
                    }
                    let shares = picks && *part == "views-menu";
                    assert!(
                        shares || !overlap(location, *rect),
                        "{locale} {width} px: {location:?} runs under {name} {rect:?}"
                    );
                }
                if crumbs {
                    let (painted, trail) = bar
                        .toolbar_row()
                        .crumbs_painted()
                        .expect("the folder's levels are a breadcrumb");
                    assert!(
                        trail.x0 >= location.x0 - 0.01 && trail.x1 <= location.x1 + 0.01,
                        "{locale} {width} px: the breadcrumb {trail:?} in {location:?}"
                    );
                    let name = painted.last().and_then(|(label, _)| label.as_deref());
                    assert!(
                        !yields || name == Some(current.as_str()),
                        "{locale} {width} px: the folder names itself in {location:?}: {painted:?} beside {shown:?}"
                    );
                } else {
                    let words = location_label(&layout, picks);
                    assert!(
                        !yields
                            || words.is_some_and(|label| declared.contains(&label.text.as_str())),
                        "{locale} {width} px: the location names itself whole while a part \
                         yields to it: {layout:?}"
                    );
                    if let Some(label) = words {
                        assert!(
                            label.rect[0] >= location.x0 - 0.01
                                && label.rect[0] + label.rect[2] <= location.x1 + 1.0,
                            "{locale} {width} px: {:?} paints outside {location:?}",
                            label.text
                        );
                    }
                }
                let standing = layout
                    .controls
                    .iter()
                    .filter(|control| {
                        control.part == "leading" && pair.contains(&control.name.as_str())
                    })
                    .count();
                assert!(
                    standing == 0 || standing == pair.len(),
                    "{locale} {width} px: {standing} of {pair:?} stand"
                );
            }
            crate::window_toolbar::declare(None);
        }
    }
    // Files keeps its actions in the row at a wide width: only the menu's own are behind it.
    crate::window_toolbar::declare(Some(files()));
    let shown = controls(&harness.window_bar_layout(1280.0)).1;
    for name in ["Show as grid", "New folder"] {
        assert!(
            shown
                .iter()
                .any(|(part, shown, _)| *part == "action" && shown == name),
            "1280 px: {name} in {shown:?}"
        );
    }
    // A compact calendar window that lists History: its navigation and name stand, its views
    // fold into the overflow menu, which names them, and the window's search keeps its area.
    crate::format::speak_locale("en-US");
    crate::window_toolbar::declare(Some(calendar()));
    let bar = bar_at(&mut harness, 420.0, true);
    let layout = harness.window_bar_layout(420.0);
    let parts: Vec<&str> = layout.controls.iter().map(|control| control.part).collect();
    for part in ["location", "overflow"] {
        assert!(parts.contains(&part), "420 px: {part} in {parts:?}");
    }
    assert!(
        !parts.contains(&"views") && !parts.contains(&"views-menu"),
        "{parts:?}"
    );
    // History and the search's reach come from the bar that carries them: the layout check
    // paints a bar of its own.
    let rects = bar.test_rects();
    let rect = |name: &str| {
        rects
            .iter()
            .find(|(n, _)| n == name)
            .map_or(Rect::ZERO, |(_, rect)| *rect)
    };
    let history = bar.history_action_rect();
    assert!(history.area() > 0.0, "420 px: History stands: {parts:?}");
    assert!(
        rect("window_more").x1 + super::SEARCH_CONTROL_WIDTH <= history.x0,
        "the window's search keeps its area before History: {parts:?}"
    );
    let tree = bar.accessibility().expect("the bar is reachable");
    for (label, checked) in [
        ("Month", false),
        ("Week", true),
        ("Day", false),
        ("Agenda", false),
        ("Tasks", false),
    ] {
        let item = find(&tree, Role::MenuItem, label)
            .unwrap_or_else(|| panic!("{label} is in the overflow menu"));
        assert_eq!(item.state.checked, Some(checked), "{label}");
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
                .filter(|(_, _, rect)| rect.x1 <= history.x0)
                .map(|(_, _, rect)| rect.x1)
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
