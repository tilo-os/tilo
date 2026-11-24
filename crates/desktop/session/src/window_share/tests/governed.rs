//! A window's share is the workspace's own: the session that shares it asks its device's daemon
//! to write the share into the workspace's governance, which the daemon holds and carries to the
//! workspace's other devices with the rest of what it writes there; the session's shell reads the
//! share back from that governance.

use std::path::Path;

use ed25519_dalek::SigningKey;

use super::*;
use crate::daemon_fixture::TestDaemon;

/// The device `daemon` serves, kept under `root` as a computer keeps its own: the key a session
/// started there signs with and the certificate its person's root gave it.
fn provisioned(root: &Path, daemon: &TestDaemon) {
    let paths = tilo_device_identity::IdentityPaths::under(root);
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("the device's key directory");
    tilo_device_identity::persist::write_device_key(&paths.device_privkey, &daemon.key())
        .expect("the device's key");
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &daemon.certificate())
        .expect("the device's certificate");
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
    daemon
        .copy(workspace)
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

/// `tree` listing the window `tab` of another device in the workspace `workspace`, as the
/// workspace's shared shell lists a window another device opened, which this one opens its own
/// view of, before anything of it is shared.
fn listing(mut tree: Layout, workspace: WorkspaceId, tab: u64) -> Layout {
    if !tree.workspaces.iter().any(|held| held.id() == workspace) {
        tree.workspaces.push(Workspace::new(workspace, "Shell"));
    }
    let held = tree
        .workspaces
        .iter_mut()
        .find(|held| held.id() == workspace)
        .expect("the workspace is on the shell");
    let mut strip = Strip::new(StripId::from(tab.wrapping_add(2)), "Shared");
    strip.window_groups.push(WindowGroup {
        id: GroupId::from(tab.wrapping_add(1)),
        tabs: vec![Tab {
            id: TabId::from(tab),
            title: "Notes".to_owned(),
            tile: TileContent::LocalApp {
                app_id: APP.to_owned(),
                label: "Notes".to_owned(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: TabId::from(tab),
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    held.strips.push(strip);
    tree.active = held.id();
    tree
}

/// A person's second device shares a window with the workspace their first device founded,
/// naming only its tab, as the window's menu and the shell's actions do. The share is in the
/// workspace's governance the second device's daemon holds, not only on its shell; it leaves for
/// the first device with what that daemon writes there; and the first device's shell then names
/// the share and asks the second device for the window, in a tab of its own beside the one that
/// stood for it — with nobody naming the window there.
#[test]
fn a_window_a_second_device_shares_reaches_the_first_through_the_workspaces_member_list() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut env = tilo_test_env::ScopedEnv::new();
    let workspace_id = WorkspaceId::new();
    let (first, second) = TestDaemon::two_devices(workspace_id, "Shell");
    let (first_root, second_root) = (
        tempfile::tempdir().expect("the first device's identity"),
        tempfile::tempdir().expect("the second device's identity"),
    );
    provisioned(first_root.path(), &first);
    provisioned(second_root.path(), &second);
    let (first_state, second_state) = (
        tempfile::tempdir().expect("the first device's state"),
        tempfile::tempdir().expect("the second device's state"),
    );
    let mut shell = Workspace::new(workspace_id, "Shell");
    shell
        .policy
        .set_owner(hex::encode(first.certificate().user_public_key));
    let tree = Layout {
        active: workspace_id,
        workspaces: vec![shell],
        ..Layout::default()
    };

    // The second device maps the window it will share.
    env.set("TILO_IDENTITY_ROOT", second_root.path());
    let mut sharer = session_of(
        &second.key(),
        "Desktop branch 4c1f",
        second_state.path(),
        &second,
    );
    sharer.session.handle().replace_tree(tree.clone());
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
        tilo_desktop_model::ids::device_salt_of_key(&second.certificate().device_public_key),
        "the window is the second device's own"
    );
    let share = window_share_id(tab);

    // The first device's own session, on the shell that lists the window as open on another
    // device, before anything of the share reached it.
    env.set("TILO_IDENTITY_ROOT", first_root.path());
    let mut viewer = session_of(&first.key(), "Desktop", first_state.path(), &first);
    viewer
        .session
        .handle()
        .replace_tree(listing(tree, workspace_id, tab));
    for _ in 0..8 {
        viewer
            .session
            .step(Duration::from_millis(2))
            .expect("a frame");
    }
    assert_eq!(
        governed(&first, workspace_id, &share),
        0,
        "the first device knows nothing of the share before it is made"
    );
    assert!(
        hosted(&viewer.session, tab).is_none(),
        "and lists the window as one it opens its own view of"
    );

    // The second device's person shares the window it maps.
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
            governed(&second, workspace_id, &share) == 1
                && on_shell(&sharer.session, workspace_id, &share) == 1
        },
    );
    assert_eq!(
        governed(&second, workspace_id, &share),
        1,
        "the share is in the workspace's governance the second device's daemon holds, one grant \
         to the viewers that reaches every role above them: a share only its own shell names \
         reaches no other device"
    );
    assert_eq!(
        on_shell(&sharer.session, workspace_id, &share),
        1,
        "and its own shell reads it from there"
    );

    // The share is the workspace's, so nobody asks for it on the first device: its daemon admits
    // what the second device's daemon wrote, its shell names the share, and it asks the device
    // the governance says hosts the window for it.
    alone(
        &mut viewer,
        &mut None,
        "the first device's daemon holds the share and its shell names it",
        |viewer| {
            governed(&first, workspace_id, &share) == 1
                && on_shell(&viewer.session, workspace_id, &share) == 1
        },
    );
    let host = hex::encode(second.certificate().device_public_key);
    let named = tab.to_string();
    let asked = |viewer: &Member| {
        rows(&shares(&viewer.session), "watching")
            .iter()
            .any(|watched| watched["tab"] == named.as_str() && watched["host"] == host.as_str())
    };
    alone(
        &mut viewer,
        &mut None,
        "the first device asks the second for the window it shares",
        asked,
    );
    // The tab that listed the window is this session's own view of it and stays as it is. The
    // window the second device shares stands beside it, in a tab of its own that names the device
    // it runs on.
    let stands = viewer.session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(Workspace::tabs)
            .filter(|shown| shown.id == tab)
            .count()
    });
    assert_eq!(stands, 1, "one tab stands under the window's own id");
    assert!(
        hosted(&viewer.session, tab).is_none(),
        "and it is still this shell's own view of the window"
    );
    let place: u64 = rows(&shares(&viewer.session), "watching")
        .iter()
        .find(|watched| watched["tab"] == named.as_str())
        .and_then(|watched| watched["shown_in"].as_str())
        .and_then(|place| place.parse().ok())
        .expect("the tab the shared window is pictured in");
    assert_ne!(place, tab, "the shared window stands in a tab of its own");
    assert_eq!(
        hosted(&viewer.session, place).as_deref(),
        Some(host.as_str()),
        "which shows it as the window running on the second device"
    );
    drop(window);
}
