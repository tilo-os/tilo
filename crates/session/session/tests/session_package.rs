//! The session's description (`apps/tl/session/surface.toml`) is what its door publishes, and
//! it reads as the package a shell uses.

use std::path::{Path, PathBuf};

use tilo_session::session_package;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

#[test]
fn the_committed_description_is_what_the_door_publishes() {
    let path = repo().join(session_package::PATH);
    let expected = session_package::description();
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed != expected && std::env::var_os("UPDATE_SNAPSHOTS").is_some_and(|v| v == "1") {
        std::fs::write(&path, &expected).expect("the description is written");
        return;
    }
    assert!(
        committed == expected,
        "{} is not what the session publishes; regenerate it with \
         UPDATE_SNAPSHOTS=1 cargo nextest run -p tilo-session --test session_package, \
         or write what it publishes:\n<<<description\n{expected}description>>>",
        session_package::PATH
    );
}

#[test]
fn the_description_reads_as_the_session_package() {
    let dir = repo().join("apps/tl/session");
    let meta = tilolang_tilo::catalogue::native_app(&dir)
        .expect("the description reads")
        .expect("the session's directory is a described app");
    assert_eq!(meta.app, session_package::APP);
    let action = |name: &str| {
        meta.actions
            .iter()
            .find(|action| action.name == name)
            .unwrap_or_else(|| panic!("the package has no `{name}`"))
    };
    let overview = action("toggle_overview");
    assert!(overview.params.is_empty(), "the chrome's action is a loose one");
    let focus = action("focus");
    assert_eq!(
        focus.params[0].ty, "Window",
        "a window's action takes the window"
    );
    let close = action("close_place");
    assert_eq!(close.params[0].ty, "Int");
    let text = std::fs::read_to_string(dir.join("surface.toml")).expect("the description");
    let addresses = tilolang_tilo::catalogue::addresses(&text).expect("each action one address");
    assert_eq!(addresses["close-place"], "shell:arrangement");
    assert_eq!(addresses["toggle-tilo-menu"], "desktop:chrome");
    assert!(
        !addresses.contains_key("focus"),
        "a window's focus goes to the window"
    );
}

/// Each action is described once, by its owner: the desk and the device's resources are
/// `so.tilo.device`'s, so the session's package describes none of them.
#[test]
fn the_session_package_describes_none_of_the_devices_resources() {
    let text = std::fs::read_to_string(repo().join(session_package::PATH)).expect("the description");
    let addresses = tilolang_tilo::catalogue::addresses(&text).expect("each action one address");
    for (published, ty) in tilolang_tilo::platform_apps::platform_roots() {
        let described: Vec<&String> = addresses
            .iter()
            .filter(|(_, address)| address.as_str() == published)
            .map(|(action, _)| action)
            .collect();
        assert!(
            described.is_empty(),
            "`{published}` is {ty}'s, yet the session's package describes {described:?} on it"
        );
    }
}
