//! A member's share of one record or item with a workspace this host serves,
//! as a page asks it (`crate::workspaces::shares`): the same rule every receiver
//! holds a device's own share to (`tilo_peer_state::workspace::share`).

#![expect(
    clippy::significant_drop_tightening,
    reason = "the server and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use serde_json::{Value, json};
use tilo_user_identity::DeviceCert;

use super::tests::{admit, hosted};
use crate::workspaces::Workspaces;
use crate::workspaces::tests::{hosting_device, test_host};

const SHARE_RESOURCE: &str = "share";
const UNSHARE_RESOURCE: &str = "unshare";
const RESOURCE_PERMITS: &str = "permits";

/// A member's ask of `workspace`: share, end a share, or what they may do at a
/// node, with its arguments.
struct Ask {
    workspace: String,
    action: &'static str,
    argument: Value,
}

fn member_request(workspace: &str, action: &'static str, argument: Value) -> Ask {
    Ask {
        workspace: workspace.to_owned(),
        action,
        argument,
    }
}

/// What the workspace answers `ask` from `device`.
async fn invoke(workspaces: &Workspaces, device: &DeviceCert, ask: Ask) -> Result<Value, String> {
    let text = |key: &str| ask.argument[key].as_str().unwrap_or_default().to_owned();
    let node = || -> Result<tilo_action_policy::TreePath, String> {
        text("node").parse().map_err(|_| "not a node".to_owned())
    };
    match ask.action {
        SHARE_RESOURCE => {
            let access = text("access").parse().map_err(|_| "no access".to_owned())?;
            workspaces
                .share_resource(&ask.workspace, device, &text("share"), &node()?, access)
                .await
                .map(|grants| json!({ "grants": grants }))
        }
        UNSHARE_RESOURCE => workspaces
            .unshare_resource(&ask.workspace, device, &text("share"))
            .await
            .map(|revoked| json!({ "revoked": revoked })),
        _ => {
            let node = node()?;
            let now = web_time::SystemTime::now();
            let state = workspaces
                .workspace(&ask.workspace)
                .await
                .map_err(|error| error.to_string())?;
            let held = state.lock().await;
            let member = held
                .peer()
                .permissions(device.device_public_key, now)
                .map_err(|error| error.to_string())?;
            drop(held);
            return Ok(json!({
                "read": member.allows(&node, tilo_action_policy::ActionKind::Read, now),
                "undoable": member.allows(&node, tilo_action_policy::ActionKind::Undoable, now),
            }));
        }
    }
    .map_err(|error| error.to_string())
}

/// An item of Notes' own record of `workspace`, by its id.
fn note(workspace: &str, id: &str) -> String {
    tilo_action_policy::TreePath::workspace(workspace)
        .app("so.tilo.notes")
        .kind("notes.notebook")
        .any(tilo_action_policy::Level::Resource)
        .item(id)
        .to_string()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one share told whole, through both paths, in order"
)]
async fn a_share_grants_the_workspace_one_item_until_its_sharer_ends_it() {
    use tilo_server_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _, _) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            crate::workspaces::SetMemberRoleRequest {
                request_id: [40; 32],
                user_public_key: bo.user_public_key,
                role: tilo_peer_state::join::Role::Viewer,
            },
        )
        .await
        .unwrap();
    let (p1, p2) = (note(&workspace, "p1"), note(&workspace, "p2"));
    let permits =
        |node: &str| member_request(&workspace, RESOURCE_PERMITS, json!({ "node": node }));
    let share = |access: &str| {
        member_request(
            &workspace,
            SHARE_RESOURCE,
            json!({ "share": "m1", "node": p1, "access": access }),
        )
    };
    let unshare = || member_request(&workspace, UNSHARE_RESOURCE, json!({ "share": "m1" }));
    let bo_writes = |node: &str| {
        let request = permits(node);
        let bo = &bo;
        async move {
            invoke(workspaces, bo, request).await.unwrap()["undoable"]
                .as_bool()
                .unwrap()
        }
    };

    // Bo, a Viewer, reads the note and writes nothing.
    let before = invoke(workspaces, &bo, permits(&p1)).await.unwrap();
    assert_eq!(before, json!({ "read": true, "undoable": false }));

    // Bo cannot hand out more than he holds.
    assert!(invoke(workspaces, &bo, share("edit")).await.is_err());

    // A view share gives no writes.
    let viewed = invoke(workspaces, &ada, share("view")).await.unwrap();
    assert_eq!(viewed, json!({ "grants": 1 }));
    assert!(!bo_writes(&p1).await);

    // Ada, an Editor, shares it to edit: Bo writes that note, not its sibling.
    invoke(workspaces, &ada, share("edit")).await.unwrap();
    assert!(bo_writes(&p1).await);
    assert!(
        !bo_writes(&p2).await,
        "a sibling note stays as Bo's role has it"
    );
    assert_eq!(
        invoke(workspaces, &ada, share("edit")).await.unwrap(),
        json!({ "grants": 1 }),
        "a repeat is the same share"
    );
    // The page's action API decides the same.
    assert_eq!(
        workspaces
            .page_workspaces(
                &bo,
                WorkspacesRequest::Permits {
                    workspace: workspace.clone(),
                    node: p1.clone(),
                },
            )
            .await,
        WorkspacesReply::Permits(true)
    );

    // Only its sharer ends it.
    assert_eq!(
        invoke(workspaces, &bo, unshare()).await.unwrap(),
        json!({ "revoked": 0 })
    );
    assert!(bo_writes(&p1).await);
    assert_eq!(
        invoke(workspaces, &ada, unshare()).await.unwrap(),
        json!({ "revoked": 1 })
    );
    assert!(!bo_writes(&p1).await);

    // Shared again through a page's action API, and ended there.
    let shared = workspaces
        .page_workspaces(
            &ada,
            WorkspacesRequest::Share {
                workspace: workspace.clone(),
                share: "m1".into(),
                node: p1.clone(),
                access: "edit".into(),
            },
        )
        .await;
    assert_eq!(shared, WorkspacesReply::Shared(1));
    assert!(bo_writes(&p1).await);
    let every = tilo_action_policy::TreePath::workspace(&workspace)
        .app("so.tilo.notes")
        .kind("notes.notebook")
        .any(tilo_action_policy::Level::Resource)
        .to_string();
    assert!(matches!(
        workspaces
            .page_workspaces(
                &ada,
                WorkspacesRequest::Share {
                    workspace: workspace.clone(),
                    share: "m2".into(),
                    node: every,
                    access: "view".into(),
                },
            )
            .await,
        WorkspacesReply::Refused(_)
    ));
    assert_eq!(
        workspaces
            .page_workspaces(
                &ada,
                WorkspacesRequest::Unshare {
                    workspace: workspace.clone(),
                    share: "m1".into(),
                },
            )
            .await,
        WorkspacesReply::Shared(1)
    );
    assert!(!bo_writes(&p1).await);
    server.shutdown().await;
}

#[tokio::test]
async fn a_share_to_edit_lifts_no_restriction() {
    use tilo_server_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _, _) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            crate::workspaces::SetMemberRoleRequest {
                request_id: [40; 32],
                user_public_key: bo.user_public_key,
                role: tilo_peer_state::join::Role::Viewer,
            },
        )
        .await
        .unwrap();
    let p1 = note(&workspace, "p1");
    let shared = json!({ "share": "m1", "node": p1, "access": "edit" });
    invoke(
        workspaces,
        &ada,
        member_request(&workspace, SHARE_RESOURCE, shared),
    )
    .await
    .unwrap();
    let permits = || {
        invoke(
            workspaces,
            &bo,
            member_request(&workspace, RESOURCE_PERMITS, json!({ "node": p1 })),
        )
    };
    assert_eq!(
        permits().await.unwrap(),
        json!({ "read": true, "undoable": true })
    );

    // Restricted, Bo writes nothing, the note shared to edit included, and
    // still reads it.
    let until = tilo_clock::unix_now_ms() + 30 * 60_000;
    workspaces
        .restrict_member(
            &workspace,
            &owner,
            crate::workspaces::RemoveMemberRequest {
                request_id: [41; 32],
                user_public_key: bo.user_public_key,
            },
            until,
        )
        .await
        .unwrap();
    assert_eq!(
        permits().await.unwrap(),
        json!({ "read": true, "undoable": false })
    );
    assert_eq!(
        workspaces
            .page_workspaces(
                &bo,
                WorkspacesRequest::Permits {
                    workspace: workspace.clone(),
                    node: p1.clone(),
                },
            )
            .await,
        WorkspacesReply::Permits(false),
        "the page's action API decides the same"
    );
    // Nor may he hand out what his restriction keeps from him.
    let reshare = json!({ "share": "m2", "node": p1, "access": "view" });
    assert!(
        invoke(
            workspaces,
            &bo,
            member_request(&workspace, SHARE_RESOURCE, reshare)
        )
        .await
        .is_err()
    );

    // Lifted, the share lets him write again.
    workspaces
        .unrestrict_member(
            &workspace,
            &owner,
            crate::workspaces::RemoveMemberRequest {
                request_id: [42; 32],
                user_public_key: bo.user_public_key,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        permits().await.unwrap(),
        json!({ "read": true, "undoable": true })
    );
    server.shutdown().await;
}

/// A window is shared under its own share id by the device it runs on, the
/// one its tab's topology namespace is registered to; nobody shares another
/// device's window, nor a window in a namespace no device holds. The share
/// is the workspace's record of who shared which window, read back by every
/// member ([`tilo_desktop_model::window_share::window_shares`]).
#[tokio::test]
async fn only_the_device_a_window_runs_on_shares_it() {
    use tilo_desktop_model::window_share::{window_node, window_share_id};

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _, _) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    // Ada's device mints its windows under the salt its key derives.
    let salt = tilo_desktop_model::ids::device_salt_of_key(&ada.device_public_key);
    let notes = (u64::from(salt) << 48) | 4;
    let share = |tab: u64, id: &str| {
        member_request(
            &workspace,
            SHARE_RESOURCE,
            json!({
                "share": id,
                "node": window_node(&workspace, "so.tilo.notes", tab).to_string(),
                "access": "view",
            }),
        )
    };
    let refused = |answer: Result<Value, String>| answer.is_err();
    assert!(
        refused(invoke(workspaces, &bo, share(notes, &window_share_id(notes))).await),
        "Bo can't share a window of Ada's device"
    );
    assert!(
        refused(invoke(workspaces, &ada, share(notes, "m1")).await),
        "a window is shared under its own share id"
    );
    let held_salts = {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let held = state.lock().await;
        tilo_desktop_model::window_share::device_salts(held.peer().admitted_workspace().unwrap())
    };
    assert_eq!(
        held_salts.get(&salt),
        Some(&hex::encode(ada.device_public_key)),
        "the workspace knows which device mints under Ada's salt"
    );
    let unheld = (1..u16::MAX)
        .find(|salt| !held_salts.contains_key(salt))
        .expect("a salt none of the workspace's devices mints under");
    let elsewhere = (u64::from(unheld) << 48) | 4;
    assert!(
        refused(
            invoke(
                workspaces,
                &ada,
                share(elsewhere, &window_share_id(elsewhere))
            )
            .await
        ),
        "nor a window in a namespace no device holds"
    );
    assert!(shared_windows(workspaces, &workspace).await.is_empty());

    invoke(workspaces, &ada, share(notes, &window_share_id(notes)))
        .await
        .unwrap();
    let now_shared = shared_windows(workspaces, &workspace).await;
    let shared = now_shared.get(&notes).expect("Ada's Notes is shared");
    assert_eq!(shared.host, hex::encode(ada.device_public_key));
    assert_eq!(shared.sharer, hex::encode(ada.user_public_key));
    assert_eq!(shared.app_id, "so.tilo.notes");

    invoke(
        workspaces,
        &ada,
        member_request(
            &workspace,
            UNSHARE_RESOURCE,
            json!({ "share": window_share_id(notes) }),
        ),
    )
    .await
    .unwrap();
    assert!(
        shared_windows(workspaces, &workspace).await.is_empty(),
        "the share ended"
    );
    server.shutdown().await;
}

/// The windows shared in `workspace` now, by the workspace's own record.
async fn shared_windows(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
) -> std::collections::BTreeMap<u64, tilo_desktop_model::window_share::WindowShare> {
    let state = workspaces.workspace(workspace).await.unwrap();
    let held = state.lock().await;
    let peer = held.peer();
    let doc = peer.admitted_workspace().unwrap();
    tilo_desktop_model::window_share::window_shares(
        &tilo_peer_state::workspace::policy_of(doc, workspace),
        &tilo_desktop_model::window_share::device_salts(doc),
        tilo_clock::unix_now_ms(),
    )
}
