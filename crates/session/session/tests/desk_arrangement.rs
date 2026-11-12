//! What a person arranges on a workspace's desk is theirs to see, and a desk this device cannot
//! write is never said to have been refused by the workspace it shows.
//! A workspace whose records are its members' sealed channels keeps its desk sealed, and a device
//! writes it with the channel keys its secret store holds. A session with no secret store — as a
//! development session on a headless machine is, and any build the platform's store does not reach
//! — holds no channel keys of any workspace. That is this device's own lack, not any workspace's
//! verdict: the desk stays the one this device keeps, and the person is told nothing, because no
//! workspace refused a write it never saw.
//! The bug this holds: the device tried the write anyway, was told by its own engine
//! `this device has no secret store, so it holds no channel keys` , and raised that as a
//! shared-desk refusal — "… did not take your window arrangement: It stays on this device: the
//! change could not be read" — a notice that names the workspace and blames the change's shape.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session \
              outlives every call made into it"
)]

use tilo_desktop::desk_verbs::CHROME_RESOURCE;
use tilo_session::{Options, Session};

/// The person at this device, as `dist/dev/nested-session.sh` mints them: a throwaway test identity
/// whose certificate lets the device govern the workspaces it founds, so the session holds one
/// whose records it seals.
const PERSON: &str = "Pat";

/// The notices the session shows now, each banner's title and body then each one retained in the
/// centre: what the person was told, oldest first.
fn notices(session: &Session) -> Vec<String> {
    let centre = session.notifications();
    let log = centre.log();
    log.active()
        .iter()
        .chain(log.history().iter())
        .map(|notification| format!("{}: {}", notification.title, notification.body))
        .collect()
}

/// A session as `dist/dev/nested-session.sh` starts it: it keeps its desk in
/// `state`, takes part in the collaboration plane, shows the tilolang shell,
/// and holds the workspace it founds there.
///
/// The test environment runs no Secret Service and names no
/// `TILO_SECRETS_DIR`, so this device holds no channel keys: the same state a
/// development session starts in.
fn session(state: &std::path::Path) -> Session {
    crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: true,
        state: Some(state.to_path_buf()),
        ..Options::default()
    })
}

/// The workspaces the desk shows, by name and id.
fn workspaces(session: &Session) -> Vec<(String, String)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .map(|workspace| (workspace.name.clone(), workspace.id().to_string()))
            .collect()
    })
}

/// The desktop's own action that opens an app, as `dist/dev/nested-session.sh` drives a session:
/// an app opened on the desk is a window of the workspace in front, and so a change of that
/// workspace's shared desk.
const OPEN_APP: &str = "open-app";

/// A session whose device holds no channel keys arranges its desk — an app is opened in the
/// workspace in front — and no workspace is said to have refused the arrangement: the desk stays
/// this device's, and the person hears nothing.
#[test]
fn a_desk_this_device_cannot_seal_is_not_said_to_have_been_refused() {
    let _settled = ui::motion::testing::settled();
    // What the session says about the desk's write: the log is where a write that was not made says
    // why, and this test reads it too.
    let logged = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    {
        let sink = LogSink(std::sync::Arc::clone(&logged));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new(
                "tilo_session::shared_desktop=warn",
            ))
            .with_ansi(false)
            .with_writer(sink)
            .try_init();
    }
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    // The identity `dist/dev/nested-session.sh` mints before it starts its session: a throwaway whose
    // certificate lets the device govern its own workspaces.
    let identity = home.config().join("tilo/identity");
    std::fs::create_dir_all(&identity).expect("the identity's directory");
    let root = home.root().join("system");
    home.env().set("TILO_IDENTITY_ROOT", &root);
    tilo_identity_host::provisioning::ensure_throwaway_test_identity(
        &identity,
        &tilo_identity_host::provisioning::collaboration_identity_paths(),
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .expect("the throwaway identity");

    let state = home.state().to_path_buf();
    let mut session = session(&state);

    // The desk is up, and the workspace it shows is one the engine holds: a workspace it founded
    // here keeps its records as sealed channels.
    crate::common::until(&mut session, "the workspace the desk shows", |session| {
        !workspaces(session).is_empty()
    });

    // The person arranges the desk: an app opens in the workspace in front,
    // which is a change of that workspace's shared desk. This device holds no
    // channel keys, so it keeps the change itself.
    session
        .invoke(
            CHROME_RESOURCE,
            OPEN_APP,
            serde_json::json!({"app": "so.tilo.lang.notes"})
                .as_object()
                .expect("an object"),
        )
        .expect("an app opens on the desk");
    crate::common::until(&mut session, "the app's window on the desk", |session| {
        session.handle().read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| &workspace.strips)
                .flat_map(|strip| &strip.window_groups)
                .flat_map(|group| &group.tabs)
                .any(|tab| tab.title == "Notes")
        })
    });
    // What the desk says to itself: the write was not made, and why.
    let refusal = logged.lock().expect("the log").clone();
    assert!(
        !refusal.contains("was not written"),
        "the desk is not said to have failed a write it never had the keys to make: {refusal}"
    );

    let told = notices(&session);
    assert!(
        !told
            .iter()
            .any(|notice| notice.contains("did not take your window arrangement")),
        "no workspace is said to have refused the desk: {told:?}"
    );
}

/// The log this test's session writes, kept where the test reads it.
struct LogSink(std::sync::Arc<std::sync::Mutex<String>>);

impl std::io::Write for LogSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut log) = self.0.lock() {
            log.push_str(&String::from_utf8_lossy(bytes));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
    type Writer = LogSink;

    fn make_writer(&'a self) -> Self::Writer {
        LogSink(std::sync::Arc::clone(&self.0))
    }
}
