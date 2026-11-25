//! The person's account credentials, as the session serves it to the apps that ask: Browser asks
//! for vault items for its pages. Every call goes through the action API the shipped app already
//! speaks, on a connection admitted as that app in its workspace. An account's password and a
//! website session are the workspace's vault's (`tilo_workspace_service::workspaces::account_logins`,
//! `tilo_workspace_service::workspaces::website_sessions`), which a session opens only with its
//! device's key service: the session's own tests of its `service_sessions` open it with a test's
//! keys.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              lives until the app it serves is done"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply, CollabClient,
};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_service::LoopWake;
use tilo_session::{AppSocket, Launches, Lists, Options, Session};
use tilo_user_identity::DeviceCert;

use crate::common::held::{WORKSPACE, actor, opened};

/// The app that fills its pages from vault items.
const BROWSER: &str = "so.tilo.browser";
/// The app that keeps email accounts.
const MAIL: &str = "so.tilo.mail";
/// Browser's record in the workspace: the route its app connection joins.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000bf";

/// The person's keys: this device, which hosts the workspace.
#[derive(Clone)]
struct Keys {
    /// The public root of the person's identity, as the workspace names its owner.
    root: [u8; 32],
    device: ed25519_dalek::SigningKey,
    cert: DeviceCert,
}

/// A person, their identity provisioned where their daemon reads it,
/// and the workspace they host in its store.
struct Fixture {
    keys: Keys,
    /// The state directory the device's daemon keeps its workspaces under.
    state: PathBuf,
    // The identity's environment and files, restored and removed last: the daemon and the apps
    // read them while they run.
    _home: tilo_test_env::TestHome,
}

/// A login whose session bus is one no Secret Service listens on: nothing of the person's
/// workspace is kept in this host's keyring.
fn fixture() -> Fixture {
    let mut home = tilo_test_env::TestHome::new().expect("a home for this machine's session");
    home.env().set(FILE_SECRETS_ENV, "1");
    let system = home.root().join("system");
    let no_session_bus = home.root().join("no-session-bus");
    home.env().set(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={}", no_session_bus.display()),
    );
    // The account's own store below the identity directory, where every reader looks, and the
    // person's devices a certificate is checked against before anything signs with it.
    tilo_identity::sources::test_trust::install();
    let personal = tilo_identity::store::account_identity_dir(&home.config().join("tilo/identity"));
    let paths = IdentityPaths::under(&system);
    // Provisioned as the device's daemon provisions: with the device key taken, and let go of
    // once this test holds it, for the daemon this test starts takes it itself.
    let taken = tilo_identity::provisioning::provisioned_as_its_daemon(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let device = taken.key().clone();
    drop(taken);
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    let keys = Keys {
        root: cert.user_public_key,
        device,
        cert,
    };
    crate::common::held::workspace_founded_on_this_device_then(
        home.state(),
        WORKSPACE,
        "Shell",
        &keys.cert,
        &keys.device,
        async |_| {},
    );
    Fixture {
        keys,
        state: home.state().to_path_buf(),
        _home: home,
    }
}

/// A shell showing the workspace, known by its id, owned by the person.
fn shell(keys: &Keys) -> Layout {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let mut workspace = Workspace::new(workspace, "Shell");
    workspace.policy.set_owner(actor(&keys.root));
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

/// The session serving `app` on `socket` , holding the fixture's workspace from its state and
/// showing its shell.
fn session(socket: &Path, fixture: &Fixture, app: &str) -> crate::common::held::Hosted {
    // The app speaks from this test's own process, so the launch the session would have recorded
    // for it names that process.
    let launches = Arc::new(Launches::default());
    launches.started(std::process::id(), app, WORKSPACE, true);
    let mut hosted = crate::common::held::hosted(
        Options {
            collab: true,
            wake: LoopWake::default(),
            app_socket: AppSocket::At(socket.to_path_buf()),
            person: "Ada".to_owned(),
            launches,
            state: Some(fixture.state.clone()),
            lists: Lists::default(),
            ..Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
    );
    hosted.session.handle().replace_tree(shell(&fixture.keys));
    for _ in 0..4 {
        hosted
            .session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    hosted
}

/// The channel key of `app` 's record in the workspace.
fn channel(app: &str) -> String {
    tilo_workspace_service::apps::route::CrdtRoute::record(app, WORKSPACE, RECORD).channel_key()
}

/// A connection to the session's app wire as `app` in the workspace, as the launcher hands an app one.
fn connect(socket: &Path, app: &str) -> CollabClient {
    let mut stream =
        std::os::unix::net::UnixStream::connect(socket).expect("the app wire's socket");
    launcher::services::write_collab_handoff(&mut stream, app, WORKSPACE)
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

/// `app`'s own calls on a connection admitted as `app`.
fn app_asks(
    socket: &Path,
    app: &str,
    commands: Vec<Command>,
    seen: &std::sync::mpsc::Sender<Vec<Result<Reply, String>>>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket, app);
        let handle = opened(&client, &channel(app))
            .await
            .expect("the app the session launched joins its workspace's channel");
        let mut replies = Vec::new();
        for command in commands {
            replies.push(
                handle
                    .browser_service_session_command(&command)
                    .await
                    .map_err(|error| error.to_string()),
            );
        }
        let _ = seen.send(replies);
    });
}

/// What `app` is answered for each of `commands`, in order, by the session that serves
/// `fixture`'s person, on a connection admitted as `app`.
fn asked_of_the_session(
    fixture: &Fixture,
    app: &'static str,
    commands: Vec<Command>,
) -> Vec<Result<Reply, String>> {
    let (_root, socket) = private_socket();
    let mut session = session(&socket, fixture, app);

    let (seen, received) = std::sync::mpsc::channel();
    let asking = std::thread::spawn(move || app_asks(&socket, app, commands, &seen));
    let replies = until_the_app_is_done(&mut session, &received);
    asking.join().expect("the app's thread");
    replies
}

/// A vault item's reference, made up.
const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";

/// What an app asks about vault items: whether its session has any used for a page, and the
/// text of one for the page action `fill`.
fn item_asks(fill: tilo_actions::InvocationId) -> Vec<Command> {
    vec![
        Command::LendsItems {},
        Command::UseItem {
            invocation: fill,
            item: ITEM.to_owned(),
            origin: "https://login.example.test".to_owned(),
        },
    ]
}

/// A person's own session keeps no vault a page is filled from: Browser is told so when it
/// asks whether items are lent, and an ask for one is refused with its reason.
#[test]
fn a_persons_own_session_has_no_vault_item_used_for_a_page_and_says_why() {
    let fill = tilo_actions::InvocationId::new();
    let replies = asked_of_the_session(&fixture(), BROWSER, item_asks(fill));
    assert!(
        matches!(&replies[0], Ok(Reply::ItemsLent { lent: false })),
        "{:?}",
        replies[0]
    );
    assert!(
        matches!(
            &replies[1],
            Ok(Reply::ItemRefused { invocation, reason })
                if *invocation == fill && reason.contains("holds no Browser of that workspace")
        ),
        "{:?}",
        replies[1]
    );
}

/// Only Browser asks about vault items: another app's question is refused, and its ask for
/// an item's text is answered with that reason.
#[test]
fn an_app_that_is_not_browser_is_told_that_only_browser_has_a_page_filled() {
    let fill = tilo_actions::InvocationId::new();
    let replies = asked_of_the_session(&fixture(), MAIL, item_asks(fill));
    assert!(replies[0].is_err(), "{:?}", replies[0]);
    assert!(
        matches!(
            &replies[1],
            Ok(Reply::ItemRefused { invocation, reason })
                if *invocation == fill && reason.contains("Only Browser")
        ),
        "{:?}",
        replies[1]
    );
}
