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

/// The centre's presses are `session.notify` effects, which carry a tuple: Clear all and a
/// notice's Dismiss each reach the session as the desk's own action, at that press rather
/// than at the next input.
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
            SWITCH_WORKSPACE.to_owned(),
            BTreeMap::from([(WORKSPACE_ARG.to_owned(), "2".to_owned())])
        )),
        "{:?} {:?}",
        part.work(),
        part.refusals()
    );
    // The session ran it: the effect is settled.
    link.serve(|_| Ok(()));
    for _ in 0..4 {
        part.prepare_frame();
    }
    assert!(!link.has_asks());
    assert!(
        part.origin.borrow().in_flight.is_empty(),
        "the effect was answered"
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
