//! An app behind a keeper, on a real desktop that goes away and comes back.

use tilo_app_keeper::wire::{End, FromKeeper, ToKeeper};
use tilo_compositor::InputEvent;
use wayland_client::Proxy as _;

use crate::support::{App, Rig, SUSPENDED};

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const YELLOW: [u8; 4] = [255, 255, 0, 255];

fn centre(rig: &Rig) -> (u32, u32) {
    let (_, _, (x, y, w, h)) = rig.windows()[0].clone();
    ((x + w / 2) as u32, (y + h / 2) as u32)
}

#[test]
fn a_window_reaches_the_desktop_with_its_title_app_id_and_pixels() {
    let mut rig = Rig::new(Some("legacy.linux."));
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Calculator", Some("org.gnome.Calculator"));
    app.show(&mut rig, (100, 80), RED);
    let windows = rig.windows();
    assert_eq!(windows.len(), 1);
    assert_eq!(
        windows[0].0.as_deref(),
        Some("legacy.linux.org.gnome.Calculator")
    );
    assert_eq!(windows[0].1.as_deref(), Some("Calculator"));
    let (x, y) = centre(&rig);
    assert_eq!(rig.pixel(x, y), RED);

    let mut nameless = App::connect(&mut rig);
    nameless.open_window(&mut rig, "Untitled", None);
    nameless.show(&mut rig, (60, 40), BLUE);
    rig.pump(&mut [&mut app, &mut nameless], |rig, _| {
        rig.windows().len() == 2
    });
    assert!(
        rig.windows()
            .iter()
            .any(|(app_id, _, _)| app_id.as_deref() == Some("legacy.linux.window"))
    );
}

#[test]
fn a_window_survives_the_desktop_restarting() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Kept", Some("org.example.Kept"));
    app.show(&mut rig, (100, 80), GREEN);
    let (x, y) = centre(&rig);
    assert_eq!(rig.pixel(x, y), GREEN);
    // A frame asked for now is answered by the next desktop.
    let _frame = app.request_frame();
    app.surface.as_ref().unwrap().commit();
    rig.settle(&mut [&mut app]);
    let done_before = app.heard.frames_done;

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    assert!(!app.broken, "the app's connection stays open");
    assert_eq!(
        app.heard.frames_done, done_before,
        "no frame is done without a desktop"
    );
    let (_, _, states) = app.heard.configures.last().unwrap();
    assert!(
        states.contains(&SUSPENDED),
        "the window is told it is suspended"
    );
    assert!(!rig.keeper.describe().attached);

    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    let windows = rig.windows();
    assert_eq!(windows[0].0.as_deref(), Some("org.example.Kept"));
    assert_eq!(windows[0].1.as_deref(), Some("Kept"));
    let (x, y) = centre(&rig);
    assert_eq!(
        rig.pixel(x, y),
        GREEN,
        "the last picture shows without the app drawing"
    );
    rig.pump(&mut [&mut app], |rig, apps| {
        rig.desktop().render().unwrap();
        apps[0].heard.frames_done > done_before
    });
    rig.settle(&mut [&mut app]);
    let (_, _, states) = app.heard.configures.last().unwrap();
    assert!(!states.contains(&SUSPENDED));
    assert!(!app.broken);
    // The app acknowledged the configure the keeper had already acknowledged for it: no second
    // acknowledgement reached the desktop.
    assert_eq!(rig.keeper.describe().fault, None);
    assert!(rig.keeper.describe().attached);
}

#[test]
fn frames_drawn_while_away_show_and_windows_closed_while_away_stay_closed() {
    let mut rig = Rig::new(None);
    let mut kept = App::connect(&mut rig);
    kept.open_window(&mut rig, "Kept", Some("kept"));
    kept.show(&mut rig, (100, 80), RED);
    let mut closing = App::connect(&mut rig);
    closing.open_window(&mut rig, "Closing", Some("closing"));
    closing.show(&mut rig, (100, 80), BLUE);
    rig.pump(&mut [&mut kept, &mut closing], |rig, _| {
        rig.windows().len() == 2
    });

    rig.drop_desktop();
    rig.settle(&mut [&mut kept, &mut closing]);
    let surface = kept.surface.clone().unwrap();
    kept.draw(&surface, (100, 80), YELLOW);
    closing.toplevel.take().unwrap().destroy();
    closing.xdg.take().unwrap().destroy();
    closing.surface.take().unwrap().destroy();
    rig.settle(&mut [&mut kept, &mut closing]);

    rig.attach_new(640, 480);
    rig.pump(&mut [&mut kept, &mut closing], |rig, _| {
        !rig.windows().is_empty()
    });
    rig.settle(&mut [&mut kept, &mut closing]);
    let windows = rig.windows();
    assert_eq!(windows.len(), 1);
    assert_eq!(windows[0].0.as_deref(), Some("kept"));
    let (x, y) = centre(&rig);
    assert_eq!(rig.pixel(x, y), YELLOW);
}

#[test]
fn input_reaches_the_app_with_serials_the_desktop_accepts() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Moved", Some("moved"));
    app.show(&mut rig, (100, 80), GREEN);
    let (x, y) = centre(&rig);
    let start = rig.windows()[0].2;

    rig.desktop()
        .input(InputEvent::Key {
            keycode: 30,
            pressed: true,
        })
        .unwrap();
    rig.desktop()
        .input(InputEvent::Key {
            keycode: 30,
            pressed: false,
        })
        .unwrap();
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0]
            .heard
            .keys
            .iter()
            .any(|(_, key, pressed)| *key == 30 && !pressed)
    });

    rig.desktop()
        .input(InputEvent::PointerMotion {
            x: f64::from(x),
            y: f64::from(y),
        })
        .unwrap();
    rig.desktop()
        .input(InputEvent::PointerButton {
            button: 0x110,
            pressed: true,
        })
        .unwrap();
    rig.pump(&mut [&mut app], |_, apps| !apps[0].heard.buttons.is_empty());
    let (serial, button, pressed) = app.heard.buttons[0];
    assert_eq!((button, pressed), (0x110, true));
    let seat = app.heard.seat.clone().unwrap();
    app.toplevel.as_ref().unwrap()._move(&seat, serial);
    rig.settle(&mut [&mut app]);
    rig.desktop()
        .input(InputEvent::PointerMotion {
            x: f64::from(x) + 100.0,
            y: f64::from(y),
        })
        .unwrap();
    rig.pump(&mut [&mut app], |rig, _| {
        rig.windows()[0].2.0 == start.0 + 100
    });
    rig.desktop()
        .input(InputEvent::PointerButton {
            button: 0x110,
            pressed: false,
        })
        .unwrap();
    rig.settle(&mut [&mut app]);
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn an_open_popup_is_dismissed_by_a_restart_and_subsurfaces_return() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Menu", Some("menu"));
    app.show(&mut rig, (100, 80), GREEN);
    let (_child, _sub) = app.subsurface((10, 10), (20, 20), RED);
    rig.pump(&mut [&mut app], |rig, _| {
        let (_, _, (x, y, _, _)) = rig.windows()[0];
        rig.pixel((x + 15) as u32, (y + 15) as u32) == RED
    });
    let (_popup_surface, _popup) = app.popup(&mut rig);
    rig.settle(&mut [&mut app]);
    assert!(!app.heard.popup_done);

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    assert!(
        app.heard.popup_done,
        "a popup open when the desktop went is done"
    );

    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    let (_, _, (x, y, _, _)) = rig.windows()[0];
    assert_eq!(rig.pixel((x + 15) as u32, (y + 15) as u32), RED);
    assert_eq!(rig.pixel((x + 60) as u32, (y + 60) as u32), GREEN);
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn the_output_stays_and_a_new_mode_updates_it_in_place() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    assert_eq!(app.heard.outputs.len(), 1);
    let output = app.heard.outputs[0].id();
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0]
            .heard
            .output_modes
            .iter()
            .any(|(_, w, h)| (*w, *h) == (640, 480))
    });

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(800, 600);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0]
            .heard
            .output_modes
            .iter()
            .any(|(_, w, h)| (*w, *h) == (800, 600))
    });
    assert!(
        app.heard.globals_removed.is_empty(),
        "no output was removed"
    );
    assert_eq!(app.heard.outputs.len(), 1);
    assert_eq!(app.heard.outputs[0].id(), output);
}

#[test]
fn a_fault_is_held_and_reported_and_a_second_ends_the_app() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Faulty", Some("faulty"));
    app.show(&mut rig, (100, 80), GREEN);
    let mut session = rig.session();

    assert!(rig.keeper.break_protocol("w1"));
    let fault = session.wait_for(&mut rig, |message| match message {
        FromKeeper::Fault { fault } => Some(fault),
        _ => None,
    });
    assert!(fault.contains("error"), "{fault}");
    assert!(!rig.keeper.describe().attached);
    rig.settle(&mut [&mut app]);
    assert!(!app.broken, "the app is held, not ended");

    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    assert!(rig.keeper.break_protocol("w1"));
    rig.pump(&mut [&mut app], |_, apps| apps[0].broken);
}

#[test]
fn an_app_that_crashes_takes_its_windows_with_it() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Crashing", Some("crashing"));
    app.show(&mut rig, (100, 80), GREEN);
    let mut session = rig.session();
    drop(app);
    rig.pump(&mut [], |rig, _| rig.windows().is_empty());
    let windows = session.wait_for(&mut rig, |message| match message {
        FromKeeper::Windows { windows } if windows.is_empty() => Some(windows),
        _ => None,
    });
    assert!(windows.is_empty());
}

#[test]
fn the_apps_end_is_reported_and_recorded() {
    let mut rig = Rig::with_options(None, vec!["sh".into(), "-c".into(), "exit 3".into()]);
    let mut session = rig.session();
    let end = session.wait_for(&mut rig, |message| match message {
        FromKeeper::Ended { end } => Some(end),
        _ => None,
    });
    assert_eq!(end, End::Exited { code: 3 });
    rig.pump(&mut [], |rig, _| rig.keeper.finished());

    // Nobody listening: the end is in the directory for a later session.
    let mut rig = Rig::with_options(None, vec!["sh".into(), "-c".into(), "kill -9 $$".into()]);
    rig.pump(&mut [], |rig, _| rig.keeper.finished());
    let recorded = std::fs::read(rig.dir.path().join("keeper").join("ended")).unwrap();
    let end: End = serde_json::from_slice(&recorded).unwrap();
    assert_eq!(end, End::Signalled { signal: 9 });
}

#[test]
fn the_control_socket_refuses_other_versions_and_keeps_placements_across_a_restart() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Placed", Some("placed"));
    app.show(&mut rig, (100, 80), GREEN);

    let mut stranger = crate::support::Session::connect(&rig);
    stranger.send(&ToKeeper::Hello { version: 999 });
    assert_eq!(
        stranger.receive(&mut rig),
        FromKeeper::VersionMismatch {
            version: tilo_app_keeper::wire::INTERFACE_VERSION
        }
    );
    assert_eq!(stranger.next(&mut rig), None, "the connection closes");
    rig.settle(&mut [&mut app]);
    assert_eq!(rig.windows().len(), 1, "the keeper keeps displaying");

    let mut session = rig.session();
    session.send(&ToKeeper::SetPlacement {
        tag: "w1".to_owned(),
        placement: "left half".to_owned(),
    });
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    session.send(&ToKeeper::Describe);
    let described = session.wait_for(&mut rig, |message| match message {
        FromKeeper::Described(described) => Some(described),
        _ => None,
    });
    assert!(!described.attached);
    assert_eq!(described.launch, "test-launch");
    assert_eq!(described.windows.len(), 1);
    assert_eq!(described.windows[0].tag, "w1");
    assert_eq!(described.windows[0].placement.as_deref(), Some("left half"));

    let mut desktop = crate::support::desktop(640, 480);
    let socket = desktop.connect_client().unwrap();
    rig.desktop = Some(desktop);
    session.attach(&socket);
    drop(socket);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    session.send(&ToKeeper::Describe);
    let described = session.wait_for(&mut rig, |message| match message {
        FromKeeper::Described(described) => Some(described),
        _ => None,
    });
    assert!(described.attached);
    assert_eq!(described.windows[0].placement.as_deref(), Some("left half"));
}

fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

fn resident_bytes() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096
}

#[test]
fn frames_through_the_keeper_hold_descriptors_not_pixels() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Busy", Some("busy"));
    app.show(&mut rig, (100, 80), GREEN);
    let surface = app.surface.clone().unwrap();
    let buffers = [app.buffer((100, 80), RED), app.buffer((100, 80), BLUE)];
    let mut measured = None;
    for frame in 0..300 {
        surface.attach(Some(&buffers[frame % 2]), 0, 0);
        surface.damage_buffer(0, 0, 100, 80);
        surface.commit();
        rig.settle(&mut [&mut app]);
        rig.desktop().render().unwrap();
        if frame == 50 {
            measured = Some((open_fds(), resident_bytes()));
        }
    }
    let (fds, resident) = measured.unwrap();
    assert!(
        open_fds() <= fds + 4,
        "descriptors leak per frame: {fds} then {}",
        open_fds()
    );
    let grown = resident_bytes().saturating_sub(resident);
    assert!(
        grown < 48 << 20,
        "memory grew by {grown} bytes over 250 frames"
    );
    assert!(
        app.heard.buffers_released > 0,
        "a buffer no longer shown is released"
    );
    assert!(!app.broken);
}

#[test]
fn an_app_that_stops_answering_pings_looks_hung_to_the_desktop() {
    use std::time::{Duration, Instant};
    use tilo_compositor::app_liveness::UNANSWERED;

    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Hung", Some("hung"));
    app.show(&mut rig, (100, 80), GREEN);
    let window = rig.desktop().windows()[0].0.id;

    // The app stops reading: it neither pongs nor draws from here on.
    app.heard.answers_pings = false;
    rig.desktop()
        .input(InputEvent::Key {
            keycode: 30,
            pressed: true,
        })
        .unwrap();
    rig.pump(&mut [&mut app], |_, apps| !apps[0].heard.pings.is_empty());
    rig.settle(&mut [&mut app]);
    let later = Instant::now() + UNANSWERED + Duration::from_millis(10);
    rig.desktop().advance_app_liveness_at(later);
    assert_eq!(
        rig.desktop().not_responding_windows(),
        [window],
        "the desktop's ping reached the app, and its silence reached the desktop"
    );

    // The app answers the ping it already read: the desktop hears it and stops calling it hung.
    app.pong();
    rig.pump(&mut [&mut app], |rig, _| {
        rig.desktop().advance_app_liveness_at(Instant::now());
        rig.desktop().not_responding_windows().is_empty()
    });
}

#[test]
fn thousands_of_commits_while_away_keep_one_callback_and_bounded_memory() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Busy", Some("busy"));
    app.show(&mut rig, (100, 80), GREEN);
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);

    let surface = app.surface.clone().unwrap();
    let buffer = app.buffer((100, 80), RED);
    let done_before = app.heard.frames_done;
    let fds = open_fds();
    let resident = resident_bytes();
    for frame in 0..3000 {
        let _ = app.request_frame();
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, 100, 80);
        surface.commit();
        if frame % 50 == 0 {
            rig.settle(&mut [&mut app]);
        }
    }
    rig.settle(&mut [&mut app]);
    let done = app.heard.frames_done - done_before;
    assert!(
        done >= 2900,
        "callbacks that can never be shown are completed while away: {done} of 3000"
    );
    assert!(
        open_fds() <= fds + 8,
        "descriptors grew with the commits: {fds} then {}",
        open_fds()
    );
    let grown = resident_bytes().saturating_sub(resident);
    assert!(
        grown < 64 << 20,
        "memory grew by {grown} bytes over 3000 commits while away"
    );
    assert!(!app.broken);

    // The one callback kept wakes the app once a desktop shows the last frame.
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, apps| {
        rig.desktop().render().unwrap();
        apps[0].heard.frames_done > done_before + done
    });
    let (_, _, (x, y, _, _)) = rig.windows()[0];
    assert_eq!(rig.pixel((x + 50) as u32, (y + 40) as u32), RED);
}

#[test]
fn subsurfaces_stack_as_the_app_places_them_and_one_destroyed_leaves() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Stacked", Some("stacked"));
    app.show(&mut rig, (100, 80), RED);
    let parent = app.surface.clone().unwrap();
    let (first, first_sub) = app.subsurface((10, 10), (20, 20), BLUE);
    let (_second, second_sub) = app.subsurface((10, 10), (20, 20), GREEN);
    let overlap = |rig: &mut Rig| {
        let (_, _, (x, y, _, _)) = rig.windows()[0];
        rig.pixel((x + 15) as u32, (y + 15) as u32)
    };
    rig.pump(&mut [&mut app], |rig, _| overlap(rig) == GREEN);

    // Below the parent: the red parent covers it, and the blue one stays above.
    second_sub.place_below(&parent);
    parent.commit();
    rig.pump(&mut [&mut app], |rig, _| overlap(rig) == BLUE);

    // Above its sibling again.
    second_sub.place_above(&first);
    parent.commit();
    rig.pump(&mut [&mut app], |rig, _| overlap(rig) == GREEN);

    // Only the wl_subsurface goes; its twin must go at the parent's next commit.
    second_sub.destroy();
    parent.commit();
    rig.pump(&mut [&mut app], |rig, _| overlap(rig) == BLUE);
    assert_eq!(rig.keeper.describe().fault, None);
    assert!(!app.broken);

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    let (_, _, (x, y, _, _)) = rig.windows()[0];
    assert_eq!(
        rig.pixel((x + 15) as u32, (y + 15) as u32),
        BLUE,
        "stacking survives the restart"
    );
    assert_eq!(rig.pixel((x + 60) as u32, (y + 60) as u32), RED);
    assert_eq!(rig.keeper.describe().fault, None);
    let _ = first_sub;
}

#[test]
fn a_surface_gets_the_desktops_own_dmabuf_feedback_and_keeps_it_across_a_restart() {
    use crate::support::SurfaceFeedbackSeen;

    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Video", Some("video"));
    app.show(&mut rig, (100, 80), GREEN);
    rig.settle(&mut [&mut app]);
    let Some(dmabuf) = app.heard.dmabuf.clone() else {
        eprintln!(
            "skipped: the headless desktop offers no linux-dmabuf with feedback (its renderer has \
             no GPU), so the keeper offers the app none"
        );
        return;
    };
    assert!(
        app.heard.dmabuf_done > 0,
        "the desktop's default dma-buf feedback reached the app"
    );

    let surface = app.surface.clone().unwrap();
    let qh = app.queue.handle();
    let _feedback = dmabuf.get_surface_feedback(&surface, &qh, SurfaceFeedbackSeen);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.surface_feedback_done > 0
    });

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    let seen = app.heard.surface_feedback_done;
    let _again = dmabuf.get_surface_feedback(&surface, &qh, SurfaceFeedbackSeen);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.surface_feedback_done > seen
    });
    assert!(!app.broken);
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_fault_is_attributed_to_the_window_that_caused_it_not_the_last_one_sent() {
    let mut rig = Rig::new(None);
    let mut first = App::connect(&mut rig);
    first.open_window(&mut rig, "First", Some("first"));
    first.show(&mut rig, (100, 80), GREEN);
    let mut second = App::connect(&mut rig);
    second.open_window(&mut rig, "Second", Some("second"));
    second.show(&mut rig, (100, 80), BLUE);
    rig.pump(&mut [&mut first, &mut second], |rig, _| {
        rig.windows().len() == 2
    });
    // The second app's surface was pushed last, so it is what "the last requests" would blame.
    let mut session = rig.session();
    assert!(rig.keeper.break_protocol("w1"));
    session.wait_for(&mut rig, |message| match message {
        FromKeeper::Fault { fault } => Some(fault),
        _ => None,
    });
    rig.drop_desktop();
    rig.settle(&mut [&mut first, &mut second]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut first, &mut second], |rig, _| {
        rig.windows().len() == 2
    });
    assert!(rig.keeper.break_protocol("w1"));
    rig.pump(&mut [&mut first, &mut second], |_, apps| apps[0].broken);
    assert!(first.broken, "the app whose window caused the fault ended");
    assert!(!second.broken, "the app pushed last is not blamed");
}

#[test]
fn a_window_icon_reaches_the_desktops_tab_and_survives_a_restart() {
    use tilo_compositor::IconArtwork;

    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Icon", Some("icon"));
    app.show(&mut rig, (100, 80), GREEN);
    rig.pump(&mut [&mut app], |_, apps| {
        !apps[0].heard.icon_sizes.is_empty()
    });
    let mut sizes = app.heard.icon_sizes.clone();
    sizes.sort_unstable();
    assert_eq!(
        sizes,
        vec![16, 32, 48],
        "the desktop's preferred icon sizes"
    );

    let _icon = app.set_window_icon((16, 16), RED);
    let icon_of = |rig: &mut Rig| {
        rig.desktop()
            .windows()
            .into_iter()
            .next()
            .and_then(|(info, _)| info.icon)
    };
    rig.pump(&mut [&mut app], |rig, _| icon_of(rig).is_some());
    let icon = icon_of(&mut rig).expect("an icon");
    let IconArtwork::Pixels(images) = icon.artwork() else {
        panic!("the icon is the pixels the app handed over");
    };
    let first = images.first().expect("one size");
    assert_eq!((first.width, first.height), (16, 16));
    assert_eq!(
        first.pixels.get(0..4),
        Some([255, 0, 0, 255].as_slice()),
        "the desktop has the app's red pixel"
    );

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    rig.pump(&mut [&mut app], |rig, _| icon_of(rig).is_some());
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_modal_dialog_takes_the_focus_from_its_parent_and_the_hint_survives_a_restart() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Parent", Some("parent"));
    app.show(&mut rig, (100, 80), GREEN);
    let parent_toplevel = app.toplevel.clone().unwrap();
    let parent_surface = app.surface.clone().unwrap();
    app.open_child(&mut rig, "Dialog", Some("dialog"), &parent_toplevel);
    app.show(&mut rig, (60, 40), BLUE);
    rig.pump(&mut [&mut app], |rig, _| rig.windows().len() == 2);
    let dialog = app.dialog();
    dialog.set_modal();
    app.surface.as_ref().unwrap().commit();
    rig.settle(&mut [&mut app]);

    let id_of = |rig: &mut Rig, title: &str| {
        rig.desktop()
            .windows()
            .into_iter()
            .find(|(info, _)| info.title.as_deref() == Some(title))
            .map(|(info, _)| info.id)
            .expect("a window")
    };
    let dialog_id = id_of(&mut rig, "Dialog");
    let token = rig.desktop().activation_token();
    app.activate_on(token, &parent_surface);
    rig.pump(&mut [&mut app], |rig, _| {
        rig.desktop().focused_window().map(|info| info.id) == Some(dialog_id)
    });

    // The next desktop is told again, so the dialog still takes the focus from its parent.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| rig.windows().len() == 2);
    rig.settle(&mut [&mut app]);
    let dialog_id = id_of(&mut rig, "Dialog");
    let token = rig.desktop().activation_token();
    app.activate_on(token, &parent_surface);
    rig.pump(&mut [&mut app], |rig, _| {
        rig.desktop().focused_window().map(|info| info.id) == Some(dialog_id)
    });
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn an_idle_inhibitor_holds_the_desktop_awake_and_returns_with_the_next_one() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Playing", Some("playing"));
    app.show(&mut rig, (100, 80), GREEN);
    assert!(!rig.desktop().idle_inhibited());
    let _inhibitor = app.inhibit_idle();
    rig.pump(&mut [&mut app], |rig, _| rig.desktop().idle_inhibited());

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    rig.pump(&mut [&mut app], |rig, _| rig.desktop().idle_inhibited());
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_shortcuts_inhibitor_follows_the_windows_focus_and_the_desktops_comings_and_goings() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Remote", Some("remote"));
    app.show(&mut rig, (100, 80), GREEN);
    let _inhibitor = app.inhibit_shortcuts();
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.shortcuts_inhibited);

    // No desktop holds the keys: the app is told its inhibitor is inactive.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    assert!(
        !app.heard.shortcuts_inhibited,
        "inactive while no desktop is attached"
    );

    // The next desktop grants it again to the focused window.
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, apps| {
        !rig.windows().is_empty() && apps[0].heard.shortcuts_inhibited
    });
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_cursor_shape_the_app_names_is_drawn_by_the_desktop_and_kept_across_a_restart() {
    use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::Shape;

    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Pointed", Some("pointed"));
    app.show(&mut rig, (100, 80), GREEN);
    let (x, y) = centre(&rig);
    rig.desktop()
        .input(InputEvent::PointerMotion {
            x: f64::from(x),
            y: f64::from(y),
        })
        .unwrap();
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.pointer_entered);
    app.set_cursor_shape(Shape::Crosshair);
    rig.pump(&mut [&mut app], |rig, _| {
        rig.desktop().cursor_shape_name() == Some("crosshair")
    });

    // The app has no reason to name it again: the next desktop gets it when its pointer enters.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    let (x, y) = centre(&rig);
    rig.desktop()
        .input(InputEvent::PointerMotion {
            x: f64::from(x),
            y: f64::from(y),
        })
        .unwrap();
    rig.pump(&mut [&mut app], |rig, apps| {
        apps[0].heard.pointer_entered && rig.desktop().cursor_shape_name() == Some("crosshair")
    });
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_frames_presentation_feedback_is_the_desktops_and_survives_a_restart() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Video", Some("video"));
    app.show(&mut rig, (100, 80), GREEN);
    assert_eq!(
        app.heard.clock_ids.first().copied(),
        Some(1),
        "the desktop's presentation clock is passed on"
    );

    let surface = app.surface.clone().unwrap();
    let _feedback = app.request_presentation();
    let _frame = app.draw(&surface, (100, 80), RED);
    rig.pump(&mut [&mut app], |rig, apps| {
        rig.desktop().render().unwrap();
        apps[0].heard.sync_outputs > 0 && !apps[0].heard.presented.is_empty()
    });
    let shown = app.heard.presented.last().copied().unwrap();
    assert!(
        shown.secs > 0 || shown.nsec > 0,
        "the desktop's own presentation time"
    );
    assert!(shown.nsec < 1_000_000_000, "a valid nanosecond part");
    assert_eq!(
        shown.seq, 0,
        "the headless desktop has no display frame counter"
    );
    assert_eq!(shown.flags, 0, "and no display flags");
    assert_eq!(
        shown.refresh, 16_666_666,
        "the output's refresh interval in nanoseconds"
    );
    assert_eq!(rig.keeper.describe().fault, None);

    // Content replaced before the desktop showed it is discarded, and a feedback asked for while
    // no desktop is attached can never be shown.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    let _away = app.request_presentation();
    let _frame = app.draw(&surface, (100, 80), BLUE);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.presentation_discarded > 0
    });

    // The next desktop answers the app's feedback again.
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    let _again = app.request_presentation();
    let _frame = app.draw(&surface, (100, 80), GREEN);
    let before = app.heard.presented.len();
    rig.pump(&mut [&mut app], |rig, apps| {
        rig.desktop().render().unwrap();
        apps[0].heard.presented.len() > before
    });
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn an_activation_token_is_the_desktops_and_activation_reaches_it() {
    let mut rig = Rig::new(None);
    let mut first = App::connect(&mut rig);
    first.open_window(&mut rig, "First", Some("first"));
    first.show(&mut rig, (100, 80), GREEN);
    let mut second = App::connect(&mut rig);
    second.open_window(&mut rig, "Second", Some("second"));
    second.show(&mut rig, (100, 80), BLUE);
    rig.pump(&mut [&mut first, &mut second], |rig, _| {
        rig.windows().len() == 2
    });
    let id_of = |rig: &mut Rig, title: &str| {
        rig.desktop()
            .windows()
            .into_iter()
            .find(|(info, _)| info.title.as_deref() == Some(title))
            .map(|(info, _)| info.id)
            .expect("a window")
    };
    let first_id = id_of(&mut rig, "First");
    assert_eq!(
        rig.desktop().focused_window().map(|info| info.id),
        Some(id_of(&mut rig, "Second")),
        "the window mapped last has the keyboard"
    );

    // The token the app gets is the desktop's, and activating with it is the desktop's decision.
    let token = first.activation_token(&mut rig);
    assert!(!token.is_empty(), "the desktop made a token");
    first.activate(token.clone());
    rig.pump(&mut [&mut first, &mut second], |rig, _| {
        rig.desktop().focused_window().map(|info| info.id) == Some(first_id)
    });

    // A token from before a restart names nothing on the next desktop.
    rig.drop_desktop();
    rig.settle(&mut [&mut first, &mut second]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut first, &mut second], |rig, _| {
        rig.windows().len() == 2
    });
    rig.settle(&mut [&mut first, &mut second]);
    let before = rig.desktop().focused_window().map(|info| info.id);
    first.activate(token);
    rig.settle(&mut [&mut first, &mut second]);
    assert_eq!(
        rig.desktop().focused_window().map(|info| info.id),
        before,
        "a stale token activates nothing"
    );
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_token_requested_while_the_desktop_is_away_is_empty() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Waiting", Some("waiting"));
    app.show(&mut rig, (100, 80), GREEN);
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    let token = app.activation_token(&mut rig);
    assert!(token.is_empty(), "no desktop can act on a token made now");
    assert!(!app.broken);
}

#[test]
fn the_apps_decoration_request_and_the_desktops_answer_travel_both_ways_across_a_restart() {
    use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::Mode;

    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Decorated", Some("decorated"));
    app.show(&mut rig, (100, 80), RED);
    let _decoration = app.decoration(Mode::ClientSide);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.decoration_modes.last() == Some(&Mode::ServerSide)
    });
    let told = app.heard.decoration_modes.len();

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    rig.settle(&mut [&mut app]);
    assert_eq!(
        app.heard.decoration_modes.len(),
        told,
        "the same answer is not repeated to the app"
    );
    assert_eq!(
        app.heard.decoration_modes.last(),
        Some(&Mode::ServerSide),
        "the app keeps the desktop's answer while the new one makes its own objects"
    );
    assert_eq!(rig.keeper.describe().fault, None);
    let (_, _, (x, y, _, _)) = rig.windows()[0];
    assert_eq!(rig.pixel((x + 50) as u32, (y + 40) as u32), RED);
}

#[test]
fn the_kde_spelling_of_the_same_decoration_question_is_answered_across_a_restart() {
    use wayland_protocols_misc::server_decoration::client::org_kde_kwin_server_decoration::Mode;

    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Kde", Some("kde"));
    app.show(&mut rig, (100, 80), GREEN);
    let _decoration = app.kde_decoration(Mode::Client);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.kde_modes.last() == Some(&Mode::Server)
    });
    let told = app.heard.kde_modes.len();

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    rig.settle(&mut [&mut app]);
    assert_eq!(app.heard.kde_modes.len(), told, "not repeated");
    assert_eq!(app.heard.kde_modes.last(), Some(&Mode::Server));
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_viewport_crops_and_scales_the_window_on_the_desktop() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Cropped", Some("cropped"));
    app.show(&mut rig, (100, 80), RED);
    assert_eq!((rig.windows()[0].2.2, rig.windows()[0].2.3), (100, 80));
    let _viewport = app.viewport(Some((0.0, 0.0, 50.0, 40.0)), Some((50, 40)));
    rig.pump(&mut [&mut app], |rig, _| {
        let (_, _, (_, _, w, h)) = rig.windows()[0];
        (w, h) == (50, 40)
    });
    let (_, _, (x, y, w, h)) = rig.windows()[0];
    assert_eq!(rig.pixel((x + w / 2) as u32, (y + h / 2) as u32), RED);

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| {
        rig.windows()
            .first()
            .is_some_and(|(_, _, (_, _, w, h))| (*w, *h) == (50, 40))
    });
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn the_desktops_fractional_scale_reaches_the_app_and_follows_a_restart() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Scaled", Some("scaled"));
    app.show(&mut rig, (100, 80), GREEN);
    let _scale = app.request_fractional_scale();
    rig.pump(&mut [&mut app], |_, apps| {
        !apps[0].heard.preferred_scales.is_empty()
    });
    assert_eq!(app.heard.preferred_scales.last().copied(), Some(120));

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new_at_scale(640, 480, 1.5);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.preferred_scales.last().copied() == Some(180)
    });
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new_at_scale(640, 480, 2.0);
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.preferred_scales.last().copied() == Some(240)
    });
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_content_type_the_app_sets_survives_a_restart() {
    use wayland_protocols::wp::content_type::v1::client::wp_content_type_v1::Type;

    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Video", Some("video"));
    app.show(&mut rig, (100, 80), GREEN);
    let _content = app.content_type(Type::Video);
    rig.settle(&mut [&mut app]);

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    assert_eq!(rig.keeper.describe().fault, None);
    let (_, _, (x, y, _, _)) = rig.windows()[0];
    assert_eq!(rig.pixel((x + 50) as u32, (y + 40) as u32), GREEN);
}
