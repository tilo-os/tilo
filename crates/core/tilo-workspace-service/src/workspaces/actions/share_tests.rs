//! `grant.set`: this host's own person giving someone, or everyone, a role over one part of a
//! workspace it serves (`crate::workspaces::shares`). The host decides each as its caller's, as
//! every receiver decides a device's own, and signs only its own person's: everyone else shares
//! from their own device, but for a window of a device this host placed in the workspace.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the server and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use serde_json::{Value, json};
use tilo_action_policy::{Grantee, Role, TreePath};
use tilo_action_types::ActionKind;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, ReceiptStatus, WORKSPACE_GRANT_SET, WORKSPACE_READ_GRANT,
    WORKSPACE_READ_REVOKE,
};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};
use tilo_user_identity::DeviceCert;

use super::ServiceWorkspaceActions;
use super::tests::{admit, caller, hosted, request, settled};
use crate::workspaces::Workspaces;
use crate::workspaces::tests::{guest_device, hosting_device, restriction, test_host};

/// One workspace this host serves, and how its members ask it.
struct Asked<'a> {
    workspaces: &'a Workspaces,
    workspace: String,
    id: WorkspaceId,
    host: ServiceWorkspaceActions,
}

impl Asked<'_> {
    /// `device`'s `grant.set`: `role` (`null` takes their share back) over `address` for
    /// `person`, a person's hex root or `everyone`.
    async fn set(
        &self,
        device: &DeviceCert,
        person: &str,
        address: &str,
        role: Option<&str>,
    ) -> Result<Value, ActionCallError> {
        let argument = json!({ "person": person, "address": address, "role": role });
        self.call(device, WORKSPACE_GRANT_SET, argument).await
    }

    /// What `device`'s governance action `action` with `argument` comes to.
    async fn call(
        &self,
        device: &DeviceCert,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionCallError> {
        let pending = self
            .host
            .in_workspace(self.id)
            .invoke(&caller(device), request(&self.workspace, action, argument));
        let receipt = settled(pending).await?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            other => panic!("a share completes at once: {other:?}"),
        }
    }

    /// Whether `device`'s person may do `kind` at `node`, as the workspace decides now.
    async fn may(&self, device: &DeviceCert, node: &str, kind: ActionKind) -> bool {
        let node: TreePath = node.parse().unwrap();
        let now = web_time::SystemTime::now();
        let state = self.workspaces.workspace(&self.workspace).await.unwrap();
        let locked = state.lock().await;
        let member = locked
            .peer()
            .permissions(device.device_public_key, now)
            .unwrap();
        member.allows(&node, kind, now)
    }

    /// `owner` keeps `member` out of a channel and lets them back in, and their read follows.
    async fn reads_follow(&self, owner: &DeviceCert, member: &DeviceCert) {
        let read = TreePath::workspace(&self.workspace).app("so.tilo.chat");
        let read = read
            .any(tilo_action_policy::Level::Kind)
            .resource("root/general");
        let person = json!({"person": hex::encode(member.user_public_key)});
        for (action, reads) in [(WORKSPACE_READ_REVOKE, false), (WORKSPACE_READ_GRANT, true)] {
            let argument = json!({ "resource": "tl:so.tilo.chat:root/general", "member": person });
            self.call(owner, action, argument).await.unwrap();
            assert_eq!(
                self.may(member, &read.to_string(), ActionKind::Read).await,
                reads
            );
        }
    }

    async fn policy(&self) -> tilo_action_policy::WorkspacePolicy {
        let state = self.workspaces.workspace(&self.workspace).await.unwrap();
        let locked = state.lock().await;
        tilo_peer_state::workspace::policy_of(
            locked.peer().admitted_workspace().unwrap(),
            &self.workspace,
        )
    }
}

/// A note in Notes in `workspace`, by its id.
fn note(workspace: &str, id: &str) -> String {
    TreePath::workspace(workspace)
        .app("so.tilo.notes")
        .kind("notes.notebook")
        .any(tilo_action_policy::Level::Resource)
        .item(id)
        .to_string()
}

fn refused(answer: Result<Value, ActionCallError>) -> String {
    match answer {
        Err(ActionCallError::Refused { reason }) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn the_owner_shares_one_note_with_everyone_until_they_take_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, id, host) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    workspaces
        .change(
            &workspace,
            (&owner, [40; 32]),
            tilo_action_policy::Change::SetRole {
                person: hex::encode(bo.user_public_key),
                role: tilo_peer_state::join::Role::Viewer,
            },
        )
        .await
        .unwrap();
    let asked = Asked {
        workspaces,
        workspace: workspace.clone(),
        id,
        host,
    };
    let (p1, p2) = (note(&workspace, "p1"), note(&workspace, "p2"));
    let writes = ActionKind::Undoable;

    // Bo, a Viewer, reads the note and writes nothing, and cannot give more than he holds.
    assert!(asked.may(&bo, &p1, ActionKind::Read).await);
    assert!(!asked.may(&bo, &p1, writes).await);
    refused(asked.set(&bo, "everyone", &p1, Some("editor")).await);

    // Ada, an Editor, shares from her own device: the host signs no share as hers.
    let reason = refused(asked.set(&ada, "everyone", &p1, Some("editor")).await);
    assert!(reason.contains("from their own device"), "{reason}");

    // The owner shares it with everyone to edit: Bo writes that note, not the one beside it.
    let shared = asked
        .set(&owner, "everyone", &p1, Some("editor"))
        .await
        .unwrap();
    assert_eq!(shared, json!({ "scope": p1, "changed": true }));
    assert!(asked.may(&bo, &p1, writes).await);
    assert!(
        !asked.may(&bo, &p2, writes).await,
        "another note stays as Bo's role has it"
    );
    assert_eq!(
        asked
            .set(&owner, "everyone", &p1, Some("editor"))
            .await
            .unwrap()["changed"],
        json!(false),
        "a repeat changes nothing"
    );
    let everyone = asked.policy().await.everyone;
    assert_eq!(everyone.len(), 1);
    assert_eq!(
        everyone[0].by,
        hex::encode(owner.user_public_key),
        "a share is its signer's"
    );
    // Restricted, Bo writes nothing, the note shared to edit included, nor hands it out; the
    // page's action API decides the same. Lifted, the share lets him write again.
    let bo_person = hex::encode(bo.user_public_key);
    let until = tilo_clock::unix_now_ms() + 30 * 60_000;
    restriction(workspaces, &workspace, &bo_person, Some(until))
        .await
        .unwrap();
    assert!(asked.may(&bo, &p1, ActionKind::Read).await);
    assert!(!asked.may(&bo, &p1, writes).await);
    let permits = WorkspacesRequest::Permits {
        workspace: workspace.clone(),
        node: p1.clone(),
    };
    assert_eq!(
        workspaces.page_workspaces(&bo, permits).await,
        WorkspacesReply::Permits(false)
    );
    refused(asked.set(&bo, "everyone", &p1, Some("viewer")).await);
    restriction(workspaces, &workspace, &bo_person, None)
        .await
        .unwrap();
    assert!(asked.may(&bo, &p1, writes).await);

    // Who reads one resource is changed the same way: kept out is a Deny on it, and let back in
    // takes the Deny away (`members#revoke-read`, `#grant-read`).
    asked.reads_follow(&owner, &bo).await;

    // Only its giver takes it back: Bo may not, the owner may.
    refused(asked.set(&bo, "everyone", &p1, None).await);
    assert!(asked.may(&bo, &p1, writes).await);
    asked.set(&owner, "everyone", &p1, None).await.unwrap();
    assert!(!asked.may(&bo, &p1, writes).await);

    // The whole workspace is a role, never a share.
    let whole = TreePath::workspace(&workspace).rest().to_string();
    let reason = refused(asked.set(&owner, "everyone", &whole, Some("viewer")).await);
    assert!(reason.contains("whole workspace is not shared"), "{reason}");
    server.shutdown().await;
}

#[tokio::test]
async fn a_person_given_files_alone_holds_files_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, id, host) = hosted(workspaces).await;
    let asked = Asked {
        workspaces,
        workspace: workspace.clone(),
        id,
        host,
    };
    let (_, bram) = guest_device();
    let bram = hex::encode(bram.user_public_key);

    // A person the workspace does not hold is let in over Files alone, by its short address.
    let given = asked
        .set(
            &owner,
            &bram,
            &format!("{workspace}/files/**"),
            Some("editor"),
        )
        .await
        .unwrap();
    let files = TreePath::workspace(&workspace)
        .app(tilo_actions::tree::FILES_APP)
        .rest();
    assert_eq!(given["scope"], json!(files.to_string()));
    let policy = asked.policy().await;
    assert_eq!(policy.member_scope(&bram), Some(vec![&files]));

    // Part of an app needs all of the app's keys, which only a full member holds.
    let reason = refused(
        asked
            .set(&owner, &bram, &note(&workspace, "p1"), Some("viewer"))
            .await,
    );
    assert!(reason.contains("is part of an app"), "{reason}");

    // Taking back the last part is leaving.
    asked
        .set(&owner, &bram, &format!("{workspace}/files/**"), None)
        .await
        .unwrap();
    assert!(!asked.policy().await.members.contains_key(&bram));
    server.shutdown().await;
}

/// A window is shared by the device it runs on: nobody shares another device's window. A
/// device this host placed in the workspace holds none of its keys, so the host signs that
/// device's own window share and its end, and no other member's. Every member reads the share
/// back ([`tilo_desktop_model::window_share::window_shares`]).
#[tokio::test]
async fn only_the_device_a_window_runs_on_shares_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, id, _) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    // Ada's device mints its windows under the salt its key derives.
    let salt = tilo_desktop_model::ids::device_salt_of_key(&ada.device_public_key);
    let notes = (u64::from(salt) << 48) | 4;
    let window = tilo_desktop_model::window_share::window_node(&workspace, "so.tilo.notes", notes);
    let (ws, window) = (workspace.as_str(), &window);
    let share = |device: DeviceCert, role: Option<Role>| async move {
        let everyone = Grantee::role(Role::Viewer);
        workspaces
            .set_scoped_role(ws, &device, (everyone, window), role)
            .await
    };
    let shared_windows = || async {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let locked = state.lock().await;
        let peer = locked.peer();
        let doc = peer.admitted_workspace().unwrap();
        tilo_desktop_model::window_share::window_shares(
            &tilo_peer_state::workspace::policy_of(doc, &workspace),
            &tilo_desktop_model::window_share::device_salts(doc),
        )
    };
    let reason = share(bo.clone(), Some(Role::Viewer))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        reason.contains("only the device a window runs on"),
        "{reason}"
    );
    let reason = share(ada.clone(), Some(Role::Viewer))
        .await
        .unwrap_err()
        .to_string();
    assert!(reason.contains("from their own device"), "{reason}");

    // Placed in the workspace, Ada's device shares its own window through this host.
    let placed = crate::environment::EnvironmentScope::of_workspace_device(id);
    let environments = &workspaces.inner.host.environments;
    environments
        .place_certified(&ada, placed, u64::MAX)
        .unwrap();
    assert!(share(ada.clone(), Some(Role::Viewer)).await.unwrap());
    assert!(
        !share(ada.clone(), Some(Role::Viewer)).await.unwrap(),
        "a repeat changes nothing"
    );
    let now_shared = shared_windows().await;
    let shared = now_shared.get(&notes).expect("Ada's Notes is shared");
    assert_eq!(shared.host, hex::encode(ada.device_public_key));
    assert_eq!(
        shared.sharer,
        hex::encode(owner.user_public_key),
        "the host signs it"
    );

    // Bo takes nobody's share back; Ada's placed device takes its own back.
    share(bo.clone(), None).await.unwrap_err();
    assert!(share(ada.clone(), None).await.unwrap());
    assert!(shared_windows().await.is_empty(), "the share ended");
    server.shutdown().await;
}
