//! A drawing tablet's pad map: holding the button the pad's settings pick for it shows what each
//! button, ring and strip does, over everything, and letting go takes it down. The pad's events go
//! through the compositor as a tablet's do, and what the map says is read from the session's
//! accessible labels, the words a screen reader gets.

use super::*;
use settings_store::hardware::{PadDial, PadDialAction, ShortcutAction, TabletConfig};
use tilo_compositor::{
    PadAction, TabletDescriptor, TabletPadDescriptor, TabletPadEvent, TabletPadGroupDescriptor,
};

/// An event of a pad of four buttons: 0 to 2 in its one group with a ring and a strip, two modes,
/// and button 3 the one that switches them.
fn pad(action: PadAction) -> InputEvent {
    InputEvent::TabletPad(TabletPadEvent {
        pad: TabletPadDescriptor {
            name: "Pen tablet Pad".into(),
            syspath: None,
            buttons: 4,
            groups: vec![TabletPadGroupDescriptor {
                buttons: vec![0, 1, 2],
                rings: vec![0],
                strips: vec![0],
                modes: 2,
            }],
        },
        tablet: TabletDescriptor {
            name: "Pen tablet".into(),
            usb_id: Some((0x056a, 0x0001)),
            syspath: None,
        },
        action,
    })
}

/// The session's words joined, as a screen reader would get them.
fn said(session: &Session) -> String {
    session.accessible_labels().join("\n")
}

/// The map button, pressed or let go.
fn map_button(held: bool) -> InputEvent {
    pad(PadAction::ShowMap { button: 2, held })
}

#[test]
fn holding_the_map_button_shows_what_each_control_does_and_letting_go_takes_it_down() {
    let mut tablet = TabletConfig {
        pad_map_button: Some(2),
        ..TabletConfig::default()
    };
    tablet.pad_buttons.insert(0, ShortcutAction::Search);
    tablet.set_dial_action(PadDial::Ring(0), 0, PadDialAction::Zoom);
    tablet.set_dial_action(PadDial::Ring(0), 1, PadDialAction::Volume);
    let config = tilo_session::SessionConfig {
        tablets: [("056a:0001:Pen tablet Pad".to_owned(), tablet)].into(),
        ..tilo_session::SessionConfig::default()
    };
    let mut session = crate::common::headless(Options {
        config,
        collab: false,
        ..Options::default()
    });
    pump(&mut session, &mut [], |_, _| true);
    let before = {
        let _settled = tilo_ui_base::motion::testing::settled();
        pump(&mut session, &mut [], |_, _| true);
        crate::common::shot(&mut session, "pad-map-before").rgba
    };

    session.compositor().input(pad(PadAction::Added)).unwrap();
    pump(&mut session, &mut [], |_, _| true);
    assert!(
        !said(&session).contains("Pen tablet Pad"),
        "a pad nobody holds a button of shows no map"
    );

    // The card fades in as the button goes down: at the instant it is pressed the words are
    // there for a screen reader, and nothing of the card is drawn yet.
    {
        let _frozen = tilo_ui_base::motion::testing::frozen();
        session.compositor().input(map_button(true)).unwrap();
        pump(&mut session, &mut [], |session, _| {
            said(session).contains("Ring 1: Zoom")
        });
        assert_eq!(
            crate::common::shot(&mut session, "pad-map-pressed").rgba,
            before,
            "the card starts from nothing"
        );
    }
    let shown = {
        let _settled = tilo_ui_base::motion::testing::settled();
        pump(&mut session, &mut [], |_, _| true);
        crate::common::shot(&mut session, "pad-map-held").rgba
    };
    assert_ne!(shown, before, "the card stands once the fade is done");

    let words = said(&session);
    for line in [
        "Button 1: Search everywhere",
        "Button 2: Does nothing",
        "Button 3: Shows this map",
        "Button 4: Next mode (now 1 of 2)",
        "Ring 1: Zoom",
        "Strip 1: Scroll",
    ] {
        assert!(words.contains(line), "the map says {line:?}: {words}");
    }

    // A mode switch while it is held: the ring's action follows its group's mode.
    session
        .compositor()
        .input(pad(PadAction::Mode { group: 0, mode: 1 }))
        .unwrap();
    pump(&mut session, &mut [], |session, _| {
        said(session).contains("Ring 1: Volume")
    });
    assert!(said(&session).contains("Button 4: Next mode (now 2 of 2)"));

    {
        let _settled = tilo_ui_base::motion::testing::settled();
        session.compositor().input(map_button(false)).unwrap();
        pump(&mut session, &mut [], |session, _| {
            !said(session).contains("Ring 1: Volume")
        });
        pump(&mut session, &mut [], |_, _| true);
        assert_eq!(
            crate::common::shot(&mut session, "pad-map-let-go").rgba,
            before,
            "letting go takes the card down"
        );
    }

    // A mode button held shows the map too.
    session
        .compositor()
        .input(pad(PadAction::Button {
            button: 3,
            pressed: true,
        }))
        .unwrap();
    pump(&mut session, &mut [], |session, _| {
        said(session).contains("Ring 1: Volume")
    });
    session.compositor().input(pad(PadAction::Removed)).unwrap();
    pump(&mut session, &mut [], |session, _| {
        !said(session).contains("Ring 1: Volume")
    });
}
