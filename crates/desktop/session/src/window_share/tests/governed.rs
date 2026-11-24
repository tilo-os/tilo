//! A window's share is the workspace's own: the session that shares it asks its device's daemon
//! to write the share into the workspace's governance, which the daemon holds and carries to the
//! workspace's other devices with the rest of what it writes there; the session's shell reads the
//! share back from that governance.

use std::path::Path;

use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;

use super::*;
use crate::daemon_fixture::TestDaemon;

/// The person both devices are certified by.
const PERSON: [u8; 32] = [0x41; 32];

/// A device of the person's, kept under `root` as a computer keeps its own: the key a session
/// started there signs with and the certificate the person's root gave it.
fn provisioned(root: &Path, device: &SigningKey, label: &str) -> DeviceCert {
    let paths = tilo_device_identity::IdentityPaths::under(root);
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("the device's key directory");
    tilo_device_identity::persist::write_device_key(&paths.device_privkey, device)
        .expect("the device's key");
    let person = SigningKey::from_bytes(&PERSON);
    let mut cert = DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("the device's certificate");
    cert
}

/// The session of the device whose key is `key`, keeping its shell under `state` and its
/// workspaces at `daemon`: the device the identity this process reads names, on a live session
/// the test carries.
fn session_of(key: &SigningKey, label: &str, state: &Path, daemon: &TestDaemon) -> Member {
    let (sync, said) = Client::without_network(
        iroh::SecretKey::from_bytes(&key.to_bytes()).public(),
        OwnPresence {
            label: label.to_owned(),
            ..OwnPresence::default()
        },
        Vec::new(),
    );
    let sync = Rc::new(sync);
    let (content, media, content_socket) = device_content();
    let session = Session::headless(
        Options {
            person: hex::encode(daemon.certificate().user_public_key),
            presentation: ui::headless::fixture_runner_inputs,
            state: Some(state.to_path_buf()),
            sync: Some(Rc::clone(&sync)),
            content_socket: Some(content_socket),
            shell_link: Some(daemon.shell_link()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("a session");
    Member {
        session,
        sync,
        _said: said,
        _content: content,
        _media: media,
    }
}

/// Step `member` 's session, and the client whose window it shows, until `done` holds.
#[track_caller]
fn alone(
    member: &mut Member,
    client: &mut Option<TestClient>,
    what: &str,
    mut done: impl FnMut(&Member) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        if let Some(client) = client.as_mut() {
            client.exchange();
        }
        member.session.step(Duration::from_millis(2)).expect("step");
        if done(member) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: not reached in time: {}",
            shares(&member.session)
        );
    }
}

/// How many grants of the share `share` the governance the daemon holds of `workspace` names.
fn governed(daemon: &TestDaemon, workspace: WorkspaceId, share: &str) -> usize {
    let workspaces = daemon.workspaces().clone();
    tilo_rt::app()
        .block_on(async move { workspaces.governed_copy(workspace).await })
        .expect("the runtime runs")
        .expect("the daemon holds the workspace")
        .1
        .governance()
        .governed_policy()
        .expect("the workspace's governance")
        .grants
        .iter()
        .filter(|grant| is_of_share(grant, share))
        .count()
}

/// How many grants of the share `share` the shell's copy of the workspace `workspace` names.
fn on_shell(session: &Session, workspace: WorkspaceId, share: &str) -> usize {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .filter(|held| held.id() == workspace)
            .flat_map(|held| &held.policy.grants)
            .filter(|grant| is_of_share(grant, share))
            .count()
    })
}

/// A person's device shares a window with the workspace, naming only its tab, as the window's
/// menu and the shell's actions do. The share is in the workspace's governance the device's daemon
/// holds, not only on the session's shell, so it leaves for the workspace's other devices with
/// what the daemon writes there; the shell reads it back from that governance.
#[test]
fn a_window_a_device_shares_is_in_the_member_list_its_daemon_holds() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut env = tilo_test_env::ScopedEnv::new();
    let key = SigningKey::from_bytes(&[0x43; 32]);
    let root = tempfile::tempdir().expect("the device's identity");
    let device = provisioned(root.path(), &key, "Desktop branch 4c1f");
    let state = tempfile::tempdir().expect("the device's state");
    env.set("TILO_IDENTITY_ROOT", root.path());

    let daemon = TestDaemon::start();
    let workspace_id = WorkspaceId::new();
    daemon.found(workspace_id, "Shell");
    let mut sharer = session_of(&key, "Desktop branch 4c1f", state.path(), &daemon);
    let mut shell = Workspace::new(workspace_id, "Shell");
    shell
        .policy
        .set_owner(hex::encode(daemon.certificate().user_public_key));
    sharer.session.handle().replace_tree(Layout {
        active: workspace_id,
        workspaces: vec![shell],
        ..Layout::default()
    });
    for _ in 0..4 {
        sharer
            .session
            .step(Duration::from_millis(1))
            .expect("a frame");
    }
    let mut window = Some(mapped(&mut sharer.session, APP));
    let tab = tab_of(&sharer.session, APP);
    assert_eq!(
        tilo_desktop_model::peer_salt_of(tab),
        tilo_desktop_model::ids::device_salt_of_key(&device.device_public_key),
        "the window is the device's own"
    );
    let share = window_share_id(tab);
    sharer
        .session
        .invoke(
            SHARES_RESOURCE,
            "share",
            &arguments(serde_json::json!({ "tab": tab.to_string() })),
        )
        .expect("the window is shared with the workspace");
    alone(&mut sharer, &mut window, "the share starts", |sharer| {
        !rows(&shares(&sharer.session), "sharing").is_empty()
    });
    // The daemon signs the share off the session's loop: it lands on a pass that follows.
    alone(
        &mut sharer,
        &mut window,
        "the share lands in the workspace's governance and on the sharer's shell",
        |sharer| {
            governed(&daemon, workspace_id, &share) == 1
                && on_shell(&sharer.session, workspace_id, &share) == 1
        },
    );
    assert_eq!(
        governed(&daemon, workspace_id, &share),
        1,
        "the share is in the workspace's governance the daemon holds, one grant to the viewers \
         that reaches every role above them: a share only its own shell names reaches no other \
         device"
    );
    assert_eq!(
        on_shell(&sharer.session, workspace_id, &share),
        1,
        "and the session's shell reads it from there"
    );
}
