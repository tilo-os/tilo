//! Text input through a keeper: the app's field reaches the desktop and comes back.

use crate::support::{App, Rig};

const RED: [u8; 4] = [255, 0, 0, 255];

#[test]
fn a_field_reaches_the_desktop_and_is_enabled_again_after_a_restart() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Typing", Some("typing"));
    app.show(&mut rig, (100, 80), RED);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);

    app.enable_text_input();
    let mut turns = 0;
    rig.pump(&mut [&mut app], |rig, _| {
        turns += 1;
        rig.desktop().text_entry().wants_typing || turns > 200
    });
    assert!(
        rig.desktop().text_entry().wants_typing,
        "the field reached the desktop: entered={} left={}",
        app.heard.text_entered,
        app.heard.text_left
    );
    assert!(app.heard.text_entered, "the desktop entered the field");
    let caret = rig
        .desktop()
        .text_caret()
        .expect("the caret rectangle reached the desktop");
    let (wx, wy, _, _) = rig.windows()[0].2;
    assert_eq!(
        (caret.x - f64::from(wx), caret.y - f64::from(wy)),
        (10.0, 12.0)
    );
    assert_eq!((caret.width, caret.height), (2.0, 20.0));

    assert_eq!(
        rig.desktop().insert_active_text("hello"),
        Some(false),
        "the field takes the text and is not secret"
    );
    let mut turns = 0;
    rig.pump(&mut [&mut app], |_, apps| {
        turns += 1;
        apps[0].heard.commits.iter().any(|text| text == "hello") || turns > 200
    });
    assert!(
        app.heard.commits.iter().any(|text| text == "hello"),
        "the typed text reached the app: commits={:?} dones={:?} entered={} left={}",
        app.heard.commits,
        app.heard.text_dones,
        app.heard.text_entered,
        app.heard.text_left
    );
    assert!(
        !app.heard.text_dones.is_empty() && app.heard.text_dones.iter().all(|serial| *serial == 1),
        "the app's own commit serial: {:?}",
        app.heard.text_dones
    );

    rig.drop_desktop();
    rig.settle(&mut [&mut app]);
    assert!(
        app.heard.text_left,
        "the field is left when the desktop goes"
    );

    rig.attach_new(640, 480);
    let mut turns = 0;
    rig.pump(&mut [&mut app], |rig, _| {
        turns += 1;
        !rig.windows().is_empty() || turns > 200
    });
    assert!(
        !rig.windows().is_empty(),
        "the window came back on the new desktop: attached={} fault={:?}",
        rig.keeper.describe().attached,
        rig.keeper.describe().fault
    );
    rig.pump(&mut [&mut app], |rig, _| {
        rig.desktop().text_entry().wants_typing
    });
    assert_eq!(rig.desktop().insert_active_text("again"), Some(false));
    rig.pump(&mut [&mut app], |_, apps| {
        apps[0].heard.commits.iter().any(|text| text == "again")
    });
}

#[test]
fn disabling_the_field_stops_the_desktop_typing_into_it() {
    let mut rig = Rig::new(None);
    let mut app = App::connect(&mut rig);
    app.open_window(&mut rig, "Typing", Some("typing"));
    app.show(&mut rig, (100, 80), RED);
    rig.pump(&mut [&mut app], |_, apps| apps[0].heard.keyboard_entered);

    app.enable_text_input();
    let mut turns = 0;
    rig.pump(&mut [&mut app], |rig, _| {
        turns += 1;
        rig.desktop().text_entry().wants_typing || turns > 200
    });
    assert!(
        rig.desktop().text_entry().wants_typing,
        "the field reached the desktop: entered={} left={}",
        app.heard.text_entered,
        app.heard.text_left
    );
    app.disable_text_input();
    rig.pump(&mut [&mut app], |rig, _| {
        !rig.desktop().text_entry().wants_typing
    });
    assert_eq!(rig.keeper.describe().fault, None);
}
