//! The desktop surface: layout, chrome and input, driven through its real model.

use super::*;
use ui::winit::dpi::PhysicalPosition;

struct Notices;

impl TileSource for Notices {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

fn desktop(windows: usize) -> (DesktopSurface, DesktopHandle) {
    let mut minter = IdMinter::new(7);
    let tabs = (0..windows)
        .map(|i| Tab {
            id: minter.mint(),
            title: format!("App{i}"),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: format!("so.tilo.app{i}"),
                label: format!("App{i}"),
                note: String::new(),
            },
            app_title: None,
        })
        .collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    DesktopSurface::new(tree, 7, Box::new(Notices))
}

fn held_by(name: &str) -> ui::widget::TabHolder {
    ui::widget::TabHolder {
        name: name.into(),
        rgb: (200, 120, 40),
    }
}

/// A window another guest opened on a shared desktop is theirs to close:
/// asked to close it, the desktop refuses at once, says why, and the
/// strip keeps every window where it was.
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
        .dispatch(
            &Command::Close {
                generation: "g".into(),
                route: route.clone(),
            },
            "g",
        )
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
        .dispatch(
            &Command::Close {
                generation: "g".into(),
                route,
            },
            "g",
        )
        .unwrap();
}

/// Workspace and strip edits go through the model's commands: a strip
/// made that way is shown, and a name the model refuses changes nothing.
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
        assert_eq!(tree.active_strip.get(&workspace.id), Some(&made));
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
            tree.active_strip.get(&workspace.id),
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

/// Hover never moves a window: the pointer over a window's bar, above
/// its top edge, on its body or in the gap beside it leaves every
/// window's frame, bar and body where they were, and focus where it
/// was (a focused window's edge is drawn in another colour, never
/// further out). Hover may recolour, never lift or grow.
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

/// A window's search control opens search, over what that window shows:
/// the control is never a dead one in this desktop.
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
    // The window bar's own search control: with nothing to search in
    // place, it opens the platform search narrowed to the window's app.
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

/// Search over one window stands over that window, as an app's own
/// palette does, not centred on the screen over another window.
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
    let palette = frame.root::<DesktopSurface>().search_palette_rect();
    assert!(
        (palette.center().x - window.center().x).abs() < 1.0
            || palette.x0 <= 8.5
            || palette.x1 >= 1400.0 - 8.5,
        "the palette {palette:?} stands over the window {window:?}"
    );
    assert!(palette.y0 > window.y0, "below the window's top");
}

/// An acknowledgement is said politely over the desktop, wakes the host
/// for its fade and end, and is no notification.
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

/// Each select a host sends applies in the frame its wake draws, in
/// order, while the strip pans between windows.
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
            .dispatch(
                &Command::Select {
                    generation: "g".into(),
                    route: route.clone(),
                },
                "g",
            )
            .unwrap();
        frame.relayout();
        assert_eq!(
            handle.snapshot("g", vec![]).active,
            Some(route),
            "select {target}"
        );
    }
}

/// A window's frame, border and rounded corners included, stands inside
/// the desktop with wallpaper around it at the strip's home and at its
/// end: past the leading edge, before the trailing edge and above the
/// bottom edge, by at least the gap between two windows. Its bar stands
/// above the frame, so the top is the bar's to keep.
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
                .dispatch(
                    &Command::Select {
                        generation: "g".into(),
                        route: entries[target].route.clone(),
                    },
                    "g",
                )
                .unwrap();
            for _ in 0..40 {
                frame.relayout();
            }
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

        let surface = frame.root_mut::<DesktopSurface>();
        surface.shown_row().unwrap().pan_to(f64::MAX);
        frame.relayout();
        let row = frame.root::<DesktopSurface>().shown_row_ref().unwrap();
        let last = row.groups.last().unwrap().get_rect();
        inside(last, viewport, &format!("{windows} windows, end, the last"));
    }
}

/// A maximized window gives up the wallpaper band under the strip: it
/// runs to the desktop's bottom edge, and the band comes back when it is
/// restored.
#[test]
fn a_maximized_window_takes_the_bottom_band_and_gives_it_back() {
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
    let bottom = |frame: &ui::headless::FrameHarness| {
        let row = frame.root::<DesktopSurface>().shown_row_ref().unwrap();
        row.groups.first().unwrap().get_rect().y1
    };
    let tiled = bottom(&frame);
    assert!(
        f64::from(height) - tiled >= STRIP_BOTTOM_GUTTER_PX - 0.5,
        "a tiled window keeps the band: it ends at {tiled}"
    );
    let route = handle.snapshot("g", vec![]).entries[0].route.clone();
    for maximized in [true, false] {
        handle
            .dispatch(
                &Command::Maximize {
                    generation: "g".into(),
                    route: route.clone(),
                    maximized,
                },
                "g",
            )
            .unwrap();
        for _ in 0..40 {
            frame.relayout();
        }
        let end = bottom(&frame);
        if maximized {
            assert!(
                (f64::from(height) - end).abs() < 0.5,
                "maximized, the window runs to the bottom edge: it ends at {end}"
            );
        } else {
            assert!(
                (end - tiled).abs() < 0.5,
                "restored, the band is back: it ends at {end}, was {tiled}"
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
        .dispatch(
            &Command::Close {
                generation: "g".into(),
                route,
            },
            "g",
        )
        .unwrap();
    // Forty frames of a 60 Hz display, on the clock the close animates by.
    let mut at = ui::time::now();
    for _ in 0..40 {
        at = at.checked_add(Duration::from_millis(16)).unwrap();
        tilo_clock::testing::with_monotonic_time(at, || {
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

/// A room layout folds into the workspace it belongs to: a window this
/// device still has open survives a layout that has not heard of it yet,
/// the workspace's own strip stays in front, and every other workspace this
/// desktop holds is untouched.
#[test]
fn a_room_layout_lands_in_its_workspace_and_leaves_the_others_alone() {
    let (mut surface, handle) = desktop(2);
    let state = AutoCommit::new();
    surface.update(&state, &ui::automerge::ROOT);
    let room = tilo_desktop_model::RoomId::parse("6ba7b810-9dad-11d1-80b4-00c04fd430c8").unwrap();
    let mut tree = handle.read(Clone::clone);
    let room_id = tilo_desktop_model::WorkspaceId::of_room(room);
    let first_strip = tree.workspaces[0].strips[0].id;
    tree.workspaces[0].id = room_id;
    tree.active = room_id;
    tree.active_strip.insert(room_id, first_strip);
    // A second workspace of another room, and one more window here that the
    // room's layout has not heard of.
    let mut other = seed_tree(&mut IdMinter::new(9), "Elsewhere", Vec::new());
    other.workspaces[0].id = tilo_desktop_model::WorkspaceId(42);
    tree.workspaces.extend(other.workspaces);
    let mine = tree.workspaces[0].strips[0].window_groups[1].tabs[0].id;
    handle.replace_tree(tree);
    let strip = handle.read(|tree| tree.workspaces[0].strips[0].id);

    // The room's layout knows only the first window of that strip.
    let mut incoming = handle.shared_tree();
    incoming
        .workspaces
        .retain(|workspace| workspace.id == room_id);
    incoming.workspaces[0].strips[0].window_groups.truncate(1);
    incoming.workspaces[0].strips[0].name = "Room".into();
    handle.apply_room_layout(incoming, &[]);

    handle.read(|tree| {
        assert_eq!(tree.workspaces.len(), 2, "the other room's desk stays");
        assert!(
            tree.workspaces.iter().any(|w| w.id.0 == 42),
            "the other room's workspace is untouched"
        );
        let held = tree.workspaces.iter().find(|w| w.id == room_id).unwrap();
        assert_eq!(held.strips[0].name, "Room", "the room's strip is done");
        let tabs: Vec<_> = held.strips[0]
            .window_groups
            .iter()
            .flat_map(|group| &group.tabs)
            .map(|tab| tab.id)
            .collect();
        assert!(
            tabs.contains(&mine),
            "a window this device still has open survives"
        );
        assert_eq!(
            tree.active_strip.get(&room_id).copied(),
            Some(strip),
            "the strip in front stays in front"
        );
    });
    surface.update(&state, &ui::automerge::ROOT);
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
        fn launch(&mut self, _: &Workspace, id: TabId, app: &str) -> Option<Tab> {
            (app == "so.tilo.clocks").then(|| Tab {
                id,
                title: "Clocks".into(),
                tile: tilo_desktop_model::TileContent::LocalApp {
                    app_id: app.into(),
                    label: "Clocks".into(),
                    note: String::new(),
                },
                app_title: None,
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
    notifications.restore_history(vec![notice]);
    surface.set_notifications(notifications.clone());
    surface.set_notification_center(true);
    surface
        .notices
        .as_mut()
        .unwrap()
        .apply(crate::notifications::CenterOutcome::OpenApp {
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
        .dispatch(
            &Command::NotificationCenter {
                generation: "g".into(),
                open: true,
            },
            "g",
        )
        .unwrap();
    let state = AutoCommit::new();
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
        .dispatch(
            &Command::NotificationCenter {
                generation: "g".into(),
                open: false,
            },
            "g",
        )
        .unwrap();
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

/// A sideways wheel turned `rightward` lines (negative: leftward), as
/// winit reports it: negated.
fn lines(rightward: f32) -> MouseScrollDelta {
    MouseScrollDelta::LineDelta(-rightward, 0.0)
}

/// One notch of a sideways wheel spun `rightward` as Firefox reports
/// it: six lines (negative: leftward).
fn notch(rightward: f32) -> MouseScrollDelta {
    lines(6.0 * rightward)
}

#[test]
fn a_wheel_spin_steps_a_strip_that_cannot_pan_by_its_travel() {
    // Unlaid, the strip cannot pan, as when its windows all fit.
    let (mut surface, handle) = desktop(5);
    let t = ui::time::now();
    assert!(surface.route_swipe(notch(1.0), 1.0, t));
    assert_eq!(
        focused(&mut surface, &handle),
        1,
        "a spin's first notch steps at once"
    );
    let gaps = [17, 50, 0, 33, 17, 17, 50];
    let mut at = 0;
    for gap in gaps {
        at += gap;
        assert!(surface.route_swipe(notch(1.0), 1.0, ms(t, at)));
    }
    // Eight notches of six lines travel 1920 px: the first notch's step
    // and one per STEP_TRAVEL_PX after it.
    assert_eq!(
        focused(&mut surface, &handle),
        3,
        "a step per window's width of travel, not per notch"
    );
    for i in 1..=12 {
        assert!(
            surface.route_swipe(notch(1.0), 1.0, ms(t, at + 17 * i)),
            "travel past the last window stays the strip's"
        );
    }
    assert_eq!(focused(&mut surface, &handle), 4);

    let t = ms(t, 2000);
    assert!(
        surface.route_swipe(notch(-1.0), 1.0, t),
        "after a pause a notch back steps back"
    );
    assert_eq!(focused(&mut surface, &handle), 3);
    assert!(
        surface.route_swipe(pixels(240.0, 0.0), 1.0, ms(t, 1000)),
        "a trackpad's travel steps the same"
    );
    assert_eq!(focused(&mut surface, &handle), 2);
    for i in 1..=3 {
        surface.route_swipe(pixels(240.0, 0.0), 1.0, ms(t, 1000 + 17 * i));
    }
    assert_eq!(focused(&mut surface, &handle), 1);
}

#[test]
fn a_sideways_spin_pans_an_overflowing_strip_and_focus_follows_it() {
    for (kind, delta) in [("wheel", notch(1.0)), ("trackpad", pixels(-240.0, 0.0))] {
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
        let surface = frame.root_mut::<DesktopSurface>();
        assert!(
            surface.shown_row_ref().unwrap().pan_max_scroll_x() > 2000.0,
            "{kind}: five windows overflow the strip"
        );
        let t = ui::time::now();
        for i in 0..6 {
            assert!(surface.route_swipe(delta, 1.0, ms(t, 17 * i)), "{kind}");
        }
        let row = surface.shown_row_ref().unwrap();
        assert!(
            (row.scroll_x() - start - 1440.0).abs() < 0.5,
            "{kind}: six notches pan six notches' travel, from {start} to {}",
            row.scroll_x()
        );
        let looked_at = panned_to(row, None).unwrap();
        assert_eq!(
            handle.read(|tree| navigation::active_route(tree).unwrap().group),
            handle.read(|tree| tree.workspaces[0].strips[0].window_groups[0].id),
            "{kind}: focus stays put while the strip moves"
        );

        // Frames lay the strip out while the wheel spins.
        frame.relayout();
        let surface = frame.root_mut::<DesktopSurface>();
        surface.end_idle_swipe(ms(t, 17 * 5 + 250));
        for _ in 0..4 {
            frame.relayout();
        }
        let surface = frame.root_mut::<DesktopSurface>();
        let shown = focused(surface, &handle);
        assert_eq!(
            handle.read(|tree| tree.workspaces[0].strips[0].window_groups[shown].id),
            looked_at,
            "{kind}: the pan's end focuses the window panned to"
        );
        assert!(
            shown >= 1,
            "{kind}: focus followed the strip to window {shown}"
        );
        assert!(
            (surface.shown_row_ref().unwrap().scroll_x() - start - 1440.0).abs() < 0.5,
            "{kind}: and the strip stays where the pan left it"
        );
    }
}

#[test]
fn a_swipe_with_nowhere_to_go_and_vertical_scrolling_belong_to_content() {
    let (mut surface, handle) = desktop(1);
    let t = ui::time::now();
    for i in 0..10 {
        assert!(
            !surface.route_swipe(lines(1.0), 1.0, ms(t, 16 * i)),
            "a lone window's app scrolls sideways"
        );
    }
    assert_eq!(focused(&mut surface, &handle), 0);
    let t = ms(t, 1000);
    for i in 0..10 {
        assert!(
            !surface.route_swipe(lines(-1.0), 1.0, ms(t, 16 * i)),
            "and so does the first window's, backwards"
        );
    }

    let (mut surface, handle) = desktop(3);
    let t = ui::time::now();
    for (i, delta) in [
        pixels(30.0, 60.0),
        pixels(0.0, -200.0),
        MouseScrollDelta::LineDelta(0.4, 0.0),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            !surface.route_swipe(delta, 1.0, ms(t, 300 * i as u64)),
            "{delta:?}"
        );
    }
    assert_eq!(focused(&mut surface, &handle), 0);
    assert!(
        surface.route_swipe(lines(1.0), 1.0, ms(t, 1000)),
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
        let surface = frame.root_mut::<DesktopSurface>();
        let home = surface.shown_row_ref().unwrap().scroll_x();
        let t = ui::time::now();
        for i in 0..10 {
            let at = ms(t, 16 * i);
            if i == 0 {
                assert!(
                    !surface.route_swipe(delta, 1.0, at),
                    "{kind}: the lone window's content has the swipe first"
                );
                // It did not scroll by it, so the strip takes it.
                surface.pull_unhandled_swipe(delta, 1.0, at);
            } else {
                assert!(
                    surface.route_swipe(delta, 1.0, at),
                    "{kind}: and the rest of the gesture is the strip's"
                );
            }
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
        assert_eq!(focused(surface, &handle), 0);

        surface.end_idle_swipe(ms(t, 16 * 9 + 250));
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

    // Windows and the seams between them cover the strip; canvas shows
    // past the last window once the strip is panned to its end.
    let t = ui::time::now();
    for i in 0..40 {
        frame
            .root_mut::<DesktopSurface>()
            .route_swipe(pixels(-240.0, 0.0), 1.0, ms(t, 17 * i));
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
        frame.root_mut::<DesktopSurface>().route_swipe(
            MouseScrollDelta::LineDelta(0.0, 3.0),
            1.0,
            t
        ),
        "a plain wheel over the canvas is the strip's"
    );
    let wheeled = scroll(&frame);
    assert!(
        (start - wheeled - 120.0).abs() < 0.5,
        "three lines up pan 120 px toward earlier windows: {start} -> {wheeled}"
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

#[test]
fn host_actions_no_longer_offered_are_reported_for_withdrawal() {
    let (_surface, handle) = desktop(1);
    let action = |id: &str| HostAction {
        id: id.into(),
        title: id.into(),
        subtitle: None,
        source: None,
        label: None,
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

fn room_window(group: u64, tab: TabId, title: &str) -> tilo_desktop_model::WindowGroup {
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
        }],
        active_tab: tab,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// A desktop of one window with someone else's Notes, just arrived from
/// the room, three windows further along the strip than the view reaches.
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
            .push(room_window(group, tab, title));
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
    // The window that had the strip to itself narrows to give the new ones
    // room; its motion runs on presented frames.
    play_frames(&mut frame, 40);
    (frame, notes)
}

/// `count` frames of a 60 Hz display, each presented, on the clock motion
/// runs by.
fn play_frames(frame: &mut ui::headless::FrameHarness, count: usize) {
    let mut at = ui::time::now();
    for _ in 0..count {
        at = at.checked_add(Duration::from_millis(16)).unwrap();
        tilo_clock::testing::with_monotonic_time(at, || {
            assert!(frame.poll_frame().is_ok(), "{:?}", frame.poll_frame());
        });
    }
}

/// Someone else opening a window out of sight leaves the view where it was
/// and says so at the edge it lies beyond: "Kim opened Notes", clear of the
/// windows' bars. A click on it brings the window into view, and the hint
/// goes.
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
    for _ in 0..40 {
        frame.relayout();
    }
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

/// Only what someone else opened in a strip already here is pointed at:
/// not this page's own windows, and not a whole strip arriving.
#[test]
fn only_windows_others_open_in_a_known_strip_are_arrivals() {
    let (_surface, handle) = desktop(1);
    let before = handle.shared_tree();
    let mut after = before.clone();
    let (own, theirs) = (9_100_001, 9_100_002);
    for (group, tab) in [(9_100_100, own), (9_100_200, theirs)] {
        after.workspaces[0].strips[0]
            .window_groups
            .push(room_window(group, tab, "Notes"));
    }
    assert_eq!(
        arrivals::opened_elsewhere(&before, &after, &[own]),
        vec![theirs]
    );
    let mut new_strip = after;
    new_strip.workspaces[0].strips[0].id = StripId(9_100_900);
    assert!(arrivals::opened_elsewhere(&before, &new_strip, &[]).is_empty());
}

/// A page opening its room's app before the room's desktop arrived finds
/// the room's window of it once it has, instead of adding one of its own
/// for everyone: a visitor's arrival never puts a window in the strip.
#[test]
fn opening_an_app_finds_the_window_a_desktop_arriving_meanwhile_brings() {
    let (surface, handle) = desktop(2);
    let mut frame =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), (1280, 800));
    frame.relayout();
    let room = handle.read(Clone::clone);
    let mut before = room.clone();
    before.workspaces[0].strips[0].window_groups.truncate(1);
    handle.replace_tree(before);
    frame.relayout();
    assert_eq!(handle.snapshot("g", vec![]).entries.len(), 1);
    let open: Command = serde_json::from_value(
        serde_json::json!({"kind":"open","generation":"g","app":"so.tilo.app1"}),
    )
    .unwrap();
    handle.dispatch(&open, "g").unwrap();
    // The room's desktop, with its window of the app, lands first.
    handle.replace_tree(room);
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
