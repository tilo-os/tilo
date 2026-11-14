use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use collab_types::Ed25519PublicKey;
use key_group::VaultKey;
use tilo_desktop_model::WorkspaceId;

use crate::encryption::{
    GovernanceTransport, InboundGovernance, KeyAccess, KeyDir, KeyDirectory, KeyRole, KeysConfig,
    WorkspaceKeys,
};
use crate::gates::{GateRegistry, Gates};
use crate::testing::{Directory, Endpoint, Policy, Workspace, key, public};

/// A device that holds nothing of the workspace's governance yet.
struct Unknown;

impl KeyDirectory for Unknown {
    fn allowed_devices(&self, _: WorkspaceId, _: u64) -> Option<BTreeSet<Ed25519PublicKey>> {
        None
    }
    fn founds(&self, _: WorkspaceId) -> bool {
        false
    }
}

fn service(
    directory: Box<dyn KeyDirectory>,
    seed: u8,
    role: KeyRole,
    dir: &std::path::Path,
) -> WorkspaceKeys {
    WorkspaceKeys::open(
        KeysConfig {
            state: Arc::new(KeyDir::new(dir.join("groups"))),
            vault: VaultKey::new([seed; 32]),
            signer: Arc::new(key(seed)),
            role,
        },
        directory,
        Arc::new(Gates::default()) as Arc<dyn GateRegistry>,
    )
    .unwrap()
}

#[test]
fn a_device_entering_opens_the_desktop_before_the_workspace_governance_arrives() {
    let workspace = Arc::new(Workspace::default());
    let workspace_id = WorkspaceId::new();
    let policy = Policy::readers([1, 2]);
    let (host_dir, page_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut host = service(
        Box::new(Directory::new(policy)),
        1,
        KeyRole::Committer,
        host_dir.path(),
    );
    let host_link = Endpoint::join(&workspace, workspace_id, 0);
    let mut page = service(Box::new(Unknown), 2, KeyRole::Follower, page_dir.path());
    let page_link = Endpoint::join(&workspace, workspace_id, 1);

    // The workspace's group exists, its host in it.
    let mut now = tilo_clock::now();
    host.access(workspace_id, now);
    host.found(workspace_id, now, 1_000);
    assert!(matches!(
        host.access(workspace_id, now),
        KeyAccess::Ready(_)
    ));
    let _ = page_link.take_inbound();

    // The page asks for the workspace's keys before it knows anything of the
    // workspace: the request is its own, for the workspace's group.
    let request = pollster::block_on(page.ask_on_entry(workspace_id, now, 1_000))
        .expect("an entering device asks for the workspace's keys");
    assert_eq!(request.workspace, workspace_id);
    assert_eq!(request.device, public(2));
    request.verify().unwrap();
    // It is asked: the pump does not ask again at once.
    page.pump(&page_link, now, 1_000);
    assert!(host_link.take_inbound().is_empty());

    // The host hands it to the group's participants, which admit the page.
    workspace
        .queues
        .lock()
        .unwrap()
        .entry(0)
        .or_default()
        .push(InboundGovernance::AccessRequest {
            workspace: workspace_id,
            request: Box::new(request),
        });
    for _ in 0..8 {
        now = now.checked_add(Duration::from_millis(250)).unwrap();
        host.pump(&host_link, now, 1_000);
    }

    // The page follows the Welcome with no governance at all, and its desktop
    // opens with the workspace's keys.
    page.pump(&page_link, now, 1_000);
    assert!(matches!(
        page.access(workspace_id, now),
        KeyAccess::Ready(_)
    ));
}
