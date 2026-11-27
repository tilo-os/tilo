#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "test fixtures"
)]

use super::*;
use tilo_session_core::surfaces::Published;
use tilolang_tilo::origin::OriginType;
use tilolang_tilo::runtime::origin::OriginWrite;
use tilolang_tilo::runtime::store::Store as _;
use tilolang_tilo::runtime::{ResourceRef, Value};
use tilolang_tilo::shell::{Mirror, SHELL_FILES};

fn reading() -> LayoutReading {
    LayoutReading {
        person: "Pat".to_owned(),
        rows: vec![LayoutRowReading {
            key: "r1".to_owned(),
            workspace: "1".to_owned(),
            name: "Main".to_owned(),
            current: true,
            ..LayoutRowReading::default()
        }],
        workspaces: vec![
            LayoutWorkspaceReading {
                key: "1".to_owned(),
                name: "Home".to_owned(),
                current: true,
                home: String::new(),
            },
            LayoutWorkspaceReading {
                key: "2".to_owned(),
                name: "Studio".to_owned(),
                current: false,
                home: String::new(),
            },
        ],
        ..LayoutReading::default()
    }
}

use crate::shell::part::tests_support::{
    compiled, part_of, part_with, publish_notices, publish_time,
};

fn part() -> (ShellPart, Rc<SessionLink>) {
    part_with(reading())
}

/// The canvas, as assistive technology names it: the map the framed style stands its places
/// on, whose free ground a press puts the caret on.
const CANVAS: &str = "Canvas";
/// The launcher's field, announced the same way: the view's own words for it, not the state's
/// name in the code (`query`).
const LAUNCHER_SEARCH: &str = "Search apps, windows, actions and files";
/// The workspaces surface's field, announced the same way rather than by the state's name in
/// the code (`name`).
const WORKSPACE_NAME: &str = "Name a new workspace";

fn click(part: &mut ShellPart, name: &str) {
    let (x, y, w, h) = part
        .control_bounds(name)
        .unwrap_or_else(|| panic!("the bar shows {name}: {:?}", part.accessible_labels()));
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
            let key = tilo_session_core::surface::SurfaceKey {
                keycode: 0,
                at: std::time::Instant::now(),
                pressed,
                repeat: false,
                keysym: u32::from(letter),
                unmodified_keysym: u32::from(letter),
                latin_keysym: u32::from(letter),
                text: pressed.then(|| letter.to_string()),
                modifiers: tilo_session_core::surface::KeyModifiers::default(),
            };
            tilo_session_core::surface::ShellSurface::key(part, &key);
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
/// compiles with no outside effect at all, so no press of it is a string the API reads.
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
/// package (`use so.tilo.session as session`), which the checker holds to what the API takes.
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
///
/// The centre's presses call the centre's own actions (`device.dismiss(device.notifications,
/// ...)`): Clear all and a notice's Dismiss each reach the session as that action, at that press
/// rather than at the next input.
#[test]
fn the_centre_draws_nothing_at_rest_and_a_press_in_it_hands_the_session_its_notice_action() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut part, link) =
        part_with(super::tests_support::Staged::from(reading()).showing("centre"));
    publish_notices(
        &link,
        &serde_json::json!([{
            "number": 1,
            "title": "Backup finished",
            "body": "12 GB safe",
            "from": "Settings",
            "kind": "info",
            "seen": true,
            "actions": [],
        }]),
    );
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
    // The idle frames ran and drew nothing, so the centre the presses land on is the one that
    // settled. Each press is answered, as the session's turn answers it, before the next.
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
                resource: "session:notifications".to_owned(),
                action: "dismiss".to_owned(),
                arguments: BTreeMap::from([("notification".to_owned(), 1.into())]),
                from: None,
            },
            Ask {
                resource: "session:notifications".to_owned(),
                action: "clear".to_owned(),
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
    let effects = part
        .instance
        .host()
        .map(|host| host.store().pending_effects().len());
    assert!(
        link.has_asks(),
        "the press reached the session: effects {effects:?}, {:?}, refusals {:?}",
        part.work(),
        part.refusals()
    );
    let opening = link.asked.borrow().front().map(|(ask, _)| ask.clone());
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
    let _settled = tilo_ui_base::motion::testing::settled();
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
        serde_json::json!({"present": true, "percentage": 71, "state": "discharging", "on_battery": true}),
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
        serde_json::json!({"present": true, "percentage": 71, "state": "charging", "on_battery": false}),
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
            link.asked
                .borrow_mut()
                .pop_front()
                .map(|(ask, _)| ask.action),
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut part, link) = part_with(LayoutReading {
        people: vec![
            LayoutPersonReading {
                key: "mia".to_owned(),
                name: "Mia Clayton".to_owned(),
            },
            LayoutPersonReading {
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
    let ask = link.asked.borrow_mut().pop_front().map(|(ask, _)| ask);
    let ask = ask.unwrap_or_else(|| panic!("the press asks; refusals {:?}", part.refusals()));
    assert_eq!(ask.action, "show", "the button shows who is here");
    assert!(
        ask.resource.contains("people"),
        "it shows the people list: {}",
        ask.resource
    );
}

/// What one discrete change costs the bar: printed per kind of change, for the record
/// of its frame budget; each must land in one turn.
#[test]
fn each_change_is_one_turn() {
    type Change = (&'static str, fn(&mut LayoutReading, &SessionLink));
    let (mut part, link) = part();
    for _ in 0..4 {
        part.prepare_frame();
    }
    let mut now = reading();
    let changes: [Change; 3] = [
        ("minute", |_, link| publish_time(link, "4:29 PM")),
        ("notification", |_, link| {
            publish_notices(
                link,
                &serde_json::json!([{
                    "number": 1,
                    "title": "Backup finished",
                    "body": "",
                    "from": "",
                    "kind": "info",
                    "seen": false,
                    "actions": [],
                }]),
            );
        }),
        ("workspace switch", |r, _| {
            // The reading lists the strip the bar shows first, so the switch moves the
            // mark on to the workspace after it.
            let [home, studio, ..] = r.workspaces.as_mut_slice() else {
                panic!("the reading lists both of the bar's workspaces");
            };
            home.current = false;
            studio.current = true;
        }),
    ];
    for (name, change) in changes {
        change(&mut now, &link);
        link.publish_layout(now.clone());
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
    click(&mut part, "Home");
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

/// A menu the person opened stands while the layout under it changes: a workspace the session
/// makes while they are choosing lists itself there, instead of the menu vanishing.
#[test]
fn the_workspaces_menu_stands_while_the_layout_changes_under_it() {
    let (mut part, link) = part();
    click(&mut part, "Home");
    assert!(part.control_bounds("Studio").is_some(), "the menu is up");
    // The session makes another workspace, in front.
    let mut reading = reading();
    reading.workspaces.push(LayoutWorkspaceReading {
        key: "3".to_owned(),
        name: "Garden".to_owned(),
        current: true,
        home: String::new(),
    });
    for workspace in &mut reading.workspaces {
        workspace.current = workspace.key == "3";
    }
    link.publish_layout(reading);
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    assert!(
        part.control_bounds("Studio").is_some(),
        "the menu stands while the layout changes under it"
    );
    assert!(
        part.control_bounds("Garden").is_some(),
        "and lists the workspace the session made under it"
    );
    // And still chooses: the workspace in front is the one the person presses there.
    click(&mut part, "Studio");
    let asked = link.asked.borrow().front().map(|(ask, _)| ask.clone());
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
}

#[test]
fn choosing_a_workspace_in_the_menu_asks_the_session_to_switch() {
    let (mut part, link) = part();
    click(&mut part, "Home");
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    click(&mut part, "Studio");
    let asked = link.asked.borrow().front().map(|(ask, _)| ask.clone());
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
/// (`session.open_app(app)`), named by its app id: the session's API gets it on the shell's
/// chrome, and the app's place on the shell comes to the front.
#[test]
fn a_press_on_a_dock_app_asks_the_session_to_open_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
        .root_resource(&app, &shell)
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
            &tilolang_tilo::runtime::ids::MemberId::person("Pat").into(),
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
    // The runtime wrote the pin to the shell's store: the next turn draws the dock over it.
    publish_time(&link, "4:29 PM");
    for _ in 0..6 {
        part.prepare_frame();
    }
    let labels = tilo_session_core::surfaces::Published::accessible_labels(&mut part);
    assert!(
        part.control_bounds("Sheets").is_some(),
        "the dock shows Sheets: {labels:?}"
    );
    click(&mut part, "Sheets");
    let asked: Vec<Ask> = link
        .asked
        .borrow()
        .iter()
        .map(|(ask, _)| ask.clone())
        .collect();
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

/// A window fullscreen on the display a part stands on owns that display: the part steps aside,
/// with not a row of it over the window and no pointer taken there, and it is back over the
/// whole screen when the window leaves fullscreen. A window fullscreen on another display
/// leaves the part in place, and so does a surface of the shell's that is up: it is the person's
/// to look at over any window, a fullscreen one too.
#[test]
fn a_part_steps_aside_only_for_a_window_fullscreen_on_its_own_display() {
    // The surface that is up (`""`: the shell at rest), the displays fullscreen windows take,
    // and whether the part, which stands on `HEADLESS-1`, steps aside.
    let rows: [(&str, &[&str], bool); 4] = [
        ("", &["HEADLESS-1"], true),
        ("", &["HEADLESS-2"], false),
        ("", &["HEADLESS-2", "HEADLESS-1"], true),
        ("centre", &["HEADLESS-1"], false),
    ];
    let packages = compiled();
    for (here, fullscreen, aside) in rows {
        let (mut part, _link) = part_of(
            &packages,
            super::tests_support::Staged::from(reading()).showing(here),
            (1280, 800),
        );
        assert!(
            !part.aside(),
            "no window owns the display: the part stands ({here:?} up)"
        );
        assert_eq!(
            part.widgets.placement().size,
            (1280, 800),
            "the screen ({here:?} up)"
        );
        part.set_fullscreen(fullscreen.iter().map(|name| (*name).to_owned()).collect());
        assert_eq!(
            part.aside(),
            aside,
            "with a window fullscreen on {fullscreen:?} and {here:?} up"
        );
        if aside {
            // The window took the display the part stands on: it draws nothing there.
            assert_eq!(
                part.widgets.placement().size.1,
                1,
                "nothing of the part covers the window on {fullscreen:?}"
            );
            assert!(
                !part.accepts_pointer(100.0, 10.0),
                "the window under it takes the pointer on {fullscreen:?}"
            );
            assert!(
                part.layers().is_empty(),
                "not a row of the part is handed to the compositor over the window on \
                 {fullscreen:?}"
            );
        } else {
            assert_eq!(
                part.widgets.placement().size,
                (1280, 800),
                "the part stands over its screen with {fullscreen:?} fullscreen and {here:?} up"
            );
        }
        // The window leaves fullscreen: the part is back, over the whole screen.
        part.set_fullscreen(Vec::new());
        assert!(!part.aside(), "back after {fullscreen:?}");
        assert_eq!(
            part.widgets.placement().size,
            (1280, 800),
            "back over the screen after {fullscreen:?}"
        );
    }
}

#[test]
fn a_field_put_in_focus_takes_the_keys_and_a_press_elsewhere_hands_them_back() {
    // The compositor routes a key to the topmost surface that wants the keyboard. A part that
    // never says it wants it never hears a key, however the field is focused: the canvas's
    // caret (and every field the shell draws) stays empty.
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut part, _link) = part();
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    assert!(
        !ShellSurface::wants_keyboard(&part),
        "a part nobody typed in leaves the keys to the window in front"
    );
    click(&mut part, "Showing Strips");
    click(&mut part, "Frames");
    click(&mut part, CANVAS);
    assert!(
        ShellSurface::text_input(&part).is_some(),
        "the press on the canvas's free ground put the caret there"
    );
    assert!(
        ShellSurface::wants_keyboard(&part),
        "the caret takes the keys: what is typed goes to the part"
    );
    type_into(&mut part, "groceries");
    assert!(
        part.says("groceries"),
        "the entry shows what was typed: {:?}",
        part.accessible_labels()
    );
    // A press on a window over the shell (or anywhere off the part) hands the keys back: what
    // is typed next reaches what the person pressed on, and the entry keeps its words.
    ShellSurface::pressed_elsewhere(&mut part, 300.0, 300.0);
    part.prepare_frame();
    assert!(
        ShellSurface::text_input(&part).is_none(),
        "the field gave the keyboard up"
    );
    assert!(
        !ShellSurface::wants_keyboard(&part),
        "the part leaves the keys to the window again"
    );
    assert!(
        part.says("groceries"),
        "what was typed stands in the entry: {:?}",
        part.accessible_labels()
    );
}

#[test]
fn a_direction_the_views_have_nothing_left_for_is_the_desktops() {
    // The compositor hands the shell's part the keys while its own controls hold the focus. A
    // direction no control of its views lies that way is not the shell's: it asks the desktop to
    // move the focus on, so the bar and the windows are one order under the controller.
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut part, _link) = part();
    for _ in 0..4 {
        part.prepare_frame();
        part.draw();
    }
    let steps: Rc<std::cell::RefCell<Vec<FocusStep>>> = Rc::default();
    let held = Rc::clone(&steps);
    part.on_focus_leave(Rc::new(move |step| held.borrow_mut().push(step)));

    // The first direction seats the ring on the shell's first control: the key stays the views'.
    ShellSurface::key(&mut part, &super::tests_support::key(0xff54, true));
    assert!(
        part.widgets.takes_keys(),
        "the direction seated the ring on a control of the shell's"
    );
    assert!(
        steps.borrow().is_empty(),
        "the key that seated the ring did not leave the views: {:?}",
        steps.borrow()
    );

    // Leftwards at the leading edge of the ring, no control of the shell's lies that way: the
    // desktop moves the focus on to the window beside it.
    for _ in 0..6 {
        if !steps.borrow().is_empty() {
            break;
        }
        ShellSurface::key(&mut part, &super::tests_support::key(0xff51, true));
    }
    assert!(
        !steps.borrow().is_empty(),
        "a direction at the edge of the shell's controls left the views"
    );
    assert!(
        steps.borrow().iter().all(|step| *step == FocusStep::Left),
        "the desktop is told the way the press points: {:?}",
        steps.borrow()
    );
    assert!(
        part.widgets.takes_keys(),
        "with no window to go to the shell keeps its ring: the desktop takes it away when one takes the focus"
    );
}

#[test]
fn a_shell_holding_the_desktops_focus_takes_the_keys_a_window_would() {
    // No window holds the keyboard: the desktop's focus is the shell's, so its bar, dock and
    // surfaces answer the person's keys — how an empty desktop is reached by keyboard and by a
    // controller.
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut part, link) = part();
    for _ in 0..4 {
        part.prepare_frame();
        part.draw();
    }
    assert!(
        !ShellSurface::wants_keyboard(&part),
        "a window in front keeps the keys while the shell's own controls hold none"
    );
    link.seat().set(true);
    assert!(
        ShellSurface::wants_keyboard(&part),
        "the shell holding the desktop's focus answers the keys"
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
        !tilo_session_core::surfaces::Published::offered(&part),
        "the API reaches the bars still there, not this one"
    );
    // The compositor tells it of the outputs left: it does not stretch over them.
    part.outputs_changed(&[tilo_session_core::surface::SurfaceOutput {
        rect: (1280, 0, 1280, 800),
        scale: 1.0,
    }]);
    assert_eq!(part.placement(), None, "it stays off the other display");
    // The display comes back: the part stands on it again.
    part.set_display(Some(display.clone()), 1.0);
    assert_eq!(part.placement(), Some(display));
    assert!(tilo_session_core::surfaces::Published::offered(&part));
    assert_eq!(
        part.widgets.placement().size,
        (1280, 800),
        "its whole screen"
    );
}

/// The person's layout: one strip on the shell, its one place a foreign window's (no
/// record), keyed `p1`.
fn one_window_layout() -> LayoutReading {
    LayoutReading {
        person: "Pat".to_owned(),
        front: String::new(),
        people: Vec::new(),
        queries: Vec::new(),
        recents: Vec::new(),
        workspaces: vec![LayoutWorkspaceReading {
            key: "w1".to_owned(),
            name: "Home".to_owned(),
            current: true,
            home: String::new(),
        }],
        rows: vec![LayoutRowReading {
            key: "r1".to_owned(),
            workspace: "w1".to_owned(),
            name: "Main".to_owned(),
            shell: String::new(),
            current: true,
            ..LayoutRowReading::default()
        }],
        strips: vec![LayoutStripReading {
            key: "s1".to_owned(),
            workspace: "w1".to_owned(),
            row: "r1".to_owned(),
            width: 600,
            placed: true,
            canvas_width: 600,
            canvas_height: 400,
            ..LayoutStripReading::default()
        }],
        places: vec![LayoutPlaceReading {
            key: "p1".to_owned(),
            strip: "s1".to_owned(),
            shows: "Terminal".to_owned(),
            ..LayoutPlaceReading::default()
        }],
    }
}

/// One row of one workspace whose two strips stand on two displays, each showing one window:
/// "Groceries" on display 0 and "Packing list" on display 1.
fn two_display_reading() -> LayoutReading {
    LayoutReading {
        person: "Pat".to_owned(),
        rows: vec![LayoutRowReading {
            key: "r1".to_owned(),
            workspace: "w1".to_owned(),
            name: "Main".to_owned(),
            current: true,
            ..LayoutRowReading::default()
        }],
        workspaces: vec![LayoutWorkspaceReading {
            key: "w1".to_owned(),
            name: "Home".to_owned(),
            current: true,
            ..LayoutWorkspaceReading::default()
        }],
        strips: vec![
            LayoutStripReading {
                key: "s0".to_owned(),
                workspace: "w1".to_owned(),
                row: "r1".to_owned(),
                display: 0,
                width: 500,
                active: "p0".to_owned(),
                ..LayoutStripReading::default()
            },
            LayoutStripReading {
                key: "s1".to_owned(),
                workspace: "w1".to_owned(),
                row: "r1".to_owned(),
                display: 1,
                width: 500,
                active: "p1".to_owned(),
                ..LayoutStripReading::default()
            },
        ],
        places: vec![
            LayoutPlaceReading {
                key: "p0".to_owned(),
                strip: "s0".to_owned(),
                title: "Groceries".to_owned(),
                ..LayoutPlaceReading::default()
            },
            LayoutPlaceReading {
                key: "p1".to_owned(),
                strip: "s1".to_owned(),
                title: "Packing list".to_owned(),
                ..LayoutPlaceReading::default()
            },
        ],
        ..LayoutReading::default()
    }
}

/// Each display's part draws its own display's strips and no other's, and only the display in use
/// carries the whole bar. The part reads the display it stands on as its own (`device.screen`),
/// so the strips it filters by that display are its own, and its bar is the whole one exactly
/// when it is the display in use.
#[test]
fn a_part_draws_its_own_displays_strips_and_no_others() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let reading = two_display_reading();
    let display =
        |id: i64, connector: &str, primary: bool| tilo_session_core::shell::LayoutDisplayReading {
            id,
            connector: connector.to_owned(),
            name: connector.to_owned(),
            width: 1280,
            height: 800,
            scale: 1.0,
            primary,
            ..tilo_session_core::shell::LayoutDisplayReading::default()
        };
    let (mut first, _) = part_with(reading.clone());
    first.set_screen(Some(display(0, "HEADLESS-1", true)));
    for _ in 0..6 {
        first.prepare_frame();
        first.draw();
    }
    assert!(
        first.says("Groceries"),
        "the display in use draws its own window: {:?}",
        first.accessible_labels()
    );
    assert!(
        !first.says("Packing list"),
        "and no other display's: {:?}",
        first.accessible_labels()
    );
    assert!(
        first.says("Notifications"),
        "the display in use carries the whole bar: {:?}",
        first.accessible_labels()
    );
    let (mut second, _) = super::tests_support::part_beside(&first, reading);
    second.set_screen(Some(display(1, "HEADLESS-2", false)));
    for _ in 0..6 {
        second.prepare_frame();
        second.draw();
    }
    assert!(
        second.says("Packing list"),
        "the other display draws its own window: {:?}",
        second.accessible_labels()
    );
    assert!(
        !second.says("Groceries"),
        "and no other display's: {:?}",
        second.accessible_labels()
    );
    assert!(
        !second.says("Notifications"),
        "the other display carries the compact bar, with no bell: {:?}",
        second.accessible_labels()
    );
}

/// Where the menu of the window the part shows as `name` stands, when it shows one. The strips
/// say a window's name three times: its frame is named for it, and so is the bar it is lifted
/// and aimed at by, both of them groups, and so is the window's own menu in that bar, which lists
/// the commands of the place the window shows. The menu is the control of that name, not a group,
/// whose centre the frame (the largest of them) holds. A window named by its frame alone has none.
fn window_menu(part: &mut ShellPart, name: &str) -> Option<(f64, f64, f64, f64)> {
    let (ox, oy) = part.widgets.placement().origin;
    let host = part.widgets.host_mut();
    let ids: std::collections::HashSet<_> = host.a11y_nodes_named(name).into_iter().collect();
    let named: Vec<_> = host
        .a11y_nodes()
        .into_iter()
        .filter(|(id, _)| ids.contains(id))
        .filter_map(|(_, node)| {
            let rect = node.bounds()?;
            let bounds = (
                f64::from(ox) + rect.x0,
                f64::from(oy) + rect.y0,
                rect.width(),
                rect.height(),
            );
            Some((node.role() == ui::accesskit::Role::Group, bounds))
        })
        .collect();
    let frame = named
        .iter()
        .map(|&(_, bounds)| bounds)
        .max_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))?;
    named
        .into_iter()
        .filter(|&(group, (x, y, w, h))| {
            let (across, down) = (x + w / 2.0, y + h / 2.0);
            !group
                && across >= frame.0
                && across <= frame.0 + frame.2
                && down >= frame.1
                && down <= frame.1 + frame.3
        })
        .map(|(_, bounds)| bounds)
        .min_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))
}

/// A part over the layout of one foreign window, its wells reporting to the handle it
/// returns.
fn showing_one_window() -> (ShellPart, ui::widget::Wells) {
    let wells = ui::widget::Wells::new();
    let (mut part, link) = super::tests_support::part_as(reading(), Some(wells.clone()));
    link.publish_layout(one_window_layout());
    for _ in 0..6 {
        part.prepare_frame();
    }
    (part, wells)
}

/// The whole screen shows the person's layout between the bar and the dock, under the
/// preset switch, and a foreign window's place is a well reported to the window policy's
/// handle, on the output under the bar.
///
/// The framed canvas the switch then shows pans under a middle drag, and the window's well goes
/// with it in the frame the drag is drawn in: the window policy reads the well, so a window
/// stands in its frame wherever the person carries the canvas.
#[test]
fn the_screen_shows_the_layout_and_a_canvas_pan_carries_each_windows_well() {
    let (mut part, wells) = showing_one_window();
    assert!(part.below_windows(), "the windows stand over the screen");
    let labels = tilo_session_core::surfaces::Published::accessible_labels(&mut part);
    for name in ["Notifications", "Showing Strips"] {
        assert!(
            labels.iter().any(|label| label == name),
            "the screen shows {name}: {labels:?}"
        );
    }
    // The preset switch is the bar's own menu, named by the preset in use: the ways of showing
    // the same layout are its items, offered when the person opens it.
    click(&mut part, "Showing Strips");
    let labels = tilo_session_core::surfaces::Published::accessible_labels(&mut part);
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
    let labels = tilo_session_core::surfaces::Published::accessible_labels(&mut part);
    let named = part.control_bounds_all("Terminal");
    assert!(
        !named.is_empty()
            && window_menu(&mut part, "Terminal").is_none()
            && labels.iter().any(|label| label == CANVAS),
        "the canvas shows: {labels:?}; Terminal names {named:?}; refused {:?}",
        part.refusals()
    );
    assert!(
        wells.well("p1").is_some_and(|well| well.width() >= 1.0),
        "the place is a well on the canvas"
    );

    // The canvas the two presses showed is the one the person now carries with a middle drag.
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

/// A surface of the shell's stands over the windows it is over: the whole screen lies under
/// them at rest, so its wells hold them, and the overview — with the pictures it names — comes
/// up over every window rather than underneath the window it is showing.
#[test]
fn a_surface_of_the_shells_stands_over_the_windows() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut part, link) = super::tests_support::part_as(reading(), None);
    for _ in 0..4 {
        part.prepare_frame();
    }
    assert!(
        part.below_windows(),
        "the shell at rest stands under the windows"
    );
    super::tests_support::publish_overlays(&link, "overview", false);
    for _ in 0..4 {
        part.prepare_frame();
    }
    assert!(
        !part.below_windows(),
        "the overview stands over the windows"
    );
    super::tests_support::publish_overlays(&link, "none", false);
    for _ in 0..4 {
        part.prepare_frame();
    }
    assert!(
        part.below_windows(),
        "and goes under them again when it closes"
    );
}

/// The overview over a window on each of `strips`, the first window's on the first strip. Each
/// strip stands in a row of its own, named as the strip is.
fn overview_with(strips: &[&str]) -> super::tests_support::Staged {
    let mut shell = reading();
    shell.rows = strips
        .iter()
        .enumerate()
        .map(|(at, name)| LayoutRowReading {
            key: format!("r{at}"),
            workspace: "1".to_owned(),
            name: (*name).to_owned(),
            current: at == 0,
            ..LayoutRowReading::default()
        })
        .collect();
    shell.strips = (0..strips.len())
        .map(|at| LayoutStripReading {
            key: at.to_string(),
            workspace: "1".to_owned(),
            row: format!("r{at}"),
            placed: true,
            ..LayoutStripReading::default()
        })
        .collect();
    super::tests_support::Staged {
        shell,
        here: "overview".to_owned(),
        revealed: false,
        windows: strips
            .iter()
            .enumerate()
            .map(|(at, strip)| {
                serde_json::json!({
                    "key": (at + 7).to_string(),
                    "group_key": format!("g{at}"),
                    "title": format!("Window {at}"),
                    "origin_app": "so.tilo.notes",
                    "app_key": "so.tilo.notes",
                    "icon": "app_window",
                    "strip": strip,
                    "strip_key": at.to_string(),
                    "workspace": "Home",
                    "workspace_key": "1",
                    "recency": at,
                    "focused": at == 0,
                    "preview": "none",
                })
            })
            .collect(),
    }
}

/// A window's menu runs the item pressed wherever the menu stands: past the end of what the
/// overview lays out (one strip, the menu hanging below it), and over the next strip's window
/// (two strips), which is drawn later and so asked first. Either way the press is the menu's.
#[test]
fn a_press_on_a_window_menus_item_runs_it_wherever_the_menu_hangs() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let packages = compiled();
    for strips in [&["Main"][..], &["Main", "Research"][..]] {
        let (mut part, link) = part_of(&packages, overview_with(strips), (1280, 800));
        for _ in 0..6 {
            part.prepare_frame();
            part.draw();
        }
        click(&mut part, "This window");
        for _ in 0..6 {
            part.prepare_frame();
            part.draw();
        }
        click(&mut part, "Close this window");
        assert_eq!(
            link.asked.borrow().front().map(|(ask, _)| ask.clone()),
            Some(Ask {
                resource: "desktop:chrome".to_owned(),
                action: "close-window".to_owned(),
                arguments: BTreeMap::from([("group".to_owned(), "g0".into())]),
                from: None,
            }),
            "with strips {strips:?}"
        );
        assert!(
            part.control_bounds("Close this window").is_none(),
            "the menu closed with strips {strips:?}"
        );
    }
}

/// Press `name` as the compositor hands the person's press of `serial` to the shell: the button
/// going down and coming up are each delivered as that press.
fn click_pressed(part: &mut ShellPart, name: &str, serial: u32) {
    let (x, y, w, h) = part
        .control_bounds(name)
        .unwrap_or_else(|| panic!("the shell shows {name}: {:?}", part.accessible_labels()));
    part.pointer_motion(x + w / 2.0, y + h / 2.0);
    part.prepare_frame();
    for pressed in [true, false] {
        tilo_session_core::surface::handling_press(Some(serial), || {
            part.pointer_button(0x110, pressed);
        });
        part.prepare_frame();
    }
    for _ in 0..6 {
        part.prepare_frame();
    }
}

/// A person's press on a window menu's item runs the item's call as that press, though the call
/// waits in the shell's queue for the session's next pass: what the call does is the person's own
/// act, so a call it makes in turn does not ask them again. The same item chosen by no press of
/// theirs runs as none, and the press of the first choice is not the second's.
#[test]
fn a_window_menu_item_the_person_pressed_runs_its_call_as_their_press() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let packages = compiled();
    let (mut part, link) = part_of(&packages, overview_with(&["Main"]), (1280, 800));
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    let mut served = Vec::new();
    let mut serve = |link: &SessionLink| {
        link.serve(|ask| {
            served.push((
                ask.action.clone(),
                tilo_session_core::surface::handled_press(),
            ));
            Ok(())
        });
    };
    click_pressed(&mut part, "This window", 41);
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    click_pressed(&mut part, "Close this window", 42);
    serve(&link);
    // Opened again and chosen by input the compositor attests no press of the person's for.
    click(&mut part, "This window");
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    click(&mut part, "Close this window");
    serve(&link);
    assert_eq!(
        served,
        vec![
            ("close-window".to_owned(), Some(42)),
            ("close-window".to_owned(), None),
        ],
        "refusals {:?}",
        part.refusals()
    );
}

/// A window's menu carries it to another row of its workspace by its place's own `move`, the row
/// named as the layout publishes it.
#[test]
fn moving_a_window_to_another_row_moves_its_place_there() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
    // The menu open is either window's: each offers the other's row.
    let labels = tilo_session_core::surfaces::Published::accessible_labels(&mut part);
    let (item, place, row) = if labels.iter().any(|label| label == "Move to Research") {
        ("Move to Research", "7", "r1")
    } else {
        assert!(
            labels.iter().any(|label| label == "Move to Main"),
            "the menu offers the other row: {labels:?}"
        );
        ("Move to Main", "8", "r0")
    };
    click(&mut part, item);
    assert!(part.refusals().is_empty(), "{:?}", part.refusals());
    let asked = link.asked.borrow().front().map(|(ask, _)| ask.clone());
    assert_eq!(
        asked,
        Some(Ask {
            resource: format!("shell:place:{place}"),
            action: "move".to_owned(),
            arguments: BTreeMap::from([("row".to_owned(), format!("shell:row:{row}").into())]),
            from: None,
        })
    );
}

#[test]
fn a_workspace_named_in_the_workspaces_surface_is_made_with_its_name() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut part, link) =
        part_with(super::tests_support::Staged::from(reading()).showing("workspaces"));
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
            .map(|(ask, _)| (ask.action.clone(), ask.arguments.clone())),
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
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut part, link) =
        part_with(super::tests_support::Staged::from(reading()).showing("launcher"));
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    click(&mut part, LAUNCHER_SEARCH);
    for letter in "quarterly".chars() {
        for pressed in [true, false] {
            let key = tilo_session_core::surface::SurfaceKey {
                keycode: 0,
                at: std::time::Instant::now(),
                pressed,
                repeat: false,
                keysym: u32::from(letter),
                unmodified_keysym: u32::from(letter),
                latin_keysym: u32::from(letter),
                text: pressed.then(|| letter.to_string()),
                modifiers: tilo_session_core::surface::KeyModifiers::default(),
            };
            tilo_session_core::surface::ShellSurface::key(&mut part, &key);
            part.prepare_frame();
        }
    }
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    // Enter on what was typed, with nothing here it names: the platform's search for it.
    for pressed in [true, false] {
        tilo_session_core::surface::ShellSurface::key(
            &mut part,
            &super::tests_support::key(0xff0d, pressed),
        );
        part.prepare_frame();
    }
    // The shell's search follows the field letter by letter, so each letter that reached it is
    // a search for the text so far; Enter then asks for what was typed whole.
    let searched: Vec<Ask> = link
        .asked
        .borrow()
        .iter()
        .map(|(ask, _)| ask.clone())
        .filter(|ask| ask.action == "search")
        .collect();
    let search = |query: &str| Ask {
        resource: "desktop:shell".to_owned(),
        action: "search".to_owned(),
        arguments: BTreeMap::from([("query".to_owned(), query.into())]),
        from: None,
    };
    let typed = "quarterly";
    let expected: Vec<Ask> = (1..=typed.len())
        .map(|letters| search(typed.get(..letters).unwrap_or_default()))
        .chain(std::iter::once(search(typed)))
        .collect();
    assert_eq!(searched, expected);
}

/// A part told its display only after it settled (the session's first frames can run before
/// its output is known) lays its bar out at the display's width: the strip, which a narrow bar
/// gives way, stands on a wide one though nothing of the shell changed since.
#[test]
fn a_part_told_its_display_after_it_settled_lays_out_at_the_displays_width() {
    let packages = crate::shell::part::compile_shell().unwrap_or_else(|why| panic!("{why}"));
    let app = packages.last().unwrap().meta.app.clone();
    let link = Rc::new(SessionLink::default());
    crate::shell::part::tests_support::publish_platform_roots(&link);
    link.publish_layout(reading());
    let mut part = ShellPart::new(
        ShellIdentity {
            app: &app,
            packages,
            person: "Pat",
            link: Rc::clone(&link),
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            seed: true,
            kept_elsewhere: false,
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
    part.outputs_changed(&[tilo_session_core::surface::SurfaceOutput {
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
        tilo_session_core::surfaces::Published::accessible_labels(&mut part)
    );
}

/// With a surface up the part covers its output, and the shell fills it: the dock rides the
/// output's bottom edge however short the surface is (the notifications with none), on a
/// phone and on a desktop, never right under what the surface holds.
#[test]
fn with_a_short_surface_up_the_dock_rides_the_outputs_bottom_edge() {
    let packages = compiled();
    for (width, height) in [(390, 844), (1280, 800)] {
        let (mut part, _link) = part_of(
            &packages,
            super::tests_support::Staged::from(reading()).showing("centre"),
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

/// The room a widget rests in is what the chrome the style laid out leaves of the display: the
/// bar's band comes off the top and the dock's off the foot, at whatever height the theme and
/// the person's settings gave them, so a widget placed at the foot stands above the dock rather
/// than under it.
#[test]
fn the_room_a_widget_rests_in_is_what_the_bar_and_dock_left() {
    let (mut part, _link) = part();
    for _ in 0..4 {
        part.prepare_frame();
    }
    let measured = part
        .origin
        .borrow()
        .room
        .borrow()
        .clone()
        .expect("the part measured the room its chrome leaves");
    let room: tilo_device_surfaces::room::Room =
        serde_json::from_value(measured).expect("the room reads");
    assert!(
        room.x.abs() < 0.5,
        "nothing stands at the leading edge: {room:?}"
    );
    assert!(
        (room.width - 1280.0).abs() < 0.5,
        "nothing narrows the room across the display: {room:?}"
    );
    // Every node the bar names — its body and the run it holds — stands above the room.
    let bar = part.control_bounds_all("Top bar");
    assert!(!bar.is_empty(), "the bar stands across the top");
    for (_, y, _, height) in bar {
        assert!(
            room.y + 0.5 >= y + height,
            "the bar's band comes off the top: {room:?}, with the bar at {y}+{height}"
        );
    }
    // The dock's body takes the display's foot: the room ends at its top edge.
    let dock = part.control_bounds_all("Dock");
    let docks_top = dock
        .iter()
        .map(|(_, y, _, _)| *y)
        .fold(f64::INFINITY, f64::min);
    assert!(docks_top.is_finite(), "the dock rides the foot");
    assert!(
        (room.y + room.height - docks_top).abs() < 0.5,
        "the dock's band comes off the foot: {room:?}, with the dock at {docks_top}"
    );
}

/// A display wide enough for every part: the bar names the strip and shows the person the
/// session publishes with nothing touching the shell. The part hands the app the space the
/// frame gave its view the moment it is laid out, so the bar is full rather than waiting for
/// the person's first press to hear how much space it has.
#[test]
fn a_wide_bar_shows_the_strip_and_the_person_with_nothing_touched() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut part, _link) = part();
    for _ in 0..8 {
        part.prepare_frame();
    }
    let labels: Vec<String> = tilo_session_core::surfaces::Published::accessible_labels(&mut part);
    assert!(
        labels.iter().any(|label| label == "Main"),
        "the bar names the strip: {labels:?}"
    );
    assert!(
        labels.iter().any(|label| label == "Pat: open your card"),
        "the bar shows the person: {labels:?}"
    );
}

/// The person's own face is pressed where it is drawn on every width of bar: it stands inside the
/// display, and a pointer's press on it opens their own card.
#[test]
fn the_persons_face_is_pressed_by_the_pointer_on_a_narrow_bar() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let packages = compiled();
    for width in [360, 480, 640, 1280] {
        let (mut part, link) = part_of(&packages, reading(), (width, 800));
        for _ in 0..8 {
            part.prepare_frame();
            part.draw();
        }
        let (x, y, w, h) = part
            .control_bounds("Pat: open your card")
            .unwrap_or_else(|| {
                panic!(
                    "a {width} wide bar shows the face: {:?}",
                    part.accessible_labels()
                )
            });
        assert!(
            x >= 0.0 && y >= 0.0 && x + w <= f64::from(width) && y + h <= 800.0,
            "a {width} wide bar draws the face on the display: {:?}",
            (x, y, w, h)
        );
        click(&mut part, "Pat: open your card");
        let asked = link.asked.borrow().front().map(|(ask, _)| ask.clone());
        assert!(
            asked.as_ref().is_some_and(|ask| ask.action == "show-person"
                && ask.arguments.get("person") == Some(&"self".into())),
            "a press on a {width} wide bar's face opens the person's own card: {asked:?}"
        );
    }
}

/// The switcher pictures each window it walks: the compositor is told to draw the window's own
/// pixels above the title that names it, and nothing at all for a window that may not be
/// pictured — whose card shows its app's mark instead.
#[test]
fn the_switcher_pictures_each_window_above_its_own_title() {
    let _settled = tilo_ui_base::motion::testing::settled();
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
    // Holding Alt+Tab opens the switcher: every window the shell holds, in the walk's order.
    let mut reading = super::tests_support::reading_with_windows();
    for (at, window) in reading
        .windows
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        window["recency"] = serde_json::json!(at + 1);
        window["preview"] = serde_json::json!("live");
    }
    super::tests_support::publish_windows(&link, &reading.windows);
    super::tests_support::publish_overlays(&link, "switcher", true);
    link.publish_layout(reading.shell);
    for _ in 0..6 {
        part.prepare_frame();
        part.draw();
    }
    let pictures = tilo_session_core::surface::ShellSurface::window_pictures(&part);
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

/// Who is here is exactly who the shell says is here, and stays so across a commit of the
/// shell's own record (a setting changes the bar's body, not the list it drew): with nobody,
/// one, four and one again, the bar names those people and no others.
#[test]
fn the_bars_people_stay_who_is_here_across_a_commit() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let (mut part, link) = part_with(reading());
    let mut now = reading();
    for count in [0_usize, 1, 4, 1] {
        now.people = (0..count)
            .map(|at| LayoutPersonReading {
                key: format!("p{at}"),
                name: HERE[at].to_owned(),
            })
            .collect();
        link.publish_layout(now.clone());
        for _ in 0..4 {
            part.prepare_frame();
        }
        // The bar's body is set: a commit of the shell's own record, as pressing the setting
        // in the shell's settings does.
        part.run_action(
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

/// The overview gives every window the share of its strip it keeps on the shell,
/// and each picture room to show the window: a picture a fraction of a pixel
/// high shows no window at all.
#[test]
fn the_overview_pictures_every_window_of_a_strip_at_its_share() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let windows: serde_json::Value = [(7, 0.5, "wide"), (8, 0.25, "narrow")]
        .into_iter()
        .enumerate()
        .map(|(at, (key, share, title))| {
            serde_json::json!({
                "key": key.to_string(),
                "title": title,
                "origin_app": "so.tilo.notes",
                "app_key": "so.tilo.notes",
                "icon": "app_window",
                "strip": "Main",
                "strip_key": "1",
                "workspace": "Home",
                "workspace_key": "1",
                "group_key": key.to_string(),
                "share": share,
                "place": at,
                "recency": at,
                "focused": at == 0,
                "preview": "live",
            })
        })
        .collect();
    let (mut part, link) = part_with(reading());
    let previews = ui::widget::Previews::new();
    part.show_pictures(
        previews.clone(),
        std::rc::Rc::new(|key: &str| key.parse::<u64>().ok()),
    );
    super::tests_support::publish_windows(&link, &windows);
    super::tests_support::publish_overlays(&link, "overview", false);
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
    // quarter, as two windows stand on the shell; a place past the width a page shows it at
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
    let packages = compiled();
    let (mut plain, _link) = part_of(&packages, reading(), (1280, 800));
    plain.outputs_changed(&[tilo_session_core::surface::SurfaceOutput {
        rect: (0, 0, 1280, 800),
        scale: 1.0,
    }]);
    plain.set_display(Some(("HEADLESS-1".to_owned(), (0, 0, 1280, 800))), 1.0);
    plain.prepare_frame();
    let one = plain.widgets_mut().placement();

    let (mut twice, _link) = part_of(&packages, reading(), (1280, 800));
    twice.outputs_changed(&[tilo_session_core::surface::SurfaceOutput {
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
    // The layer is the frame the tree paints: at 2 it is twice as wide, once the frame of that
    // size is drawn on the threads that draw it.
    let width = |part: &mut ShellPart| {
        part.wait_drawn();
        part.layers()[0].frame.size().0
    };
    let plain_width = width(&mut plain);
    let wide_width = width(&mut twice);
    assert_eq!(
        wide_width,
        plain_width * 2,
        "a 2 display's part paints the pixels it shows, not half of them: {plain_width} then {wide_width}"
    );
}

/// The shell's preset reaches the screen: a change of the shell's own record made from outside
/// the shell's views — `apply(windows_desktop)`, as the settings page's card and the REPL run it —
/// runs the boundaries that read it and patches the window, so the shell the person sees is the
/// one the record names.
///
/// The shape that failed it: a shell part over a shell with one local window, its layout
/// published, then the settings' apply. The record changed and the view's own `is_stale` said so,
/// but nothing it ran changed a tree, and the screen stayed on the strips (`stale=true`,
/// `patches=0`) — the record change never marked the boundary that read it.
///
/// The shell's part on another display draws the change in its next frame, not a frame later.
#[test]
fn the_shells_preset_reaches_the_screen() {
    let shell = || super::tests_support::Staged {
        shell: reading(),
        windows: serde_json::json!([{
            "key": "11",
            "title": "Release planning",
            "origin_app": "Notes",
            "app_key": "so.tilo.notes",
            "icon": "notebook",
            "strip": "Main",
            "workspace": "Home",
            "recency": 1,
            "focused": true,
        }]),
        here: "none".to_owned(),
        revealed: false,
    };
    let (mut part, link) = super::tests_support::part_as(shell(), None);
    let (mut beside, beside_link) = super::tests_support::part_beside(&part, shell());
    for (part, link) in [(&mut part, link), (&mut beside, beside_link)] {
        link.publish_layout(one_window_layout());
        for _ in 0..6 {
            part.prepare_frame();
        }
        let labels = tilo_session_core::surfaces::Published::accessible_labels(part);
        assert!(
            labels.iter().any(|l| l == "Showing Strips"),
            "the shell shows the strips: {labels:?}"
        );
    }

    part.run_action("apply", vec![serde_json::json!("windows_desktop")])
        .unwrap_or_else(|why| panic!("apply: {why}"));
    for _ in 0..6 {
        part.prepare_frame();
    }

    beside.prepare_frame();
    for part in [&mut beside, &mut part] {
        let labels = tilo_session_core::surfaces::Published::accessible_labels(part);
        assert!(
            !labels.iter().any(|l| l.starts_with("Showing ")),
            "the traditional desktop's bar carries the window's commands, not the preset \
             switch: {labels:?}"
        );
    }
    let labels = tilo_session_core::surfaces::Published::accessible_labels(&mut part);
    assert!(
        labels.iter().any(|l| l == "Dock"),
        "the desktop's dock rides its foot: {labels:?}"
    );
    let view = part
        .instance
        .view(tilo_render_protocol::patch::Level::Root)
        .unwrap();
    let stats = view.borrow().stats();
    assert!(
        stats.renders > stats.boundaries as u64,
        "the preset ran the boundaries that read the record: {stats:?}"
    );
}

/// The shell styles arrive on a reading the shell draws after its first frame, as the device's
/// daemon publishes them once it read its store: the switch re-runs when the styles' collection
/// moves, and offers them without a restart (shell.md §6).
#[test]
fn the_style_switch_lists_the_styles_a_later_reading_carries() {
    let (mut part, link) = super::tests_support::part_as(reading(), None);
    super::tests_support::publish_styles(&link, &serde_json::json!([]));
    for _ in 0..4 {
        part.prepare_frame();
    }
    // With no styles yet the bar's menu falls back to naming the strips, and offers nothing.
    let labels = tilo_session_core::surfaces::Published::accessible_labels(&mut part);
    assert!(
        labels.iter().any(|l| l == "Showing Strips"),
        "the bar shows before the styles come: {labels:?}"
    );
    super::tests_support::publish_styles(
        &link,
        &crate::shell::part::tests_support::shipped_styles(),
    );
    for _ in 0..6 {
        part.prepare_frame();
    }
    click(&mut part, "Showing Strips");
    let labels = tilo_session_core::surfaces::Published::accessible_labels(&mut part);
    for name in ["Strips", "Frames", "Spaces", "Desktop"] {
        assert!(
            labels.iter().any(|l| l == name),
            "the switch lists the {name} style once the reading carries it: {labels:?}"
        );
    }
}

/// A window that arrives past the canvas' edge is shown: the canvas pans to it, and on every
/// frame of the way each window's well has moved with the canvas, the one that stood there and
/// the one arriving alike, so no window trails its frame.
#[test]
fn the_canvas_pans_to_a_window_arriving_past_its_edge_carrying_every_well() {
    let wells = ui::widget::Wells::new();
    let (mut part, link) = super::tests_support::part_as(reading(), Some(wells.clone()));
    link.publish_layout(one_window_layout());
    click(&mut part, "Showing Strips");
    click(&mut part, "Frames");
    for _ in 0..30 {
        part.prepare_frame();
    }
    let mut layout = one_window_layout();
    let mut far = layout.strips[0].clone();
    far.key = "s2".to_owned();
    far.canvas_x = 1100;
    layout.strips.push(far);
    let mut arriving = layout.places[0].clone();
    arriving.key = "p2".to_owned();
    arriving.strip = "s2".to_owned();
    layout.places.push(arriving);
    link.publish_layout(layout);
    // The frames of the way are drawn 8 ms apart for the two seconds at most the way may take.
    // The way is over once a frame drew nothing and the part has nothing left to turn for
    // (`due`: a reading to take in, a turn that ran out of exchanges, space or input the window
    // owes its views) and names no further frame (`next_deadline`: the camera's glide and every
    // other motion of the tree ask for theirs there). A frame after that would draw nothing
    // either, so every frame that draws is among the ones read.
    let started = std::time::Instant::now();
    let mut frames = Vec::new();
    while started.elapsed() < std::time::Duration::from_secs(2) {
        let drew = part.prepare_frame();
        if let (Some(one), Some(two)) = (wells.well("p1"), wells.well("p2")) {
            frames.push((one, two));
        }
        if !drew && !part.due() && ShellSurface::next_deadline(&part).is_none() {
            break;
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

/// A collection the shell mirrors keeps the session's order: a tab tabbed into another window
/// goes after that window's own tab, so the mirror's places are taken out from the first one out
/// of order and inserted again in the session's order, rather than left where they were inserted.
/// An item is kept once its insert is written, so the next reading inserts it no more.
#[test]
fn the_mirror_follows_the_session_s_order_of_its_items() {
    let root = ResourceRef::root("root-places", "places");
    let ty = OriginType {
        app: "so.tilo.session".to_owned(),
        ty: "place".to_owned(),
        kind: "session.place".to_owned(),
        fields: BTreeMap::from([("key".to_owned(), "key".to_owned())]),
    };
    let mut mirror = Mirror::default();
    let mut sync = |now: &[&str]| {
        let now: Vec<String> = now.iter().map(|key| (*key).to_owned()).collect();
        let writes = mirror.sync(
            &now,
            &root,
            Some("places"),
            Some(&ty),
            Clone::clone,
            |key| vec![("key", Value::Text(key.clone()))],
        );
        let mut made = Vec::new();
        let changes: Vec<String> = writes
            .iter()
            .map(|write| match write {
                OriginWrite::Insert { fields, .. } => {
                    let Value::Text(key) = &fields[0].1 else {
                        panic!("a key")
                    };
                    made.push(Some(root.item(key, "place")));
                    format!("+{key}")
                }
                OriginWrite::Remove { item, .. } => {
                    made.push(None);
                    format!("-{}", item.item.clone().unwrap_or_default())
                }
                OriginWrite::Set { .. } => {
                    made.push(None);
                    "set".to_owned()
                }
            })
            .collect();
        mirror.made(Some(made));
        changes
    };
    assert_eq!(sync(&["a", "b"]), ["+a", "+b"]);
    assert_eq!(sync(&["a", "b", "c"]), ["+c"], "a new one goes last");
    assert!(sync(&["a", "b", "c"]).is_empty(), "nothing moved");
    // `a` moved behind `c`: everything from the first one out of order is inserted again.
    assert_eq!(sync(&["b", "c", "a"]), ["-a", "-b", "-c", "+b", "+c", "+a"]);
    assert!(sync(&["b", "c", "a"]).is_empty(), "the mirror is in order");
    // A new one before the last: it and the one after it go in again.
    assert_eq!(sync(&["b", "d", "c", "a"]), ["-c", "-a", "+d", "+c", "+a"]);
}

/// Where the person's workspace keeps the shell's records, a change of them is the person's call
/// of the shell's action on its root there, handed to the session once, so the choice is the
/// workspace's, the one every device of theirs and their agent read.
#[test]
fn a_choice_of_a_shell_kept_in_the_workspace_is_a_call_of_its_action_there() {
    let packages = crate::shell::part::compile_shell().unwrap_or_else(|why| panic!("{why}"));
    let app = packages.last().unwrap().meta.app.clone();
    let link = Rc::new(SessionLink::default());
    crate::shell::part::tests_support::publish_platform_roots(&link);
    link.publish_layout(reading());
    let mut part = ShellPart::new(
        ShellIdentity {
            app: &app,
            packages,
            person: "Pat",
            link: Rc::clone(&link),
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            seed: true,
            kept_elsewhere: true,
        },
        1.0,
        std::sync::Arc::new(|| {}),
        None,
    );
    part.prepare_frame();
    part.run_action("set_frames_snap", vec![serde_json::json!(false)])
        .unwrap_or_else(|why| panic!("{why}"));
    part.run_action("dock_on", vec![serde_json::json!("left")])
        .unwrap_or_else(|why| panic!("{why}"));
    let calls = link.take_own_calls();
    let named: Vec<(String, String, serde_json::Value)> = calls
        .into_iter()
        .map(|(address, arguments)| {
            (
                address.resource.clone(),
                address.action,
                serde_json::Value::Object(arguments),
            )
        })
        .collect();
    assert_eq!(named.len(), 2, "both choices are calls: {named:?}");
    assert!(
        named
            .iter()
            .all(|(resource, _, _)| resource.starts_with("tl:so.tilo.shell:root-")),
        "each call is on the shell's root as it publishes it: {named:?}"
    );
    assert_eq!(named[0].1, "set_frames_snap");
    assert_eq!(named[0].2, serde_json::json!({"on": false}));
    assert_eq!(named[1].1, "dock_on");
    assert!(
        named[1].2.get("edge").is_some(),
        "the edge goes by its name: {named:?}"
    );
    for _ in 0..2 {
        part.prepare_frame();
    }
    assert!(link.take_own_calls().is_empty(), "a call is handed once");
}

/// What the shell draws over the windows under a floating window is its card: the cover names the
/// window that floats, at the card's bar and body, and a place still waiting for its window covers
/// nothing of the windows under it.
#[test]
fn a_float_covers_the_windows_under_it_once_its_window_fills_it() {
    use tilo_desktop_model::{FloatCorner, FloatPlace, IdMinter, Tab, TileContent};
    use tilo_session_core::desktop::state::DesktopHandle;

    let mut minter = IdMinter::new(21);
    let mut tab = |tile: TileContent, title: &str| Tab {
        id: minter.mint(),
        title: title.to_owned(),
        tile,
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let filled = tab(
        TileContent::Foreign {
            app_id: "so.tilo.mail".to_owned(),
            surface_id: 42,
        },
        "Inbox",
    );
    let waiting = tab(
        TileContent::LocalApp {
            app_id: "so.tilo.notes".to_owned(),
            label: "Notes".to_owned(),
            note: String::new(),
        },
        "Notes",
    );
    let tree = tilo_session_core::desktop::seed_tree(&mut minter, "Studio", vec![filled, waiting]);
    let desktop = DesktopHandle::new(tree, minter.salt());
    desktop.set_viewport_width(1280.0);
    let (strip, groups) = desktop.read(|tree| {
        let workspace = tree.workspaces.first().expect("the shell has a workspace");
        let strip = workspace.strips.first().expect("and a strip");
        (
            strip.id,
            strip
                .window_groups
                .iter()
                .map(|group| group.id)
                .collect::<Vec<_>>(),
        )
    });
    let area = {
        let shared = desktop.shared.borrow();
        tilo_session_core::desktop::state::shown::work_area(&shared, None)
    };
    {
        let mut shared = desktop.shared.borrow_mut();
        // A window opened over the shell stands as a card; one already floating keeps the place
        // the record holds, which no window fills yet.
        let _ = tilo_session_core::desktop::state::floats::open_as_card(
            &mut shared,
            strip,
            groups[0],
            area,
        );
        let corner = FloatPlace::new(FloatCorner::TopLeft, 40, 40, 640, 480);
        if let Some(workspace) = shared.tree.workspaces.first_mut() {
            workspace.layout.set_float(groups[1], Some(corner));
        }
    }
    tilo_session_core::desktop::state::shown::lay_out(&mut desktop.shared.borrow_mut());

    let (mut part, _link) = part();
    part.carry_floats(desktop);
    let covers = part.drawn_over_windows();
    assert_eq!(
        covers.len(),
        1,
        "a place no window fills yet draws nothing over the shell: {covers:?}"
    );
    let cover = covers.first().expect("the window that floats is covered");
    assert_eq!(cover.window, 42, "the cover is of the window that floats");
    // A window an app opens floating stands as a card beside the window it opened next to: the
    // cover is that card alone, so the windows behind show under the shadow it casts.
    assert_eq!(
        (f64::from(cover.rect.2), f64::from(cover.rect.3)),
        ui::floating_layer::FLOAT_CARD_SIZE,
        "the card's bar and body, as the shell draws it: {:?}",
        cover.rect
    );
}

/// The person's bar and dock sizes reach the shell they see: a size chosen through the shell's
/// own settings actions changes the height its bar or its dock is laid out at, and the band the
/// session measures from that layout follows it, so what the session draws under the bar — a
/// popover, the roster, a banner — stands where the bar actually ends. Both the bar and the dock
/// are checked against their own setting, so one choice never moves the other.
#[test]
fn the_bar_and_dock_follow_the_sizes_the_settings_choose() {
    let (mut part, _link) = part();
    // The tallest node the body names is the body: the chrome's own ground, not the run it holds.
    let height = |part: &mut ShellPart, name: &str| {
        let tallest = part
            .control_bounds_all(name)
            .into_iter()
            .map(|(_, _, _, height)| height)
            .reduce(f64::max);
        tallest.unwrap_or_else(|| panic!("the shell draws {name}"))
    };
    let bar = height(&mut part, "Top bar");
    let dock = height(&mut part, "Dock");
    let (bar_band, dock_band) = part.bands().expect("the part measured its chrome");
    assert!(
        (f64::from(bar_band) - bar).abs() < 1.0,
        "the band the session measures is the bar's height: {bar_band} against {bar}"
    );
    assert!(
        (f64::from(dock_band) - dock).abs() < 1.0,
        "the band the session measures is the dock's height: {dock_band} against {dock}"
    );

    let choose = |part: &mut ShellPart, bar: &str, dock: &str| {
        part.run_action("set_bar_size", vec![serde_json::json!(bar)])
            .unwrap_or_else(|why| panic!("set the bar size: {why}"));
        part.run_action("set_dock_size", vec![serde_json::json!(dock)])
            .unwrap_or_else(|why| panic!("set the dock size: {why}"));
        for _ in 0..4 {
            part.prepare_frame();
        }
    };

    choose(&mut part, "large", "small");
    let large_bar = height(&mut part, "Top bar");
    let small_dock = height(&mut part, "Dock");
    assert!(
        large_bar > bar + 3.0,
        "a larger bar is taller: {bar} became {large_bar}"
    );
    assert!(
        small_dock < dock - 3.0,
        "a smaller dock is shorter: {dock} became {small_dock}"
    );

    choose(&mut part, "small", "large");
    let small_bar = height(&mut part, "Top bar");
    let large_dock = height(&mut part, "Dock");
    assert!(
        small_bar < bar - 3.0,
        "a smaller bar is shorter: {bar} became {small_bar}"
    );
    assert!(
        large_dock > dock + 3.0,
        "a larger dock is taller: {dock} became {large_dock}"
    );

    let (bar_band, dock_band) = part.bands().expect("the part measured its chrome again");
    assert!(
        (f64::from(bar_band) - small_bar).abs() < 1.0,
        "the measured band follows the bar: {bar_band} against {small_bar}"
    );
    assert!(
        (f64::from(dock_band) - large_dock).abs() < 1.0,
        "the measured band follows the dock: {dock_band} against {large_dock}"
    );
}
