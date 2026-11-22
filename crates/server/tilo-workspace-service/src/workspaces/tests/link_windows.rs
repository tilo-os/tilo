//! A member's windows that reach the workspace over its direct link stay the
//! workspace's across a restart of the server and of the member: the
//! member's next windows build on them, and are taken in.

use super::*;
use std::time::Duration;

/// How long a link or the governor may take to do what a test waits for.
const WAIT: Duration = Duration::from_secs(10);

/// A member device of `workspace` with `key`, linked to the server's direct
/// link of it as its runtime links, with what it holds of the workspace from
/// the server: the pairing, which ends with the test, and the member's link.
async fn linked(
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
    key: SigningKey,
) -> (
    tokio::task::JoinHandle<Result<(), String>>,
    tilo_peer_state::direct::DirectSync,
) {
    let host = workspaces
        .direct()
        .link(workspace_id)
        .expect("the server serves the workspace over a direct link");
    let member = tilo_peer_state::direct::DirectSync::new(host.read(Clone::clone), key);
    let mut caught_up = member.caught_up();
    let linked = member.clone();
    let pairing =
        tokio::spawn(
            async move { tilo_peer_state::direct::DirectSync::pair(&linked, &host).await },
        );
    tokio::time::timeout(WAIT, caught_up.wait_for(|caught_up| *caught_up))
        .await
        .expect("the member catches up with the server")
        .unwrap();
    (pairing, member)
}

/// Wait until the governor of `workspace` committed everything its copy holds
/// (what a restart starts from) and that copy holds `titles` windows.
async fn committed_with(workspaces: &Workspaces, workspace: &str, titles: usize) {
    let state = workspaces.workspace(workspace).await.unwrap();
    tokio::time::timeout(WAIT, async {
        loop {
            {
                let governed = state.lock().await;
                let held = titles_in(governed.peer().admitted_workspace().unwrap()).len();
                if held == titles && governed.peer().workspace_heads() == governed.published_heads {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("the governor commits the member's windows the link admitted ({titles} windows)")
    });
}

/// `member`'s change closing the windows it made in its namespace `salt`,
/// made on `doc`, the workspace's document as the member's device holds it.
fn closed_on(
    mut doc: automerge::AutoCommit,
    workspace: &str,
    member: &DeviceCert,
    salt: u16,
) -> neighbour_wire::NeighbourMsg {
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
    tilo_desktop_model::shared_resource::device::withdraw(&mut doc, &|id| {
        tilo_desktop_model::peer_salt_of(id) == salt
    })
    .unwrap();
    doc.commit();
    neighbour_wire::NeighbourMsg::SyncResp {
        workspace: workspace.to_owned(),
        changes: doc
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    }
}

/// The workspace's document as the governor of `workspace` holds it.
async fn governors_copy(workspaces: &Workspaces, workspace: &str) -> automerge::AutoCommit {
    let state = workspaces.workspace(workspace).await.unwrap();
    let governed = state.lock().await;
    governed.peer().admitted_workspace().unwrap().clone()
}

/// The windows a member opens over the link are committed by the governor as
/// they come: the server restarts, the member's device restarts holding them
/// as the workspace's, and the windows it closes next, a change built on
/// them, are taken in. The link took them into the governor's copy, and
/// nothing wrote that copy to disk until some other change came: a restart
/// before one lost them, and the server refused every later change of the
/// member's for history it no longer had, as long as the member ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_members_windows_the_link_admitted_outlive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    let workspace = created.workspace.clone();
    let (member, salt) = admitted_member(&workspaces, &workspace, &key, &cert, 3).await;
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let opened = terminals_opened(&workspaces, &workspace, &member, salt).await;
    let (pairing, link) = linked(&workspaces, created.workspace_id, member_key.clone()).await;
    link.send(postcard::from_bytes(&opened).unwrap()).unwrap();
    committed_with(&workspaces, &workspace, 2).await;
    // What the member's device holds of the workspace: its own windows in it.
    let members_copy = governors_copy(&workspaces, &workspace).await;
    pairing.abort();
    drop(link);
    drop(workspaces);
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    assert_eq!(
        titles_in(&governors_copy(&workspaces, &workspace).await).len(),
        2,
        "the server starts again with the member's windows"
    );
    // The member's device restarted too: a new link, from what it kept.
    let (pairing, link) = linked(&workspaces, created.workspace_id, member_key).await;
    link.send(closed_on(members_copy, &workspace, &member, salt))
        .unwrap();
    committed_with(&workspaces, &workspace, 0).await;
    assert!(
        workspaces
            .inner
            .unbuildable_windows
            .lock()
            .unwrap()
            .is_empty(),
        "no change of the member's was refused for history the workspace lost"
    );
    pairing.abort();
    drop(link);
    drop(workspaces);
    server.shutdown().await;
}

/// A workspace whose server retained a member's windows its link admitted,
/// and stopped before its governor committed them: the start takes them in
/// from what the server retains, so the member's next change, built on
/// them, is taken in rather than refused for good.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_start_takes_in_a_members_windows_retained_but_never_committed() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    let workspace = created.workspace.clone();
    let (member, salt) = admitted_member(&workspaces, &workspace, &key, &cert, 3).await;
    let opened = terminals_opened(&workspaces, &workspace, &member, salt).await;
    let message: neighbour_wire::NeighbourMsg = postcard::from_bytes(&opened).unwrap();
    // Retained as the server retains every frame its link admits, and never
    // taken into the governor's copy: the server stopped first.
    workspaces
        .inner
        .records
        .carry(
            created.workspace_id,
            member.device_public_key,
            &crate::workspace_documents::HostCommit { body: opened },
        )
        .unwrap();
    let mut members_copy = governors_copy(&workspaces, &workspace).await;
    let neighbour_wire::NeighbourMsg::SyncResp { changes, .. } = &message else {
        unreachable!("the member's windows are a change of the workspace's own document");
    };
    let changes: Vec<_> = changes
        .iter()
        .map(|change| automerge::Change::from_bytes(change.clone()).unwrap())
        .collect();
    tilo_automerge::ApplyIncoming::apply_incoming(&mut members_copy, changes).unwrap();
    assert!(titles_in(&governors_copy(&workspaces, &workspace).await).is_empty());
    drop(workspaces);
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    committed_with(&workspaces, &workspace, 2).await;
    let closed = postcard::to_stdvec(&closed_on(members_copy, &workspace, &member, salt)).unwrap();
    workspaces
        .absorb(&workspace, member.device_public_key, &closed)
        .await
        .expect("the member's next change builds on what the workspace holds");
    committed_with(&workspaces, &workspace, 0).await;
    drop(workspaces);
    server.shutdown().await;
}
