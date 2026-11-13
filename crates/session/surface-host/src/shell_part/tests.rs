#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "test fixtures"
)]

use super::*;
use tilo_session_core::door::Published;

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
        // The desk styles every installed package offers: the four the shell ships, as the
        // session publishes them (shell.md §6), so the switch lists them.
        styles: crate::shell_part::tests_support::shipped_styles(),
        ..DeskReading::default()
    }
}

use crate::shell_part::tests_support::{part_on, part_with};

fn part() -> (ShellPart, Rc<SessionLink>) {
    part_with(reading())
}

/// The canvas's entry, as assistive technology names it: the field is bound to a view state,
/// whose name in the code (`typed`) is no word a person says, so it is announced by its own
/// words.
const CANVAS_ENTRY: &str = "Find something, or ask";
/// The launcher's field, announced the same way: the view's own words for it, not the state's
/// name in the code (`query`).
const LAUNCHER_SEARCH: &str = "Search apps, windows, actions and files";
/// The workspaces surface's field, announced the same way rather than by the state's name in
/// the code (`name`).
const WORKSPACE_NAME: &str = "Name a new workspace";

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

/// Type `text` into the field that has the keyboard, a key at a time.
fn type_into(part: &mut ShellPart, text: &str) {
    for letter in text.chars() {
        for pressed in [true, false] {
            let key = tilo_session_core::surface::HostKey {
                keycode: 0,
                pressed,
                repeat: false,
                keysym: u32::from(letter),
                unmodified_keysym: u32::from(letter),
                latin_keysym: u32::from(letter),
                text: pressed.then(|| letter.to_string()),
                modifiers: tilo_session_core::surface::KeyModifiers::default(),
            };
            tilo_session_core::surface::SurfaceHost::key(part, &key);
            part.prepare_frame();
        }
    }
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
}

/// The shell the session ships is every file of the package the session runs: a file added to
/// `apps/tl/shell` and not listed here would compile in `tilolang check` and fail only in the
/// session. `scenarios.tl` is the one file it does not ship, and the only one: the harness the
/// session runs nothing of, its scenarios staging the shell over other apps' records for its
/// shots and tests (`SHELL_FILES`).
#[test]
fn the_shell_ships_every_file_of_its_package() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
    // The files the package's compiler reads, in the order it reads them, but its fixtures.
    let on_disk: Vec<String> = tilolang_tilo::tilolang::source::source_paths(&dir)
        .expect("the shell's package")
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name != "scenarios.tl")
        .collect();
    let shipped: Vec<String> = SHELL_FILES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    assert_eq!(
        shipped, on_disk,
        "SHELL_FILES lists the package's files the session runs, in path order"
    );
}

/// The package's scenarios (`scenarios.tl`) are the one file `SHELL_FILES` leaves out, and it
/// holds declarations the package's ids lock names: the session compiles the shell without it,
/// so a file that declared anything else there would drift from what the session runs.
#[test]
fn the_scenarios_are_the_only_file_the_session_leaves_out() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
    let on_disk: Vec<String> = tilolang_tilo::tilolang::source::source_paths(&dir)
        .expect("the shell's package")
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    let left_out: Vec<&String> = on_disk
        .iter()
        .filter(|name| {
            !SHELL_FILES
                .iter()
                .any(|(shipped, _)| shipped == &name.as_str())
        })
        .collect();
    assert_eq!(
        left_out,
        vec![&"scenarios.tl".to_owned()],
        "the session compiles the shell without its scenarios, and without nothing else"
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
            seen: true,
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
            seen: true,
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
                arguments: BTreeMap::from([("notice".to_owned(), "1".into())]),
                from: None,
            },
            Ask {
                resource: DESK_RESOURCE.to_owned(),
                action: "clear-notices".to_owned(),
                arguments: BTreeMap::new(),
                from: None,
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
    let opening = link.asked.borrow().front().cloned();
    assert_eq!(
        opening.as_ref().map(|ask| ask.action.clone()),
        Some("show-surface".to_owned())
    );
    assert_eq!(
        opening.map(|ask| ask.arguments.get("surface").cloned()),
        Some(Some("centre".into())),
        "the bell opens the notification centre"
    );
}

/// The device's status on the bar reads the resources the session publishes — the sound, the
/// connection and the battery, the same ones the quick settings read — and each press opens
/// the quick settings.
#[test]
fn the_bars_status_reads_the_devices_published_resources() {
    let _settled = ui::motion::testing::settled();
    let (mut part, link) = part();
    link.publish_reading(
        "sound",
        "so.tilo.device#Sound",
        serde_json::json!({"output": 40, "volume": 0.42, "muted": false}),
    );
    link.publish_reading(
        "network",
        "so.tilo.device#Network",
        serde_json::json!({
            "wifi": {"available": true, "on": true, "networks": []},
            "link": {"kind": "wifi", "name": "Station", "signal": 3},
        }),
    );
    link.publish_reading(
        "power:battery",
        "so.tilo.device#Battery",
        serde_json::json!({"present": true, "percentage": 71, "state": "discharging", "on-battery": true}),
    );
    for _ in 0..6 {
        part.prepare_frame();
    }
    for said in ["Volume, 42%", "Wi-Fi, Station", "71%"] {
        assert!(
            part.says(said),
            "the bar says {said}: {:?}",
            part.accessible_labels()
        );
    }
    // The battery's glyph says it charges: the same reading, its other fact.
    link.publish_reading(
        "power:battery",
        "so.tilo.device#Battery",
        serde_json::json!({"present": true, "percentage": 71, "state": "charging", "on-battery": false}),
    );
    for _ in 0..6 {
        part.prepare_frame();
    }
    assert!(part.says("71%"), "the charge stays as it charges");
    assert!(
        part.says("71%, charging"),
        "and says it takes charge: {:?}",
        part.accessible_labels()
    );
    for name in ["Volume, 42%", "Wi-Fi, Station", "71%, charging"] {
        click(&mut part, name);
        assert_eq!(
            link.asked.borrow_mut().pop_front().map(|ask| ask.action),
            Some("show-surface".to_owned()),
            "a press on {name} opens the quick settings: refusals {:?}",
            part.refusals()
        );
    }
}

/// Who else is here shows as their faces, read from the session's presence, and the people
/// button opens the session's roster.
#[test]
fn the_bars_faces_are_who_is_here_and_the_button_opens_the_roster() {
    let _settled = ui::motion::testing::settled();
    let (mut part, link) = part_with(DeskReading {
        people: vec![
            PersonReading {
                key: "mia".to_owned(),
                name: "Mia Clayton".to_owned(),
            },
            PersonReading {
                key: "sam".to_owned(),
                name: "Sam Okafor".to_owned(),
            },
        ],
        ..reading()
    });
    for _ in 0..6 {
        part.prepare_frame();
    }
    for said in [
        "Mia Clayton",
        "Sam Okafor",
        "Mia Clayton and Sam Okafor are here",
    ] {
        assert!(
            part.says(said),
            "the bar says {said}: {:?}",
            part.accessible_labels()
        );
    }
    click(&mut part, "Mia Clayton and Sam Okafor are here");
    assert_eq!(
        link.asked.borrow_mut().pop_front().map(|ask| ask.action),
        Some("toggle-collaborators".to_owned()),
        "refusals {:?}",
        part.refusals()
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
        ("notification", |r| {
            r.notices.push(NoticeReading {
                key: "n1".to_owned(),
                title: "Backup finished".to_owned(),
                seen: false,
                ..Default::default()
            });
        }),
        ("workspace switch", |r| {
            r.workspace = "Studio".to_owned();
            // The reading lists the strip the bar shows first, so the switch moves the
            // mark on to the workspace after it.
            let [desk, studio, ..] = r.workspaces.as_mut_slice() else {
                panic!("the reading lists both of the bar's workspaces");
            };
            desk.current = false;
            studio.current = true;
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
fn the_part_covers_the_screen_at_rest_and_while_its_menu_is_up() {
    let (mut part, _link) = part();
    assert_eq!(
        part.widgets.placement().size,
        (1280, 800),
        "the whole screen at rest"
    );
    assert!(
        part.accepts_pointer(900.0, 400.0),
        "the shell is the screen under the windows"
    );
    click(&mut part, "Desk");
    assert_eq!(
        part.widgets.placement().size,
        (1280, 800),
        "still the whole screen"
    );
    assert!(part.control_bounds("Studio").is_some(), "the menu is up");
    // A press away from the menu closes it.
    part.pointer_motion(900.0, 400.0);
    part.prepare_frame();
    part.pointer_button(0x110, true);
    part.prepare_frame();
    part.pointer_button(0x110, false);
    for _ in 0..4 {
        part.prepare_frame();
    }
    assert!(part.control_bounds("Studio").is_none(), "the menu closed");
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
            BTreeMap::from([("workspace".to_owned(), "2".into())])
        )),
        "{:?} {:?}",
        part.work(),
        part.refusals()
    );
    // The session ran it.
    link.serve(|_| Ok(()));
    assert!(!link.has_asks());
}

/// A press on an app the person pinned to the dock asks the session to open that app
/// (`session.open_app(app)`), named by its app id: the session's door gets it on the desk's
/// chrome, and the app's place on the desk comes to the front.
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
    // The runtime committed the pin: the part applies it to the record its views read, as a
    // press in the shell's settings does, and the next turn draws the dock over it.
    part.apply_committed_for_test();
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
            resource: "desktop:chrome".to_owned(),
            action: "open-app".to_owned(),
            arguments: BTreeMap::from([("app".to_owned(), "so.tilo.sheets".into())]),
            from: None,
        }],
        "{:?}",
        part.refusals()
    );
}

#[test]
fn a_part_on_a_fullscreen_display_steps_aside_and_comes_back() {
    let (mut part, _link) = part();
    assert!(!part.aside(), "no window owns the display: the part stands");
    assert_eq!(part.widgets.placement().size, (1280, 800), "the screen");
    // A fullscreen window takes the display the part stands on: it draws nothing there.
    part.set_fullscreen(vec!["HEADLESS-1".to_owned()]);
    assert!(part.aside(), "the part stepped aside");
    assert_eq!(
        part.widgets.placement().size.1,
        1,
        "nothing of the part covers the window"
    );
    assert!(
        !part.accepts_pointer(100.0, 10.0),
        "the window under it takes the pointer"
    );
    assert!(
        part.layers().is_empty(),
        "not a row of the part is handed to the compositor over the window"
    );
    // The window leaves fullscreen: the part is back, over the whole screen.
    part.set_fullscreen(Vec::new());
    assert!(!part.aside());
    assert_eq!(
        part.widgets.placement().size,
        (1280, 800),
        "back over the screen"
    );
}

#[test]
fn a_field_put_in_focus_takes_the_keys_and_a_press_elsewhere_hands_them_back() {
    // The compositor routes a key to the topmost surface that wants the keyboard. A part that
    // never says it wants it never hears a key, however the field is focused: the canvas's
    // entry (and every field the shell draws) stays empty.
    let _settled = ui::motion::testing::settled();
    let (mut part, _link) = part();
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    assert!(
        !SurfaceHost::wants_keyboard(&part),
        "a part nobody typed in leaves the keys to the window in front"
    );
    click(&mut part, "Showing Strips");
    click(&mut part, "Frames");
    click(&mut part, CANVAS_ENTRY);
    assert!(
        SurfaceHost::text_input(&part).is_some(),
        "the press put the caret in the canvas's entry"
    );
    assert!(
        SurfaceHost::wants_keyboard(&part),
        "the entry has the caret: what is typed goes to the part"
    );
    type_into(&mut part, "groceries");
    assert!(
        part.says("groceries"),
        "the entry shows what was typed: {:?}",
        part.accessible_labels()
    );
    // A press on a window over the shell (or anywhere off the part) hands the keys back: what
    // is typed next reaches what the person pressed on, and the entry keeps its words.
    SurfaceHost::pressed_elsewhere(&mut part, 300.0, 300.0);
    part.prepare_frame();
    assert!(
        SurfaceHost::text_input(&part).is_none(),
        "the field gave the keyboard up"
    );
    assert!(
        !SurfaceHost::wants_keyboard(&part),
        "the part leaves the keys to the window again"
    );
    assert!(
        part.says("groceries"),
        "what was typed stands in the entry: {:?}",
        part.accessible_labels()
    );
}

#[test]
fn a_part_whose_display_went_stands_nowhere_until_it_comes_back() {
    let (mut part, _link) = part();
    let display = part.placement().expect("the part stands on its display");
    part.set_display(None, 1.0);
    assert!(part.layers().is_empty(), "nothing of it is drawn");
    assert!(!part.accepts_pointer(100.0, 10.0), "it takes no pointer");
    assert!(
        !crate::Published::offered(&part),
        "the door reaches the bars still standing, not this one"
    );
    // The compositor tells it of the outputs left: it does not stretch over them.
    part.outputs_changed(&[tilo_session_core::surface::HostedOutput {
        rect: (1280, 0, 1280, 800),
        scale: 1.0,
    }]);
    assert_eq!(part.placement(), None, "it stays off the other display");
    // The display comes back: the part stands on it again.
    part.set_display(Some(display.clone()), 1.0);
    assert_eq!(part.placement(), Some(display));
    assert!(crate::Published::offered(&part));
    assert_eq!(
        part.widgets.placement().size,
        (1280, 800),
        "its whole screen"
    );
}

#[test]
fn a_fullscreen_window_on_another_display_leaves_this_part_standing() {
    let (mut part, _link) = part();
    part.set_fullscreen(vec!["HEADLESS-2".to_owned()]);
    assert!(
        !part.aside(),
        "the window owns the other display: this part stands"
    );
    assert_eq!(part.widgets.placement().size, (1280, 800));
}

/// The person's arrangement: one strip on the desk, its one place a foreign window's (no
/// record), keyed `p1`.
fn one_window_arrangement() -> ArrangementReading {
    ArrangementReading {
        workspaces: vec![ArrangementWorkspaceReading {
            key: "w1".to_owned(),
            name: "Desk".to_owned(),
            current: true,
        }],
        rows: vec![ArrangementRowReading {
            key: "r1".to_owned(),
            workspace: "w1".to_owned(),
            name: "Main".to_owned(),
            shell: String::new(),
        }],
        strips: vec![ArrangementStripReading {
            key: "s1".to_owned(),
            workspace: "w1".to_owned(),
            row: "r1".to_owned(),
            width: 600,
            placed: true,
            canvas_width: 600,
            canvas_height: 400,
            ..ArrangementStripReading::default()
        }],
        places: vec![ArrangementPlaceReading {
            key: "p1".to_owned(),
            strip: "s1".to_owned(),
            shows: "Terminal".to_owned(),
            ..ArrangementPlaceReading::default()
        }],
    }
}

/// Where the menu of the window the part shows as `name` stands, when it shows one. The strips
/// say a window's name twice: its frame is named for it, and so is the window's own menu in its
/// bar, which lists the commands of the place the window shows. The frame is the largest control
/// of that name, and the menu the smallest of the others whose centre the frame holds: an icon
/// in the window's bar. A window named by its frame alone has none.
fn window_menu(part: &mut ShellPart, name: &str) -> Option<(f64, f64, f64, f64)> {
    let named = part.control_bounds_all(name);
    let frame = named
        .iter()
        .copied()
        .max_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))?;
    named
        .into_iter()
        .filter(|&(x, y, w, h)| {
            let (across, down) = (x + w / 2.0, y + h / 2.0);
            w * h < frame.2 * frame.3
                && across >= frame.0
                && across <= frame.0 + frame.2
                && down >= frame.1
                && down <= frame.1 + frame.3
        })
        .min_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))
}

/// A part over the arrangement of one foreign window, its wells reporting to the handle it
/// returns.
fn showing_one_window() -> (ShellPart, ui::widget::Wells) {
    let wells = ui::widget::Wells::new();
    let (mut part, link) = super::tests_support::part_as(reading(), Some(wells.clone()));
    link.publish_arrangement(one_window_arrangement());
    for _ in 0..6 {
        part.prepare_frame();
    }
    (part, wells)
}

/// The whole screen shows the person's arrangement between the bar and the dock, under the
/// preset switch, and a foreign window's place is a well reported to the window policy's
/// handle, on the output under the bar.
#[test]
fn the_screen_shows_the_arrangement_and_reports_each_windows_well() {
    let (mut part, wells) = showing_one_window();
    assert!(part.below_windows(), "the windows stand over the screen");
    let labels = crate::Published::accessible_labels(&mut part);
    for name in ["Notifications", "Showing Strips"] {
        assert!(
            labels.iter().any(|label| label == name),
            "the screen shows {name}: {labels:?}"
        );
    }
    // The preset switch is the bar's own menu, named by the preset in use: the ways of showing
    // the same arrangement are its items, offered when the person opens it.
    click(&mut part, "Showing Strips");
    let labels = crate::Published::accessible_labels(&mut part);
    for name in ["Frames", "Spaces"] {
        assert!(
            labels.iter().any(|label| label == name),
            "the preset menu offers {name}: {labels:?}"
        );
    }
    let well = wells
        .well("p1")
        .unwrap_or_else(|| panic!("the window's place is a well: {labels:?}"));
    assert!(
        well.width() >= 1.0 && well.height() >= 1.0 && well.x0 >= 0.0 && well.y0 > 0.0,
        "the well stands on the output, under the bar: {well:?}"
    );

    // Each window's own controls stand in its one bar — its menu, named for the window it is the
    // menu of (the place shows a terminal and has no title of its own, so it is called by what
    // it shows), and its close, beside its maximize; the framed canvas names the window by its
    // frame alone, with no menu of the window's, and shows its entry (what typed text finds is
    // placed on the canvas) instead.
    assert!(
        window_menu(&mut part, "Terminal").is_some()
            && labels.iter().any(|label| label == "Close")
            && labels.iter().any(|label| label == "Maximize"),
        "the windows show their controls: {labels:?}; Terminal names {:?}",
        part.control_bounds_all("Terminal")
    );
    // The preset switch shows the same place on the framed canvas, its well there.
    click(&mut part, "Frames");
    let labels = crate::Published::accessible_labels(&mut part);
    let named = part.control_bounds_all("Terminal");
    assert!(
        !named.is_empty()
            && window_menu(&mut part, "Terminal").is_none()
            && labels.iter().any(|label| label == CANVAS_ENTRY),
        "the canvas shows: {labels:?}; Terminal names {named:?}; refused {:?}",
        part.refusals()
    );
    assert!(
        wells.well("p1").is_some_and(|well| well.width() >= 1.0),
        "the place is a well on the canvas"
    );
}

/// A surface of the desk's stands over the windows it is over: the whole screen lies under
/// them at rest, so its wells hold them, and the overview — with the pictures it names — comes
/// up over every window rather than underneath the window it is showing.
#[test]
fn a_surface_of_the_desks_stands_over_the_windows() {
    let _settled = ui::motion::testing::settled();
    let (mut part, link) = super::tests_support::part_as(reading(), None);
    for _ in 0..4 {
        part.prepare_frame();
    }
    assert!(
        part.below_windows(),
        "the desk at rest stands under the windows"
    );
    link.publish(DeskReading {
        here: "overview".to_owned(),
        ..reading()
    });
    for _ in 0..4 {
        part.prepare_frame();
    }
    assert!(
        !part.below_windows(),
        "the overview stands over the windows"
    );
    link.publish(reading());
    for _ in 0..4 {
        part.prepare_frame();
    }
    assert!(
        part.below_windows(),
        "and goes under them again when it closes"
    );
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
                app_key: "so.tilo.notes".to_owned(),
                icon: "app_window".to_owned(),
                strip: (*strip).to_owned(),
                strip_key: at.to_string(),
                workspace: "Desk".to_owned(),
                recency: i64::try_from(at).unwrap(),
                focused: at == 0,
                preview: "none".to_owned(),
                ..WindowReading::default()
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
                action: "close-tab".to_owned(),
                arguments: BTreeMap::new(),
                from: None,
            }),
            "with strips {strips:?}"
        );
        assert!(
            part.control_bounds("Close").is_none(),
            "the menu closed with strips {strips:?}"
        );
    }
}

/// A window's menu carries it to another strip of its workspace by the chrome's `move-tab`,
/// the strip named by its id in the destination the chrome reads.
#[test]
fn moving_a_window_to_another_strip_names_the_strip_by_its_id() {
    let _settled = ui::motion::testing::settled();
    let (mut part, link) = part_with(overview_with(&["Main", "Research"]));
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    click(&mut part, "This window");
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    // The menu open is either window's: each offers the other's strip.
    let labels = crate::Published::accessible_labels(&mut part);
    let (item, tab, strip) = if labels.iter().any(|label| label == "Move to Research") {
        ("Move to Research", "7", "1")
    } else {
        assert!(
            labels.iter().any(|label| label == "Move to Main"),
            "the menu offers the other strip: {labels:?}"
        );
        ("Move to Main", "8", "0")
    };
    click(&mut part, item);
    assert!(part.refusals().is_empty(), "{:?}", part.refusals());
    let asked = link
        .asked
        .borrow()
        .front()
        .cloned()
        .expect("the move is asked");
    assert_eq!(
        (asked.resource.as_str(), asked.action.as_str()),
        ("desktop:chrome", "move-tab")
    );
    assert_eq!(
        asked
            .arguments
            .get("tab")
            .and_then(serde_json::Value::as_str),
        Some(tab)
    );
    let to: tilo_desktop_surface::navigation::Destination =
        serde_json::from_value(asked.arguments.get("to").cloned().expect("the destination"))
            .unwrap_or_else(|why| {
                panic!("the destination reads as the chrome's: {why}: {asked:?}")
            });
    assert_eq!(
        to,
        tilo_desktop_surface::navigation::Destination {
            strip: Some(strip.to_owned()),
            ..Default::default()
        }
    );
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
        .control_bounds(WORKSPACE_NAME)
        .expect("the surface shows its field");
    assert!(
        width >= 100.0,
        "the name field is wide enough to type in: {width}"
    );
    click(&mut part, WORKSPACE_NAME);
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
            BTreeMap::from([("name".to_owned(), "studio".into())]),
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
    click(&mut part, LAUNCHER_SEARCH);
    for letter in "quarterly".chars() {
        for pressed in [true, false] {
            let key = tilo_session_core::surface::HostKey {
                keycode: 0,
                pressed,
                repeat: false,
                keysym: u32::from(letter),
                unmodified_keysym: u32::from(letter),
                latin_keysym: u32::from(letter),
                text: pressed.then(|| letter.to_string()),
                modifiers: tilo_session_core::surface::KeyModifiers::default(),
            };
            tilo_session_core::surface::SurfaceHost::key(&mut part, &key);
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
            action: "search".to_owned(),
            arguments: BTreeMap::from([("query".to_owned(), "quarterly".into())]),
            from: None,
        }),
    );
}

/// A part told its display only after it settled (the session's first frames can run before
/// its output is known) lays its bar out at the display's width: the strip, which a narrow bar
/// gives way, stands on a wide one though nothing of the desk changed since.
#[test]
fn a_part_told_its_display_after_it_settled_lays_out_at_the_displays_width() {
    let packages = crate::shell_part::compile_shell().unwrap_or_else(|why| panic!("{why}"));
    let app = packages.last().unwrap().meta.app.clone();
    let link = Rc::new(SessionLink::default());
    crate::shell_part::tests_support::publish_platform_roots(&link);
    link.publish(reading());
    let mut part = ShellPart::new(
        ShellIdentity {
            app: &app,
            packages,
            person: "Pat",
            link: Rc::clone(&link),
            record: Rc::new(RefCell::new(tilo_resource::Doc::new())),
            seed: true,
        },
        1.0,
        std::sync::Arc::new(|| {}),
        None,
    );
    // It settles with no display: nothing more is due.
    for _ in 0..8 {
        part.prepare_frame();
    }
    assert!(!part.due(), "the part settled");
    part.outputs_changed(&[tilo_session_core::surface::HostedOutput {
        rect: (0, 0, 1280, 800),
        scale: 1.0,
    }]);
    part.set_display(Some(("HEADLESS-1".to_owned(), (0, 0, 1280, 800))), 1.0);
    for _ in 0..4 {
        part.prepare_frame();
    }
    assert!(
        part.control_bounds("Main").is_some(),
        "a wide bar names the strip: {:?}",
        crate::Published::accessible_labels(&mut part)
    );
}

/// With a surface up the part covers its output, and the desk fills it: the dock rides the
/// output's bottom edge however short the surface is (the notifications with none), on a
/// phone and on a desktop, never right under what the surface holds.
#[test]
fn with_a_short_surface_up_the_dock_rides_the_outputs_bottom_edge() {
    for (width, height) in [(390, 844), (1280, 800)] {
        let (mut part, _link) = crate::shell_part::tests_support::part_on(
            DeskReading {
                here: "centre".to_owned(),
                ..reading()
            },
            (width, height),
        );
        for _ in 0..4 {
            part.prepare_frame();
        }
        assert_eq!(
            part.widgets.placement().size,
            (width, height),
            "the surface covers the output"
        );
        let (_, y, _, h) = part
            .control_bounds("Dock")
            .unwrap_or_else(|| panic!("the dock shows at {width}x{height}"));
        let bottom = y + h;
        assert!(
            bottom <= f64::from(height) + 0.5 && bottom >= f64::from(height) - 24.0,
            "at {width}x{height} the dock ends at {bottom}, not at the bottom edge"
        );
    }
}

/// A display wide enough for every part: the bar names the strip and shows the person the
/// session publishes with nothing touching the desk. The part hands the app the space the
/// frame gave its view the moment it is laid out, so the bar is full rather than waiting for
/// the person's first press to hear how much space it has.
#[test]
fn a_wide_bar_shows_the_strip_and_the_person_with_nothing_touched() {
    let _settled = ui::motion::testing::settled();
    let (mut part, _link) = part();
    for _ in 0..8 {
        part.prepare_frame();
    }
    let labels: Vec<String> = crate::Published::accessible_labels(&mut part);
    assert!(
        labels.iter().any(|label| label == "Main"),
        "the bar names the strip: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "Pat"),
        "the bar shows the person: {labels:?}"
    );
}

/// The switcher pictures each window it walks: the compositor is told to draw the window's own
/// pixels above the title that names it, and nothing at all for a window that may not be
/// pictured — whose card shows its app's mark instead.
#[test]
fn the_switcher_pictures_each_window_above_its_own_title() {
    let _settled = ui::motion::testing::settled();
    let (mut part, link) = part_with(super::tests_support::reading_with_windows());
    let previews = ui::widget::Previews::new();
    // What the compositor holds for each window the reading names: every one but "12", whose
    // app the session left out of the pictures.
    part.show_pictures(
        previews.clone(),
        Rc::new(
            |key: &str| {
                if key == "12" { None } else { key.parse().ok() }
            },
        ),
    );
    // Holding Alt+Tab opens the switcher: every window the desk holds, in the walk's order.
    let mut reading = super::tests_support::reading_with_windows();
    reading.here = "switcher".to_owned();
    reading.switcher_revealed = true;
    for (at, window) in reading.windows.iter_mut().enumerate() {
        window.recency = i64::try_from(at).unwrap_or(0) + 1;
        window.preview = "live".to_owned();
    }
    link.publish(reading);
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    let pictures = tilo_session_core::surface::SurfaceHost::window_pictures(&part);
    let picture = pictures
        .first()
        .unwrap_or_else(|| panic!("the switcher pictures the window it may: {pictures:?}"));
    assert_eq!(
        picture.window, 11,
        "the picture is the compositor's window for the reading's key"
    );
    let (x, y, w, _) = part
        .control_bounds("Release planning")
        .unwrap_or_else(|| panic!("the card names its window: {:?}", part.refusals()));
    let (left, top, wide, high) = picture.rect;
    assert!(
        top + high <= y + 1.0,
        "the picture stands above the title naming its window: picture {:?}, title at {x},{y}",
        picture.rect
    );
    assert!(
        wide > 1.0 && (left - x).abs() <= 2.0 && (left + wide - (x + w)).abs() <= 2.0,
        "the picture spans the card's width: picture {:?}, title at {x} wide {w}",
        picture.rect
    );
    assert!(
        previews.picture("12").is_some(),
        "the card of a window that may not be pictured still stands"
    );
    assert_eq!(
        pictures.len(),
        1,
        "a window that may not be pictured is not drawn over: {pictures:?}"
    );
}

/// The people who come and go in [`the_bars_people_stay_who_is_here_across_a_commit`].
const HERE: [&str; 4] = ["Mia Clayton", "Sam Okafor", "Jo Park", "Ada Byron"];

/// Who is here is exactly who the desk says is here, and stays so across a commit of the
/// shell's own record (a setting changes the bar's body, not the list it drew): with nobody,
/// one, four and one again, the bar names those people and no others.
#[test]
fn the_bars_people_stay_who_is_here_across_a_commit() {
    let _settled = ui::motion::testing::settled();
    let (mut part, link) = part_with(reading());
    let mut now = reading();
    for count in [0_usize, 1, 4, 1] {
        now.people = (0..count)
            .map(|at| PersonReading {
                key: format!("p{at}"),
                name: HERE[at].to_owned(),
            })
            .collect();
        link.publish(now.clone());
        for _ in 0..4 {
            part.prepare_frame();
        }
        // The bar's body is set: a commit of the shell's own record, as pressing the setting
        // in the shell's settings does.
        part.run_setting(
            "set_bar_finish",
            vec![serde_json::json!(if count == 1 {
                "translucent"
            } else {
                "solid"
            })],
        )
        .unwrap_or_else(|why| panic!("the bar takes a finish: {why}"));
        for _ in 0..4 {
            part.prepare_frame();
        }
        let labels = part.accessible_labels();
        let said = |name: &str| labels.iter().any(|label| label.contains(name));
        let expected = match count {
            0 => "People".to_owned(),
            1 => format!("{} is here", HERE[0]),
            2 => format!("{} and {} are here", HERE[0], HERE[1]),
            _ => format!("{} and {} others are here", HERE[0], count - 1),
        };
        assert!(
            labels.iter().any(|label| label.contains(&expected)),
            "the bar says {expected:?}: {labels:?}"
        );
        // Nobody who left is still named: going from four to one names one person, not four.
        for name in &HERE[count..] {
            assert!(
                !said(name),
                "{count} are here: {name} is still said: {labels:?}"
            );
        }
    }
}

/// The overview gives every window the share of its strip it keeps on the desk,
/// and each picture room to show the window: a picture a fraction of a pixel
/// high shows no window at all.
#[test]
fn the_overview_pictures_every_window_of_a_strip_at_its_share() {
    let _settled = ui::motion::testing::settled();
    let windows: Vec<WindowReading> = [(7, 0.5, "wide"), (8, 0.25, "narrow")]
        .into_iter()
        .enumerate()
        .map(|(at, (key, share, title))| WindowReading {
            key: key.to_string(),
            title: title.to_owned(),
            origin_app: "so.tilo.notes".to_owned(),
            app_key: "so.tilo.notes".to_owned(),
            icon: "app_window".to_owned(),
            strip: "Main".to_owned(),
            strip_key: "1".to_owned(),
            workspace: "Desk".to_owned(),
            workspace_key: "1".to_owned(),
            group_key: key.to_string(),
            share,
            local: true,
            place: i64::try_from(at).unwrap(),
            recency: i64::try_from(at).unwrap(),
            focused: at == 0,
            preview: "live".to_owned(),
            ..WindowReading::default()
        })
        .collect();
    let (mut part, link) = part_with(reading());
    let previews = ui::widget::Previews::new();
    part.show_pictures(
        previews.clone(),
        std::rc::Rc::new(|key: &str| key.parse::<u64>().ok()),
    );
    link.publish(DeskReading {
        here: "overview".to_owned(),
        windows,
        workspaces: vec![RoomReading {
            key: "1".to_owned(),
            name: "Desk".to_owned(),
            current: true,
            ..RoomReading::default()
        }],
        ..reading()
    });
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    let pictures: Vec<(u64, (f64, f64, f64, f64))> = part
        .window_pictures()
        .into_iter()
        .map(|picture| (picture.window, picture.rect))
        .collect();
    assert_eq!(
        pictures.len(),
        2,
        "both windows are pictured: {pictures:?} of {:?}",
        previews.pictures()
    );
    for (window, (x, y, width, height)) in &pictures {
        assert!(*height >= 24.0, "window {window} has room: {pictures:?}");
        assert!(*width >= 24.0, "window {window} has room: {pictures:?}");
        assert!(
            *x >= 0.0 && *y >= 0.0 && x + width <= 1280.0 && y + height <= 800.0,
            "window {window} is on the display: {pictures:?}"
        );
    }
    // The window keeping half the strip is given at least the room of the one keeping a
    // quarter, as two windows stand on the desk; a place past the width a page shows it at
    // keeps a window's shape instead of growing taller than the page.
    let width = |window: u64| {
        pictures
            .iter()
            .find(|(key, _)| *key == window)
            .map_or(0.0, |(_, (_, _, width, _))| *width)
    };
    assert!(
        width(7) >= width(8),
        "the wider window takes the wider room: {pictures:?}"
    );
}

/// A part on an output at `scale` draws its whole screen at that scale: a
/// bar on a 2 display paints twice the pixels of a 1 display's, rather than
/// one scale's worth stretched over twice the area (which is what made
/// everything on a `HiDPI` panel grainy).
#[test]
fn a_part_on_a_scaled_output_paints_at_that_scale() {
    let (mut plain, _link) = part_on(reading(), (1280, 800));
    plain.outputs_changed(&[tilo_session_core::surface::HostedOutput {
        rect: (0, 0, 1280, 800),
        scale: 1.0,
    }]);
    plain.set_display(Some(("HEADLESS-1".to_owned(), (0, 0, 1280, 800))), 1.0);
    plain.prepare_frame();
    let one = plain.widgets_mut().placement();

    let (mut twice, _link) = part_on(reading(), (1280, 800));
    twice.outputs_changed(&[tilo_session_core::surface::HostedOutput {
        rect: (0, 0, 1280, 800),
        scale: 2.0,
    }]);
    twice.set_display(Some(("HEADLESS-1".to_owned(), (0, 0, 1280, 800))), 2.0);
    twice.prepare_frame();
    let two = twice.widgets_mut().placement();

    assert_eq!(one.size, two.size, "the same screen in logical pixels");
    assert!(
        (one.scale - 1.0).abs() < f64::EPSILON && (two.scale - 2.0).abs() < f64::EPSILON,
        "each part draws at its own output's scale: {one:?} then {two:?}"
    );
    // The layer is the frame the tree paints: at 2 it is twice as wide.
    let width = |part: &mut ShellPart| part.layers()[0].frame.size().0;
    let plain_width = width(&mut plain);
    let wide_width = width(&mut twice);
    assert_eq!(
        wide_width,
        plain_width * 2,
        "a 2 display's part paints the pixels it shows, not half of them: {plain_width} then {wide_width}"
    );
}

/// The desk's preset reaches the screen: a change of the shell's own record made from outside
/// the shell's views — `apply(windows_desktop)`, as the settings page's card and the REPL run it —
/// runs the boundaries that read it and patches the window, so the desk the person sees is the
/// one the record names.
///
/// The shape that failed it: a shell part over a desk with one local window, its arrangement
/// published, then the settings' apply. The record changed and the view's own `is_stale` said so,
/// but nothing it ran changed a tree, and the screen stayed on the strips (`stale=true`,
/// `patches=0`) — the record change never marked the boundary that read it.
#[test]
fn the_desks_preset_reaches_the_screen() {
    let mut desk = reading();
    desk.windows = vec![WindowReading {
        key: "11".to_owned(),
        title: "Release planning".to_owned(),
        origin_app: "Notes".to_owned(),
        app_key: "so.tilo.notes".to_owned(),
        icon: "notebook".to_owned(),
        strip: "Main".to_owned(),
        workspace: "Desk".to_owned(),
        recency: 1,
        focused: true,
        local: true,
        ..WindowReading::default()
    }];
    let (mut part, link) = super::tests_support::part_as(desk, None);
    link.publish_arrangement(one_window_arrangement());
    for _ in 0..6 {
        part.prepare_frame();
    }
    let labels = crate::Published::accessible_labels(&mut part);
    assert!(
        labels.iter().any(|l| l == "Showing Strips"),
        "the desk shows the strips: {labels:?}"
    );

    part.run_setting("apply", vec![serde_json::json!("windows_desktop")])
        .unwrap_or_else(|why| panic!("apply: {why}"));
    for _ in 0..6 {
        part.prepare_frame();
    }

    let labels = crate::Published::accessible_labels(&mut part);
    assert!(
        !labels.iter().any(|l| l.starts_with("Showing ")),
        "the traditional desktop's bar carries the window's commands, not the preset switch: \
         {labels:?}"
    );
    assert!(
        labels.iter().any(|l| l == "Dock"),
        "the desktop's dock rides its foot: {labels:?}"
    );
    let view = part
        .instance
        .view(tilo_render_protocol::patch::Lane::Root)
        .unwrap();
    let stats = view.borrow().stats();
    assert!(
        stats.renders > stats.boundaries as u64,
        "the preset ran the boundaries that read the record: {stats:?}"
    );
}

/// The desk styles arrive on a reading the shell draws after its first frame, as the session
/// publishes them once its store is read: the switch re-runs when the styles' collection moves,
/// and offers them without a restart (shell.md §6).
#[test]
fn the_style_switch_lists_the_styles_a_later_reading_carries() {
    let mut desk = reading();
    desk.styles = Vec::new();
    let (mut part, link) = super::tests_support::part_as(desk, None);
    for _ in 0..4 {
        part.prepare_frame();
    }
    // With no styles yet the bar's menu falls back to naming the strips, and offers nothing.
    let labels = crate::Published::accessible_labels(&mut part);
    assert!(
        labels.iter().any(|l| l == "Showing Strips"),
        "the bar shows before the styles come: {labels:?}"
    );
    let mut with_styles = reading();
    with_styles.styles = crate::shell_part::tests_support::shipped_styles();
    link.publish(with_styles);
    for _ in 0..6 {
        part.prepare_frame();
    }
    click(&mut part, "Showing Strips");
    let labels = crate::Published::accessible_labels(&mut part);
    for name in ["Strips", "Frames", "Spaces", "Desktop"] {
        assert!(
            labels.iter().any(|l| l == name),
            "the switch lists the {name} style once the reading carries it: {labels:?}"
        );
    }
}

/// The shell's views run in the session, which holds the person's formats: a day's
/// `start_of_week()` goes back to the day their region starts a week on, as their desk's calendar
/// and their app windows read it, not to the Monday a view with no formats falls back on.
#[test]
fn a_shells_week_starts_on_the_persons_first_day() {
    let platform = ShellPlatform {
        packages: Rc::new(Vec::new()),
        waker: std::sync::Arc::new(|| {}),
        link: Rc::new(SessionLink::default()),
    };
    let before = ui::format::region::current();
    let monday = ui::format::region::RegionFormats {
        first_day: ui::format::region::FirstDayOfWeek::Monday,
        ..before
    };
    ui::format::region::install(monday);
    assert_eq!(platform.week_start(), 0, "a week that starts on Monday");
    ui::format::region::install(ui::format::region::RegionFormats {
        first_day: ui::format::region::FirstDayOfWeek::Sunday,
        ..monday
    });
    assert_eq!(
        platform.week_start(),
        6,
        "the person starts their week on Sunday"
    );
    ui::format::region::install(before);
}

/// The framed canvas pans under a middle drag, and the window's well goes with it in the frame
/// the drag is drawn in: the window policy reads the well, so a window stands in its frame
/// wherever the person carries the canvas.
#[test]
fn a_canvas_pan_carries_the_windows_well_with_it() {
    let (mut part, wells) = showing_one_window();
    click(&mut part, "Showing Strips");
    click(&mut part, "Frames");
    for _ in 0..30 {
        part.prepare_frame();
    }
    let before = wells.well("p1").expect("the place is a well on the canvas");
    // A point on the canvas' ground, clear of the place: below and right of its well.
    let (x, y) = (before.x1 + 40.0, before.y1 + 40.0);
    part.pointer_motion(x, y);
    part.prepare_frame();
    part.pointer_button(0x112, true);
    part.prepare_frame();
    let mut seen = Vec::new();
    for step in 1..=5 {
        part.pointer_motion(x - 20.0 * f64::from(step), y - 10.0 * f64::from(step));
        part.prepare_frame();
        seen.push(wells.well("p1"));
    }
    part.pointer_button(0x112, false);
    part.prepare_frame();
    let after = wells.well("p1").expect("the place is still a well");
    assert!(
        (after.x0 - (before.x0 - 100.0)).abs() < 1.0 && (after.y0 - (before.y0 - 50.0)).abs() < 1.0,
        "the well moved with the canvas: {before:?} -> {after:?}, along the way {seen:?}"
    );
}

/// A window that arrives past the canvas' edge is shown: the canvas pans to it, and on every
/// frame of the way each window's well has moved with the canvas, the one that stood there and
/// the one arriving alike, so no window trails its frame.
#[test]
fn the_canvas_pans_to_a_window_arriving_past_its_edge_carrying_every_well() {
    let wells = ui::widget::Wells::new();
    let (mut part, link) = super::tests_support::part_as(reading(), Some(wells.clone()));
    link.publish_arrangement(one_window_arrangement());
    click(&mut part, "Showing Strips");
    click(&mut part, "Frames");
    for _ in 0..30 {
        part.prepare_frame();
    }
    let mut arrangement = one_window_arrangement();
    let mut far = arrangement.strips[0].clone();
    far.key = "s2".to_owned();
    far.canvas_x = 1100;
    arrangement.strips.push(far);
    let mut arriving = arrangement.places[0].clone();
    arriving.key = "p2".to_owned();
    arriving.strip = "s2".to_owned();
    arrangement.places.push(arriving);
    link.publish_arrangement(arrangement);
    let started = std::time::Instant::now();
    let mut frames = Vec::new();
    while started.elapsed() < std::time::Duration::from_secs(2) {
        part.prepare_frame();
        if let (Some(one), Some(two)) = (wells.well("p1"), wells.well("p2")) {
            frames.push((one, two));
        }
        std::thread::sleep(std::time::Duration::from_millis(8));
    }
    let (_, last) = *frames.last().expect("the arriving place is a well");
    assert!(
        last.x0 >= 0.0 && last.x1 <= 1280.0,
        "the window that arrived stands whole on the screen: {last:?}"
    );
    let apart = frames[0].1.x0 - frames[0].0.x0;
    assert!(
        frames
            .iter()
            .all(|(one, two)| ((two.x0 - one.x0) - apart).abs() < 1.0),
        "every well moved with the canvas on every frame: {frames:?}"
    );
}
