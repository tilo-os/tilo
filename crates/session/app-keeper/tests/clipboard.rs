//! Clipboard, primary selection and drag-and-drop through a keeper: real clients on a real
//! headless desktop, with the app behind the keeper.

use std::io::Read as _;
use std::sync::{Arc, Mutex};

use tilo_compositor::{InputEvent, Selection};

use crate::support::{App, Rig};

const RED: [u8; 4] = [255, 0, 0, 255];
const MIME: &str = "text/plain;charset=utf-8";

fn centre(rig: &Rig) -> (u32, u32) {
    let (_, _, (x, y, w, h)) = rig.windows()[0].clone();
    ((x + w / 2) as u32, (y + h / 2) as u32)
}

/// Read a pipe the desktop wrote into, pumping the rig until the writer closed it.
fn read(rig: &mut Rig, app: &mut App, reader: std::io::PipeReader) -> Vec<u8> {
    let bytes = Arc::new(Mutex::new(None));
    let written = Arc::clone(&bytes);
    std::thread::spawn(move || {
        let mut read = Vec::new();
        let mut reader = reader;
        let _ = reader.read_to_end(&mut read);
        *written.lock().unwrap() = Some(read);
    });
    rig.pump(&mut [app], |_, _| bytes.lock().unwrap().is_some());
    bytes.lock().unwrap().take().unwrap()
}

#[test]
fn an_apps_copy_reaches_the_desktop_and_pastes_after_a_restart() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Copying", Some("copying"));
    app.show(&mut rig, (100, 80), RED);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);

    app.own_selection(MIME, b"kept text", false);
    rig.pump(&mut [&mut app], |rig, _| {
        rig.desktop()
            .selection_formats(Selection::Clipboard)
            .iter()
            .any(|mime| mime == MIME)
    });
    let reader = rig
        .desktop()
        .receive_selection(Selection::Clipboard, MIME)
        .expect("the desktop reads the app's copy");
    assert_eq!(read(&mut rig, &mut app, reader), b"kept text");
    assert_eq!(app.heard.sent, [MIME], "the desktop read the app's source");

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    assert!(
        app.heard.selection.is_none() && app.heard.primary_selection.is_none(),
        "an offer from the old desktop is cancelled to the app"
    );

    rig.attach_new(640, 480);
    // The copy is on the new desktop before the app's window has been focused: a paste into
    // another app first finds it.
    rig.pump(&mut [&mut app], |rig, _| {
        rig.desktop()
            .selection_formats(Selection::Clipboard)
            .iter()
            .any(|mime| mime == MIME)
    });
    assert!(
        !app.heard.keyboard_entered,
        "the copy was offered before the kept app was focused"
    );
    let reader = rig
        .desktop()
        .receive_selection(Selection::Clipboard, MIME)
        .expect("the app's copy was re-offered to the new desktop");
    assert_eq!(read(&mut rig, &mut app, reader), b"kept text");
    assert_eq!(app.heard.sent, [MIME, MIME]);

    // A new window comes back, and a copy made elsewhere replaces what the kept app had.
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
    rig.desktop()
        .set_selection(
            Selection::Clipboard,
            vec![(MIME.to_owned(), b"copied elsewhere".to_vec())],
        )
        .expect("the desktop sets a selection");
    let reader = rig
        .desktop()
        .receive_selection(Selection::Clipboard, MIME)
        .expect("the newer copy is what the desktop serves");
    assert_eq!(read(&mut rig, &mut app, reader), b"copied elsewhere");
}

#[test]
fn an_old_x11_names_copy_pastes_into_a_text_plain_reader_on_both_selections() {
    // What the keeper's X11 server offers for an X11 program that names its text only
    // `UTF8_STRING`, as old toolkits do.
    for primary in [false, true] {
        let selection = if primary {
            Selection::Primary
        } else {
            Selection::Clipboard
        };
        let mut rig = Rig::new(None);
        let mut app = App::connect(&mut rig);
        app.open_window(&mut rig, "Copying", Some("copying"));
        app.show(&mut rig, (100, 80), RED);
        rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);

        app.own_selection("UTF8_STRING", b"old text", primary);
        for round in 0..2 {
            rig.pump(&mut [&mut app], |rig, _| {
                rig.desktop()
                    .selection_formats(selection)
                    .iter()
                    .any(|mime| mime == "text/plain")
            });
            for wanted in ["text/plain", MIME] {
                let reader = rig
                    .desktop()
                    .receive_selection(selection, wanted)
                    .expect("the desktop reads the copy");
                assert_eq!(
                    read(&mut rig, &mut app, reader),
                    b"old text",
                    "read as {wanted}"
                );
            }
            let sent = if primary {
                &app.heard.primary_sent
            } else {
                &app.heard.sent
            };
            assert_eq!(
                sent.last().map(String::as_str),
                Some("UTF8_STRING"),
                "the app is asked under the name it offered"
            );
            if round == 0 {
                rig.drop_desktop();
                rig.settle(&mut [&mut app]);
                rig.attach_new(640, 480);
                rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
            }
        }
    }
}

#[test]
fn the_primary_selection_crosses_a_restart_too() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Selecting", Some("selecting"));
    app.show(&mut rig, (100, 80), RED);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);

    app.own_selection(MIME, b"selected", true);
    rig.pump(&mut [&mut app], |rig, _| {
        rig.desktop()
            .selection_formats(Selection::Primary)
            .iter()
            .any(|mime| mime == MIME)
    });
    let reader = rig
        .desktop()
        .receive_selection(Selection::Primary, MIME)
        .expect("the desktop reads the app's primary selection");
    assert_eq!(read(&mut rig, &mut app, reader), b"selected");

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    assert!(app.heard.primary_selection.is_none());
    rig.attach_new(640, 480);
    rig.pump(&mut [&mut app], |rig, _| {
        rig.desktop()
            .selection_formats(Selection::Primary)
            .iter()
            .any(|mime| mime == MIME)
    });
    assert!(
        !app.heard.keyboard_entered,
        "the primary selection was offered before the kept app was focused"
    );
    let reader = rig
        .desktop()
        .receive_selection(Selection::Primary, MIME)
        .expect("the primary selection was re-offered");
    assert_eq!(read(&mut rig, &mut app, reader), b"selected");
    rig.pump(&mut [&mut app], |rig, _| !rig.windows().is_empty());
}

#[test]
fn a_paste_from_the_desktop_reaches_the_app_and_is_cancelled_when_it_goes() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Pasting", Some("pasting"));
    app.show(&mut rig, (100, 80), RED);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);

    rig.desktop()
        .set_selection(
            Selection::Clipboard,
            vec![(MIME.to_owned(), b"from the desktop".to_vec())],
        )
        .unwrap();
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0]
            .selection_mimes(false)
            .iter()
            .any(|mime| mime == MIME)
    });
    assert_eq!(
        app.receive(&mut rig, MIME, false).as_deref(),
        Some(&b"from the desktop"[..])
    );

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    assert!(
        app.heard.selection.is_none(),
        "the app is told the clipboard is gone"
    );
}

#[test]
fn a_paste_from_the_desktop_reaches_the_primary_selection_too() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Pasting", Some("pasting"));
    app.show(&mut rig, (100, 80), RED);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);

    rig.desktop()
        .set_selection(
            Selection::Primary,
            vec![(MIME.to_owned(), b"selected".to_vec())],
        )
        .unwrap();
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0]
            .selection_mimes(true)
            .iter()
            .any(|mime| mime == MIME)
    });
    assert_eq!(
        app.receive(&mut rig, MIME, true).as_deref(),
        Some(&b"selected"[..])
    );
}

#[test]
fn a_drag_from_the_app_runs_on_the_desktop_and_reads_back() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Dragging", Some("dragging"));
    app.show(&mut rig, (100, 80), RED);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);
    let (x, y) = centre(&rig);
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
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0]
            .heard
            .buttons
            .iter()
            .any(|(_, button, pressed)| *button == 0x110 && *pressed)
    });

    app.start_drag(MIME, b"dragged");
    rig.settle(&mut [&mut app]);
    rig.desktop()
        .input(InputEvent::PointerMotion {
            x: f64::from(x) + 2.0,
            y: f64::from(y) + 2.0,
        })
        .unwrap();
    let mut turns = 0;
    rig.pump(&mut [&mut app], |_, apps| {
        turns += 1;
        apps[0].heard.drag_entered || apps[0].broken || turns > 200
    });
    assert!(
        app.heard.drag_entered,
        "the drag never reached the app: broken={} fault={:?} buttons={:?} pointer_entered={} left={} motion={}",
        app.broken,
        rig.keeper.describe().fault,
        app.heard.buttons,
        app.heard.pointer_entered,
        app.heard.drag_left,
        app.heard.drag_motion
    );
    assert_eq!(
        app.receive_drag(&mut rig, MIME).as_deref(),
        Some(&b"dragged"[..])
    );

    rig.desktop()
        .input(InputEvent::PointerButton {
            button: 0x110,
            pressed: false,
        })
        .unwrap();
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.drops > 0);
    rig.settle(&mut [&mut app]);
    assert_eq!(rig.keeper.describe().fault, None);
}

#[test]
fn a_drag_open_when_the_desktop_goes_is_cancelled_to_the_app() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Dragging", Some("dragging"));
    app.show(&mut rig, (100, 80), RED);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);
    let (x, y) = centre(&rig);
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
    app.start_drag(MIME, b"dragged");
    rig.settle(&mut [&mut app]);
    rig.desktop()
        .input(InputEvent::PointerMotion {
            x: f64::from(x) + 2.0,
            y: f64::from(y) + 2.0,
        })
        .unwrap();
    let mut turns = 0;
    rig.pump(&mut [&mut app], |_, apps| {
        turns += 1;
        apps[0].heard.drag_entered || apps[0].broken || turns > 200
    });
    assert!(
        app.heard.drag_entered,
        "the drag never reached the app: broken={} fault={:?} buttons={:?} pointer_entered={}",
        app.broken,
        rig.keeper.describe().fault,
        app.heard.buttons,
        app.heard.pointer_entered
    );

    // A drag still open when the desktop goes must not outlive it: the keeper notices the
    // socket close, as it does at an in-place restart, with nothing telling it first.
    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    assert!(
        !rig.keeper.describe().attached,
        "the keeper noticed the desktop going with a drag open"
    );
    assert!(
        app.heard.source_cancelled,
        "the drag's source hears it was cancelled"
    );
    assert!(app.heard.drag_left, "the drag's target hears it left");
    assert!(!app.broken);
}
