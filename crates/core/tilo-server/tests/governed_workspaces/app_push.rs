//! The version of an app a workspace uses: its `install-app` governance
//! (`workspace/apps#install` in its action API) names it, every member is
//! to use it, and a change made with another version waits until its author
//! has. A developer pushes a version and a bot's program through that API
//! (`tilolang push`, proved in `workspace_actions/api_apps.rs`); here the
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

/// A version's compiled surface.
fn kept_of(version: u8) -> String {
    tilo_surface_compile::compile(if version == 1 { V1 } else { V2 })
        .expect("the fixture is a surface")
}

/// The topic the workspace's host keeps, as a member reads it.
async fn hosted_topic(
    workspaces: &tilo_server::workspaces::Workspaces,
    workspace: &str,
) -> Option<String> {
    let state = workspaces.state(workspace).await?;
    let doc = AutoCommit::load(&opened(&state, &KEYS, (APP, ""))?).unwrap();
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
    // The surfaces it installs are the process's: no other test runs here.
    if !crate::common::in_own_process(
        "governed_workspaces::app_push::a_member_follows_the_version_the_workspace_runs_and_its_held_edit_goes_in",
    ) {
        return;
    }
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

    // Bob's copy, from what the host holds; he edits with v1.
    let mut bob_link = Linked::open(&server, &phone, workspace.workspace_id)
        .await
        .expect("the workspace admits Bob's phone");
    crate::common::receives(&bob_link, |peer| {
        role(peer, phone.device_pubkey()) == Some(collab_types::Role::Editor)
    })
    .await;
    crate::common::wait_until_subscribed(&server, workspace.workspace_id, &phone.device_pubkey())
        .await;
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "v1").unwrap();
    doc.commit_edit("fixture", "topic", &phone.cert.user_public_key);
    bob_link
        .publish(|peer| sealed_checkpoint(peer, &KEYS, &phone.sk, (APP, ""), doc.save()))
        .expect("Bob's v1 edit goes out on his link");
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
                pinned: false,
                payload: tilo_peer_state::workspace_membership::AppPayload::Tapp,
            }),
        )
        .await
        .unwrap();

    // Bob, still on v1, edits: the workspace holds it.
    crate::common::receives(&bob_link, |peer| {
        peer.governance().app_version(APP).is_some()
    })
    .await;
    let before = doc.get_heads();
    doc.put(ROOT, "topic", "while behind").unwrap();
    doc.commit_edit("fixture", "topic", &phone.cert.user_public_key);
    bob_link
        .publish(|peer| sealed_changes(peer, &KEYS, &phone.sk, (APP, ""), doc.save_after(&before)))
        .expect("Bob's edit made behind goes out on his link");

    // His device follows: the workspace names a file his install lacks.
    let release = bob_link.read(|peer| peer.governance().app_version(APP).unwrap().release);
    let installed = hex::encode([1_u8 ^ 0xff; 32]);
    let wanted = release
        .file_to_fetch(Some(&installed))
        .expect("Bob's v1 is not the workspace's version")
        .to_owned();
    assert_eq!(wanted, file);
    bob_link
        .send(NeighbourMsg::DocumentContentWant {
            requester: tilo_peer_state::peer_handle(&phone.device_pubkey()),
            app_id: APP.into(),
            cluster_id: workspace.workspace.clone(),
            hash: wanted.clone(),
            have: Vec::new(),
        })
        .expect("Bob's ask goes out on his link");
    let mut fetched = Vec::new();
    tilo_test_env::within("the workspace hands Bob the package", async {
        while fetched.len() < package.len() {
            if let Some(heard) = bob_link.next().await
                && let NeighbourMsg::DocumentContentPiece {
                    offset,
                    bytes,
                    hash,
                    ..
                } = heard.message
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
    bob_link
        .publish(|peer| sealed_changes(peer, &KEYS, &phone.sk, (APP, ""), doc.save_after(&before)))
        .expect("Bob's edit goes out again on his link");
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
