//! The person's account custody, as the session serves it to the apps that ask: Browser lists,
//! resolves and forgets the website sessions the person selected, Settings seals an account's
//! secret, and Mail is released the one it keeps. Every call goes through the action door the
//! shipped app already speaks, on a connection admitted as that app in its workspace, and nothing
//! of it enters a workspace's records: the vault stays on the device.

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

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use accounts::{AccountId, AccountStore, SecretVault};
use browser_continuity::sessions::{Cookie, SameSite, Service, SessionCookies};
use ed25519_dalek::Signer as _;
use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply, CollabClient,
};
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_identity_host::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::{self, testing};
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, Launches, Lists, Options, SearchOptions, Session};
use tilo_user_identity::{DeviceCert, IdentityPaths};

// A host admits its members' records through an Automerge worker: this binary is that worker, as it
// is the session's own.
tilo_automerge_isolate::test_worker!();

/// The app whose website sessions these are.
const BROWSER: &str = "so.tilo.browser";
/// The keeper of the person's accounts.
const SETTINGS: &str = "so.tilo.settings";
/// The app that keeps email accounts.
const MAIL: &str = "so.tilo.mail";
/// Browser's record in the workspace: the route its app connection joins.
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000bf";
/// The workspace the session holds, as the desk names it.
const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";
/// The secret the person's email account is sealed with.
const PASSWORD: &str = "an app password only custody is released";
/// What the selected website session's cookie holds.
const MARKER: &str = "private-session-marker";
/// The person's email account, as their accounts record keys it.
const ACCOUNT: &str = "me@example.test";

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

/// The person's account vault under this host's one custody: the keyring where it answers,
/// otherwise an owner-only key file beside the ciphertext.
fn person_vault(root: &Path) -> accounts::AccountCredentials {
    accounts::AccountCredentials::open_in(root)
        .expect("this host can keep the person's account vault")
}

/// A login on a host whose keyring answers, or on one that has none and
/// keeps the vault key in an owner-only file beside the ciphertext.
///
/// A keyring that answers is the file-backed secret store
/// [`tilo_secrets::SECRETS_DIR_ENV`] names standing in for the Secret
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
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let device = tilo_user_identity::persist::load_device_key(&paths).expect("this device's key");
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    // The person's other device, enrolled as a pairing enrolls one.
    let now = tilo_clock::unix_now_ns();
    let fleet =
        tilo_identity_host::device_fleet::DeviceFleetStore::from_env().expect("the device fleet");
    let profile = tilo_identity_host::registry::identity_for_workspace(WORKSPACE)
        .expect("the identity registry")
        .expect("the workspace is answered by this device's person");
    let token = fleet
        .mint_enrollment(
            &profile.id,
            tilo_user_identity::DeviceCapabilities::member(),
            now,
            now.saturating_add(3_600_000_000_000),
        )
        .expect("this device may enroll another of the person's");
    let phone = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
    let phone_cert = fleet
        .complete_enrollment(
            &profile.id,
            &token,
            tilo_identity_host::enrollment::EnrollmentRole::PersonalDevice,
            phone.verifying_key().to_bytes(),
            "The phone",
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
    hold_workspace(&state, &keys);
    Fixture {
        keys,
        state,
        _env: env,
        _root: root,
        _home: home,
    }
}

/// The workspace's host admits `certificate` 's device as an editor, redeeming the invitation it
/// issues for it: what pairing another of the person's devices does.
fn admit(
    governor: &mut testing::governor::TestGovernor,
    key: &ed25519_dalek::SigningKey,
    certificate: &DeviceCert,
) -> Result<(), String> {
    use tilo_peer_state::join;
    let now = std::time::SystemTime::now();
    let ns = testing::ns(now)?;
    let hint = governor.hint(iroh::EndpointAddr::new(
        iroh::EndpointId::from_bytes(&governor.workspace_host().device_public_key)
            .map_err(|error| error.to_string())?,
    ));
    let invitation = governor.issue(
        vec![hint],
        join::MemberRole::Editor,
        Some(certificate.user_public_key),
        Some(certificate.device_label.clone()),
        ns.saturating_add(60_000_000_000),
    );
    let mut request_id = [0_u8; 32];
    request_id[..8].copy_from_slice(&certificate.device_public_key[..8]);
    let answered = governor.handle(
        certificate.device_public_key,
        join::JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: certificate.clone(),
            request_id,
        }),
    );
    let join::JoinTransportResponse::Challenge(challenge) = answered else {
        return Err(format!("the host refuses the invitation: {answered:?}"));
    };
    let answered = governor.handle(
        certificate.device_public_key,
        join::JoinTransportRequest::Complete(join::JoinCompleteRequest {
            proof: join::sign_join_proof(challenge.challenge, key)?,
        }),
    );
    if !matches!(answered, join::JoinTransportResponse::Complete(_)) {
        return Err(format!("the host refuses the proof: {answered:?}"));
    }
    Ok(())
}

/// The workspace's owner, as this device holds it in `state` , with the person's other device
/// admitted: what the session opens when it starts.
fn hold_workspace(state: &Path, keys: &Keys) {
    let record = state.join("tilo").join("rooms.json");
    std::fs::create_dir_all(record.parent().expect("the state's workspace directory"))
        .expect("this device's state directory");
    let owner = owner::open_file(&owner::owner_file_of(&record))
        .expect("an owner for this device's workspace");
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let mut governor =
        testing::enroll_interactive_governor_in(&owner, &keys.device, &keys.cert, WORKSPACE)
            .expect("this device governs the workspace it hosts");
    admit(&mut governor, &keys.phone, &keys.phone_cert).expect("the person's other device joins");
    // The governor's checkpoint now names both of them: the host admits it as the publication a
    // host's own workspace makes.
    let host = governor.workspace_host().device_public_key;
    let frames: Vec<Vec<u8>> = governor
        .outgoing()
        .map(|(bytes, _)| bytes.to_vec())
        .collect();
    for frame in frames {
        owner
            .receive(
                workspace,
                TransportOrigin::RetainedRelayDelta(host),
                postcard::from_bytes(&frame).expect("a publication is a message"),
                None,
                std::time::SystemTime::now(),
            )
            .expect("the host admits the workspace's people");
    }
}

/// The person `root` certifies, as a workspace names its owner.
fn principal(root: &[u8; 32]) -> String {
    match collab_types::caps::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(*root),
    ) {
        collab_types::caps::Principal::Person(id) => id,
        other => panic!("{other:?} is not a person"),
    }
}

/// A desk showing the workspace, known by its id, owned by the person.
fn desk(keys: &Keys) -> DesktopTree {
    let workspace = WorkspaceId::parse(WORKSPACE).expect("a workspace id");
    let slot = WorkspaceSlot::of_workspace(workspace);
    let mut workspace = Workspace::in_workspace(slot, workspace, "Desk");
    workspace.set_owner(principal(&keys.root));
    DesktopTree {
        workspaces: vec![workspace.clone()],
        active: workspace.slot,
        ..DesktopTree::default()
    }
}

/// What an app presents to join the workspace's channel: the certificate, and the device's proof
/// that it is the certificate's.
fn presentation(device: &ed25519_dalek::SigningKey, cert: &DeviceCert) -> Vec<u8> {
    runtime_collab::CertifiedJoinIdentity {
        workspace_id: WORKSPACE.to_owned(),
        device_certificate: serde_json::to_vec(cert).expect("a certificate is JSON"),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(
                WORKSPACE,
            ))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("a presentation encodes")
}

/// A directory for the plane's socket that only this user reaches: the broker refuses to serve one
/// anybody else could open.
fn private_socket() -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the plane in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the plane's directory is this user's");
    let socket = root.path().join("collab.sock");
    (root, socket)
}

/// The session serving `app` on `socket` , holding the fixture's workspace from its state and
/// showing its desk.
fn session(socket: &Path, fixture: &Fixture, app: &str) -> Session {
    // The app speaks from this test's own process, so the launch the session would have recorded
    // for it names that process.
    let launches = Arc::new(Launches::default());
    launches.started(std::process::id(), app, WORKSPACE, true);
    let mut session = crate::common::headless(Options {
        collab: false,
        wake: LoopWake::default(),
        action_plane: ActionPlane::At(socket.to_path_buf()),
        person: "Ada".to_owned(),
        launches,
        state: Some(fixture.state.clone()),
        search: SearchOptions {
            apps: Some(std::sync::Arc::new(Lists::default())),
            ..SearchOptions::default()
        },
        ..Options::default()
    });
    session.handle().replace_tree(desk(&fixture.keys));
    for _ in 0..4 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    session
}

/// The channel key of `app` 's record in the workspace.
fn channel(app: &str) -> String {
    tilo_action_broker::route::CrdtRoute::record(app, WORKSPACE, RECORD).channel_key()
}

/// A connection to the session's plane as `app` in the workspace, as the launcher hands an app one.
fn connect(socket: &Path, app: &str) -> CollabClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("the plane's socket");
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
    common::wait_for(session, "the app to finish its calls", |session| {
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

/// A person's selected website session, sealed in `root` as selection leaves it: its account entry,
/// and its cookies in custody under the person's key.
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
    let mut account = accounts::Account::new(
        id.clone(),
        account_custody::service_sessions::SERVICE_SESSION_TYPE,
    );
    account.config = serde_json::json!({"service": "slack", "setup_binding": "prepared-operation"});
    let mut store = AccountStore::load_in(root);
    store.upsert(account);
    store.save_in(root).expect("the person's accounts");
    let mut vault = SecretVault::open_person(Some(root));
    vault.put(
        &id,
        &account_custody::service_sessions::service_session_secret(&session)
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

/// Which custody the person's vault reports on a host of this kind.
fn custody_on(keyring: Keyring) -> accounts::SecretCustody {
    match keyring {
        Keyring::Answers => accounts::SecretCustody::System,
        Keyring::Missing => accounts::SecretCustody::LocalFile,
    }
}

fn browser_lists_resolves_and_forgets(keyring: Keyring) {
    let fixture = fixture(keyring);
    let root = config_root();
    assert_eq!(
        SecretVault::open_person(Some(&root)).custody(),
        custody_on(keyring),
        "the person's vault is keyed by this host's one custody"
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
            "and resolved to the cookies custody keeps for it: {:?}",
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
            let mut replies = Vec::new();
            for command in [
                Command::List {},
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
        "the person's custody is theirs, whichever of their devices asks: {:?}",
        replies[2]
    );
}

/// Settings' own call on a connection admitted as Settings: seal the person's account secret.
fn settings_app(
    socket: &Path,
    device: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
    seen: &std::sync::mpsc::Sender<Result<Reply, String>>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket, SETTINGS);
        let handle = client
            .join_channel(&channel(SETTINGS), &presentation(device, cert))
            .await
            .expect("the app the session launched joins its workspace's channel");
        let sealed = handle
            .browser_service_session_command(&Command::StoreAccountSecret {
                account: ACCOUNT.to_owned(),
                password: zeroize::Zeroizing::new(PASSWORD.to_owned()),
            })
            .await
            .map_err(|error| error.to_string());
        let _ = seen.send(sealed);
    });
}

#[test]
fn settings_seals_an_account_secret_in_the_persons_vault() {
    let fixture = fixture(Keyring::Missing);
    let root = config_root();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(&root).expect("the person's accounts directory");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("the person's accounts directory is theirs");
        let mut store = AccountStore::load_in(&root);
        store.upsert(accounts::Account::new(
            AccountId::from(ACCOUNT.to_owned()),
            accounts::email::TYPE_ID,
        ));
        store.save_in(&root).expect("the person's accounts");
        // The person's vault is theirs before the session seals anything in it: what a device that
        // signed in once holds.
        let _ = person_vault(&root);
    }
    assert_eq!(
        SecretVault::open_person(Some(&root)).custody(),
        custody_on(Keyring::Missing),
        "with no keyring, an owner-only key beside the vault keeps it"
    );
    let (_root, socket) = private_socket();
    let mut session = session(&socket, &fixture, SETTINGS);

    let (seen, received) = std::sync::mpsc::channel();
    let Keys { device, cert, .. } = fixture.keys;
    let app = std::thread::spawn(move || settings_app(&socket, &device, &cert, &seen));
    let sealed = until_the_app_is_done(&mut session, &received);
    app.join().expect("the app's thread");
    assert!(
        matches!(&sealed, Ok(Reply::AccountSecretStored { account }) if account == ACCOUNT),
        "Settings seals the account's secret: {sealed:?}"
    );

    // The secret is the person's: the app that keeps email accounts is released it, and no other
    // app is.
    let store = AccountStore::load_in(&root);
    let credentials = person_vault(&root);
    assert_eq!(
        accounts::custody::release(&store, &credentials, MAIL, ACCOUNT)
            .expect("Mail keeps email accounts")
            .as_str(),
        PASSWORD,
        "the sealed secret is the password Settings gave"
    );
    for app in [BROWSER, "so.tilo.notes", "so.tilo.calendar"] {
        assert!(
            accounts::custody::release(&store, &credentials, app, ACCOUNT).is_err(),
            "{app} is not released the account's secret"
        );
    }
    assert!(
        accounts::custody::release(&store, &credentials, SETTINGS, ACCOUNT).is_err(),
        "Settings is the keeper of accounts, and never reads a secret back"
    );
}

/// Mail's own call on a connection admitted as Mail: the account secret it keeps.
fn mail_app(
    socket: &Path,
    device: &ed25519_dalek::SigningKey,
    cert: &DeviceCert,
    seen: &std::sync::mpsc::Sender<Result<Reply, String>>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime for the app");
    runtime.block_on(async {
        let client = connect(socket, MAIL);
        let handle = client
            .join_channel(&channel(MAIL), &presentation(device, cert))
            .await
            .expect("the app the session launched joins its workspace's channel");
        let released = handle
            .browser_service_session_command(&Command::ReleaseAccountSecret {
                account: ACCOUNT.to_owned(),
            })
            .await
            .map_err(|error| error.to_string());
        let _ = seen.send(released);
    });
}

#[test]
fn mail_is_released_the_account_secret_it_keeps() {
    let fixture = fixture(Keyring::Answers);
    let root = config_root();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(&root).expect("the person's accounts directory");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("the person's accounts directory is theirs");
        let mut store = AccountStore::load_in(&root);
        store.upsert(accounts::Account::new(
            AccountId::from(ACCOUNT.to_owned()),
            accounts::email::TYPE_ID,
        ));
        store.save_in(&root).expect("the person's accounts");
        let mut credentials = person_vault(&root);
        accounts::custody::seal(&mut credentials, SETTINGS, ACCOUNT, PASSWORD)
            .expect("Settings seals the account's secret");
    }
    assert_eq!(
        SecretVault::open_person(Some(&root)).custody(),
        custody_on(Keyring::Answers),
        "the keyring keeps the person's vault key, not a file beside it"
    );
    let (_root, socket) = private_socket();
    let mut session = session(&socket, &fixture, MAIL);

    let (seen, received) = std::sync::mpsc::channel();
    let Keys { device, cert, .. } = fixture.keys;
    let app = std::thread::spawn(move || mail_app(&socket, &device, &cert, &seen));
    let released = until_the_app_is_done(&mut session, &received);
    app.join().expect("the app's thread");
    let Ok(Reply::AccountSecret { account, password }) = &released else {
        panic!("Mail is released the account's secret: {released:?}");
    };
    assert_eq!(account, ACCOUNT);
    assert_eq!(password.as_str(), PASSWORD);
}
