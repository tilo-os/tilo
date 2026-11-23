//! One device, two writes to one workspace at the same moment: one the
//! device authors into a record, the other the write its own session hands
//! to the workspace's link. Both land, and neither is refused for history it
//! was not built on.

use super::*;

/// The app whose record the device authors.
const APP: &str = "so.tilo.notes";
/// The record it authors, and the rounds written into it.
const RECORD: &str = "plan";

/// A change `device` makes to `workspace`'s own document, the layout: what
/// its session hands the workspace's link, as a device writes it.
async fn layout_change(
    workspaces: &Workspaces,
    workspace: &str,
    device: &DeviceCert,
    salt: u16,
) -> neighbour_wire::NeighbourMsg {
    let mut doc = {
        let state = workspaces.workspace(workspace).await.unwrap();
        let governed = state.lock().await;
        governed.peer().admitted_workspace().unwrap().clone()
    };
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(device.device_public_key.to_vec()));
    write_windows(&mut doc, workspace, device, salt, &one_chat_window(salt));
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

/// A write the host authors and one its link takes in at the same moment both
/// land. The link takes a write into the workspace's copy without the
/// workspace's other lock; a change built from a copy taken before that lock
/// was taken, and committed after the link's write landed, no longer held the
/// history the copy did, and was refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_authored_and_one_the_link_takes_in_at_one_moment_both_land() {
    use automerge::transaction::Transactable as _;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    let workspace = created.workspace.clone();
    let link = workspaces
        .direct()
        .link(created.workspace_id)
        .expect("the host serves the workspace over its link");
    for round in 0..40u16 {
        let message = layout_change(&workspaces, &workspace, &cert, round).await;
        let device = cert.device_public_key;
        let admitted = {
            let link = link.clone();
            tokio::spawn(async move { link.admit_one(device, message).await })
        };
        let authored = {
            let workspaces = workspaces.clone();
            let cert = cert.clone();
            let workspace = workspace.clone();
            tokio::spawn(async move {
                workspaces
                    .author_workspace_document(&workspace, &cert, (APP, RECORD), |doc| {
                        doc.put(automerge::ROOT, "round", i64::from(round))
                            .map(drop)
                            .map_err(|error| error.to_string())
                    })
                    .await
            })
        };
        let (_, kept) = admitted.await.unwrap();
        assert!(
            kept.is_some(),
            "round {round}: the link takes the session's write in"
        );
        authored.await.unwrap().unwrap_or_else(|error| {
            panic!("round {round}: the device's own write is refused: {error}")
        });
    }
    drop(link);
    drop(workspaces);
    server.shutdown().await;
}
