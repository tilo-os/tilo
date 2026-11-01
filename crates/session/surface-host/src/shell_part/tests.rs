#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "test fixtures"
)]

use super::*;

fn reading() -> DeskReading {
    DeskReading {
        person: "Pat".to_owned(),
        clock: "4:28 PM".to_owned(),
        workspace: "Desk".to_owned(),
        strip: "Main".to_owned(),
        workspaces: vec![
            WorkspaceReading {
                key: "1".to_owned(),
                name: "Desk".to_owned(),
                current: true,
            },
            WorkspaceReading {
                key: "2".to_owned(),
                name: "Studio".to_owned(),
                current: false,
            },
        ],
        ..DeskReading::default()
    }
}

use crate::shell_part::tests_support::part_with;

fn part() -> (ShellPart, Rc<SessionLink>) {
    part_with(reading())
}

fn click(part: &mut ShellPart, name: &str) {
    let (x, y, w, h) = part
        .control_bounds(name)
        .unwrap_or_else(|| panic!("the bar shows {name}"));
    part.pointer_motion(x + w / 2.0, y + h / 2.0);
    part.prepare_frame();
    part.pointer_button(0x110, true);
    part.prepare_frame();
    part.pointer_button(0x110, false);
    for _ in 0..6 {
        part.prepare_frame();
    }
}

/// The shell the session ships is every file of the package: a file added to `apps/tl/shell`
/// and not listed here would compile in `tilolang check` and fail only in the session.
#[test]
fn the_shell_ships_every_file_of_its_package() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
    // The files the package's compiler reads, in the order it reads them.
    let on_disk: Vec<String> = tilolang_tilo::tilolang::source::source_paths(&dir)
        .expect("the shell's package")
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    let shipped: Vec<String> = SHELL_FILES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    assert_eq!(
        shipped, on_disk,
        "SHELL_FILES lists the package's files in path order"
    );
}

/// The shell reaches the session only by calling its actions (`session.focus(window(w))`): it
/// compiles with no outside effect at all, so no press of it is a string the door reads.
#[test]
fn the_shell_compiles_with_no_outside_effect() {
    compile_shell().expect("the shell compiles");
    for (name, text) in SHELL_FILES {
        assert!(
            !text.contains("outside("),
            "{name} hands the session an outside effect; call the session's action instead"
        );
    }
}

/// No app names one of the session's actions as a string: each calls it on the session's
/// package (`use so.tilo.session as session`), which the checker holds to what the door takes.
#[test]
fn no_app_hands_the_session_a_string() {
    let apps = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
    let mut pending = vec![apps];
    let mut found = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("apps/ reads") {
            let path = entry.expect("an entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "tl")
                && std::fs::read_to_string(&path)
                    .expect("a .tl file reads")
                    .contains("outside(\"session.")
            {
                found.push(path.display().to_string());
            }
        }
    }
    assert!(found.is_empty(), "outside(\"session.…\") in {found:?}");
}

/// The centre up, with its search field, and nothing changing: the part draws no frame.
/// The field reports itself dirty every frame, as a leaf does; that is no reason to draw.
#[test]
fn with_the_centre_up_and_nothing_changing_the_part_draws_nothing() {
    let _settled = ui::motion::testing::settled();
    let (mut part, _link) = part_with(DeskReading {
        here: "centre".to_owned(),
        notices: vec![NoticeReading {
            key: "1".to_owned(),
            title: "Backup finished".to_owned(),
            body: "12 GB safe".to_owned(),
            origin_app: "Settings".to_owned(),
            kind: "info".to_owned(),
            read: true,
            group: "Settings".to_owned(),
            ..NoticeReading::default()
        }],
        ..reading()
    });
    for _ in 0..8 {
        part.prepare_frame();
    }
    assert!(
        part.control_bounds("Backup finished").is_some(),
        "the centre shows the notice"
    );
    let before = part.work();
    for _ in 0..20 {
        part.prepare_frame();
    }
    let after = part.work();
    assert_eq!(
        (after.turns - before.turns, after.drawn - before.drawn),
        (0, 0),
        "idle, the centre runs and draws nothing"
    );
}

/// The centre's presses call the desk's own actions (`session.dismiss_notice(...)`): Clear all
/// and a notice's Dismiss each reach the session as that action, at that press rather than at
/// the next input.
#[test]
fn a_press_in_the_centre_hands_the_desk_its_notice_action() {
    let _settled = ui::motion::testing::settled();
    let (mut part, link) = part_with(DeskReading {
        here: "centre".to_owned(),
        notices: vec![NoticeReading {
            key: "1".to_owned(),
            title: "Backup finished".to_owned(),
            body: "12 GB safe".to_owned(),
            origin_app: "Settings".to_owned(),
            kind: "info".to_owned(),
            read: true,
            group: "Settings".to_owned(),
            ..NoticeReading::default()
        }],
        ..reading()
    });
    for _ in 0..8 {
        part.prepare_frame();
    }
    // Each press is answered, as the session's turn answers it, before the next.
    let mut served = Vec::new();
    for name in ["Dismiss", "Clear all"] {
        click(&mut part, name);
        link.serve(|ask| {
            served.push(ask.clone());
            Ok(())
        });
    }
    assert_eq!(
        served,
        vec![
            Ask {
                resource: DESK_RESOURCE.to_owned(),
                action: "dismiss-notice".to_owned(),
                arguments: BTreeMap::from([("notice".to_owned(), "1".to_owned())]),
            },
            Ask {
                resource: DESK_RESOURCE.to_owned(),
                action: "clear-notices".to_owned(),
                arguments: BTreeMap::new(),
            },
        ],
        "refusals {:?}",
        part.refusals()
    );
}

#[test]
fn a_press_on_the_bar_hands_its_effect_to_the_session() {
    let (mut part, link) = part();
    click(&mut part, "Notifications");
    let outbox = part
        .instance
        .host()
        .map(|host| host.store().outbox_pending_count());
    assert!(
        link.has_asks(),
        "the press reached the session: outbox {outbox:?}, {:?}, refusals {:?}",
        part.work(),
        part.refusals()
    );
    assert_eq!(
        link.asked.borrow().front().map(|ask| ask.action.clone()),
        Some("toggle-quick-settings".to_owned())
    );
}

/// What one discrete change costs the bar: printed per kind of change, for the record
/// of its frame budget; each must land in one turn.
#[test]
fn each_change_is_one_turn() {
    type Change = (&'static str, fn(&mut DeskReading));
    let (mut part, link) = part();
    for _ in 0..4 {
        part.prepare_frame();
    }
    let mut now = reading();
    let changes: [Change; 3] = [
        ("minute", |r| r.clock = "4:29 PM".to_owned()),
        ("notification", |r| r.unread = 1),
        ("workspace switch", |r| {
            r.workspace = "Studio".to_owned();
            r.workspaces[0].current = false;
            r.workspaces[1].current = true;
        }),
    ];
    for (name, change) in changes {
        change(&mut now);
        link.publish(now.clone());
        let before = part.work();
        part.prepare_frame();
        part.prepare_frame();
        let after = part.work();
        let ms = |a: std::time::Duration, b: std::time::Duration| {
            a.saturating_sub(b).as_secs_f64() * 1000.0
        };
        println!(
            "SHELL-CHANGE {name}: {} turns, {:.3} ms (update {:.3}, commit {:.3} of which sync {:.3}, receive {:.3}, layout {:.3}), {} drawn {:.3} ms",
            after.turns - before.turns,
            ms(after.turning, before.turning),
            ms(after.updating, before.updating),
            ms(after.committing, before.committing),
            ms(after.syncing, before.syncing),
            ms(after.receiving, before.receiving),
            ms(after.laying_out, before.laying_out),
            after.drawn - before.drawn,
            ms(after.drawing, before.drawing),
        );
        assert_eq!(after.turns - before.turns, 1, "{name} is one turn");
    }
}

#[test]
fn the_part_covers_its_band_at_rest_and_the_outputs_while_its_menu_is_up() {
    let (mut part, _link) = part();
    assert_eq!(
        part.widgets.placement().size,
        (1280, 32),
        "the band at rest"
    );
    assert!(
        !part.accepts_pointer(900.0, 400.0),
        "the desk under it takes presses"
    );
    click(&mut part, "Desk");
    assert_eq!(
        part.widgets.placement().size,
        (1280, 800),
        "room for the menu"
    );
    assert!(part.control_bounds("Studio").is_some(), "the menu is up");
    // A press away from the menu closes it.
    assert!(part.accepts_pointer(900.0, 400.0));
    part.pointer_motion(900.0, 400.0);
    part.prepare_frame();
    part.pointer_button(0x110, true);
    part.prepare_frame();
    part.pointer_button(0x110, false);
    for _ in 0..4 {
        part.prepare_frame();
    }
    assert!(part.control_bounds("Studio").is_none(), "the menu closed");
    assert_eq!(
        part.widgets.placement().size,
        (1280, 32),
        "back to the band"
    );
}

#[test]
fn choosing_a_workspace_in_the_menu_asks_the_session_to_switch() {
    let (mut part, link) = part();
    click(&mut part, "Desk");
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    click(&mut part, "Studio");
    let asked = link.asked.borrow().front().cloned();
    assert_eq!(
        asked.map(|ask| (ask.action, ask.arguments)),
        Some((
            "switch-workspace".to_owned(),
            BTreeMap::from([("workspace".to_owned(), "2".to_owned())])
        )),
        "{:?} {:?}",
        part.work(),
        part.refusals()
    );
    // The session ran it.
    link.serve(|_| Ok(()));
    assert!(!link.has_asks());
}

/// A press on an app the person pinned to the dock calls the app's own `open`
/// (`session.open(app)`): the session's door gets it on the app's resource, as the session
/// publishes it.
#[test]
fn a_press_on_a_dock_app_asks_the_session_to_open_it() {
    let _settled = ui::motion::testing::settled();
    let (mut part, link) = part();
    let host = part.instance.host().expect("the shell runs");
    let app = part.instance.app().to_owned();
    let meta = host.package(&app).expect("the shell's package").meta;
    let shell = meta
        .named_roots
        .iter()
        .find(|root| root.name == SHELL_RECORD)
        .map(|root| root.ty.clone())
        .expect("the shell's record");
    let shell = host
        .root_record(&app, &shell)
        .expect("the shell's record is made");
    let pin = meta
        .actions
        .iter()
        .find(|action| action.name == "pin")
        .expect("the shell's `pin`")
        .id
        .clone();
    // The person pins Sheets, as from a menu.
    host.runtime()
        .invoke_action(
            &tilolang_tilo::runtime::Principal::new("Pat"),
            (
                pin.as_str(),
                vec![
                    Value::resource(&shell),
                    Value::Text("so.tilo.sheets".to_owned()),
                    Value::Text("Sheets".to_owned()),
                    Value::Text("table".to_owned()),
                    Value::Text(String::new()),
                ],
            ),
        )
        .expect("Sheets is pinned");
    // The dock rides the bottom on its own.
    part.set_edges(HostEdges {
        bar: false,
        bar_top: true,
        dock: true,
        dock_bottom: true,
    });
    // The next reading of the desk's draws the dock again, over the record as it is now.
    link.publish(DeskReading {
        clock: "4:29 PM".to_owned(),
        ..reading()
    });
    for _ in 0..6 {
        part.prepare_frame();
    }
    let labels = crate::Published::accessible_labels(&mut part);
    assert!(
        part.control_bounds("Sheets").is_some(),
        "the dock shows Sheets: {labels:?}"
    );
    click(&mut part, "Sheets");
    let asked: Vec<Ask> = link.asked.borrow().iter().cloned().collect();
    assert_eq!(
        asked,
        vec![Ask {
            resource: "app.so.tilo.sheets".to_owned(),
            action: "open".to_owned(),
            arguments: BTreeMap::new(),
        }],
        "{:?}",
        part.refusals()
    );
}

#[test]
fn a_part_on_a_fullscreen_display_steps_aside_and_comes_back() {
    let (mut part, _link) = part();
    assert!(!part.aside(), "no window owns the display: the bar stands");
    assert_eq!(part.widgets.placement().size, (1280, 32), "the band");
    // A fullscreen window takes the display the part stands on: it draws nothing there.
    part.set_fullscreen(vec!["HEADLESS-1".to_owned()]);
    assert!(part.aside(), "the bar stepped aside");
    assert_eq!(
        part.widgets.placement().size.1,
        1,
        "nothing of the bar covers the window"
    );
    assert!(
        !part.accepts_pointer(100.0, 10.0),
        "the window under it takes the pointer"
    );
    assert!(
        part.layers().is_empty(),
        "not a row of the bar is handed to the compositor over the window"
    );
    // The window leaves fullscreen: the bar is back, in its band.
    part.set_fullscreen(Vec::new());
    assert!(!part.aside());
    assert_eq!(
        part.widgets.placement().size,
        (1280, 32),
        "back to the band"
    );
}

#[test]
fn a_part_whose_display_went_stands_nowhere_until_it_comes_back() {
    let (mut part, _link) = part();
    let display = part.placement().expect("the part stands on its display");
    part.set_display(None);
    assert!(part.layers().is_empty(), "nothing of it is drawn");
    assert!(!part.accepts_pointer(100.0, 10.0), "it takes no pointer");
    assert!(
        !crate::Published::offered(&part),
        "the door reaches the bars still standing, not this one"
    );
    // The compositor tells it of the outputs left: it does not stretch over them.
    part.outputs_changed(&[(1280, 0, 1280, 800)]);
    assert_eq!(part.placement(), None, "it stays off the other display");
    // The display comes back: the bar stands on it again.
    part.set_display(Some(display.clone()));
    assert_eq!(part.placement(), Some(display));
    assert!(crate::Published::offered(&part));
    assert_eq!(part.widgets.placement().size, (1280, 32), "in its band");
}

#[test]
fn a_fullscreen_window_on_another_display_leaves_this_bar_standing() {
    let (mut part, _link) = part();
    part.set_fullscreen(vec!["HEADLESS-2".to_owned()]);
    assert!(
        !part.aside(),
        "the window owns the other display: this bar stands"
    );
    assert_eq!(part.widgets.placement().size, (1280, 32));
}

#[test]
fn the_part_stands_in_the_band_the_shell_records_dock_names() {
    let (mut part, _link) = part();
    assert_eq!(
        part.widgets.placement().origin,
        (0, 0),
        "the first shell: a bar at the top"
    );
    part.set_edges(HostEdges {
        bar: false,
        bar_top: true,
        dock: true,
        dock_bottom: true,
    });
    assert_eq!(
        part.widgets.placement().origin.1,
        800 - 32,
        "a dock at the bottom alone stands in the bottom band"
    );
    assert_eq!(part.widgets.placement().size, (1280, 32));
}

/// The overview over a window on each of `strips`, the first window's on the first strip.
fn overview_with(strips: &[&str]) -> DeskReading {
    DeskReading {
        here: "overview".to_owned(),
        windows: strips
            .iter()
            .enumerate()
            .map(|(at, strip)| WindowReading {
                key: (at + 7).to_string(),
                title: format!("Window {at}"),
                origin_app: "so.tilo.notes".to_owned(),
                icon: "app_window".to_owned(),
                strip: (*strip).to_owned(),
                workspace: "Desk".to_owned(),
                recency: i64::try_from(at).unwrap(),
                focused: at == 0,
                preview: "none".to_owned(),
            })
            .collect(),
        ..reading()
    }
}

/// A window's menu runs the item pressed wherever the menu stands: past the end of what the
/// overview lays out (one strip, the menu hanging below it), and over the next strip's window
/// (two strips), which is drawn later and so asked first. Either way the press is the menu's.
#[test]
fn a_press_on_a_window_menus_item_runs_it_wherever_the_menu_hangs() {
    let _settled = ui::motion::testing::settled();
    for strips in [&["Main"][..], &["Main", "Research"][..]] {
        let (mut part, link) = part_with(overview_with(strips));
        for _ in 0..6 {
            part.prepare_frame();
            part.draw();
        }
        click(&mut part, "This window");
        for _ in 0..6 {
            part.prepare_frame();
            part.draw();
        }
        click(&mut part, "Close");
        assert_eq!(
            link.asked.borrow().front().cloned(),
            Some(Ask {
                resource: "desktop:tab:7".to_owned(),
                action: "close".to_owned(),
                arguments: BTreeMap::new(),
            }),
            "with strips {strips:?}"
        );
        assert!(
            part.control_bounds("Close").is_none(),
            "the menu closed with strips {strips:?}"
        );
    }
}

/// Type `text` into the field that has the keyboard, a key at a time.
fn type_into(part: &mut ShellPart, text: &str) {
    for letter in text.chars() {
        for pressed in [true, false] {
            let key = tilo_compositor::HostKey {
                keycode: 0,
                pressed,
                repeat: false,
                keysym: u32::from(letter),
                unmodified_keysym: u32::from(letter),
                text: pressed.then_some(letter),
                modifiers: tilo_compositor::KeyModifiers::default(),
            };
            tilo_compositor::SurfaceHost::key(part, &key);
            part.prepare_frame();
        }
    }
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
}

#[test]
fn a_workspace_named_in_the_workspaces_surface_is_made_with_its_name() {
    let _settled = ui::motion::testing::settled();
    let (mut part, link) = part_with(DeskReading {
        here: "workspaces".to_owned(),
        ..reading()
    });
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    let (_, _, width, _) = part
        .control_bounds("Name")
        .expect("the surface shows its field");
    assert!(
        width >= 100.0,
        "the name field is wide enough to type in: {width}"
    );
    click(&mut part, "Name");
    type_into(&mut part, "studio");
    click(&mut part, "Create");
    assert!(part.refusals().is_empty(), "{:?}", part.refusals());
    assert_eq!(
        link.asked
            .borrow()
            .front()
            .map(|ask| (ask.action.clone(), ask.arguments.clone())),
        Some((
            "new-workspace".to_owned(),
            BTreeMap::from([("name".to_owned(), "studio".to_owned())]),
        )),
    );
}

#[test]
fn every_letter_typed_into_the_launcher_reaches_its_search() {
    // Once something is typed, the launcher's footer and body change; the panel keeps its
    // header's field through that, so the next letter lands in the same field.
    let _settled = ui::motion::testing::settled();
    let (mut part, link) = part_with(DeskReading {
        here: "launcher".to_owned(),
        ..reading()
    });
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    click(&mut part, "Query");
    for letter in "quarterly".chars() {
        for pressed in [true, false] {
            let key = tilo_compositor::HostKey {
                keycode: 0,
                pressed,
                repeat: false,
                keysym: u32::from(letter),
                unmodified_keysym: u32::from(letter),
                text: pressed.then_some(letter),
                modifiers: tilo_compositor::KeyModifiers::default(),
            };
            tilo_compositor::SurfaceHost::key(&mut part, &key);
            part.prepare_frame();
        }
    }
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    click(&mut part, "Search everywhere for quarterly");
    assert_eq!(
        link.asked.borrow().front().cloned(),
        Some(Ask {
            resource: "desktop:desk".to_owned(),
            action: "open-search".to_owned(),
            arguments: BTreeMap::from([("query".to_owned(), "quarterly".to_owned())]),
        }),
    );
}
