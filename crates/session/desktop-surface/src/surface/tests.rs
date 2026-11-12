//! The desktop surface: layout, chrome and input, driven through its real model.

use super::paging::panned_to;
use super::*;
use crate::navigation::{ChromeEffect, DeskEffect, Route};
use ui::automerge::AutoCommit;
use ui::search::Scope;
use ui::widget::{STRIP_BOTTOM_GUTTER_PX, STRIP_TOP_GAP_PX, TileContent};
use ui::winit::dpi::PhysicalPosition;
use ui::winit::event::MouseScrollDelta;

struct Notices;

impl TileSource for Notices {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

pub(super) fn desktop(windows: usize) -> (DesktopSurface, DesktopHandle) {
    desk_with(windows, Box::new(Notices))
}

/// `windows` windows, each running an app of its own.
fn windows_tabs(minter: &mut IdMinter, windows: usize) -> Vec<Tab> {
    (0..windows)
        .map(|i| Tab {
            id: minter.mint(),
            title: format!("App{i}"),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: format!("so.tilo.app{i}"),
                label: format!("App{i}"),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        })
        .collect()
}

/// The same desk, with a source of its own for the windows' content.
pub(super) fn desk_with(
    windows: usize,
    tiles: Box<dyn TileSource>,
) -> (DesktopSurface, DesktopHandle) {
    let mut minter = IdMinter::new(7);
    let tabs = windows_tabs(&mut minter, windows);
    let tree = seed_tree(&mut minter, "Desk", tabs);
    DesktopSurface::new(tree, 7, tiles)
}

/// A desk whose one window holds `windows` windows' tabs on its bar, as a strip does once the
/// windows have been gathered into one.
pub(super) fn one_window_desk(windows: usize) -> (DesktopSurface, DesktopHandle) {
    let mut minter = IdMinter::new(7);
    let tabs = windows_tabs(&mut minter, windows);
    let mut tree = seed_tree(&mut minter, "Desk", tabs);
    let strip = &mut tree.workspaces[0].strips[0];
    let id = strip.window_groups[0].id;
    let tabs: Vec<Tab> = strip
        .window_groups
        .drain(..)
        .flat_map(|group| group.tabs)
        .collect();
    let active_tab = tabs[0].id;
    strip.window_groups = vec![tilo_desktop_model::WindowGroup {
        id,
        tabs,
        active_tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }];
    DesktopSurface::new(tree, 7, Box::new(Notices))
}

fn held_by(name: &str) -> ui::widget::TabHolder {
    ui::widget::TabHolder {
        name: name.into(),
        rgb: (200, 120, 40),
    }
}

/// A window another guest opened on a shared desktop is theirs to close: asked to close it, the
/// desktop refuses at once, says why, and the strip keeps every window where it was.
#[test]
fn a_window_someone_else_holds_is_not_closed_and_the_desktop_says_why() {
    let (surface, handle) = desktop(3);
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    frame.relayout();
    let route = handle.snapshot("g", vec![]).entries[1].route.clone();
    let tab: TabId = route.tab.parse().unwrap();
    assert!(handle.set_holders(HashMap::from([(tab, held_by("Ben"))])));
    let refusal = handle
        .dispatch(DeskEffect::CloseTab {
            tab: route.tab.clone(),
        })
        .unwrap_err();
    assert!(refusal.contains("Ben"), "{refusal}");
    let revision = handle.revision();
    for _ in 0..4 {
        frame.relayout();
    }
    assert_eq!(handle.snapshot("g", vec![]).entries.len(), 3);
    assert_eq!(handle.revision(), revision, "nothing changed to undo");
    // Search does not offer to close it either.
    assert!(handle.focus_tab(tab));
    frame.relayout();
    let held = |id: TabId| id == tab;
    let offered = handle.read(|tree| search::offered_commands(tree, &held));
    assert!(!offered.iter().any(|id| id == "desktop.close-window"));
    // Once it is the person's own (or they moderate), closing works.
    assert!(handle.set_holders(HashMap::new()));
    let offered = handle.read(|tree| search::offered_commands(tree, &|_| false));
    assert!(offered.iter().any(|id| id == "desktop.close-window"));
    handle
        .dispatch(DeskEffect::CloseTab { tab: route.tab })
        .unwrap();
}

/// Workspace and strip edits go through the model's commands: a strip made that way is shown, and a
/// name the model refuses changes nothing.
#[test]
fn workspace_and_strip_commands_run_through_the_models_reduction() {
    use tilo_desktop_model::commands::DesktopCommand;
    let (_surface, handle) = desktop(1);
    let made = handle
        .apply_command(&DesktopCommand::CreateStrip {
            name: "Second".into(),
        })
        .unwrap()
        .expect("a strip was made");
    handle.read(|tree| {
        let workspace = tree.active_workspace().unwrap();
        assert_eq!(workspace.strips.len(), 2);
        assert_eq!(tree.active_strip.get(&workspace.slot), Some(&made));
    });
    handle
        .apply_command(&DesktopCommand::RenameWorkspace {
            name: " Lounge ".into(),
        })
        .unwrap();
    assert!(
        handle
            .apply_command(&DesktopCommand::RenameWorkspace { name: " ".into() })
            .is_err()
    );
    handle
        .apply_command(&DesktopCommand::RemoveStrip { strip: made.0 })
        .unwrap();
    handle.read(|tree| {
        let workspace = tree.active_workspace().unwrap();
        assert_eq!(workspace.name, "Lounge");
        assert_eq!(workspace.strips.len(), 1);
        assert_eq!(
            tree.active_strip.get(&workspace.slot),
            Some(&workspace.strips[0].id)
        );
    });
    // Windows change through navigation, never through this door.
    assert!(
        handle
            .apply_command(&DesktopCommand::OpenApp {
                app: "so.tilo.notes".into(),
                beside: None,
            })
            .is_err()
    );
}

/// Hover never moves a window: the pointer over a window's bar, above its top edge, on its body or
/// in the gap beside it leaves every window's frame, bar and body where they were, and focus where
/// it was (a focused window's edge is drawn in another colour, never further out). Hover may
/// recolour, never lift or grow.
#[test]
fn hovering_a_window_leaves_every_window_where_it_was() {
    let (surface, _handle) = desktop(2);
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1600, 900),
        Theme::dark(),
    );
    for _ in 0..6 {
        frame.relayout();
    }
    let geometry = |frame: &ui::headless::FrameHarness| {
        frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .map(|row| {
                row.groups
                    .iter()
                    .map(|group| {
                        (
                            group.get_rect(),
                            group.bar_rect(),
                            group
                                .content_rect_for_tab(group.active_tab_id())
                                .unwrap_or_default(),
                            group.focused(),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let rest = geometry(&frame);
    assert_eq!(rest.len(), 2, "two windows on the strip: {rest:?}");
    let (first, first_bar, first_body, _) = rest[0];
    let (second, _, _, _) = rest[1];
    let gap_x = f64::midpoint(first.x1, second.x0);
    let spots = [
        ("bar", first_bar.center()),
        (
            "bar's top",
            Point::new(first_bar.center().x, first_bar.y0 + 1.0),
        ),
        (
            "above the bar",
            Point::new(first_bar.center().x, (first_bar.y0 - 4.0).max(0.0)),
        ),
        (
            "body's top edge",
            Point::new(first_body.center().x, first_body.y0 + 2.0),
        ),
        ("body", first_body.center()),
        ("gap", Point::new(gap_x, first_body.center().y)),
        ("gap at the bar", Point::new(gap_x, first_bar.center().y)),
    ];
    for (name, p) in spots {
        // A slow approach and a settle, so any aim-driven reveal fires.
        for step in 0..8 {
            let t = f64::from(step) / 7.0;
            let q = Point::new(p.x, p.y + (1.0 - t) * 20.0);
            frame.send(&ui::synthetic_input::cursor_moved(q.x, q.y));
            frame.relayout();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        for _ in 0..20 {
            frame.relayout();
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        assert_eq!(geometry(&frame), rest, "hovering the {name} at {p:?}");
    }
}

/// A window's search control opens search, over what that window shows: the control is never a dead
/// one in this desktop.
#[test]
fn a_windows_search_control_opens_search() {
    let (surface, handle) = desktop(1);
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1000, 700),
        Theme::dark(),
    );
    frame.relayout();
    assert!(!handle.search_open());
    // The window bar's own search control: with nothing to search in place, it opens the platform
    // search narrowed to the window's app.
    let control = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .and_then(|row| row.groups.first())
        .map(|group| group.window_search().anchor())
        .expect("the window offers its search control");
    assert!(control.area() > 0.0, "the search control is shown");
    frame.click_at(control.center().x, control.center().y);
    for _ in 0..3 {
        frame.relayout();
    }
    assert!(handle.search_open());
}

/// A desk with apps installed, whose windows are notices.
struct Installed;

impl TileSource for Installed {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn search_applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        vec![tilo_desktop_search::providers::AppEntry {
            id: "so.tilo.chat".into(),
            title: "Chat".into(),
            category: Some("Talk".into()),
            description: None,
        }]
    }
}

/// "All apps" under the apps of the empty palette opens the catalogue: every installed app, by its
/// name, with nothing typed.
#[test]
fn the_all_apps_row_opens_the_catalogue() {
    let (surface, handle) = desk_with(1, Box::new(Installed));
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1280, 800),
        Theme::dark(),
    );
    for _ in 0..3 {
        frame.relayout();
    }
    handle.open_search(Scope::Workspace, None);
    for _ in 0..3 {
        frame.relayout();
    }
    let all_apps = crate::i18n::t("search-launcher-all-apps");
    // The row under the apps: one step down from the best app, then Enter.
    frame.key("Down");
    for _ in 0..3 {
        frame.relayout();
    }
    frame.key("Enter");
    for _ in 0..3 {
        frame.relayout();
    }
    assert!(handle.search_open(), "the catalogue replaces the search");
    let (query, rows) = handle.search_rows();
    assert_eq!(query, "", "the catalogue opens with nothing typed");
    assert!(
        rows.iter().any(|row| row.title == "Chat"),
        "every app is listed: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| row.title != all_apps),
        "and the catalogue does not offer itself again: {rows:?}"
    );
}

/// Search over one window stands over that window, as an app's own palette does, not centred on the
/// screen over another window.
#[test]
fn a_windows_search_stands_over_that_window() {
    let (surface, handle) = desktop(2);
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1400, 800),
        Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let (control, window) = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .and_then(|row| {
            row.groups
                .iter()
                .filter(|group| {
                    group.get_rect().x1 <= 1400.0 && group.window_search().anchor().area() > 0.0
                })
                .max_by(|a, b| a.get_rect().x0.total_cmp(&b.get_rect().x0))
        })
        .map(|group| (group.window_search().anchor(), group.get_rect()))
        .expect("a window on screen offers its search control");
    assert!(
        (window.center().x - 700.0).abs() > 100.0,
        "the window {window:?} is off the screen's centre"
    );
    frame.click_at(control.center().x, control.center().y);
    for _ in 0..3 {
        frame.relayout();
    }
    assert!(handle.search_open());
    // What search stands over is the window it was opened from, under that window's bar: the chrome
    // draws the card there, over the windows.
    let area = handle
        .search_area()
        .expect("search says what it stands over");
    assert!(
        (area.x0 - window.x0).abs() < 0.5 && (area.x1 - window.x1).abs() < 0.5,
        "search stands over the window {window:?}, not {area:?}"
    );
    assert!(
        area.y0 > window.y0 && area.y1 <= window.y1,
        "the area {area:?} starts under the window's bar in {window:?}"
    );
}

/// An acknowledgement is said politely over the desktop, wakes the host for its fade and end, and
/// is no notification.
#[test]
fn an_acknowledgement_passes_over_the_desktop_without_a_notification() {
    let (mut surface, handle) = desktop(1);
    let notifications = DesktopNotifications::default();
    surface.set_notifications(notifications.clone());
    let said = |surface: &DesktopSurface| {
        surface
            .children()
            .last()
            .and_then(|layer| layer.accessibility())
            .and_then(|node| node.live.zip(node.value))
    };
    // Present and silent before anything is said.
    assert_eq!(
        said(&surface),
        Some((ui::a11y::Politeness::Polite, String::new()))
    );
    handle.acknowledge("Link copied");
    assert_eq!(
        said(&surface),
        Some((ui::a11y::Politeness::Polite, "Link copied".to_owned()))
    );
    assert!(surface.needs_redraw());
    assert!(Widget::next_wake_deadline(&surface).is_some());
    assert_eq!(notifications.counts(), (0, 0));
}

/// Each select a host sends applies in the frame its wake draws, in order, while the strip pans
/// between windows.
#[test]
fn selects_apply_in_the_frame_they_wake() {
    let (surface, handle) = desktop(3);
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    for _ in 0..4 {
        frame.relayout();
    }
    let entries = handle.snapshot("g", vec![]).entries;
    for target in [0, 2, 1, 2] {
        let route = entries[target].route.clone();
        handle
            .dispatch(DeskEffect::Focus {
                tab: route.tab.clone(),
            })
            .unwrap();
        frame.relayout();
        assert_eq!(
            handle.snapshot("g", vec![]).active,
            Some(route),
            "select {target}"
        );
    }
}

/// A window's frame, border and rounded corners included, stands inside the desktop with wallpaper
/// around it at the strip's home and at its end: past the leading edge, before the trailing edge
/// and above the bottom edge, by at least the gap between two windows. Its bar stands above the
/// frame, so the top is the bar's to keep.
#[test]
fn windows_stand_clear_of_the_desktop_edges_at_both_ends_of_the_strip() {
    let gap = ui::widget::HANDLE_SLOT_PX;
    let (width, height) = (1920_u32, 1080_u32);
    let inside = |rect: Rect, viewport: Rect, what: &str| {
        assert!(
            rect.x0 - viewport.x0 >= gap,
            "{what}: frame {rect:?} starts {} px into {viewport:?}",
            rect.x0 - viewport.x0
        );
        assert!(
            viewport.x1 - rect.x1 >= gap,
            "{what}: frame {rect:?} ends {} px before {viewport:?}'s end",
            viewport.x1 - rect.x1
        );
        assert!(
            viewport.y1 - rect.y1 >= gap,
            "{what}: frame {rect:?} ends {} px above {viewport:?}'s bottom",
            viewport.y1 - rect.y1
        );
    };
    for windows in [1, 4] {
        let (surface, handle) = desktop(windows);
        handle.set_viewport_width(f64::from(width));
        let mut frame = ui::headless::FrameHarness::for_root(
            ui::state::State::new(),
            surface,
            (width, height),
            Theme::dark(),
        );
        for _ in 0..4 {
            frame.relayout();
        }
        let viewport = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
        let row = frame.root::<DesktopSurface>().shown_row_ref().unwrap();
        assert_eq!(row.scroll_x(), 0.0, "{windows} windows: the strip is home");
        let first = row.groups.first().unwrap().get_rect();
        inside(
            first,
            viewport,
            &format!("{windows} windows, home, the first"),
        );
        if windows == 1 {
            continue;
        }

        // Brought into view, the last window keeps the gutter after it,
        // one in the middle stands clear of the edge it came in from,
        // and the first one, brought back, rests home again.
        let entries = handle.snapshot("g", vec![]).entries;
        for (target, what) in [
            (windows - 1, "the last"),
            (1, "the second"),
            (0, "the first"),
        ] {
            handle
                .dispatch(DeskEffect::Focus {
                    tab: entries[target].route.tab.clone(),
                })
                .unwrap();
            play_frames(&mut frame, 40);
            let row = frame.root::<DesktopSurface>().shown_row_ref().unwrap();
            let rect = row.groups[target].get_rect();
            inside(
                rect,
                viewport,
                &format!("{windows} windows, {what} focused"),
            );
        }
        assert_eq!(
            frame
                .root::<DesktopSurface>()
                .shown_row_ref()
                .unwrap()
                .scroll_x(),
            0.0,
            "the first window focused again rests home"
        );

        let mut surface = frame.root_mut::<DesktopSurface>();
        surface.shown_row().unwrap().pan_to(f64::MAX);
        drop(surface);
        frame.relayout();
        let row = frame.root::<DesktopSurface>().shown_row_ref().unwrap();
        let last = row.groups.last().unwrap().get_rect();
        inside(last, viewport, &format!("{windows} windows, end, the last"));
    }
}

/// A tiled strip keeps wallpaper above and below its windows — a step under the bar, so their own
/// top edge and corners stand clear of it, and a band at the bottom edge — and a maximized window
/// takes both bands: it runs from the bar's band to the desktop's bottom edge, and they come back
/// when it is restored.
#[test]
fn the_strip_keeps_the_bands_above_and_below_it_and_a_maximized_window_takes_both() {
    let (width, height) = (1440_u32, 900_u32);
    let (surface, handle) = desktop(2);
    handle.set_viewport_width(f64::from(width));
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (width, height),
        Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let bands = |frame: &ui::headless::FrameHarness| {
        let rect = frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .unwrap()
            .groups
            .first()
            .unwrap()
            .get_rect();
        (rect.y0, rect.y1)
    };
    let (tiled_top, tiled_end) = bands(&frame);
    assert!(
        (tiled_top - STRIP_TOP_GAP_PX).abs() < 0.5,
        "a tiled window starts a step under the bar's band: it starts at {tiled_top}"
    );
    assert!(
        (f64::from(height) - tiled_end - STRIP_BOTTOM_GUTTER_PX).abs() < 0.5,
        "and keeps the band under it: it ends at {tiled_end}"
    );
    let route = handle.snapshot("g", vec![]).entries[0].route.clone();
    for maximized in [true, false] {
        handle
            .dispatch(ChromeEffect::Maximize {
                group: route.group.clone(),
                on: maximized,
            })
            .unwrap();
        for _ in 0..40 {
            frame.relayout();
        }
        let (top, end) = bands(&frame);
        if maximized {
            assert!(
                top.abs() < 0.5,
                "maximized, the window starts where the bar's band ends: it starts at {top}"
            );
            assert!(
                (f64::from(height) - end).abs() < 0.5,
                "and runs to the bottom edge: it ends at {end}"
            );
        } else {
            assert!(
                (top - tiled_top).abs() < 0.5 && (end - tiled_end).abs() < 0.5,
                "restored, both bands are back: {top} to {end}, were {tiled_top} to {tiled_end}"
            );
        }
    }
}

#[test]
fn closing_a_window_keeps_every_frame_settling() {
    let (surface, handle) = desktop(3);
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    for _ in 0..4 {
        frame.relayout();
    }
    let route = handle.snapshot("g", vec![]).entries[0].route.clone();
    handle
        .dispatch(DeskEffect::CloseTab { tab: route.tab })
        .unwrap();
    // Forty frames of a 60 Hz display, on the clock the close animates by.
    let mut at = ui::time::now();
    for _ in 0..40 {
        at = at.checked_add(Duration::from_millis(16)).unwrap();
        ui::time::testing::with_monotonic_time(at, || {
            assert!(frame.poll_frame().is_ok(), "{:?}", frame.poll_frame());
        });
    }
    assert_eq!(handle.snapshot("g", vec![]).entries.len(), 2);
}

#[test]
fn clicking_a_window_body_makes_it_active() {
    let (surface, handle) = desktop(3);
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    for _ in 0..4 {
        frame.relayout();
    }
    let active = || handle.read(|tree| navigation::active_route(tree).map(|ids| ids.group));
    let first = active();
    let device_id = ui::winit::event::DeviceId::dummy();
    frame.send_host_event(&WindowEvent::CursorMoved {
        device_id,
        position: PhysicalPosition::new(1270.0, 500.0),
    });
    for state in [ElementState::Pressed, ElementState::Released] {
        frame.send_host_event(&WindowEvent::MouseInput {
            device_id,
            state,
            button: MouseButton::Left,
        });
    }
    let clicked = active();
    assert!(clicked.is_some());
    assert_ne!(clicked, first, "the window under the pointer takes focus");
}

/// A window dragged wider by its trailing edge keeps the width the person dragged, over the frames
/// that follow.
#[test]
fn an_edge_drag_sets_the_width_and_the_strip_keeps_it() {
    let (surface, handle) = desktop(3);
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1920, 1080));
    for _ in 0..4 {
        frame.relayout();
    }
    let (window, x, y) = frame
        .widget_mut::<GroupRow>()
        .and_then(|row| row.groups.first().map(|group| (group.id, group.get_rect())))
        .map(|(id, rect)| (id, rect.x1 - 3.0, rect.center().y))
        .expect("a window with a trailing edge to drag");
    let width = || {
        handle.read(|tree| {
            tree.workspaces[0].strips[0]
                .window_groups
                .iter()
                .find(|group| group.id == window)
                .map(|group| group.width)
        })
    };
    let before = width();
    let device_id = ui::winit::event::DeviceId::dummy();
    frame.send_host_event(&WindowEvent::CursorMoved {
        device_id,
        position: PhysicalPosition::new(x, y),
    });
    frame.send_host_event(&WindowEvent::MouseInput {
        device_id,
        state: ElementState::Pressed,
        button: MouseButton::Left,
    });
    frame.send_host_event(&WindowEvent::CursorMoved {
        device_id,
        position: PhysicalPosition::new(x + 120.0, y),
    });
    frame.send_host_event(&WindowEvent::MouseInput {
        device_id,
        state: ElementState::Released,
        button: MouseButton::Left,
    });
    for _ in 0..4 {
        frame.relayout();
    }
    let dragged = width().expect("the dragged window is still in its strip");
    assert_ne!(before, Some(dragged), "the drag widens the window");
    assert!(
        matches!(dragged, WindowWidth::Fraction(_)),
        "the strip keeps the width the person dragged, not a tier: {dragged:?}"
    );
    for _ in 0..4 {
        frame.relayout();
    }
    assert_eq!(
        width(),
        Some(dragged),
        "and it stays over the frames that follow"
    );
}

/// The desk mints its ids under the device's range, and a tree kept beside the device's state file
/// carries none of its own: installing the kept tree lays this device's changes in this desk's
/// range, not in "nobody's".
#[test]
fn a_tree_kept_beside_the_state_file_installs_in_this_desks_range() {
    let (_surface, handle) = desktop(1);
    let minted = handle.read(Clone::clone);
    assert_eq!(minted.salt, 7, "the desk mints in the device's range");
    let mut kept = minted.clone();
    kept.salt = 0;
    handle.replace_tree(kept);
    let installed = handle.read(Clone::clone);
    assert_eq!(
        installed.salt, minted.salt,
        "the tree installed writes in this desk's range"
    );
    assert_eq!(
        installed.workspaces[0].strips[0].id, minted.workspaces[0].strips[0].id,
        "and it is the tree that was kept"
    );
}

#[test]
fn shared_snapshot_keeps_stream_tab_and_focus_but_applies_real_closes() {
    let (mut surface, handle) = desktop(3);
    let state = AutoCommit::new();
    surface.update(&state, &ui::automerge::ROOT);
    let mut local = handle.read(Clone::clone);
    let strip = &mut local.workspaces[0].strips[0];
    let stream = strip.window_groups[2].tabs[0].id;
    strip.window_groups[2].tabs[0].tile = tilo_desktop_model::TileContent::PeerWindow {
        app_id: "so.tilo.terminal".into(),
        label: "Terminal".into(),
        note: String::new(),
        missing: None,
    };
    let removed = strip.window_groups[1].tabs[0].id;
    handle.replace_tree(local.clone());
    surface.update(&state, &ui::automerge::ROOT);
    assert!(handle.focus_tab(stream));
    surface.update(&state, &ui::automerge::ROOT);

    let projected = handle.shared_tree();
    assert!(projected.active_group.is_empty());
    assert!(
        projected.workspaces[0].strips[0]
            .window_groups
            .iter()
            .flat_map(|g| &g.tabs)
            .all(|t| t.id != stream)
    );
    assert!(handle.focus_tab(removed));
    surface.update(&state, &ui::automerge::ROOT);
    assert_eq!(
        serde_json::to_value(handle.shared_tree()).unwrap(),
        serde_json::to_value(projected).unwrap()
    );
    assert!(handle.focus_tab(stream));
    surface.update(&state, &ui::automerge::ROOT);
    let before = handle.snapshot("g", vec![]).active;
    let mut incoming = local;
    incoming.remove_tab(stream);
    incoming.remove_tab(removed);
    incoming.workspaces[0].name = "Shared rename".into();
    // Reproduce the browser's inbound snapshot replacement.
    handle.replace_shared_tree(incoming, &[stream]);
    surface.update(&state, &ui::automerge::ROOT);
    assert_eq!(handle.snapshot("g", vec![]).active, before);
    handle.read(|tree| {
        assert_eq!(tree.workspaces[0].name, "Shared rename");
        let tabs: Vec<_> = tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect();
        assert!(tabs.contains(&stream));
        assert!(!tabs.contains(&removed));
    });
    // Removal of the containing workspace still retires the stream.
    let mut incoming = handle.shared_tree();
    incoming.workspaces.clear();
    handle.replace_shared_tree(incoming, &[stream]);
    surface.update(&state, &ui::automerge::ROOT);
    assert!(handle.snapshot("g", vec![]).active.is_none());
    assert!(handle.read(|tree| tree.workspaces.is_empty()));
}

#[test]
fn a_retained_notice_can_reopen_its_application_after_its_window_closed() {
    struct Relaunch;
    impl TileSource for Relaunch {
        fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
            TileHost::new(TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
        fn launch(&mut self, _: &Workspace, id: TabId, app: &str, _: Option<TabId>) -> Option<Tab> {
            (app == "so.tilo.clocks").then(|| Tab {
                id,
                title: "Clocks".into(),
                tile: tilo_desktop_model::TileContent::LocalApp {
                    app_id: app.into(),
                    label: "Clocks".into(),
                    note: String::new(),
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            })
        }
    }
    let mut minter = IdMinter::new(7);
    let tree = seed_tree(&mut minter, "Desk", Vec::new());
    let (mut surface, handle) = DesktopSurface::new(tree, 7, Box::new(Relaunch));
    let notifications = DesktopNotifications::default();
    let mut notice = crate::notifications::Notification::new("Timer finished", "Tea is ready");
    notice.app_id = Some("so.tilo.clocks".into());
    notice.app_name = Some("Clocks".into());
    let wall = ui::time::SystemTime::now();
    let kept =
        crate::notifications::encode_history(&[notice], |_| false, wall, ui::time::now()).unwrap();
    notifications.restore_kept(&kept, wall).unwrap();
    surface.set_notifications(notifications.clone());
    let mut centre = NotificationLayer::new(notifications.clone());
    centre.set_center_open(true);
    centre.apply(crate::notifications::CenterOutcome::OpenApp {
        app_id: "so.tilo.clocks".into(),
        label: "Clocks".into(),
    });
    let state = AutoCommit::new();
    surface.update(&state, &ui::automerge::ROOT);
    assert!(handle.focus_app("so.tilo.clocks"));
    assert_eq!(handle.snapshot("g", vec![]).entries.len(), 1);
    assert_eq!(notifications.counts(), (0, 1));
}

#[test]
fn notification_navigation_shows_expired_real_banners_in_the_center() {
    let (mut surface, handle) = desktop(1);
    let notifications = DesktopNotifications::default();
    surface.set_notifications(notifications.clone());
    assert_eq!(notifications.counts(), (0, 0));
    let notice = crate::notifications::Notification::new("Timer finished", "Tea is ready");
    let id = notice.id;
    let shown = ui::time::now();
    let expires = shown + notice.dismiss_after;
    notifications.post(notice);
    assert!(
        !notifications.expire(expires),
        "a banner never on screen does not expire"
    );
    notifications.present(shown, &[id], false);
    assert!(notifications.expire(expires));
    handle
        .dispatch(ChromeEffect::Notifications { open: true })
        .unwrap();
    let state = AutoCommit::new();
    surface.update(&state, &ui::automerge::ROOT);
    // The desk asks whoever draws the centre over the windows to open it.
    assert_eq!(notifications.take_centre_ask(), Some(true));
    let mut centre = NotificationLayer::new(notifications.clone());
    centre.set_center_open(true);
    surface.update(&state, &ui::automerge::ROOT);
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(
        snapshot.notifications,
        Some(NotificationsState {
            active: 0,
            retained: 1,
            center_open: true,
        })
    );
    assert_eq!(notifications.log().history()[0].id, id);
    handle
        .dispatch(ChromeEffect::Notifications { open: false })
        .unwrap();
    surface.update(&state, &ui::automerge::ROOT);
    assert_eq!(notifications.take_centre_ask(), Some(false));
    centre.set_center_open(false);
    surface.update(&state, &ui::automerge::ROOT);
    assert!(
        !handle
            .snapshot("g", vec![])
            .notifications
            .unwrap()
            .center_open
    );
    assert_eq!(notifications.counts(), (0, 1));
}

fn focused(surface: &mut DesktopSurface, handle: &DesktopHandle) -> usize {
    let state = AutoCommit::new();
    surface.update(&state, &ui::automerge::ROOT);
    handle.read(|tree| {
        let active = navigation::active_route(tree).unwrap();
        tree.workspaces[0].strips[0]
            .window_groups
            .iter()
            .position(|g| g.id == active.group)
            .unwrap()
    })
}

fn pixels(dx: f64, dy: f64) -> MouseScrollDelta {
    MouseScrollDelta::PixelDelta(PhysicalPosition::new(dx, dy))
}

fn ms(start: Instant, ms: u64) -> Instant {
    start.checked_add(Duration::from_millis(ms)).unwrap()
}

/// A sideways wheel turned `rightward` lines (negative: leftward), as winit reports it: negated.
fn lines(rightward: f32) -> MouseScrollDelta {
    MouseScrollDelta::LineDelta(-rightward, 0.0)
}

/// One notch of a sideways wheel spun `rightward` as Firefox reports it: six lines (negative:
/// leftward).
fn notch(rightward: f32) -> MouseScrollDelta {
    lines(6.0 * rightward)
}

/// Route one wheel event the content under the pointer left to the strip,
/// as the desktop's host routes it, and hand back whether the strip took it.
fn wheel(surface: &mut DesktopSurface, delta: MouseScrollDelta, at: Instant) -> bool {
    surface.end_idle_swipe(at);
    let Some(travel) = surface.strip_travel(delta, 1.0) else {
        return false;
    };
    surface.route_strip_swipe(travel, at) || surface.take_sideways(travel, false, at)
}

/// Route one wheel event over content that scrolls sideways with it, which takes the gesture's
/// first event.
fn wheel_in_app(surface: &mut DesktopSurface, delta: MouseScrollDelta, at: Instant) -> bool {
    surface.end_idle_swipe(at);
    let Some(travel) = surface.strip_travel(delta, 1.0) else {
        return false;
    };
    surface.route_strip_swipe(travel, at) || surface.take_sideways(travel, true, at)
}

#[test]
fn a_wheel_spin_steps_a_strip_that_cannot_pan_by_its_travel() {
    // Unlaid, the strip cannot pan, as when its windows all fit.
    let (mut surface, handle) = desktop(5);
    let t = ui::time::now();
    assert!(wheel(&mut surface, notch(1.0), t));
    assert_eq!(
        focused(&mut surface, &handle),
        1,
        "a spin's first notch steps at once"
    );
    let gaps = [17, 50, 0, 33, 17, 17, 50];
    let mut at = 0;
    for gap in gaps {
        at += gap;
        assert!(wheel(&mut surface, notch(1.0), ms(t, at)));
    }
    // Eight notches of six lines travel 8 × 6 × line_of_wheel(): the first notch's step and one per
    // STEP_TRAVEL_PX after it.
    assert_eq!(
        focused(&mut surface, &handle),
        4,
        "a step per window's width of travel, not per notch"
    );
    for i in 1..=12 {
        assert!(
            wheel(&mut surface, notch(1.0), ms(t, at + 17 * i)),
            "travel past the last window stays the strip's"
        );
    }
    assert_eq!(focused(&mut surface, &handle), 4);

    let t = ms(t, 2000);
    assert!(
        wheel(&mut surface, notch(-1.0), t),
        "after a pause a notch back steps back"
    );
    assert_eq!(focused(&mut surface, &handle), 3);
    assert!(
        wheel(&mut surface, pixels(240.0, 0.0), ms(t, 1000)),
        "a trackpad's travel steps the same"
    );
    assert_eq!(focused(&mut surface, &handle), 2);
    for i in 1..=3 {
        wheel(&mut surface, pixels(240.0, 0.0), ms(t, 1000 + 17 * i));
    }
    assert_eq!(focused(&mut surface, &handle), 1);
}

#[test]
fn a_sideways_spin_pans_an_overflowing_strip_and_focus_follows_it() {
    // Six notches a kind: a wheel's six lines each, a trackpad's 240 px.
    for (kind, delta, travel) in [
        ("wheel", notch(1.0), 36.0 * ui::widget::line_of_wheel()),
        ("trackpad", pixels(-240.0, 0.0), 6.0 * 240.0),
    ] {
        let (surface, handle) = desktop(5);
        let mut frame = ui::headless::FrameHarness::for_root_with(
            ui::state::State::new(),
            surface,
            (1280, 800),
            ui::headless::FrameOptions::default(),
        );
        for _ in 0..4 {
            frame.relayout();
        }
        let start = frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .unwrap()
            .scroll_x();
        let mut surface = frame.root_mut::<DesktopSurface>();
        assert!(
            surface.shown_row_ref().unwrap().pan_max_scroll_x() > 2000.0,
            "{kind}: five windows overflow the strip"
        );
        let t = ui::time::now();
        for i in 0..6 {
            assert!(wheel(&mut surface, delta, ms(t, 17 * i)), "{kind}");
        }
        let row = surface.shown_row_ref().unwrap();
        assert!(
            (row.scroll_x() - start - travel).abs() < 0.5,
            "{kind}: six notches pan six notches' travel, from {start} to {}",
            row.scroll_x()
        );
        let looked_at = panned_to(row, None).unwrap();
        assert_eq!(
            handle.read(|tree| navigation::active_route(tree).unwrap().group),
            handle.read(|tree| tree.workspaces[0].strips[0].window_groups[0].id),
            "{kind}: focus stays put while the strip moves"
        );
        drop(surface);

        // Frames lay the strip out while the wheel spins.
        frame.relayout();
        frame
            .root_mut::<DesktopSurface>()
            .end_idle_swipe(ms(t, 17 * 5 + 250));
        for _ in 0..4 {
            frame.relayout();
        }
        let shown = focused(&mut frame.root_mut::<DesktopSurface>(), &handle);
        assert_eq!(
            handle.read(|tree| tree.workspaces[0].strips[0].window_groups[shown].id),
            looked_at,
            "{kind}: the pan's end focuses the window panned to"
        );
        assert!(
            shown >= 1,
            "{kind}: focus followed the strip to window {shown}"
        );
        let surface = frame.root::<DesktopSurface>();
        assert!(
            (surface.shown_row_ref().unwrap().scroll_x() - start - travel).abs() < 0.5,
            "{kind}: and the strip stays where the pan left it"
        );
    }
}

/// A laid-out desk of `windows` windows, its strip overflowed so it can pan.
fn laid_out(windows: usize) -> (ui::headless::FrameHarness, DesktopHandle) {
    let (surface, handle) = desktop(windows);
    let mut frame = ui::headless::FrameHarness::for_root_with(
        ui::state::State::new(),
        surface,
        (1280, 800),
        ui::headless::FrameOptions::default(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    assert!(
        frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .unwrap()
            .pan_max_scroll_x()
            > 2000.0,
        "five windows overflow the strip"
    );
    (frame, handle)
}

fn strip_offset(frame: &ui::headless::FrameHarness) -> f64 {
    frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .unwrap()
        .scroll_x()
}

/// Frames at 60 Hz from `t` until nothing on the desktop is moving, handing each frame's strip
/// offset to `watch` .
fn settle_frames(
    frame: &mut ui::headless::FrameHarness,
    t: ui::time::Instant,
    mut watch: impl FnMut(f64),
) -> f64 {
    let mut at = t;
    for _ in 0..600 {
        let offset = ui::time::testing::with_monotonic_time(at, || {
            frame.relayout();
            strip_offset(frame)
        });
        watch(offset);
        if !frame.frame_schedule().needs_animation_frame {
            // The pass that brings the strip to rest may queue the change it leaves: the frames
            // after it apply that.
            for step in 1..=3u64 {
                let later = at.checked_add(Duration::from_millis(step)).unwrap_or(at);
                ui::time::testing::with_monotonic_time(later, || frame.relayout());
            }
            return strip_offset(frame);
        }
        at = at.checked_add(Duration::from_millis(17)).unwrap_or(at);
    }
    panic!("the strip is still moving after ten seconds of frames");
}

/// Three fingers on the pad carry the strip shown: one pixel of their travel is one of its own from
/// the first report on, past either end against the rubber band, and a strip whose windows all fit
/// steps focus by their travel.
#[test]
fn three_fingers_carry_the_strip_under_them() {
    let (mut frame, _handle) = laid_out(5);
    let start = strip_offset(&frame);
    let t = ui::time::now();
    let mut surface = frame.root_mut::<DesktopSurface>();
    // The gesture reports its total travel, frame by frame.
    for (i, travel) in [0.0, 12.0, 40.0, 75.0, 96.0].into_iter().enumerate() {
        let took = ui::time::testing::with_monotonic_time(ms(t, 16 * i as u64), || {
            surface.carry_strip(travel, i == 0)
        });
        if travel > 0.0 {
            assert!(took, "the fingers carry the strip at {travel} px");
        }
        assert!(
            (surface.shown_row_ref().unwrap().scroll_x() - start - travel).abs() < 0.5,
            "the strip is where the fingers are, {travel} px along it: {}",
            surface.shown_row_ref().unwrap().scroll_x()
        );
    }
    // Reversing inside the gesture follows them back.
    for (i, travel) in [70.0, 30.0, 0.0].into_iter().enumerate() {
        ui::time::testing::with_monotonic_time(ms(t, 80 + 16 * i as u64), || {
            surface.carry_strip(travel, false)
        });
        assert!(
            (surface.shown_row_ref().unwrap().scroll_x() - start - travel).abs() < 0.5,
            "the strip follows the fingers back to {travel} px"
        );
    }
    drop(surface);

    // Past the end the strip pulls against the rubber band: the fingers still move it, by less, and
    // letting go springs it back to the end.
    let max = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .unwrap()
        .pan_max_scroll_x();
    let mut surface = frame.root_mut::<DesktopSurface>();
    let pull = |surface: &mut DesktopSurface, at: u64, travel: f64, began: bool| {
        ui::time::testing::with_monotonic_time(ms(t, at), || surface.carry_strip(travel, began));
        surface.shown_row_ref().unwrap().scroll_x()
    };
    let stretched = pull(&mut surface, 200, 4000.0, true);
    assert!(
        stretched > max + 100.0,
        "the pull shows past the end: {stretched} of {max}"
    );
    let further = pull(&mut surface, 216, 6000.0, false);
    assert!(
        further > stretched,
        "the pull goes on: {stretched} -> {further}"
    );
    assert!(
        further - stretched < 2000.0,
        "and the band resists: {stretched} -> {further}"
    );
    ui::time::testing::with_monotonic_time(ms(t, 232), || surface.release_strip(false));
    drop(surface);
    let rested = settle_frames(&mut frame, ms(t, 250), |_| {});
    assert!(
        (rested - max).abs() < 0.5,
        "and letting go springs it back to its end: {rested} of {max}"
    );
}

/// Letting go of three fingers leaves the strip coasting at the speed they let go at and then
/// resting, with focus on the window it left in view; the hand taken back throws it nowhere.
#[test]
fn letting_go_of_three_fingers_coasts_the_strip_and_focus_follows() {
    for cancelled in [false, true] {
        let (mut frame, handle) = laid_out(5);
        let start = strip_offset(&frame);
        let t = ui::time::now();
        let mut surface = frame.root_mut::<DesktopSurface>();
        // A flick, its last frames fast enough to carry on past the window the fingers let go of.
        for (i, travel) in [150.0, 500.0, 900.0, 1300.0].into_iter().enumerate() {
            ui::time::testing::with_monotonic_time(ms(t, 16 * i as u64), || {
                surface.carry_strip(travel, i == 0)
            });
        }
        let carried = surface.shown_row_ref().unwrap().scroll_x();
        assert!(
            (carried - start - 1300.0).abs() < 0.5,
            "carried to {carried}"
        );
        drop(surface);
        // The frame that shows the last report, and the fingers lifting in it.
        let left = ui::time::testing::with_monotonic_time(ms(t, 64), || {
            frame.relayout();
            let released = frame.root_mut::<DesktopSurface>().release_strip(cancelled);
            assert!(released, "the fingers were holding the strip");
            strip_offset(&frame)
        });
        let rested = settle_frames(&mut frame, ms(t, 80), |_| {});
        if cancelled {
            assert!(
                (rested - left).abs() < 0.5,
                "the hand taken back leaves the strip where it let it: {rested} of {left}"
            );
        } else {
            assert!(
                rested > left + 20.0,
                "the flick coasts on from {left} to {rested}"
            );
        }
        let focused = handle.snapshot("g", vec![]).active.unwrap();
        let first = handle.snapshot("g", vec![]).entries[0].route.clone();
        assert_ne!(
            focused.group, first.group,
            "focus followed the strip past the first window ({cancelled})"
        );
        let rect = frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .and_then(|row| {
                row.groups
                    .iter()
                    .find(|group| group.id.to_string() == focused.group)
                    .map(ui::Widget::get_rect)
            })
            .expect("the focused window is drawn");
        assert!(
            rect.x0 >= -0.5 && rect.x1 <= 1280.5,
            "and it is wholly in view where the strip rests: {rect:?} ({cancelled})"
        );
    }
}

/// Focusing a window past the edge of the screen slides the strip to it over the toolkit's motion
/// rather than cutting to it, and leaves it in view.
#[test]
fn focusing_a_window_off_screen_slides_the_strip_into_frame() {
    let (mut frame, handle) = laid_out(5);
    let start = strip_offset(&frame);
    let last = handle
        .snapshot("g", vec![])
        .entries
        .last()
        .unwrap()
        .route
        .clone();
    handle
        .dispatch(DeskEffect::Focus {
            tab: last.tab.clone(),
        })
        .unwrap();
    let t = ui::time::now();
    let mut offsets = Vec::new();
    let rested = settle_frames(&mut frame, t, |offset| offsets.push(offset));
    assert_eq!(handle.snapshot("g", vec![]).active.as_ref(), Some(&last));
    assert!(
        rested > start + 100.0,
        "the strip went to the window: {start} to {rested}"
    );
    let slid = offsets
        .iter()
        .any(|&offset| offset > start + 5.0 && offset < rested - 5.0);
    assert!(slid, "the strip slid rather than cut: {offsets:?}");
    let rect = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .and_then(|row| {
            row.groups
                .iter()
                .find(|group| group.id.to_string() == last.group)
                .map(ui::Widget::get_rect)
        })
        .expect("the focused window is drawn");
    assert!(
        rect.x0 >= -0.5 && rect.x1 <= 1280.5,
        "and the focused window is wholly in view: {rect:?}"
    );
}

#[test]
fn a_sideways_gesture_belongs_to_the_content_that_takes_it() {
    let (mut surface, handle) = desktop(1);
    let t = ui::time::now();
    for i in 0..10 {
        assert!(
            !wheel_in_app(&mut surface, lines(1.0), ms(t, 16 * i)),
            "a lone window's app scrolls sideways"
        );
    }
    assert_eq!(focused(&mut surface, &handle), 0);
    let t = ms(t, 1000);
    for i in 0..10 {
        assert!(
            !wheel_in_app(&mut surface, lines(-1.0), ms(t, 16 * i)),
            "and the whole of its gesture stays the app's, backwards included"
        );
    }

    let (mut surface, handle) = desktop(3);
    let t = ui::time::now();
    for (i, delta) in [pixels(30.0, 60.0), pixels(0.0, -200.0)]
        .into_iter()
        .enumerate()
    {
        assert!(
            !wheel(&mut surface, delta, ms(t, 300 * i as u64)),
            "{delta:?}"
        );
    }
    assert_eq!(focused(&mut surface, &handle), 0);
    assert!(
        wheel(&mut surface, lines(1.0), ms(t, 1000)),
        "a mouse's sideways wheel click steps like the shell's"
    );
    assert_eq!(focused(&mut surface, &handle), 1);
}

#[test]
fn a_swipe_nothing_scrolls_pulls_the_strip_against_its_rubber_band() {
    // Toward the strip's start, where a lone window at home cannot pan.
    for (kind, delta) in [("trackpad", pixels(40.0, 0.0)), ("wheel", notch(-1.0))] {
        let (surface, handle) = desktop(1);
        let mut frame = ui::headless::FrameHarness::for_root_with(
            ui::state::State::new(),
            surface,
            (1280, 800),
            ui::headless::FrameOptions::default(),
        );
        for _ in 0..4 {
            frame.relayout();
        }
        let mut surface = frame.root_mut::<DesktopSurface>();
        let home = surface.shown_row_ref().unwrap().scroll_x();
        let t = ui::time::now();
        for i in 0..10 {
            assert!(
                wheel(&mut surface, delta, ms(t, 16 * i)),
                "{kind}: the strip takes what the lone window's content leaves"
            );
        }
        let pulled = home - surface.shown_row_ref().unwrap().scroll_x();
        assert!(
            pulled > 20.0,
            "{kind}: the strip gives past its start, by {pulled} px"
        );
        let travel = if kind == "trackpad" { 400.0 } else { 2400.0 };
        assert!(
            pulled < travel / 2.0,
            "{kind}: against resistance, {pulled} px of {travel}"
        );
        assert_eq!(focused(&mut surface, &handle), 0);

        surface.end_idle_swipe(ms(t, 16 * 9 + 250));
        drop(surface);
        assert!(frame.settle_for_capture(Duration::from_secs(3)), "{kind}");
        let rest = frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .unwrap()
            .scroll_x();
        assert!(
            (rest - home).abs() < 0.5,
            "{kind}: let go, it springs back home ({home}), not {rest}"
        );
    }
}

#[test]
fn a_mouse_pans_the_strip_from_empty_canvas() {
    /// Point the mouse at canvas no window covers.
    fn find_canvas(frame: &mut ui::headless::FrameHarness) -> (f64, f64) {
        for y in (4..800).step_by(16).rev() {
            for x in (4..1280).step_by(8).rev() {
                let (x, y) = (f64::from(x), f64::from(y));
                frame.send(&ui::synthetic_input::cursor_moved(x, y));
                if frame
                    .root::<DesktopSurface>()
                    .shown_row_ref()
                    .unwrap()
                    .pointer_over_background()
                {
                    return (x, y);
                }
            }
        }
        panic!("some canvas shows around the windows");
    }
    fn scroll(frame: &ui::headless::FrameHarness) -> f64 {
        frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .unwrap()
            .scroll_x()
    }
    let (surface, _handle) = desktop(5);
    let mut frame = ui::headless::FrameHarness::for_root_with(
        ui::state::State::new(),
        surface,
        (1280, 800),
        ui::headless::FrameOptions::default(),
    );
    for _ in 0..4 {
        frame.relayout();
    }

    // Windows and the seams between them cover the strip; canvas shows past the last window once
    // the strip is panned to its end.
    let t = ui::time::now();
    for i in 0..40 {
        wheel(
            &mut frame.root_mut::<DesktopSurface>(),
            pixels(-240.0, 0.0),
            ms(t, 17 * i),
        );
    }
    // Let go past the end, the strip springs back onto it.
    frame
        .root_mut::<DesktopSurface>()
        .end_idle_swipe(ms(t, 17 * 40 + 250));
    assert!(frame.settle_for_capture(Duration::from_secs(3)));

    find_canvas(&mut frame);
    let start = scroll(&frame);
    let t = ms(t, 2000);
    assert!(
        wheel(
            &mut frame.root_mut::<DesktopSurface>(),
            MouseScrollDelta::LineDelta(0.0, 3.0),
            t
        ),
        "a plain wheel over the canvas is the strip's"
    );
    let wheeled = scroll(&frame);
    let travel = 3.0 * ui::widget::line_of_wheel();
    assert!(
        (start - wheeled - travel).abs() < 0.5,
        "three lines up pan three lines toward earlier windows: {start} -> {wheeled}"
    );
    frame
        .root_mut::<DesktopSurface>()
        .end_idle_swipe(ms(t, 1000));
    for _ in 0..4 {
        frame.relayout();
    }

    let (x, y) = find_canvas(&mut frame);
    let start = scroll(&frame);
    // The wheel left 120 px of strip to pan forward to.
    frame.drag_at((x, y), (x - 300.0, y));
    for _ in 0..4 {
        frame.relayout();
    }
    let dragged = scroll(&frame);
    assert!(
        dragged > start + 60.0,
        "dragging the canvas left pans toward later windows: {start} -> {dragged}"
    );
}

// A mouse with only a vertical wheel pans the strip over the strip's own chrome — canvas, and a
// window's tab bar, which is the desktop's and not the app's; over the app's content the wheel
// stays the app's.
#[test]
fn a_plain_wheel_over_a_windows_bar_pans_the_strip() {
    fn scroll(frame: &ui::headless::FrameHarness) -> f64 {
        frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .unwrap()
            .scroll_x()
    }
    let (surface, _handle) = desktop(5);
    let mut frame = ui::headless::FrameHarness::for_root_with(
        ui::state::State::new(),
        surface,
        (1280, 800),
        ui::headless::FrameOptions::default(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let t = ui::time::now();
    // Somewhere to pan to in both directions.
    for i in 0..10 {
        wheel(
            &mut frame.root_mut::<DesktopSurface>(),
            pixels(-120.0, 0.0),
            ms(t, 17 * i),
        );
    }
    frame
        .root_mut::<DesktopSurface>()
        .end_idle_swipe(ms(t, 17 * 10 + 250));
    assert!(frame.settle_for_capture(Duration::from_secs(2)));
    for _ in 0..4 {
        frame.relayout();
    }
    let (bar, content) = {
        let row = frame.root::<DesktopSurface>().shown_row_ref().unwrap();
        let viewport = row.viewport();
        let bounds = row
            .groups
            .iter()
            .map(WindowGroup::presentation_bounds)
            .find(|bounds| {
                viewport.contains(bounds.tab_bar.center())
                    && bounds
                        .client
                        .is_some_and(|client| viewport.contains(client.center()))
            })
            .expect("a window shows its bar and its content");
        (bounds.tab_bar.center(), bounds.client.unwrap().center())
    };

    // Over the app's own content the wheel is the app's: the strip stays.
    frame.send(&ui::synthetic_input::cursor_moved(content.x, content.y));
    let start = scroll(&frame);
    frame.send(&ui::synthetic_input::mouse_wheel(0.0, 3.0));
    assert!(
        (scroll(&frame) - start).abs() < 0.5,
        "the content under the wheel scrolls, not the strip"
    );

    // Over the bar, which is the strip's own: a plain wheel pans it.
    frame.send(&ui::synthetic_input::cursor_moved(bar.x, bar.y));
    assert!(
        frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .unwrap()
            .pointer_over_chrome(),
        "a bar is the strip's, where an app cannot scroll: {bar:?}"
    );
    assert!(
        wheel(
            &mut frame.root_mut::<DesktopSurface>(),
            MouseScrollDelta::LineDelta(0.0, 3.0),
            ms(t, 3000)
        ),
        "a plain wheel over the bar is the strip's"
    );
    let wheeled = scroll(&frame);
    let travel = 3.0 * ui::widget::line_of_wheel();
    assert!(
        (start - wheeled - travel).abs() < 0.5,
        "three lines up pan three lines toward earlier windows: {start} -> {wheeled}"
    );
}

#[test]
fn what_an_app_leaves_of_a_sideways_wheel_pans_the_strip_it_stands_in() {
    fn scroll(frame: &ui::headless::FrameHarness) -> f64 {
        frame
            .root::<DesktopSurface>()
            .shown_row_ref()
            .unwrap()
            .scroll_x()
    }
    let (surface, handle) = desktop(5);
    let mut frame = ui::headless::FrameHarness::for_root_with(
        ui::state::State::new(),
        surface,
        (1280, 800),
        ui::headless::FrameOptions::default(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let t = ui::time::now();
    // Somewhere to pan to in both directions.
    for i in 0..10 {
        wheel(
            &mut frame.root_mut::<DesktopSurface>(),
            pixels(-120.0, 0.0),
            ms(t, 17 * i),
        );
    }
    frame
        .root_mut::<DesktopSurface>()
        .end_idle_swipe(ms(t, 17 * 10 + 250));
    assert!(frame.settle_for_capture(Duration::from_secs(2)));
    for _ in 0..4 {
        frame.relayout();
    }
    let shown = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .unwrap()
        .groups[0]
        .active_tab_id();
    let start = scroll(&frame);

    // The app of a window on the shown strip did not scroll with its sideways wheel: the strip
    // moves by what it left, toward later windows.
    handle.sideways_wheel(shown, 120.0);
    frame.relayout();
    assert!(frame.settle_for_capture(Duration::from_secs(2)));
    let panned = scroll(&frame);
    assert!(
        (panned - start - 120.0).abs() < 0.5,
        "the strip pans by the travel the app left: {start} -> {panned}"
    );

    // The rest of the gesture pans it on, event by event.
    handle.sideways_wheel(shown, 60.0);
    frame.relayout();
    assert!(frame.settle_for_capture(Duration::from_secs(2)));
    let panned_on = scroll(&frame);
    assert!(
        (panned_on - panned - 60.0).abs() < 0.5,
        "every event the app leaves pans the strip: {panned} -> {panned_on}"
    );

    // An app's window on another strip is not the one the person looks at.
    handle.sideways_wheel(shown + 1_000, 120.0);
    frame.relayout();
    assert!(frame.settle_for_capture(Duration::from_secs(2)));
    assert!(
        (scroll(&frame) - panned_on).abs() < 0.5,
        "a window off this strip moves nothing here"
    );
}

#[test]
fn host_actions_no_longer_offered_are_reported_for_withdrawal() {
    let (_surface, handle) = desktop(1);
    let action = |id: &str| HostAction {
        id: id.into(),
        title: id.into(),
        subtitle: None,
        icon: None,
        destructive: false,
        unasked: false,
        source: None,
    };
    assert!(
        handle
            .set_host_actions(vec![action("keep"), action("pair")])
            .is_empty()
    );
    assert_eq!(
        handle.set_host_actions(vec![action("pair"), action("recover")]),
        ["keep"]
    );
}

fn workspace_window(group: u64, tab: TabId, title: &str) -> tilo_desktop_model::WindowGroup {
    tilo_desktop_model::WindowGroup {
        id: group,
        tabs: vec![Tab {
            id: tab,
            title: title.into(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: format!("so.tilo.{}", title.to_lowercase()),
                label: title.into(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// A desktop of one window with someone else's Notes, just arrived from the workspace, three
/// windows further along the strip than the view reaches.
fn notes_opened_out_of_sight() -> (ui::headless::FrameHarness, TabId) {
    crate::i18n::ensure();
    let (width, height) = (1440_u32, 900_u32);
    let (surface, handle) = desktop(1);
    handle.set_viewport_width(f64::from(width));
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (width, height),
        Theme::dark(),
    );
    frame.relayout();
    let mut incoming = handle.shared_tree();
    let notes = 9_000_003;
    let windows = [
        (9_000_100, 9_000_001, "Sheets"),
        (9_000_200, 9_000_002, "Chat"),
        (9_000_300, notes, "Notes"),
    ];
    for (group, tab, title) in windows {
        incoming.workspaces[0].strips[0]
            .window_groups
            .push(workspace_window(group, tab, title));
    }
    handle.replace_shared_tree(incoming, &[]);
    let kim = ui::widget::TabHolder {
        name: "Kim".into(),
        rgb: ui::presence::user_color("kim"),
    };
    let holders = windows
        .into_iter()
        .map(|(_, tab, _)| (tab, kim.clone()))
        .collect();
    handle.set_holders(holders);
    for _ in 0..4 {
        frame.relayout();
    }
    // The window that had the strip to itself narrows to give the new ones workspace; its motion
    // runs on presented frames.
    play_frames(&mut frame, 40);
    (frame, notes)
}

thread_local! {
    /// The desk's monotonic clock once a test has started stepping frames.
    static CLOCK: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Run `step` on the frame clock moved on by `after` : these tests' frames last exactly as long as
/// a 60 Hz display's, however fast or loaded the machine running them is.
fn on_clock<T>(after: Duration, step: impl FnOnce() -> T) -> T {
    let at = CLOCK
        .get()
        .unwrap_or_else(ui::time::now)
        .checked_add(after)
        .unwrap();
    CLOCK.set(Some(at));
    ui::time::testing::with_monotonic_time(at, step)
}

/// `count` frames of a 60 Hz display, each presented, on the clock motion runs by.
fn play_frames(frame: &mut ui::headless::FrameHarness, count: usize) {
    for _ in 0..count {
        on_clock(Duration::from_millis(16), || {
            assert!(frame.poll_frame().is_ok(), "{:?}", frame.poll_frame());
        });
    }
}

/// Someone else opening a window out of sight leaves the view where it was and says so at the edge
/// it lies beyond: "Kim opened Notes", clear of the windows' bars. A click on it brings the window
/// into view, and the hint goes.
#[test]
fn a_window_someone_else_opens_out_of_sight_is_pointed_at_from_the_edge() {
    let (mut frame, notes) = notes_opened_out_of_sight();
    let surface = frame.root::<DesktopSurface>();
    let row = surface.shown_row_ref().unwrap();
    assert_eq!(row.scroll_x(), 0.0, "the view stays where it was");
    let hints = surface.arrival_hints.hint_rects();
    let texts = surface.arrival_hints.hint_texts();
    let notes_hint = hints.iter().find(|(tab, _)| *tab == notes).map_or_else(
        || panic!("Notes is pointed at: {hints:?} {texts:?}"),
        |(_, rect)| *rect,
    );
    assert!(
        texts.iter().any(|text| text == "Kim opened Notes"),
        "{texts:?}"
    );
    let viewport = surface.get_rect();
    assert!(
        notes_hint.x1 <= viewport.x1 && notes_hint.center().x > viewport.center().x,
        "the hint {notes_hint:?} stands at the right edge of {viewport:?}"
    );
    for group in &row.groups {
        let bar = group.bar_rect();
        assert!(
            bar.intersect(notes_hint).is_zero_area(),
            "the hint {notes_hint:?} keeps clear of a window bar {bar:?}"
        );
    }

    let at = notes_hint.center();
    frame.click_at(at.x, at.y);
    play_frames(&mut frame, 60);
    let surface = frame.root::<DesktopSurface>();
    let row = surface.shown_row_ref().unwrap();
    let shown = row
        .groups
        .iter()
        .find(|group| group.tabs().iter().any(|tab| tab.id == notes))
        .map(ui::widget::Widget::get_rect)
        .unwrap();
    assert!(
        shown.x0 >= viewport.x0 && shown.x1 <= viewport.x1,
        "the click brought Notes {shown:?} into view {viewport:?}"
    );
    assert!(
        surface
            .arrival_hints
            .hint_rects()
            .iter()
            .all(|(tab, _)| *tab != notes),
        "a window in view is pointed at no more"
    );
}

/// Only what someone else opened in a strip already here is pointed at: not this page's own
/// windows, and not a whole strip arriving.
#[test]
fn only_windows_others_open_in_a_known_strip_are_arrivals() {
    let (_surface, handle) = desktop(1);
    let before = handle.shared_tree();
    let mut after = before.clone();
    let (own, theirs) = (9_100_001, 9_100_002);
    for (group, tab) in [(9_100_100, own), (9_100_200, theirs)] {
        after.workspaces[0].strips[0]
            .window_groups
            .push(workspace_window(group, tab, "Notes"));
    }
    assert_eq!(
        arrivals::opened_elsewhere(&before, &after, &[own]),
        vec![theirs]
    );
    let mut new_strip = after;
    new_strip.workspaces[0].strips[0].id = StripId(9_100_900);
    assert!(arrivals::opened_elsewhere(&before, &new_strip, &[]).is_empty());
}

/// A page opening its workspace's app before the workspace's desktop arrived finds the workspace's
/// window of it once it has, instead of adding one of its own for everyone: a visitor's arrival
/// never puts a window in the strip.
#[test]
fn opening_an_app_finds_the_window_a_desktop_arriving_meanwhile_brings() {
    let (surface, handle) = desktop(2);
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    frame.relayout();
    let workspace = handle.read(Clone::clone);
    let mut before = workspace.clone();
    before.workspaces[0].strips[0].window_groups.truncate(1);
    handle.replace_tree(before);
    frame.relayout();
    assert_eq!(handle.snapshot("g", vec![]).entries.len(), 1);
    let open = ChromeEffect::ShowApp {
        app: "so.tilo.app1".into(),
    };
    handle.dispatch(open).unwrap();
    // The workspace's desktop, with its window of the app, lands first.
    handle.replace_tree(workspace);
    for _ in 0..3 {
        frame.relayout();
    }
    let snapshot = handle.snapshot("g", vec![]);
    assert_eq!(snapshot.entries.len(), 2, "no second window of the app");
    let active = snapshot.active.expect("a window is focused");
    let focused = snapshot
        .entries
        .iter()
        .find(|entry| entry.route == active)
        .unwrap();
    assert_eq!(focused.app.as_deref(), Some("so.tilo.app1"));
}

/// A person names a window: the tab shows their name in place of the window's title, keeps it when
/// the window renames itself, and gives the window's title back when they clear it.
#[test]
fn a_window_takes_the_name_the_person_gives_it_and_gives_it_back() {
    fn rename(
        frame: &mut ui::headless::FrameHarness,
        handle: &DesktopHandle,
        route: &Route,
        name: &str,
    ) {
        handle
            .dispatch(ChromeEffect::Rename {
                tab: None,
                group: Some(route.group.clone()),
                strip: None,
                workspace: None,
                name: name.into(),
            })
            .unwrap();
        for _ in 0..4 {
            frame.relayout();
        }
    }
    let (surface, handle) = desktop(2);
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1280, 800),
        Theme::dark(),
    );
    frame.relayout();
    let route = handle.snapshot("g", vec![]).entries[1].route.clone();
    let label = |handle: &DesktopHandle| {
        handle
            .snapshot("g", vec![])
            .entries
            .iter()
            .find(|entry| entry.route == route)
            .map(|entry| entry.label.clone())
            .unwrap()
    };
    assert_eq!(label(&handle), "App1");
    rename(&mut frame, &handle, &route, "Quarterly plan");
    assert_eq!(label(&handle), "Quarterly plan");
    // The tab the person sees carries the name too.
    let shown: Vec<String> = frame
        .root::<DesktopSurface>()
        .rows
        .values()
        .flat_map(|row| row.groups.iter())
        .flat_map(|group| group.tabs().iter().map(|tab| tab.title.clone()))
        .collect();
    assert!(
        shown.iter().any(|title| title == "Quarterly plan"),
        "{shown:?}"
    );
    // The window's own title still arrives behind the name, and shows once the name is cleared.
    handle.read(|tree| {
        assert!(find_tab(tree, route.ids().unwrap().strip, route.tab.parse().unwrap()).is_some());
    });
    rename(&mut frame, &handle, &route, "  ");
    assert_eq!(label(&handle), "App1");
    // A name longer than a tab could ever show is refused, and nothing moves.
    let refusal = handle
        .dispatch(ChromeEffect::Rename {
            tab: None,
            group: Some(route.group),
            strip: None,
            workspace: None,
            name: "x".repeat(navigation::MAX_NAME_CHARS + 1),
        })
        .unwrap_err();
    assert!(refusal.contains("too long"), "{refusal}");
}

/// A tile source that counts the windows it builds content for, so a move that carries a window's
/// live content over is told from one that builds the window again.
struct Counted {
    built: std::rc::Rc<std::cell::Cell<usize>>,
}

impl TileSource for Counted {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        self.built.set(self.built.get().saturating_add(1));
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

fn counted_desktop(
    windows: usize,
) -> (
    DesktopSurface,
    DesktopHandle,
    std::rc::Rc<std::cell::Cell<usize>>,
) {
    let built = std::rc::Rc::new(std::cell::Cell::new(0));
    let tiles = Box::new(Counted {
        built: std::rc::Rc::clone(&built),
    });
    let (surface, handle) = desk_with(windows, tiles);
    (surface, handle, built)
}

/// Drag the window `tab` of the shown strip out to `release` : the bar reports the release, as a
/// person letting go of a dragged tab does.
fn drag_out(frame: &mut ui::headless::FrameHarness, tab: TabId, below: bool) -> bool {
    let staged = frame.widget_mut::<GroupRow>().is_some_and(|row| {
        let band = row.get_rect();
        let x = band.center().x;
        let y = if below {
            band.y1 + 60.0
        } else {
            band.y0 - 60.0
        };
        let Some(group) = row.groups.first_mut() else {
            return false;
        };
        group.bar_mut_for_tests().pending_detach = Some((tab, Point::new(x, y)));
        true
    });
    // The release the bar reports, and the pass that sweeps it into the row's own detach.
    frame.send_host_event(&ui::synthetic_input::cursor_moved(900.0, 700.0));
    for _ in 0..4 {
        frame.relayout();
    }
    staged
}

/// A window dragged out of its strip lands in the strip below it and the desk follows it there,
/// live content and all.
#[test]
fn a_window_dragged_out_of_its_strip_moves_to_the_strip_below_it() {
    use tilo_desktop_model::commands::DesktopCommand;
    let (surface, handle, built) = counted_desktop(2);
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    for _ in 0..4 {
        frame.relayout();
    }
    let second = handle
        .apply_command(&DesktopCommand::CreateStrip {
            name: "Second".into(),
        })
        .unwrap()
        .expect("a second strip");
    let (workspace, first) = handle.read(|tree| {
        let workspace = tree.active_workspace().unwrap();
        let first = workspace
            .strips
            .iter()
            .map(|strip| strip.id)
            .find(|&strip| strip != second)
            .unwrap();
        (workspace.slot, first)
    });
    handle
        .dispatch(ChromeEffect::ShowStrip {
            workspace: workspace.0.to_string(),
            strip: first.0.to_string(),
        })
        .unwrap();
    for _ in 0..4 {
        frame.relayout();
    }
    assert_eq!(
        handle.snapshot("g", vec![]).active_strip.as_deref(),
        Some(first.0.to_string().as_str())
    );
    let built_before = built.get();
    let (group, tab) = handle.read(|tree| {
        let strip = tree.workspaces[0]
            .strips
            .iter()
            .find(|strip| strip.id == first)
            .unwrap();
        (strip.window_groups[0].id, strip.window_groups[0].tabs[0].id)
    });
    assert!(drag_out(&mut frame, tab, true), "the strip is on screen");
    let (home, shown, focused) = handle.read(|tree| {
        let home = tree
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .find(|strip| {
                strip
                    .window_groups
                    .iter()
                    .any(|group| group.tabs.iter().any(|t| t.id == tab))
            })
            .map(|strip| strip.id);
        (
            home,
            tree.active_strip.get(&workspace).copied(),
            tree.active_group_for(second),
        )
    });
    assert_eq!(home, Some(second), "the window moved into the strip below");
    assert_eq!(shown, Some(second), "and the desk shows it there");
    assert!(focused.is_some(), "the moved window is focused there");
    let strip = handle.read(|tree| {
        tree.workspaces[0]
            .strips
            .iter()
            .find(|strip| strip.id == first)
            .unwrap()
            .clone()
    });
    assert!(
        strip.window_groups.iter().all(|g| g.id != group),
        "the window it left does not stay empty behind it"
    );
    assert_eq!(
        built.get(),
        built_before,
        "the window keeps the application it was running"
    );
    assert_eq!(
        handle.snapshot("g", vec![]).active_strip.as_deref(),
        Some(second.0.to_string().as_str())
    );
}

/// Let go where no strip lies, a dragged window goes back where it was,
/// and nothing about the desk changes.
#[test]
fn a_window_dragged_off_the_only_strip_stays_where_it_was() {
    let (surface, handle) = desktop(2);
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    for _ in 0..4 {
        frame.relayout();
    }
    let before = handle.read(Clone::clone);
    let tab = before.workspaces[0].strips[0].window_groups[0].tabs[0].id;
    assert!(drag_out(&mut frame, tab, true), "the strip is on screen");
    let after = handle.read(Clone::clone);
    assert_eq!(after.workspaces[0].strips[0].window_groups.len(), 2);
    assert_eq!(
        after.workspaces[0].strips[0].window_groups[0].tabs[0].id, tab,
        "the window is back in its own window"
    );
    assert_eq!(after.active_strip, before.active_strip);
}

/// Drag `tab` sideways along its own bar and let go over the gap before the tab at `at` (the end of
/// the bar when `at` is the tab count): the pointer presses the pill, moves past it and releases,
/// as a hand does.
fn drag_along_bar(
    frame: &mut ui::headless::FrameHarness,
    group: GroupId,
    tab: TabId,
    at: usize,
) -> bool {
    let points = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .and_then(|row| {
            let group = row.groups.iter().find(|shown| shown.id == group)?;
            let rects = group.tab_rects();
            let from = group.tabs().iter().position(|t| t.id == tab)?;
            let pill = rects.get(from)?;
            let y = pill.center().y;
            // Let go between the midpoints of the pills either side of the gap, which is the gap
            // the bar resolves and the drop line shows.
            let x = rects.get(at).map_or(rects.last()?.x1, |next| next.x0);
            Some((Point::new(pill.center().x, y), Point::new(x, y)))
        });
    let Some((start, end)) = points else {
        return false;
    };
    frame.send(&ui::synthetic_input::cursor_moved(start.x, start.y));
    frame.send(&ui::synthetic_input::mouse_press());
    frame.send(&ui::synthetic_input::cursor_moved(end.x, end.y));
    frame.send(&ui::synthetic_input::mouse_release());
    for _ in 0..4 {
        frame.relayout();
    }
    true
}

/// The order a window shows its tabs in, as the bar itself has them.
fn bar_order(frame: &ui::headless::FrameHarness, group: GroupId) -> Vec<TabId> {
    frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .and_then(|row| row.groups.iter().find(|g| g.id == group))
        .map(|group| group.tabs().iter().map(|tab| tab.id).collect())
        .unwrap_or_default()
}

/// A tab dragged along its own bar stays where it was let go: the desk's tree holds that order, not
/// the one it had before the drag, and the window the person pressed is still the one they are on.
#[test]
fn a_tab_dragged_along_its_bar_keeps_the_order_it_lands_in() {
    let (surface, handle) = one_window_desk(3);
    let mut frame = ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        surface,
        (1280, 800),
        Theme::dark(),
    );
    for _ in 0..4 {
        frame.relayout();
    }
    let (group, tabs, shown) = handle.read(|tree| {
        let group = tree.workspaces[0].strips[0].window_groups[0].clone();
        (
            group.id,
            group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            group.active_tab,
        )
    });
    assert_eq!(shown, tabs[0], "the desk starts on the first window");
    assert_eq!(bar_order(&frame, group), tabs);

    // Right along the bar: the first window is let go past the last one.
    assert!(
        drag_along_bar(&mut frame, group, tabs[0], tabs.len()),
        "the bar is on screen"
    );
    let (landed, on) = handle.read(|tree| {
        let group = &tree.workspaces[0].strips[0].window_groups[0];
        (
            group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            group.active_tab,
        )
    });
    assert_eq!(
        landed,
        vec![tabs[1], tabs[2], tabs[0]],
        "the tree keeps the order the tab landed in"
    );
    assert_eq!(bar_order(&frame, group), landed, "the bar shows that order");
    assert_eq!(on, tabs[0], "the person stays on the window they pressed");

    // And back: the same window is dragged to the front again.
    assert!(
        drag_along_bar(&mut frame, group, tabs[0], 0),
        "the bar is still on screen"
    );
    let (landed, on) = handle.read(|tree| {
        let group = &tree.workspaces[0].strips[0].window_groups[0];
        (
            group.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>(),
            group.active_tab,
        )
    });
    assert_eq!(landed, tabs, "dragged left, the window is back in front");
    assert_eq!(bar_order(&frame, group), landed, "the bar shows that order");
    assert_eq!(on, tabs[0], "and the person is still on that window");
}

/// A window's own icon — the one its program set for that window — is on its tab, where its app's
/// artwork stands, and goes when the window lets go of it.
#[test]
fn a_windows_own_icon_is_on_its_tab() {
    struct Own;
    impl ui::icons::AppImageSource for Own {
        fn key(&self) -> &'static str {
            "window:91:1"
        }

        fn pixels(&self, _device_px: u32) -> Option<ui::peniko::ImageData> {
            Some(ui::raster::rgba8_premultiplied(
                [255u8, 0, 255, 255].repeat(16 * 16),
                16,
                16,
            ))
        }
    }

    /// How many pixels of the window's own magenta `frame` shows.
    fn magenta(frame: &ui::headless::FrameHarness) -> usize {
        frame
            .render_rgba()
            .chunks_exact(4)
            .filter(|px| matches!(px, [r, g, b, _] if *r > 200 && *g < 60 && *b > 200))
            .count()
    }

    let mut minter = IdMinter::new(11);
    let tab = Tab {
        id: minter.mint(),
        title: "Tool".to_owned(),
        tile: tilo_desktop_model::TileContent::Foreign {
            app_id: "org.example.Tool".to_owned(),
            surface_id: 91,
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let tree = seed_tree(&mut minter, "Desk", vec![tab]);
    let (surface, handle) = DesktopSurface::new(tree, 11, Box::new(Notices));
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    frame.relayout();
    assert_eq!(magenta(&frame), 0, "the window brings no artwork yet");

    assert!(handle.set_window_artwork(91, Some(ui::icons::AppImage::new(Own))));
    frame.relayout();
    assert!(
        magenta(&frame) > 0,
        "the icon the program set is on the window's tab"
    );

    assert!(handle.set_window_artwork(91, None));
    frame.relayout();
    assert_eq!(magenta(&frame), 0, "and gone when the window's icon is");
}

/// A two-finger sideways swipe as the gate's browser sends it: seven wheel
/// events, a trackpad's lead-in and tail, 96 px over the whole gesture.
const BRIEF_SWIPE: [f64; 7] = [6.0, 12.0, 18.0, 24.0, 18.0, 12.0, 6.0];

/// A swipe a hand carries across more than a window: 1680 px in seven events.
const LONG_SWIPE: [f64; 7] = [105.0, 210.0, 315.0, 420.0, 315.0, 210.0, 105.0];

/// A swipe thrown past everything the strip has: 2800 px in seven events.
const FAR_SWIPE: [f64; 7] = [175.0, 350.0, 525.0, 700.0, 525.0, 350.0, 175.0];

/// The wheel travel one event's `part` asks for when the fingers go
/// `forward` (positive toward later windows).
fn swipe_travel(forward: bool, part: f64) -> f64 {
    if forward { part } else { -part }
}

/// Swipe the shown strip by `steps`, `forward` or back, one wheel event a
/// frame, and hand back where the strip stood after each event. The app of the
/// window under the fingers leaves every event to the strip, as an app with no
/// sideways scroll of its own does, so this is the path a swipe takes over
/// such a window.
fn swipe_the_strip(
    frame: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
    windows: usize,
    steps: &[f64],
    forward: bool,
) -> Vec<f64> {
    let shown = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .unwrap()
        .groups[0]
        .active_tab_id();
    let mut offsets = Vec::new();
    for &part in steps {
        handle.sideways_wheel(shown, swipe_travel(forward, part));
        frame.relayout();
        assert!(
            frame.settle_for_capture(Duration::from_secs(1)),
            "{windows}"
        );
        offsets.push(
            frame
                .root::<DesktopSurface>()
                .shown_row_ref()
                .unwrap()
                .scroll_x(),
        );
    }
    offsets
}

/// Rest the strip after the swipe's fingers lifted: the wheel gesture is over
/// once it pauses, and what it left behind is where the strip stays.
fn rest_the_strip(frame: &mut ui::headless::FrameHarness) -> f64 {
    frame
        .root_mut::<DesktopSurface>()
        .end_idle_swipe(ms(ui::time::now(), 250));
    assert!(frame.settle_for_capture(Duration::from_secs(3)));
    for _ in 0..4 {
        frame.relayout();
    }
    frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .unwrap()
        .scroll_x()
}

/// A sideways swipe moves the strip with the fingers, a pixel of travel a
/// pixel of strip, and leaves it where they stopped: a brief swipe and a long
/// one, each way, on a strip of six windows and on one of forty.
#[test]
fn a_sideways_swipe_pans_the_strip_with_the_fingers_and_rests_where_they_stopped() {
    for windows in [6usize, 40] {
        for (what, steps) in [
            ("a brief swipe", &BRIEF_SWIPE[..]),
            ("a long swipe", &LONG_SWIPE[..]),
        ] {
            for forward in [true, false] {
                let (mut frame, handle) = laid_out(windows);
                // Somewhere the whole swipe has room: home going on, the far
                // end coming back.
                let max = frame
                    .root::<DesktopSurface>()
                    .shown_row_ref()
                    .unwrap()
                    .pan_max_scroll_x();
                let start = if forward { 0.0 } else { max };
                frame
                    .root_mut::<DesktopSurface>()
                    .shown_row()
                    .unwrap()
                    .set_scroll_x_for_test(start);
                for _ in 0..4 {
                    frame.relayout();
                }
                let travel: f64 = steps.iter().sum();
                assert!(
                    travel <= max - start + 0.5 || !forward,
                    "{windows} windows: {what} has room to pan on from {start} of {max}"
                );
                let offsets = swipe_the_strip(&mut frame, &handle, windows, steps, forward);
                // Every event moves the strip by its own travel, the fingers'
                // way: never against them, never standing still while they move.
                let mut asked = start;
                for (index, (&part, &offset)) in steps.iter().zip(offsets.iter()).enumerate() {
                    asked += swipe_travel(forward, part);
                    let want = asked.clamp(0.0, max);
                    assert!(
                        (offset - want).abs() < 0.5,
                        "{windows} windows, {what}, {}: event {} left the strip at {offset}, not {want}",
                        if forward { "forward" } else { "back" },
                        index + 1
                    );
                }
                let rest = rest_the_strip(&mut frame);
                let want = (start + if forward { travel } else { -travel }).clamp(0.0, max);
                assert!(
                    (rest - want).abs() < 0.5,
                    "{windows} windows, {what}, {}: the strip rested at {rest}, not {want}: a swipe is not snapped back",
                    if forward { "forward" } else { "back" }
                );
            }
        }
    }
}

/// A strip only a little wider than the screen, as a desk of a few wide
/// windows is: a swipe asked for more than the strip has stops at its end the
/// fingers' way and stays there, and one asked for where the strip has none
/// moves nothing at all. Neither turns the strip around.
#[test]
fn a_swipe_on_a_strip_that_barely_overflows_stops_at_its_end_and_never_turns_back() {
    // Six windows, on a screen a little narrower than they need.
    let (frame, _) = laid_out(6);
    let content = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .unwrap()
        .content_width();
    drop(frame);
    let width = u32::try_from(ui::num::i64_of_f64((content - 120.0).round())).unwrap();
    let build = |width: u32| {
        let (surface, handle) = desktop(6);
        let mut frame = ui::headless::FrameHarness::for_root_with(
            ui::state::State::new(),
            surface,
            (width, 800),
            ui::headless::FrameOptions::default(),
        );
        for _ in 0..4 {
            frame.relayout();
        }
        (frame, handle)
    };

    // Forward, and further than the strip can go.
    let (mut frame, handle) = build(width);
    let max = frame
        .root::<DesktopSurface>()
        .shown_row_ref()
        .unwrap()
        .pan_max_scroll_x();
    // The strip's own overflow, plus the glimpse of canvas its end keeps.
    assert!(
        max > 0.0 && max < 1500.0,
        "the strip barely overflows: {max} px"
    );
    let offsets = swipe_the_strip(&mut frame, &handle, 6, &FAR_SWIPE, true);
    assert!(
        offsets.windows(2).all(|pair| pair[1] >= pair[0] - 0.5),
        "a forward swipe never turns the strip back: {offsets:?}"
    );
    let rest = rest_the_strip(&mut frame);
    assert!(
        (rest - max).abs() < 0.5,
        "asked for more than it has, the strip stops at its end {max}, not {rest}"
    );

    // And back from home, where it has nothing: the strip gives against its
    // rubber band the fingers' way, never the other way, and springs back.
    let (mut frame, handle) = build(width);
    let offsets = swipe_the_strip(&mut frame, &handle, 6, &BRIEF_SWIPE, false);
    assert!(
        offsets.iter().all(|&offset| offset <= 0.5),
        "a swipe back from home only gives past its start, never the fingers' way: {offsets:?}"
    );
    let rest = rest_the_strip(&mut frame);
    assert!(
        rest.abs() < 0.5,
        "a strip at home springs back home: {rest}"
    );
}

/// The bell counts what the person has not seen across a restart: a kept notice they never looked
/// at is unread in the next session, and one they read stays read.
#[test]
fn unread_notices_stay_unread_across_a_restart() {
    let wall = ui::time::SystemTime::now();
    let before = DesktopNotifications::default();
    before.post_quiet(crate::notifications::Notification::new("Seen", ""));
    let _ = before.take_history_dirty();
    before.mark_read();
    assert!(
        before.take_history_dirty(),
        "reading the centre is kept with it"
    );
    before.post_quiet(crate::notifications::Notification::new("New", ""));
    assert_eq!(before.unread(), 1);
    let kept = before.kept(wall).unwrap();
    let after = DesktopNotifications::default();
    after.restore_kept(&kept, wall).unwrap();
    assert_eq!(after.counts(), (0, 2), "both rows are back in the centre");
    assert_eq!(after.unread(), 1, "only the one never seen is unread");
    let unread: Vec<_> = after
        .log()
        .history()
        .iter()
        .filter(|notice| after.is_unread(notice.id))
        .map(|notice| notice.title.clone())
        .collect();
    assert_eq!(unread, ["New"]);
}
