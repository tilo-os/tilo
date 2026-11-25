//! Tilo's API at the session's own socket: a program of this login —
//! `tilolang repl`, `tilo` — answers as the person against the workspaces
//! this desktop holds, because the socket's own permissions and the kernel's
//! account of the peer are what admit it, and the desktop answers as this
//! device's own certificate, kept with its identity. No key is written beside
//! the socket.
//!
//! A request over that socket, as both commands make one: the listing it asks
//! first (`account/workspaces#list`) comes from the workspaces this device
//! entered, and a program it then sends (`POST …/programs`) is carried out in
//! the workspace as the person, through this session's own API.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::json;
use tilo_session::Options;

use crate::common::{api_account, api_request, app_store, device_in, headless, seeded_apps, until};

/// A session on this computer, with the home it keeps it in: the home lives as long as the
/// session, since nothing of the session's reads it after the home is gone.
struct Desktop {
    session: tilo_session::Session,
    _home: tilo_test_env::TestHome,
}

/// The headless session that runs as the shell of this computer's device daemon and serves its
/// person's API at `api`: what a program of this login reaches as the person. A computer's session
/// reaches its workspaces over the daemon's shell link, and the API answers from the device's one
/// dispatch on that link, so the daemon is started here as the person's own service starts it.
fn session_with_api(root: &std::path::Path, api: &std::path::Path) -> Desktop {
    session_serving(root, api, seeded_apps())
}

/// [`session_with_api`] running `services`.
fn session_serving(
    root: &std::path::Path,
    api: &std::path::Path,
    services: Vec<Box<dyn tilo_service::Service>>,
) -> Desktop {
    use std::os::unix::fs::PermissionsExt as _;
    // A device of the person's own, as a person's computer is: the API's key is this device's
    // certificate, and its workspaces are held by the daemon.
    let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
    home.env()
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    let state = device_in(&mut home);
    // The daemon started below takes the device key itself, so this process lets go of the one it
    // took for the identity.
    crate::common::let_go_of_device_keys();
    // The apps a program compiles against: this test's store, installed as a person's device
    // installs them, and named to this home as a device names its own. It is installed after
    // the home is made, since the home names the temporary directory the store is made in.
    let store = app_store();
    home.env().set("TILO_APPS_DATA_DIR", store);
    let profile = tempfile::tempdir().expect("a profile for the daemon");
    std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the profile is this user's");
    let within = tilo_test_env::hang_bound();
    let daemon = crate::common::start_daemon(profile.path());
    let person = daemon.person(within).expect("its person's services run");
    let shell_link = daemon
        .link_shell(within)
        .expect("the daemon makes the session's link");
    let session = headless(Options {
        person,
        state: Some(state),
        shell_link: Some(shell_link),
        shell_daemon: Some(Box::new((daemon, profile))),
        collab: false,
        services,
        app_socket: tilo_session::AppSocket::At(root.join("app_wire.sock")),
        api_socket: Some(api.to_owned()),
        ..Options::default()
    });
    Desktop {
        session,
        _home: home,
    }
}

/// Once the thread serving the API is up: the socket's name is there as soon as it is bound, and
/// a connection is what says it serves.
fn served(session: &mut tilo_session::Session, api: &std::path::Path) {
    until(session, "the session's API socket", |_| {
        std::os::unix::net::UnixStream::connect(api).is_ok()
    });
}

/// The workspace the desktop holds, by its id: the list a program of the person's reads first.
fn held_workspace(session: &mut tilo_session::Session, api: &std::path::Path) -> String {
    let mut listed = serde_json::Value::Null;
    until(session, "the desktop's workspace list", |_| {
        listed = api_account(api, "account/workspaces", "list", &json!({}))
            .unwrap_or(serde_json::Value::Null);
        listed
            .get("workspaces")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|workspaces| !workspaces.is_empty())
    });
    listed["workspaces"][0]["id"]
        .as_str()
        .expect("the desktop lists the workspace it entered")
        .to_owned()
}

/// A setter of a settings section, called through the API as a generated SDK calls it, is typed
/// by the sections' own surface whichever app shows the section: the person's and the device's
/// sections are both set, as the session reads them. A value of the wrong type is refused, and so are types
/// written for another surface of the sections.
#[test]
fn a_sections_setter_is_typed_by_the_sections_surface_at_the_sessions_api() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let socket = root.path().join("api.sock");
    let desktop = session_serving(
        root.path(),
        &socket,
        vec![Box::new(settings_sections::service::SettingsService::new())],
    );
    let mut session = desktop.session;
    served(&mut session, &socket);
    let api = crate::common::Api::open(&mut session, root.path());
    // What `tilo sdk gen` writes the sections' types for: their compiled surface.
    let kept = include_bytes!("../../../desktop/settings-sections/published.json");
    collab_types::surface::SurfaceFile::compiled(kept)
        .expect("the sections' compiled surface parses");
    let kept = std::str::from_utf8(kept).expect("the compiled surface is UTF-8");
    let surface =
        tilo_api::v1::surface_version(tilo_actions::remote::settings_resources::SURFACE_APP, kept);
    let appearance = tilo_app::framework::section_address("appearance", None);
    let power = tilo_app::framework::section_address("power", None);
    assert!(
        power.starts_with("devices/"),
        "power is a section of this device: {power}"
    );
    let set = |session: &mut tilo_session::Session,
               address: &str,
               setter: &str,
               value: serde_json::Value,
               surface: String| {
        api.send_typed(
            &format!("resources/{address}/actions/{setter}"),
            Some(json!({"value": value})),
            Some(surface),
        )
        .answer(session)
    };
    for (address, setter, value) in [
        (&appearance, "set-mode", json!("light")),
        (&power, "set-blank-minutes", json!(10)),
    ] {
        let answer = set(&mut session, address, setter, value, surface.clone());
        assert!(
            answer.is_ok(),
            "{address}#{setter} is set through its types: {answer:?}"
        );
    }
    for (address, field, wanted) in [
        (&appearance, "mode", json!("light")),
        (&power, "blank_minutes", json!(10)),
    ] {
        let read = crate::common::wait_for(&mut session, "the setting to change", |session| {
            session.settle_services();
            let read = session.read(address)?.get(field).cloned()?;
            (read == wanted).then_some(read)
        });
        assert_eq!(read, wanted, "{address}.{field} is what the API set");
    }

    let refused = set(&mut session, &appearance, "set-mode", json!(5), surface);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.ends_with("(invalid_arguments)")),
        "a number is no light-or-dark mode: {refused:?}"
    );
    let other = "so.tilo.settings.sections@0000000000000000".to_owned();
    let refused = set(&mut session, &power, "set-blank-minutes", json!(15), other);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.ends_with("(surface_changed)")),
        "types written for another surface of the sections are refused: {refused:?}"
    );
}

/// A request at the socket lists the person's workspaces, and a program it sends to one of them
/// answers as the person: `describe(tasks)` reaches the workspace's tasks.
#[test]
fn a_program_of_this_login_answers_as_the_person_at_the_sessions_own_api() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api.sock");
    let desktop = session_with_api(root.path(), &api);
    let mut session = desktop.session;
    served(&mut session, &api);

    // The list a program of the person's reads: workspace this device entered.
    let workspace = held_workspace(&mut session, &api);
    let answer = api_request(
        &api,
        "POST",
        &format!("/v1/workspaces/{workspace}/programs"),
        Some(&json!({"program": "1 + 1"})),
    )
    .expect("the program answered");
    let text = answer
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    assert!(
        text.trim() == "2" && answer.get("status") != Some(&json!("failed")),
        "a program answered as the person: {answer}"
    );
}

/// The socket's directory holds the socket and nothing else: the session's key never leaves the
/// process, so no key file is written beside it.
#[test]
fn the_socket_is_all_its_directory_holds() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api").join("api.sock");
    let desktop = session_with_api(root.path(), &api);
    let mut session = desktop.session;
    served(&mut session, &api);

    let directory = api.parent().expect("the socket's directory");
    // The bind stages the socket in a dot-named directory beside it and moves it into place;
    // that one is gone once the socket answers.
    let mut held: Vec<String> = Vec::new();
    until(&mut session, "the socket's directory", |_| {
        held = std::fs::read_dir(directory)
            .expect("the socket's directory")
            .map(|entry| {
                entry
                    .expect("an entry of the socket's directory")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        !held.iter().any(|name| name.starts_with('.'))
    });
    held.sort();
    assert_eq!(
        held,
        vec![tilo_paths::login::API_SOCKET_FILE.to_owned()],
        "the socket's directory holds the socket and nothing else, never a key: {held:?}"
    );
}

/// A program of this login reaches the workspaces its person holds and no other: one sent to a
/// workspace this computer does not hold is refused, and nothing runs.
#[test]
fn a_program_is_refused_a_workspace_its_person_does_not_hold() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let api = root.path().join("api.sock");
    let desktop = session_with_api(root.path(), &api);
    let mut session = desktop.session;
    served(&mut session, &api);
    let held = held_workspace(&mut session, &api);

    let elsewhere = uuid::Uuid::new_v4().to_string();
    assert_ne!(held, elsewhere);
    let refused = api_request(
        &api,
        "POST",
        &format!("/v1/workspaces/{elsewhere}/programs"),
        Some(&json!({"program": "1 + 1"})),
    );
    // Refused by the API's answer, not by a connection that failed.
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| !why.contains("API socket")),
        "a workspace the person does not hold is refused: {refused:?}"
    );
}
