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
         UPDATE_SNAPSHOTS=1 cargo nextest run -p tilo-session --test it session_package::, \
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
    let reveal = action("reveal_top_bar");
    assert!(
        reveal.params.is_empty(),
        "the chrome's action is a loose one"
    );
    let focus = action("focus");
    assert_eq!(
        focus.params[0].ty, "Window",
        "a window's action takes the window"
    );
    let close = action("close");
    assert_eq!(
        close.params[0].ty, "Place",
        "a place's action takes the place"
    );
    // What a place shows is a resource, passed as the value a program has; what `place` may go
    // without is optional, as a tilolang action's is: left out, or given as the plain value.
    let place = action("place");
    let param = |name: &str| {
        place
            .params
            .iter()
            .find(|param| param.name == name)
            .unwrap_or_else(|| panic!("`place` has no `{name}`"))
    };
    assert_eq!(param("resource").ty, "tilo.host.Resource");
    assert_eq!(param("resource").default, None, "a place shows something");
    assert_eq!(param("x").ty, "Option<Int>");
    assert_eq!(
        param("x").default,
        Some(tilolang_tilo::runtime::meta::DefaultMeta::none())
    );
    let text = std::fs::read_to_string(dir.join("surface.toml")).expect("the description");
    let addresses = tilolang_tilo::catalogue::addresses(&text).expect("each action one address");
    assert_eq!(addresses["place"], "shell:arrangement");
    assert_eq!(addresses["tilo-menu"], "desktop:chrome");
    assert!(
        !addresses.contains_key("focus"),
        "a window's focus goes to the window"
    );
    assert!(
        !addresses.contains_key("close"),
        "a place's close goes to the place"
    );
}

/// Each action is described once, by its owner: the desk and the device's resources are
/// `so.tilo.device`'s, so the session's package describes none of them.
#[test]
fn the_session_package_describes_none_of_the_devices_resources() {
    let text =
        std::fs::read_to_string(repo().join(session_package::PATH)).expect("the description");
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

/// The chrome's `move-tab` takes its destination as a plain record of named fields, which
/// tilolang has no value of: the package declares the record's fields as parameters of their
/// own, each optional as its field is, and a call's arguments for them reach the door nested
/// under the destination's name again.
#[test]
fn a_record_parameter_is_declared_as_its_fields_and_nested_back_on_the_wire() {
    let text =
        std::fs::read_to_string(repo().join(session_package::PATH)).expect("the description");
    let surfaces = tilolang_tilo::catalogue::parse_surfaces(&text).expect("the description reads");
    let move_tab = surfaces
        .iter()
        .flat_map(|surface| &surface.verbs)
        .find(|verb| verb.id == "move-tab")
        .expect("the chrome's move-tab");
    let declared: Vec<(&str, bool, Option<&str>)> = move_tab
        .params
        .iter()
        .map(|param| (param.name.as_str(), param.required, param.within.as_deref()))
        .collect();
    assert_eq!(
        declared,
        [
            ("tab", true, None),
            ("strip", false, Some("to")),
            ("workspace", false, Some("to")),
            ("display", false, Some("to")),
            ("before", false, Some("to")),
            ("group", false, Some("to")),
        ]
    );
    let wire = tilo_actions::surface::nest_arguments(
        move_tab,
        vec![
            ("tab".to_owned(), serde_json::json!("7")),
            ("strip".to_owned(), serde_json::json!("3")),
        ],
    );
    let destination: tilo_desktop_surface::navigation::Destination =
        serde_json::from_value(wire["to"].clone()).expect("the chrome reads the destination");
    assert_eq!(
        serde_json::Value::Object(wire),
        serde_json::json!({"tab": "7", "to": {"strip": "3"}})
    );
    assert_eq!(destination.strip.as_deref(), Some("3"));
}
