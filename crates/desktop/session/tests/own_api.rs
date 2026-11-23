//! Tilo's API at the session's own socket: a program of this login —
//! `tilolang repl`, `tilo` — answers as the person against the workspaces
//! this desktop holds, because the socket's own permissions and the kernel's
//! account of the peer are what admit it, and the desktop answers as the one
//! key the session keeps in memory. No key is written beside the socket.
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

use crate::common::{
    PERSON, api_account, api_request, app_store, device_in, headless, seeded_apps, until,
};

/// A session that holds its workspaces here, with the home it keeps them in: the home lives as
/// long as the session, since nothing of the session's reads it after the home is gone.
struct Desktop {
    _home: tilo_test_env::TestHome,
    session: tilo_session::Session,
}

/// The headless session that holds its workspaces here and serves its person's API at `api`:
/// what a program of this login reaches as the person.
fn session_with_api(root: &std::path::Path, api: &std::path::Path) -> Desktop {
    // A device of the person's own, as a person's computer is: the API's key is this device's
    // certificate, and its workspaces are held here.
    let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
    let state = device_in(&mut home);
    // The apps a program compiles against: this test's store, installed as a person's device
    // installs them, and named to this home as a device names its own. It is installed after
    // the home is made, since the home names the temporary directory the store is made in.
    let store = app_store();
    home.env().set("TILO_APPS_DATA_DIR", store);
    let session = headless(Options {
        person: PERSON.to_owned(),
        state: Some(state),
        shell: true,
        collab: false,
        // The seeded apps, `tasks` among them: what a program reads.
        services: seeded_apps(),
        app_socket: tilo_session::AppSocket::At(root.join("app_wire.sock")),
        api_server: Some(api.to_owned()),
        ..Options::default()
    });
    Desktop {
        _home: home,
        session,
    }
}

/// Once the thread serving the API is up: the socket's name is there as soon as it is bound, and
/// a connection is what says it serves.
fn served(session: &mut tilo_session::Session, api: &std::path::Path) {
    until(session, "the session's API socket", |_| {
        std::os::unix::net::UnixStream::connect(api).is_ok()
    });
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
    let mut listed = serde_json::Value::Null;
    until(&mut session, "the desktop's workspace list", |_| {
        listed = api_account(&api, "account/workspaces", "list", &json!({}))
            .unwrap_or(serde_json::Value::Null);
        listed
            .get("workspaces")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|workspaces| !workspaces.is_empty())
    });

    let answer = {
        let workspace = listed
            .get("workspaces")
            .and_then(serde_json::Value::as_array)
            .and_then(|workspaces| workspaces.first())
            .expect("the desktop lists the workspace it entered");
        let workspace = workspace
            .get("id")
            .and_then(serde_json::Value::as_str)
            .expect("a workspace id")
            .to_owned();
        api_request(
            &api,
            "POST",
            &format!("/v1/workspaces/{workspace}/programs"),
            Some(&json!({"program": "1 + 1"})),
        )
        .expect("the program answered")
    };
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
