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
    let show = action("show_surface");
    assert_eq!(show.params.len(), 1, "the desk's action is a loose one");
    assert_eq!(show.params[0].name, "surface");
    let focus = action("focus");
    assert_eq!(
        focus.params[0].ty, "Window",
        "a window's action takes the window"
    );
    let close = action("close_place");
    assert_eq!(close.params[0].ty, "Int");
    let text = std::fs::read_to_string(dir.join("surface.toml")).expect("the description");
    let addresses = tilolang_tilo::catalogue::addresses(&text).expect("each action one address");
    assert_eq!(addresses["show-surface"], "session:desk");
    assert_eq!(addresses["close-place"], "shell:arrangement");
    assert_eq!(addresses["toggle-tilo-menu"], "desktop:chrome");
    assert!(
        !addresses.contains_key("focus"),
        "a window's focus goes to the window"
    );
}
