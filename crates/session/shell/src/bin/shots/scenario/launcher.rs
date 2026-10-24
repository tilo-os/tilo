//! Global search before anything is typed, on a fresh device and a busy
//! one.

use winit::keyboard::{Key, NamedKey};

use super::{Action, Scenario};

/// Global search before anything is typed: the launcher. A device
/// with nothing going on shows only Tilo's starting points, then every app
/// through "All apps".
pub(super) fn fresh() -> Scenario {
    Scenario {
        name: "launcher_fresh",
        description: "Super+Space on a device with no activity: the launcher lists Tilo's starting points and All apps; choosing All apps lists every app.",
        actions: vec![
            Action::SeedLauncherFresh,
            Action::OpenPalette,
            Action::WaitSettled,
            Action::Snapshot("01-launcher-fresh"),
            Action::Key(Key::Named(NamedKey::ArrowUp)),
            Action::Key(Key::Named(NamedKey::Enter)),
            Action::WaitSettled,
            Action::Snapshot("02-all-apps"),
        ],
    }
}

/// The busy launcher: who is where, what waits, what to continue,
/// then the apps; widened to every workspace; typing turns it into
/// results and clearing brings it back.
pub(super) fn busy() -> Scenario {
    Scenario {
        name: "launcher_busy",
        description: "Super+Space with people in two workspaces, a call ringing, a share waiting and recent notes; widen to every workspace, type, clear.",
        actions: vec![
            Action::SeedLauncherBusy,
            Action::OpenPalette,
            Action::WaitSettled,
            Action::Snapshot("01-launcher-workspace"),
            Action::Key(Key::Named(NamedKey::Backspace)),
            Action::WaitSettled,
            Action::Snapshot("02-launcher-every-workspace"),
            Action::Type("pi"),
            Action::WaitSettled,
            Action::Snapshot("03-typed"),
            Action::Key(Key::Named(NamedKey::Backspace)),
            Action::Key(Key::Named(NamedKey::Backspace)),
            Action::WaitSettled,
            Action::Snapshot("04-cleared"),
        ],
    }
}
