#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

use super::*;
use tilo_compositor::KeyModifiers;

/// The readings the session publishes every turn, empty for each resource no service of this
/// device serves: a view reads an absent sound, battery or connection as none.
pub(super) fn publish_platform_roots(link: &SessionLink) {
    for (resource, ty) in tilolang_tilo::platform_apps::platform_roots() {
        link.publish_reading(resource, &ty, serde_json::json!({}));
    }
}

/// The four desk styles `so.tilo.shell` ships, as the session lists them: a view of the
/// arrangement each, named by its doc line (shell.md §6).
pub(super) fn shipped_styles() -> Vec<DeskStyleReading> {
    [
        ("strips", "Strips"),
        ("frames", "Frames"),
        ("spaces", "Spaces"),
        ("desktop", "Desktop"),
    ]
    .into_iter()
    .map(|(name, line)| DeskStyleReading {
        style: format!("so.tilo.shell:{name}"),
        package: "so.tilo.shell".to_owned(),
        name: name.to_owned(),
        line: line.to_owned(),
        loads: true,
        reason: String::new(),
    })
    .collect()
}

/// A desk at rest with two windows and two workspaces.
pub(super) fn reading_with_windows() -> DeskReading {
    DeskReading {
        person: "Pat".to_owned(),
        clock: "4:28 PM".to_owned(),
        workspace: "Desk".to_owned(),
        strip: "Main".to_owned(),
        styles: shipped_styles(),
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
    part_at(reading, None, size)
}

/// A part over `reading`, its outputs set, its wells reporting to `wells`.
pub(super) fn part_as(
    reading: DeskReading,
    wells: Option<ui::widget::Wells>,
) -> (ShellPart, Rc<SessionLink>) {
    part_at(reading, wells, (1280, 800))
}

/// A part over `reading` on one output `size` logical pixels big, its wells reporting to
/// `wells`.
fn part_at(
    reading: DeskReading,
    wells: Option<ui::widget::Wells>,
    size: (u32, u32),
) -> (ShellPart, Rc<SessionLink>) {
    let packages = crate::shell_part::compile_shell().unwrap_or_else(|why| panic!("{why}"));
    let app = packages
        .last()
        .expect("the shell compiles last")
        .meta
        .app
        .clone();
    let link = Rc::new(SessionLink::default());
    publish_platform_roots(&link);
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
        1.0,
        std::sync::Arc::new(|| {}),
        wells,
    );
    let (width, height) = size;
    let (w, h) = (
        i32::try_from(width).unwrap(),
        i32::try_from(height).unwrap(),
    );
    part.outputs_changed(&[tilo_compositor::HostedOutput {
        rect: (0, 0, w, h),
        scale: 1.0,
    }]);
    part.set_display(Some(("HEADLESS-1".to_owned(), (0, 0, width, height))), 1.0);
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
        latin_keysym: keysym,
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

/// The key above Tab, held with Alt: the walk over the app in front's windows.
pub(super) fn grave(shift: bool) -> HostKey {
    HostKey {
        modifiers: KeyModifiers {
            alt: true,
            shift,
            ..KeyModifiers::default()
        },
        ..key(0x0060, true)
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
