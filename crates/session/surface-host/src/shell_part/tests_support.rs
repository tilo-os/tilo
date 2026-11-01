#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

use super::*;
use tilo_compositor::KeyModifiers;

/// A desk at rest with two windows and two workspaces.
pub(super) fn reading_with_windows() -> DeskReading {
    DeskReading {
        person: "Pat".to_owned(),
        clock: "4:28 PM".to_owned(),
        workspace: "Desk".to_owned(),
        strip: "Main".to_owned(),
        windows: vec![
            WindowReading {
                key: "11".to_owned(),
                title: "Release planning".to_owned(),
                origin_app: "Notes".to_owned(),
                strip: "Main".to_owned(),
                workspace: "Desk".to_owned(),
                focused: true,
                ..WindowReading::default()
            },
            WindowReading {
                key: "12".to_owned(),
                title: "Inbox".to_owned(),
                origin_app: "Mail".to_owned(),
                strip: "Main".to_owned(),
                workspace: "Desk".to_owned(),
                ..WindowReading::default()
            },
        ],
        ..DeskReading::default()
    }
}

/// A part over `reading`, its outputs set.
pub(super) fn part_with(reading: DeskReading) -> (ShellPart, Rc<SessionLink>) {
    part_on(reading, (1280, 800))
}

/// A part over `reading` on one output `size` logical pixels big.
pub(super) fn part_on(reading: DeskReading, size: (u32, u32)) -> (ShellPart, Rc<SessionLink>) {
    let packages = crate::shell_part::compile_shell().unwrap_or_else(|why| panic!("{why}"));
    let app = packages
        .last()
        .expect("the shell compiles last")
        .meta
        .app
        .clone();
    let link = Rc::new(SessionLink::default());
    link.publish(reading);
    let mut part = ShellPart::new(
        ShellIdentity {
            app: &app,
            packages,
            person: "Pat",
            link: Rc::clone(&link),
            record: Rc::new(RefCell::new(tilo_resource::Doc::new())),
            seed: true,
        },
        32,
        1.0,
        std::sync::Arc::new(|| {}),
        None,
    );
    let (width, height) = size;
    let (w, h) = (
        i32::try_from(width).unwrap(),
        i32::try_from(height).unwrap(),
    );
    part.outputs_changed(&[(0, 0, w, h)]);
    part.set_display(Some(("HEADLESS-1".to_owned(), (0, 0, width, height))));
    for _ in 0..4 {
        part.prepare_frame();
    }
    (part, link)
}

pub(super) fn key(keysym: u32, pressed: bool) -> HostKey {
    HostKey {
        keycode: 0,
        pressed,
        repeat: false,
        keysym,
        unmodified_keysym: keysym,
        text: None,
        modifiers: KeyModifiers::default(),
    }
}

/// Tab, held with Alt (`pressed` is Alt's, not Tab's); Shift walks the other way.
pub(super) fn tab(alt: bool, shift: bool) -> HostKey {
    HostKey {
        modifiers: KeyModifiers {
            alt,
            shift,
            ..KeyModifiers::default()
        },
        ..key(0xff09, true)
    }
}

/// `Alt_L`, held (`true`) or let go.
pub(super) fn alt(pressed: bool) -> HostKey {
    HostKey {
        modifiers: KeyModifiers {
            alt: pressed,
            ..KeyModifiers::default()
        },
        ..key(0xffe9, pressed)
    }
}
