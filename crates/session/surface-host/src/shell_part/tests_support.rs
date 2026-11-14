#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

use super::*;
use tilo_session_core::surface::KeyModifiers;

/// The readings the session publishes every turn, empty for each resource no service of this
/// device serves: a view reads an absent sound, battery or connection as none.
pub(super) fn publish_platform_roots(link: &SessionLink) {
    for (resource, ty) in tilolang_tilo::platform_apps::platform_roots() {
        link.publish_reading(resource, &ty, serde_json::json!({}));
    }
    publish_time(link, "4:28 PM");
    publish_styles(link, &shipped_styles());
}

/// The time the session publishes, to the minute.
pub(super) fn publish_time(link: &SessionLink, clock: &str) {
    link.publish_reading(
        "session:time",
        "so.tilo.device#Time",
        serde_json::json!({ "clock": clock }),
    );
}

/// The notification centre the session publishes, holding `notices`.
pub(super) fn publish_notices(link: &SessionLink, notices: &serde_json::Value) {
    link.publish_reading(
        "session:notifications",
        "so.tilo.device#Notifications",
        serde_json::json!({ "count": 0, "notices": notices }),
    );
}

/// The four desk styles `so.tilo.shell` ships, as the session lists them: a view of the
/// arrangement each, named by its doc line (shell.md §6).
pub(super) fn shipped_styles() -> serde_json::Value {
    [
        ("strips", "Strips"),
        ("frames", "Frames"),
        ("spaces", "Spaces"),
        ("desktop", "Desktop"),
    ]
    .into_iter()
    .map(|(name, line)| {
        serde_json::json!({
            "style": format!("so.tilo.shell:{name}"),
            "package": "so.tilo.shell",
            "name": name,
            "line": line,
            "loads": true,
            "reason": "",
        })
    })
    .collect()
}

/// The desk styles the session publishes.
pub(super) fn publish_styles(link: &SessionLink, styles: &serde_json::Value) {
    link.publish_reading(
        "session:styles",
        "so.tilo.device#Styles",
        serde_json::json!({ "offered": styles }),
    );
}

/// The windows the session publishes, each with every field the session writes: what a window
/// leaves out reads as nothing (an empty name, no place, not focused).
pub(super) fn publish_windows(link: &SessionLink, windows: &serde_json::Value) {
    let nothing = serde_json::json!({
        "key": "", "title": "", "origin_app": "", "app_key": "", "icon": "", "strip": "",
        "strip_key": "", "workspace": "", "workspace_key": "", "group_key": "", "tab_index": 0,
        "share": 0.0, "local": false, "device": "", "device_name": "", "place": 0, "recency": 0,
        "focused": false, "preview": "", "not_responding": false,
    });
    let all: Vec<serde_json::Value> = windows
        .as_array()
        .into_iter()
        .flatten()
        .map(|window| {
            let mut full = nothing.clone();
            if let (Some(full), Some(window)) = (full.as_object_mut(), window.as_object()) {
                full.extend(window.clone());
            }
            full
        })
        .collect();
    link.publish_reading(
        "session:windows",
        "so.tilo.device#Windows",
        serde_json::json!({ "all": all }),
    );
}

/// What the session publishes for a part to draw: the desk, and the windows on it.
pub(super) struct Staged {
    pub(super) desk: DeskReading,
    pub(super) windows: serde_json::Value,
}

impl From<DeskReading> for Staged {
    fn from(desk: DeskReading) -> Self {
        Self {
            desk,
            windows: serde_json::json!([]),
        }
    }
}

/// A window of the desk, as the session publishes it, standing in the workspace `1`.
pub(super) fn window(key: &str, title: &str, app: &str, strip: &str) -> serde_json::Value {
    serde_json::json!({
        "key": key,
        "title": title,
        "origin_app": app,
        "strip": strip,
        "workspace": "Desk",
        "workspace_key": "1",
    })
}

/// A desk at rest with two windows and two workspaces.
pub(super) fn reading_with_windows() -> Staged {
    let mut release = window("11", "Release planning", "Notes", "Main");
    release["focused"] = serde_json::json!(true);
    Staged {
        desk: DeskReading {
            person: "Pat".to_owned(),
            workspace: "Desk".to_owned(),
            strip: "Main".to_owned(),
            workspaces: vec![WorkspaceReading {
                key: "1".to_owned(),
                name: "Desk".to_owned(),
                current: true,
            }],
            ..DeskReading::default()
        },
        windows: serde_json::json!([release, window("12", "Inbox", "Mail", "Main")]),
    }
}

/// The shell package compiled, as every part of these tests loads it. A test that stands more
/// than one part compiles it once and hands each part a copy of its own ([`part_of`]): compiling
/// it again is the same work with the same result, and a part shares nothing of the packages it
/// is handed with another.
pub(super) fn compiled() -> Vec<Compiled> {
    crate::shell_part::compile_shell().unwrap_or_else(|why| panic!("{why}"))
}

/// A part over `reading`, its outputs set.
pub(super) fn part_with(reading: impl Into<Staged>) -> (ShellPart, Rc<SessionLink>) {
    part_on(reading, (1280, 800))
}

/// A part over `reading` on one output `size` logical pixels big.
pub(super) fn part_on(
    reading: impl Into<Staged>,
    size: (u32, u32),
) -> (ShellPart, Rc<SessionLink>) {
    part_at(compiled(), reading, None, size)
}

/// A part of its own over `reading` on one output `size` logical pixels big, running a copy of
/// `packages` ([`compiled`]): its link, its record and its instance are made for it alone.
pub(super) fn part_of(
    packages: &[Compiled],
    reading: impl Into<Staged>,
    size: (u32, u32),
) -> (ShellPart, Rc<SessionLink>) {
    part_at(packages.to_vec(), reading, None, size)
}

/// A part over `reading`, its outputs set, its wells reporting to `wells`.
pub(super) fn part_as(
    reading: impl Into<Staged>,
    wells: Option<ui::widget::Wells>,
) -> (ShellPart, Rc<SessionLink>) {
    part_at(compiled(), reading, wells, (1280, 800))
}

/// A part running `packages` over `reading` on one output `size` logical pixels big, its wells
/// reporting to `wells`.
fn part_at(
    packages: Vec<Compiled>,
    reading: impl Into<Staged>,
    wells: Option<ui::widget::Wells>,
    size: (u32, u32),
) -> (ShellPart, Rc<SessionLink>) {
    let app = packages
        .last()
        .expect("the shell compiles last")
        .meta
        .app
        .clone();
    let link = Rc::new(SessionLink::default());
    publish_platform_roots(&link);
    let staged: Staged = reading.into();
    publish_windows(&link, &staged.windows);
    link.publish(staged.desk);
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
    part.outputs_changed(&[tilo_session_core::surface::HostedOutput {
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
