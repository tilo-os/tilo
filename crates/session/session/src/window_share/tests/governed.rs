//! A window's share is the workspace's own: the device that shares it writes the share into the
//! governance its engine holds, so it leaves for the workspace's host with the rest of what that
//! device writes there, and every other device of the workspace reads it from the governance it
//! admitted. Two devices of one person, the first hosting the workspace and the second admitted
//! to it as that person's further device, as a machine copied from the first is.

use std::path::Path;

use ed25519_dalek::SigningKey;
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::testing::{self, governor::TestGovernor};
use tilo_session_client::engine::Engine;
use tilo_user_identity::DeviceCert;

use super::*;

/// The person both devices are certified by.
const PERSON: [u8; 32] = [0x41; 32];

/// A device of the person's, kept under `root` as a computer keeps its own: the key a session
/// started there signs with and the certificate the person's root gave it.
fn provisioned(root: &Path, device: &SigningKey, label: &str) -> DeviceCert {
    let paths = tilo_user_identity::IdentityPaths::under(root);
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("the device's key directory");
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, device)
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
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("the device's certificate");
    cert
}

/// The engine of the device certified as `cert`, keeping its workspaces under `state`, with the
/// workspace its desk already lists.
fn engine_of(
    cert: &DeviceCert,
    key: &SigningKey,
    state: &Path,
    listed: Option<WorkspaceId>,
) -> Engine {
    Engine::open(
        tilo_network_actor::HostIdentity::kept(cert.clone(), key.clone()),
        Some(state),
        listed.map(|id| (WorkspaceSlot::of_workspace(id), id)),
        crate::platform::custody::of_this_desk(),
    )
    .expect("the device's engine opens")
}

/// The workspace the desk kept under `state` opens when it has none: the one the session's engine
/// and the person's other device act on.
fn workspace_on_the_desk(state: &Path) -> WorkspaceId {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("a session");
    session.step(Duration::from_millis(1)).expect("a frame");
    session
        .handle()
        .read(|tree| tree.workspaces.first().map(Workspace::id))
        .expect("a workspace on the desk")
}

/// The session of the device whose key is `key`, keeping its desk and its workspaces under
/// `state`: the device the identity this process reads names, on a plane the test carries.
fn session_of(key: &SigningKey, label: &str, state: &Path) -> Member {
    let (sync, said) = Client::planeless(
        iroh::SecretKey::from_bytes(&key.to_bytes()).public(),
        OwnPresence {
            label: label.to_owned(),
            ..OwnPresence::default()
        },
        Vec::new(),
    );
    let sync = Rc::new(sync);
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            state: Some(state.to_path_buf()),
            sync: Some(Rc::clone(&sync)),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("a session");
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    Member {
        session,
        sync,
        said,
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

/// How many grants of the share `share` the governance `engine` holds of `workspace` names.
fn governed(engine: &Engine, workspace: &str, share: &str) -> usize {
    engine
        .policy(workspace)
        .expect("the workspace's governance")
        .grants
        .iter()
        .filter(|grant| is_of_share(grant, share))
        .count()
}

/// How many grants of the share `share` the desk's copy of the workspace `workspace` names.
fn on_desk(session: &Session, workspace: WorkspaceId, share: &str) -> usize {
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
/// workspace's shared desk lists a window another device opened, which this one opens its own
/// view of, before anything of it is shared.
fn listing(mut tree: DesktopTree, workspace: WorkspaceId, tab: u64) -> DesktopTree {
    if !tree.workspaces.iter().any(|held| held.id() == workspace) {
        tree.workspaces.push(Workspace::in_workspace(
            WorkspaceSlot::of_workspace(workspace),
            workspace,
            "Desk",
        ));
    }
    let held = tree
        .workspaces
        .iter_mut()
        .find(|held| held.id() == workspace)
        .expect("the workspace is on the desk");
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
    tree.active = held.slot;
    tree
}

/// A person's second device shares a window with the workspace their first device hosts, naming
/// only its tab, as the window's menu and the desk's actions do. The share is in the workspace's
/// governance on the device that shares, not only on its desk; it waits there to leave for the
/// workspace's host; the host admits it as it admits what a link from that device brings; and the
/// first device's desk then names the share and asks the second device for the window, in the
/// tab that stood for it — with nobody naming the window there.
#[test]
fn a_window_a_second_device_shares_reaches_the_first_through_the_workspaces_governance() {
    tilo_window_share::flatpak_share::read_encoders();
    let mut env = tilo_test_env::ScopedEnv::new();
    let (first_key, second_key) = (
        SigningKey::from_bytes(&[0x42; 32]),
        SigningKey::from_bytes(&[0x43; 32]),
    );
    let (first_root, second_root) = (
        tempfile::tempdir().expect("the first device's identity"),
        tempfile::tempdir().expect("the second device's identity"),
    );
    let first = provisioned(first_root.path(), &first_key, "Desktop");
    let second = provisioned(second_root.path(), &second_key, "Desktop branch 4c1f");
    let (first_state, second_state) = (
        tempfile::tempdir().expect("the first device's state"),
        tempfile::tempdir().expect("the second device's state"),
    );

    // The second device's desk lists the workspace. The first device hosts it and governs it,
    // and admits the second as its person's further device: what either holds of it comes from
    // the first.
    env.set("TILO_IDENTITY_ROOT", second_root.path());
    let workspace_id = workspace_on_the_desk(second_state.path());
    let workspace = workspace_id.to_string();
    let first_engine = engine_of(&first, &first_key, first_state.path(), None);
    {
        let second_engine = engine_of(
            &second,
            &second_key,
            second_state.path(),
            Some(workspace_id),
        );
        second_engine
            .delete(workspace_id)
            .expect("the second device lets go of the workspace its desk founded");
        let governor =
            TestGovernor::empty_interactive(first.clone(), first_key.clone(), &workspace)
                .expect("the first device governs the workspace");
        let governor = testing::enroll_into(governor, first_engine.owner(), &first_key, &first)
            .expect("the workspace is the first device's");
        let governor = testing::enroll_into(governor, second_engine.owner(), &second_key, &second)
            .expect("the workspace admits the person's second device");
        let now = tilo_clock::wall();
        for (bytes, _) in governor.outgoing() {
            for engine in [&first_engine, &second_engine] {
                engine
                    .owner()
                    .receive(
                        workspace_id,
                        TransportOrigin::RetainedRelayDelta(first.device_public_key),
                        postcard::from_bytes(bytes).expect("a workspace change"),
                        now,
                    )
                    .expect("a device takes the workspace as its host has it");
            }
        }
    }

    // The second device's person shares the window it maps, naming its tab.
    let mut sharer = session_of(&second_key, "Desktop branch 4c1f", second_state.path());
    let mut window = Some(mapped(&mut sharer.session, APP));
    let tab = tab_of(&sharer.session, APP);
    assert_eq!(
        tilo_desktop_model::peer_salt_of(tab),
        tilo_desktop_model::ids::device_salt_of_key(&second.device_public_key),
        "the window is the second device's own"
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
    let second_engine = sharer
        .session
        .shared
        .engine
        .clone()
        .expect("the session holds its person's workspaces");
    // The device's signer signs the share off the loop: it lands on a pass that follows.
    alone(
        &mut sharer,
        &mut window,
        "the share lands in the workspace's governance and on the sharer's desk",
        |sharer| {
            governed(&second_engine, &workspace, &share) == 1
                && on_desk(&sharer.session, workspace_id, &share) == 1
        },
    );
    assert_eq!(
        governed(&second_engine, &workspace, &share),
        1,
        "the share is in the workspace's governance on the device that shares it, one grant \
         to the viewers that reaches every role above them: a share only its own desk names \
         reaches no other device"
    );
    assert_eq!(
        on_desk(&sharer.session, workspace_id, &share),
        1,
        "and its own desk reads it from there"
    );

    // What the second device wrote into the workspace's governance waits in its custody for the
    // workspace's host: what its link to that host sends. The governance alone is carried here;
    // the desk's records are another matter.
    let now = tilo_clock::wall();
    let written: Vec<NeighbourMsg> = second_engine
        .owner()
        .with_outgoing(workspace_id, now, |_, outgoing| {
            Ok(outgoing
                .entries()
                .map(|entry| entry.bytes().to_vec())
                .collect::<Vec<_>>())
        })
        .expect("the second device's custody of the workspace")
        .iter()
        .map(|bytes| postcard::from_bytes(bytes).expect("a frame of the workspace"))
        .filter(|frame| matches!(frame, NeighbourMsg::SyncResp { .. }))
        .collect();
    assert!(
        !written.is_empty(),
        "the share waits to leave the second device for the workspace's host"
    );

    // The first device's own session, on the desk that lists the window as open on another
    // device, before anything of the share reached it.
    drop(window);
    drop(second_engine);
    drop(sharer);
    drop(first_engine);
    env.set("TILO_IDENTITY_ROOT", first_root.path());
    let mut viewer = session_of(&first_key, "Desktop", first_state.path());
    let listed = listing(
        viewer.session.handle().read(Clone::clone),
        workspace_id,
        tab,
    );
    viewer.session.handle().replace_tree(listed);
    for _ in 0..8 {
        viewer
            .session
            .step(Duration::from_millis(2))
            .expect("a frame");
    }
    let first_engine = viewer
        .session
        .shared
        .engine
        .clone()
        .expect("the session holds its person's workspaces");
    assert_eq!(
        governed(&first_engine, &workspace, &share),
        0,
        "the first device knows nothing of the share before it arrives"
    );
    assert!(
        hosted(&viewer.session, tab).is_none(),
        "and lists the window as one it opens its own view of"
    );

    // The workspace's host admits what its person's second device wrote as it admits what that
    // device's link brings: live, in the device's own name.
    for frame in written {
        first_engine
            .owner()
            .receive(
                workspace_id,
                TransportOrigin::RelayDelta(second.device_public_key),
                frame,
                now,
            )
            .expect("the workspace's host admits what its person's second device wrote");
    }
    assert_eq!(
        governed(&first_engine, &workspace, &share),
        1,
        "the governance the first device holds names the share"
    );

    // The share is the workspace's, so nobody asks for it on the first device: its desk names
    // the share and asks the device the governance says hosts the window for it.
    alone(
        &mut viewer,
        &mut None,
        "the first device's desk names the share its workspace's governance holds",
        |viewer| on_desk(&viewer.session, workspace_id, &share) == 1,
    );
    let host = hex::encode(second.device_public_key);
    let named = tab.to_string();
    let asked = |viewer: &Member| {
        rows(&shares(&viewer.session), "watching")
            .iter()
            .any(|watched| watched["tab"] == named.as_str() && watched["host"] == host.as_str())
    };
    let admitted = first_engine.roster_devices(&workspace);
    alone(
        &mut viewer,
        &mut None,
        &format!(
            "the first device asks the second for the window it shares (the workspace admits \
             {admitted:?})"
        ),
        asked,
    );
    // The tab that listed the window is this desk's own view of it and stays as it is. The window
    // the second device shares stands beside it, in a tab of its own that names the device it
    // runs on.
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
        "and it is still this desk's own view of the window"
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
}
