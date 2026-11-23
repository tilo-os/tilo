//! The person's account credentials, as the session serves it to the apps that ask: Browser lists,
//! resolves and forgets the website sessions the person selected, and asks for vault items for
//! its pages. Every call goes through the action API the shipped app already speaks, on a
//! connection admitted as that app in its workspace. An account's password is the workspace's
//! vault's (`tilo_workspace_service::workspaces::account_logins`).

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

use accounts::{AccountId, SecretVault};
use browser_continuity::sessions::{Cookie, SameSite, Service, SessionCookies};
use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply, CollabClient,
};
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_peer_state::owner;
use tilo_service::LoopWake;
use tilo_session::{AppSocket, Launches, Lists, Options, Session};
use tilo_user_identity::DeviceCert;

use crate::common::held::{WORKSPACE, actor, presentation};

/// The app whose website sessions these are.
const BROWSER: &str = "so.tilo.browser";
/// The app that keeps email accounts.
const MAIL: &str = "so.tilo.mail";
/// Browser's record in the workspace: the route its app connection joins.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000bf";
/// What the selected website session's cookie holds.
const MARKER: &str = "private-session-marker";

/// Whether this host has a keyring to keep the person's vault key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Keyring {
    Answers,
    Missing,
}

/// The person's keys: this device, which hosts the workspace, and another of their devices, which
/// only joined it.
#[derive(Clone)]
struct Keys {
    /// The public root of the person's identity, as the workspace names its owner.
    root: [u8; 32],
    device: ed25519_dalek::SigningKey,
    cert: DeviceCert,
    /// Another of the person's devices, holding this person's key as their phone does.
    phone: ed25519_dalek::SigningKey,
    phone_cert: DeviceCert,
}

/// A person, their identity provisioned where this test's session reads it,
/// and the workspace they host in its state.
struct Fixture {
    keys: Keys,
    /// The state directory the session keeps its workspaces under.
    state: PathBuf,
    // The identity's environment and files, restored and removed last: the session and the apps
    // read them while they run.
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
    _home: tempfile::TempDir,
}

/// A login on a host whose keyring answers, or on one that has none and
/// keeps the vault key in an owner-only file beside the ciphertext.
///
/// A keyring that answers is the file-backed secret store
/// [`tilo_secrets::SECRETS_DIR_ENV`] names in place of the Secret
/// Service. Either way the session bus is one no Secret Service listens on,
/// so on the second the host has no keyring whichever machine runs this.
fn fixture(keyring: Keyring) -> Fixture {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let home = tempfile::tempdir().expect("a home for this machine's session");
    let xdg = root.path().join("config");
    let system = root.path().join("system");
    let state = root.path().join("state");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("TILO_IDENTITY_ROOT", &system)
        .set(FILE_SECRETS_ENV, "1")
        .set("HOME", home.path());
    env.set(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={}", root.path().join("no-session-bus").display()),
    );
    if keyring == Keyring::Answers {
        env.set(tilo_secrets::SECRETS_DIR_ENV, root.path().join("secrets"));
    }
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
    // The person's other device, enrolled as a pairing enrolls one.
    let now = tilo_clock::unix_now_ns();
    let fleet =
        tilo_identity::device_fleet::DeviceFleetStore::from_env().expect("the device fleet");
    let profile = tilo_identity::store::identity_for_workspace(WORKSPACE)
        .expect("the identity registry")
        .expect("the workspace is answered by this device's person");
    let phone = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
    let phone_cert = fleet
        .certify_device(
            &profile.id,
            phone.verifying_key().to_bytes(),
            "The phone",
            tilo_user_identity::DeviceCapabilities::member(),
            now,
        )
        .expect("the second device is enrolled");
    let keys = Keys {
        root: cert.user_public_key,
        device,
        cert,
        phone,
        phone_cert,
    };
    hold_workspace(&state, &keys, &selected_account());
    Fixture {
        keys,
        state,
        _env: env,
        _root: root,
        _home: home,
    }
}

/// The workspace, founded on this device's own workspace service in `state` as the person makes
/// one here, with the person's website session `selected` among its accounts and their other
/// device admitted as an editor: what pairing another of the person's devices does.
fn hold_workspace(state: &Path, keys: &Keys, selected: &accounts::Account) {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let phone_state = tempfile::tempdir().expect("the phone's own state");
    crate::common::held::workspace_founded_on_this_device_then(
        state,
        WORKSPACE,
        "Shell",
        &keys.cert,
        &keys.device,
        async |service| {
            service
                .workspaces()
                .keep_workspace_account(WORKSPACE, selected)
                .await
                .expect("the workspace keeps the selected website session");
            let mut request_id = [0_u8; 32];
            request_id[..8].copy_from_slice(&keys.phone_cert.device_public_key[..8]);
            let issued = service
                .workspaces()
                .issue(
                    WORKSPACE,
                    &keys.cert,
                    tilo_server_protocol::workspaces::IssueInvitationRequest {
                        request_id,
                        role: tilo_workspace_join::Role::Editor,
                        recipient_user_public_key: Some(keys.phone_cert.user_public_key),
                        invitee_name: None,
                        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(60_000_000_000),
                        inviter_name: None,
                    },
                )
                .await
                .expect("the workspace invites the person's other device");
            let phone_owner =
                owner::open_file(&phone_state.path().join("owner")).expect("the phone's own owner");
            tilo_peer_state::owner::enroll::prepare_and_complete(
                tilo_peer_state::join_transport::WorkspaceJoinClient::local(
                    service.join_service(),
                    keys.phone_cert.device_public_key,
                ),
                tilo_identity::identity_store::DeviceIdentity::kept(
                    keys.phone_cert.clone(),
                    keys.phone.clone(),
                ),
                workspace,
                None,
                tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation),
                phone_owner,
            )
            .await
            .expect("the person's other device joins");
        },
    );
}

/// A shell showing the workspace, known by its id, owned by the person.
fn shell(keys: &Keys) -> Layout {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let mut workspace = Workspace::new(workspace, "Shell");
    workspace.set_owner(actor(&keys.root));
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
fn session(socket: &Path, fixture: &Fixture, app: &str) -> Session {
    // The app speaks from this test's own process, so the launch the session would have recorded
    // for it names that process.
    let launches = Arc::new(Launches::default());
    launches.started(std::process::id(), app, WORKSPACE, true);
    let mut session = crate::common::headless(Options {
        collab: true,
        wake: LoopWake::default(),
        app_socket: AppSocket::At(socket.to_path_buf()),
        person: "Ada".to_owned(),
        launches,
        state: Some(fixture.state.clone()),
        lists: Lists::default(),
        ..Options::default()
    });
    session.handle().replace_tree(shell(&fixture.keys));
    for _ in 0..4 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    session
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

/// This login's config root: where the person's accounts and their vault are.
fn config_root() -> PathBuf {
    accounts::config_root().expect("this session's config root")
}

/// The workspace's account of the person's selected website session.
fn selected_account() -> accounts::Account {
    let mut account = accounts::Account::new(
        AccountId::from("selected-slack"),
        account_credentials::service_sessions::SERVICE_SESSION_TYPE,
    );
    account.config = serde_json::json!({"service": "slack", "setup_binding": "prepared-operation"});
    account
}

/// A person's selected website session, its cookies sealed in `root` under the person's key as
/// selection leaves it; the workspace keeps its account ([`selected_account`]).
fn select_in(root: &Path, identity: [u8; 32]) -> SessionCookies {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::create_dir_all(root).expect("the person's accounts directory");
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
        .expect("the person's accounts directory is theirs");
    let session = SessionCookies {
        schema: 1,
        session_id: "selected-slack".into(),
        service: Service::Slack,
        identity_public_key: identity,
        setup_binding: "prepared-operation".into(),
        source_snapshot_digest: [9; 32],
        cookies: vec![Cookie {
            host: "app.slack.com".into(),
            domain: "slack.com".into(),
            host_only: false,
            name: "d".into(),
            value: MARKER.into(),
            path: "/".into(),
            expires_unix_seconds: None,
            secure: true,
            http_only: true,
            same_site: SameSite::None,
        }],
        origin_storage: Vec::new(),
    };
    let id = AccountId::from(session.session_id.clone());
    let mut vault = SecretVault::open_person(Some(root));
    vault.put(
        &id,
        &account_credentials::service_sessions::service_session_secret(&session)
            .expect("the selected session is sealable"),
    );
    vault.try_save().expect("the person's vault");
    session
}

/// Browser's own calls on a connection admitted as Browser: the person's selection listed,
/// resolved, and then forgotten.
fn browser_app(
    socket: &Path,
    device: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
    seen: &std::sync::mpsc::Sender<Vec<Result<Reply, String>>>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket, BROWSER);
        let handle = client
            .join_channel(&channel(BROWSER), &presentation(device, cert))
            .await
            .expect("the app the session launched joins its workspace's channel");
        let mut replies = Vec::new();
        for command in [
            Command::List {},
            Command::Resolve {
                id: "selected-slack".into(),
                setup_binding: "prepared-operation".into(),
            },
            Command::Forget {
                id: "selected-slack".into(),
                setup_binding: "prepared-operation".into(),
            },
            Command::List {},
        ] {
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

#[test]
fn browser_lists_resolves_and_forgets_the_selection_where_the_keyring_holds_the_vault() {
    browser_lists_resolves_and_forgets(Keyring::Answers);
}

/// The same on a host with no keyring: the person's vault is just as much theirs, sealed under the
/// owner-only key beside it.
#[test]
fn browser_lists_resolves_and_forgets_the_selection_where_a_key_file_holds_the_vault() {
    browser_lists_resolves_and_forgets(Keyring::Missing);
}

/// Which ownership the person's vault reports on a host of this kind.
fn backend_on(keyring: Keyring) -> accounts::VaultBackend {
    match keyring {
        Keyring::Answers => accounts::VaultBackend::System,
        Keyring::Missing => accounts::VaultBackend::LocalFile,
    }
}

fn browser_lists_resolves_and_forgets(keyring: Keyring) {
    let fixture = fixture(keyring);
    let root = config_root();
    assert_eq!(
        SecretVault::open_person(Some(&root)).backend(),
        backend_on(keyring),
        "the person's vault is keyed by this host's one ownership"
    );
    let selected = select_in(&root, fixture.keys.root);
    assert_eq!(
        accounts::vault_dir_in(&root).join("vault.key").exists(),
        keyring == Keyring::Missing,
        "the key lives where this host keeps it, never in two places"
    );
    let (_root, socket) = private_socket();
    let mut session = session(&socket, &fixture, BROWSER);

    let (seen, received) = std::sync::mpsc::channel();
    let Keys { device, cert, .. } = fixture.keys;
    let app = std::thread::spawn(move || browser_app(&socket, &device, &cert, &seen));
    let replies = until_the_app_is_done(&mut session, &received);
    app.join().expect("the app's thread");

    assert!(
        matches!(
            &replies[0],
            Ok(Reply::List { sessions })
                if sessions.len() == 1
                    && sessions[0].id == "selected-slack"
                    && sessions[0].service == Service::Slack
                    && sessions[0].setup_binding == "prepared-operation"
        ),
        "the person's selected website session is listed: {:?}",
        replies[0]
    );
    let Ok(Reply::Resolved { session: resolved }) = &replies[1] else {
        panic!(
            "and resolved to the cookies ownership keeps for it: {:?}",
            replies[1]
        );
    };
    assert_eq!(
        resolved, &selected,
        "byte for byte the selection the person made"
    );
    assert!(
        matches!(
            &replies[2],
            Ok(Reply::Forgotten { id, setup_binding })
                if id == "selected-slack" && setup_binding == "prepared-operation"
        ),
        "the workspace's administrator forgets it: {:?}",
        replies[2]
    );
    assert!(
        matches!(&replies[3], Ok(Reply::List { sessions }) if sessions.is_empty()),
        "and the next list is empty: {:?}",
        replies[3]
    );
}

/// The same calls from another of the person's devices: the selection is the person's, so their
/// phone reads it and forgets it, and this device's next list no longer holds it.
#[test]
fn the_persons_other_device_reads_and_forgets_the_selection() {
    let fixture = fixture(Keyring::Answers);
    select_in(&config_root(), fixture.keys.root);
    let (_root, socket) = private_socket();
    let mut session = session(&socket, &fixture, BROWSER);

    let (seen, received) = std::sync::mpsc::channel();
    let Keys {
        phone, phone_cert, ..
    } = fixture.keys;
    let app = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime for the app");
        runtime.block_on(async {
            let client = connect(&socket, BROWSER);
            let handle = client
                .join_channel(&channel(BROWSER), &presentation(&phone, &phone_cert))
                .await
                .expect("the person's other device joins its workspace's channel");
            // The session's copy of the workspace learns of the phone from its own service once it
            // runs, so the phone asks again until its list shows the selection.
            let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
            let listed = loop {
                let listed = handle
                    .browser_service_session_command(&Command::List {})
                    .await
                    .map_err(|error| error.to_string());
                let shown = matches!(&listed, Ok(Reply::List { sessions }) if !sessions.is_empty());
                if shown || std::time::Instant::now() >= deadline {
                    break listed;
                }
            };
            let mut replies = vec![listed];
            for command in [
                Command::Forget {
                    id: "selected-slack".into(),
                    setup_binding: "prepared-operation".into(),
                },
                Command::List {},
            ] {
                replies.push(
                    handle
                        .browser_service_session_command(&command)
                        .await
                        .map_err(|error| error.to_string()),
                );
            }
            let _ = seen.send(replies);
        });
    });
    let replies = until_the_app_is_done(&mut session, &received);
    app.join().expect("the app's thread");

    assert!(
        matches!(&replies[0], Ok(Reply::List { sessions }) if sessions.len() == 1),
        "the person's other device reads the selection this one keeps: {:?}",
        replies[0]
    );
    assert!(
        matches!(&replies[1], Ok(Reply::Forgotten { id, .. }) if id == "selected-slack"),
        "and forgets it: {:?}",
        replies[1]
    );
    assert!(
        matches!(&replies[2], Ok(Reply::List { sessions }) if sessions.is_empty()),
        "the person's ownership is theirs, whichever of their devices asks: {:?}",
        replies[2]
    );
}

/// `app`'s own calls on a connection admitted as `app`.
fn app_asks(
    socket: &Path,
    app: &str,
    commands: Vec<Command>,
    keys: &Keys,
    seen: &std::sync::mpsc::Sender<Vec<Result<Reply, String>>>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket, app);
        let handle = client
            .join_channel(&channel(app), &presentation(&keys.device, &keys.cert))
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
    let keys = fixture.keys.clone();
    let asking = std::thread::spawn(move || app_asks(&socket, app, commands, &keys, &seen));
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
    let replies = asked_of_the_session(&fixture(Keyring::Answers), BROWSER, item_asks(fill));
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
    let replies = asked_of_the_session(&fixture(Keyring::Answers), MAIL, item_asks(fill));
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
