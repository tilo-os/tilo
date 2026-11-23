//! The version of an app a workspace uses: its `install-app` governance
//! (`workspace/apps#install-app` in its action API) names it, every member is
//! to use it, and a change made with another version waits until its author
//! has. A developer pushes a version and a bot's program through that API
//! (`tilo-app push`, proved in `workspace_actions/api_apps.rs`); here the
//! workspace's server is asked directly and every member follows.
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, roles, roles::governance};
use tilo_peer_state::workspace_membership::{AppRelease, WorkspaceMembershipCommand};

use super::*;

/// The version of the fixture app's surface: the second adds a field.
const V1: &str = "[[resources]]\nkind = \"fixture\"\nlabel = \"Fixture\"\nversion = 1\n\n\
    [[resources.fields]]\nowner = \"fixture\"\npath = \"topic\"\nstored = \"value\"\n\
    title = \"Topic\"\ntype = \"String\"\n";
const V2: &str = "[[resources]]\nkind = \"fixture\"\nlabel = \"Fixture\"\nversion = 1\n\n\
    [[resources.fields]]\nowner = \"fixture\"\npath = \"topic\"\nstored = \"value\"\n\
    title = \"Topic\"\ntype = \"String\"\n\n\
    [[resources.fields]]\nowner = \"fixture\"\npath = \"notes\"\nstored = \"value\"\n\
    title = \"Notes\"\ntype = \"String\"\n";

/// The version Bob's device runs, as its installed app would say: 1 or 2.
static BOB_RUNS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

fn surface_of(version: u8) -> [u8; 32] {
    *blake3::hash(kept_of(version).as_bytes()).as_bytes()
}

/// A version's surface, in the form it is kept by.
fn kept_of(version: u8) -> String {
    let (_, kept) =
        collab_types::surface::SurfaceFile::kept(if version == 1 { V1 } else { V2 }.as_bytes())
            .expect("the fixture is a surface");
    kept
}

/// The topic the workspace's host holds.
async fn hosted_topic(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
) -> Option<String> {
    let bytes = workspaces
        .resource_document(workspace, APP, "")
        .await
        .unwrap()?;
    let doc = AutoCommit::load(&bytes).unwrap();
    doc.get(ROOT, "topic")
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok())
}

/// Ada pushes her v2 into the workspace while Bob runs v1. Bob's edit made with
/// v1 is held; his device sees the workspace's version, fetches its package
/// from the workspace by hash, checks it, installs it, and the same edit, sent
/// again with v2, is in the workspace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_follows_the_version_the_workspace_runs_and_its_held_edit_goes_in() {
    use std::sync::atomic::Ordering;
    crate::common::install_process_surfaces(
        collab_types::Surfaces::from_source(std::sync::Arc::new(
            collab_types::surface::ContentSurfaces::new(|_, wanted| {
                [1, 2]
                    .into_iter()
                    .map(kept_of)
                    .find(|kept| blake3::hash(kept.as_bytes()).to_hex().as_str() == wanted)
                    .map(String::into_bytes)
            }),
        )),
        |_| Some(surface_of(BOB_RUNS.load(Ordering::SeqCst))),
    );
    BOB_RUNS.store(1, Ordering::SeqCst);
    let alice = TestUser::fresh("Alice");
    let ada = TestUser::fresh("Ada");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::member());
    let laptop = ada.enrol("ada-laptop", DeviceCapabilities::member());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let server = start(state.path(), &alice, &host).await;
    let workspaces = server.workspaces().unwrap();
    let workspace = workspaces.create("Lounge").await.unwrap();
    super::members::admit(&server, &host, &laptop, &workspace.workspace).await;
    super::members::admit(&server, &host, &phone, &workspace.workspace).await;
    let who = hex::encode(laptop.cert.user_public_key);
    workspaces
        .put_workspace_grants(
            &workspace.workspace,
            &host.cert,
            [7; 32],
            vec![Grant::new(
                format!("developer:install-app:{who}"),
                Grantee::person(who.clone()),
                roles::governance_action(&workspace.workspace, governance::INSTALL_APP),
                KindSet::ALL,
                Answer::Allow,
                Grantee::person(hex::encode(host.cert.user_public_key)),
            )],
        )
        .await
        .unwrap();

    // Bob's replica, from the workspace's history; he edits with v1.
    let mut bob_peer = StatePeer::new(
        &workspace.workspace,
        host.cert.clone(),
        vec![host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    let bob_session = session(&server, &phone, workspace.workspace_id).await;
    let mut presence = Vec::new();
    receive_until(
        "Bob is in the workspace",
        &bob_session,
        &mut bob_peer,
        &mut presence,
        |peer, _| role(peer, phone.device_pubkey()) == Some(collab_types::Role::Editor),
    )
    .await;
    crate::common::wait_until_subscribed(&server, workspace.workspace_id, &phone.device_pubkey())
        .await;
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "v1").unwrap();
    doc.commit_edit("fixture", "topic", &phone.cert.user_public_key);
    let first = bob_peer
        .publication_mut()
        .publish_incremental(APP, "", doc.save_after(&[]), &phone.sk, SystemTime::now())
        .unwrap();
    publish(&bob_session, &first);
    tilo_test_env::within("Bob's v1 edit in the workspace", async {
        while hosted_topic(workspaces, &workspace.workspace)
            .await
            .as_deref()
            != Some("v1")
        {
            tokio::task::yield_now().await;
        }
    })
    .await;

    // Ada hands the workspace her v2 package and makes it the workspace's version.
    let package = b"a developer's v2 build of the fixture app".repeat(9000);
    let file = workspaces
        .keep_app_file(&workspace.workspace, &package)
        .unwrap();
    workspaces
        .govern_app(
            &workspace.workspace,
            &laptop.cert,
            [9; 32],
            WorkspaceMembershipCommand::InstallApp(AppRelease {
                app: APP.into(),
                version: "2.0.0-dev.1".into(),
                surface: hex::encode(surface_of(2)),
                package: hex::encode([2_u8 ^ 0xff; 32]),
                publisher: hex::encode(laptop.cert.user_public_key),
                file: Some(file.clone()),
                dev: true,
            }),
        )
        .await
        .unwrap();

    // Bob, still on v1, edits: the workspace holds it.
    receive_until(
        "Bob reads the workspace's version",
        &bob_session,
        &mut bob_peer,
        &mut presence,
        |peer, _| peer.governance().app_version(APP).is_some(),
    )
    .await;
    let before = doc.get_heads();
    doc.put(ROOT, "topic", "while behind").unwrap();
    doc.commit_edit("fixture", "topic", &phone.cert.user_public_key);
    let held = bob_peer
        .publication_mut()
        .publish_incremental(
            APP,
            "",
            doc.save_after(&before),
            &phone.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&bob_session, &held);

    // His device follows: the workspace names a file his install lacks.
    let release = bob_peer.governance().app_version(APP).unwrap().release;
    let installed = hex::encode([1_u8 ^ 0xff; 32]);
    let wanted = release
        .file_to_fetch(Some(&installed))
        .expect("Bob's v1 is not the workspace's version")
        .to_owned();
    assert_eq!(wanted, file);
    bob_session
        .publish_presence(
            postcard::to_stdvec(&NeighbourMsg::DocumentContentWant {
                requester: tilo_peer_state::peer_handle(&phone.device_pubkey()),
                app_id: APP.into(),
                cluster_id: workspace.workspace.clone(),
                hash: wanted.clone(),
                have: Vec::new(),
            })
            .unwrap(),
        )
        .unwrap();
    let mut fetched = Vec::new();
    tilo_test_env::within("the workspace hands Bob the package", async {
        while fetched.len() < package.len() {
            if let Some(Inbound::DocumentContent { frame, .. }) = bob_session.recv().await
                && let Ok(NeighbourMsg::DocumentContentPiece {
                    offset,
                    bytes,
                    hash,
                    ..
                }) = postcard::from_bytes(&frame)
                && hash == wanted
                && offset == fetched.len() as u64
            {
                fetched.extend_from_slice(&bytes);
            }
        }
    })
    .await;
    release
        .check_file(&fetched)
        .expect("the package is the workspace's version");
    assert!(release.check_file(b"something else").is_err());
    assert_eq!(
        hosted_topic(workspaces, &workspace.workspace)
            .await
            .as_deref(),
        Some("v1"),
        "an edit made with another version than the workspace's is held"
    );

    // Installed: his device runs v2, and the same edit goes out again.
    BOB_RUNS.store(2, Ordering::SeqCst);
    let again = bob_peer
        .publication_mut()
        .publish_incremental(
            APP,
            "",
            doc.save_after(&before),
            &phone.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&bob_session, &again);
    tilo_test_env::within("Bob's edit made behind is in the workspace", async {
        while hosted_topic(workspaces, &workspace.workspace)
            .await
            .as_deref()
            != Some("while behind")
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    server.shutdown().await;
}
