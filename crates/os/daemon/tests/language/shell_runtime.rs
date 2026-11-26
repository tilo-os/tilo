//! The device's shell runtime (`ush`) asks the daemon, at its native endpoint, for Terminal's
//! environment records and a hosted workspace's Terminal shell: the person's own program on this
//! computer, as their other programs ask it. The app is Terminal, fixed by the daemon; the
//! workspace is the one the Terminal window runs for, and a request naming none is refused in
//! words, as is a command that is not one of a record's.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::Path;

use serde_json::{Value, json};
use tilo_device_identity::login::daemon::DeviceActions;
use tilo_device_identity::login::{OPEN_TERMINAL, TERMINAL_RECORDS};

use crate::language::host;

/// The workspace the daemon listening at `native` serves its person in.
fn served_workspace(native: &Path) -> String {
    let mut stream = std::os::unix::net::UnixStream::connect(native).unwrap();
    writeln!(stream, "{}", json!({"native": "status"})).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    let status: Value = serde_json::from_str(&line).unwrap();
    status["workspace"].as_str().unwrap().to_owned()
}

#[test]
fn terminals_environments_round_trip_through_ush_at_the_daemon() {
    // Terminal is installed, as it is on every computer: the workspace takes its records, an
    // environment among them, as its surface declares them.
    host::in_own_process_with(&["terminal"], |profile| {
        let running = tilo_runtime::daemon::Running::start(profile).unwrap();
        let native = running.native_socket().to_owned();
        let actions = DeviceActions::at(&native);

        // A command naming no workspace is refused in words, never carried out in one guessed.
        // The ask waits for the person's services, which serve the workspace from then on.
        let refused = actions
            .ask(TERMINAL_RECORDS, json!({"command": "Records"}))
            .expect_err("no workspace is named");
        assert!(
            refused.message.contains("open Terminal in a workspace"),
            "{refused:?}"
        );
        let workspace = served_workspace(&native);
        assert!(!workspace.is_empty(), "the person's workspace is served");

        // ush keeps an environment as one of Terminal's records in the window's workspace, and
        // reads it back whole, through the daemon: a fresh workspace keeps none, and takes its
        // default environment as Terminal's.
        let home = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(ush::store::STORE_ENV, home.path());
        env.set(ush::store::RECORDS_ENV, &native);
        env.set(ush::store::WORKSPACE_ID_ENV, &workspace);
        let records = ush::records::Records::of(&ush::Store::discover());
        assert_eq!(records.workspace(), workspace);
        assert_eq!(records.keys().unwrap(), Vec::<String>::new());
        assert!(
            records
                .load(tilo_environment::DEFAULT_RECORD)
                .unwrap()
                .is_none()
        );
        let default = tilo_environment::Environment {
            name: tilo_environment::DEFAULT_NAME.to_owned(),
            ..Default::default()
        };
        let made = records
            .create(tilo_environment::DEFAULT_RECORD, &default)
            .unwrap();
        assert_eq!(made.workspace, workspace, "the record names its workspace");
        let read = records
            .load(tilo_environment::DEFAULT_RECORD)
            .unwrap()
            .expect("the environment is kept");
        assert_eq!(read, made);
        assert_eq!(
            records.keys().unwrap(),
            vec![tilo_environment::DEFAULT_RECORD.to_owned()]
        );

        // Nothing of the workspace but Terminal's records is read or written.
        let query = collab_types::store::wire::Command::Query(collab_types::store::Query::of(
            "notes.note",
            10,
        ));
        let refused = actions
            .ask(
                TERMINAL_RECORDS,
                json!({"workspace": workspace, "command": query}),
            )
            .expect_err("a query is no record command");
        assert!(
            refused
                .message
                .contains("only Terminal's environment records are read and written here"),
            "{refused:?}"
        );

        // The person's own workspace runs on this computer: its shells are opened here, and the
        // daemon says so at once, handing nothing over.
        let (answer, handed) = actions
            .ask_receiving(
                OPEN_TERMINAL,
                json!({"workspace": workspace, "cols": 80, "rows": 24}),
                |native| runtime_portal::fd_transport::recv_with_fd(native),
            )
            .unwrap();
        assert_eq!(answer["hosted"], false, "{answer}");
        assert!(handed.is_none());

        running.stop().unwrap();
    });
}
