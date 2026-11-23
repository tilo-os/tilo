//! Settings shares the Wi-Fi passphrase this device learned with the person's other devices, and
//! answers for a network they shared, over the action API a shipped app already speaks: the
//! session answers it from its own device fabric, sealed to each of the person's devices.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it serves is done"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use runtime_collab::{
    CollabClient, PortableWifiSecurity, WifiContinuityCommand, WifiContinuityReply,
};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_service::LoopWake;
use tilo_session::{AppSocket, Launches, Options, Session};
use tilo_user_identity::DeviceCert;

use crate::common::held::{WORKSPACE, actor, presentation};

/// What Settings is called in the workspace.
const SETTINGS: &str = "so.tilo.settings";
/// The record of Settings the apps change.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000bb";

const SSID: &str = "Home";
const PASSWORD: &str = "a passphrase sealed to the person's devices";

/// The person's keys: this device's, as the identity store provisioned them.
struct Person {
    device: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    // The environment the identity was provisioned in, restored before the directories it names are
    // removed.
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
    _home: tempfile::TempDir,
}

fn person() -> Person {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let home = tempfile::tempdir().expect("a home for this machine's session");
    let xdg = root.path().join("config");
    let system = root.path().join("system");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("TILO_IDENTITY_ROOT", &system)
        .set("FILE_SECRETS_ENV", FILE_SECRETS_ENV)
        .set("HOME", home.path());
    let personal = xdg.join("tilo/identity");
    let paths = IdentityPaths::under(&system);
    tilo_identity::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let device = tilo_device_identity::persist::load_device_key(&paths).expect("this device's key");
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    Person {
        device,
        cert,
        _env: env,
        _root: root,
        _home: home,
    }
}

/// A shell showing the workspace, known by its id, owned by the person.
fn shell(person: &Person) -> Layout {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let mut workspace = Workspace::new(workspace, "Shell");
    workspace.set_owner(actor(&person.cert.user_public_key));
    Layout {
        workspaces: vec![workspace.clone()],
        active: workspace.id(),
        ..Layout::default()
    }
}

/// A directory for the app wire's socket that only this user reaches: the broker refuses to serve one
/// anybody else could open.
fn private_socket() -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the app wire in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the app wire's directory is this user's");
    let socket = root.path().join("collab.sock");
    (root, socket)
}

/// A session serving Settings on `socket` , on this computer's certified device, with the person's
/// shell. The passphrase is kept in the person's own store, so the session keeps its state under
/// `state`.
fn session(socket: &Path, person: &Person, state: &Path) -> Session {
    // The app speaks from this test's own process, so the launch the session would have recorded
    // for it names that process.
    let launches = Arc::new(Launches::default());
    launches.started(std::process::id(), SETTINGS, WORKSPACE, true);
    let mut session = crate::common::headless(Options {
        collab: true,
        wake: LoopWake::default(),
        app_socket: AppSocket::At(socket.to_path_buf()),
        person: "Ada".to_owned(),
        launches,
        state: Some(state.to_path_buf()),
        ..Options::default()
    });
    session.handle().replace_tree(shell(person));
    for _ in 0..4 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    session
}

/// The channel key of Settings' record in the workspace.
fn channel() -> String {
    tilo_workspace_service::apps::route::CrdtRoute::record(SETTINGS, WORKSPACE, RECORD)
        .channel_key()
}

/// An app connected to the session's app wire as Settings in the workspace, as the launcher hands it
/// the connection.
fn connect(socket: &Path) -> CollabClient {
    let mut stream =
        std::os::unix::net::UnixStream::connect(socket).expect("the app wire's socket");
    launcher::services::write_collab_handoff(&mut stream, SETTINGS, WORKSPACE)
        .expect("the launcher names the app on the connection");
    stream
        .set_nonblocking(true)
        .expect("the connection is ready for the runtime");
    let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
    let (reader, writer) = stream.into_split();
    let (client, carrying) = CollabClient::over(reader, writer);
    tokio::spawn(carrying);
    client
}

/// Settings' own calls, on a connection to the session's app wire: the passphrase it learned here is
/// shared, then read back, as their other device would.
fn app(
    socket: &Path,
    device: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
    seen: &std::sync::mpsc::Sender<Vec<WifiContinuityReply>>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket);
        let handle = client
            .join_channel(&channel(), &presentation(device, cert))
            .await
            .expect("the app the session launched joins its workspace's channel");
        let mut replies = Vec::new();
        replies.push(
            handle
                .wifi_continuity_command(&WifiContinuityCommand::Share {
                    ssid: SSID.to_owned(),
                    security: PortableWifiSecurity::Wpa2,
                    password: PASSWORD.to_owned(),
                })
                .await
                .expect("the session takes the passphrase"),
        );
        replies.push(
            handle
                .wifi_continuity_command(&WifiContinuityCommand::Lookup {
                    ssid: SSID.to_owned(),
                    security: PortableWifiSecurity::Wpa2,
                })
                .await
                .expect("the session answers for the network"),
        );
        // A network nobody shared is not one this device can join with.
        replies.push(
            handle
                .wifi_continuity_command(&WifiContinuityCommand::Lookup {
                    ssid: "Nowhere".to_owned(),
                    security: PortableWifiSecurity::Wpa2,
                })
                .await
                .expect("the session answers for the network"),
        );
        let _ = seen.send(replies);
    });
}

/// The session's pass until the app is done, as the session runs its loop.
fn until_the_app_is_done<T>(session: &mut Session, seen: &std::sync::mpsc::Receiver<T>) -> T {
    crate::common::wait_for(session, "the app to finish its calls", |session| {
        session.settle_services();
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
        seen.try_recv().ok()
    })
}

#[test]
fn settings_shares_and_reads_a_wifi_passphrase_through_the_session() {
    let person = person();
    let (_root, socket) = private_socket();
    let state = tempfile::tempdir().expect("a state directory for the session");
    let mut session = session(&socket, &person, state.path());

    let (seen, received) = std::sync::mpsc::channel();
    let (device, cert) = (person.device.clone(), person.cert);
    let app = std::thread::spawn(move || app(&socket, &device, &cert, &seen));
    let replies = until_the_app_is_done(&mut session, &received);
    app.join().expect("the app's thread");

    assert_eq!(
        replies[0],
        WifiContinuityReply::Shared {
            ssid: SSID.to_owned()
        },
        "the session seals the passphrase to the person's devices"
    );
    assert_eq!(
        replies[1],
        WifiContinuityReply::Password {
            password: PASSWORD.to_owned()
        },
        "and reads it back for this device, which it was sealed to"
    );
    assert_eq!(
        replies[2],
        WifiContinuityReply::NotFound,
        "a network nobody shared has no passphrase"
    );
}
