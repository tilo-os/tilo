//! A governed workspace's access file is a projection of its governance: a
//! join is answered once governance has committed it and the session layer
//! admits the device, and the file follows.

use super::*;
use std::collections::HashSet;

/// The session layer's runtime of `workspace_id` on this host.
async fn runtime(
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
) -> crate::workspace::SharedWorkspace {
    workspaces
        .inner
        .host
        .workspaces
        .lock()
        .await
        .get(&workspace_id)
        .cloned()
        .expect("the host serves the workspace")
}

#[tokio::test]
async fn a_join_is_answered_while_its_access_write_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Plans").await.unwrap();
    let hosted = runtime(&workspaces, created.workspace_id).await;
    let root = hosted.lock().await.store.root().to_path_buf();
    let (release, reached) = crate::store::test_barrier::install(&root, "access-write");
    let (guest_key, guest) = guest_device();
    tilo_test_env::within(
        "a join answered while its access write is held",
        entry_join(
            &workspaces,
            &cert,
            &created.workspace,
            (&guest_key, &guest),
            41,
        ),
    )
    .await;
    // The write reached its disk step and stays there, and the device is
    // admitted all the same.
    tilo_test_env::within("the access write to reach its disk step", reached)
        .await
        .unwrap();
    assert!(
        crate::workspace_io::fresh_membership(&hosted)
            .await
            .unwrap()
            .membership
            .contains(&guest.device_public_key)
    );
    drop(release);
    let turns = hosted.lock().await.authorization.clone();
    turns.settled().await;
    let store = hosted.lock().await.store.clone();
    assert!(
        store
            .load_membership()
            .unwrap()
            .contains(&guest.device_public_key),
        "the file follows governance"
    );
    drop(workspaces);
    server.shutdown().await;
}

/// A host stopped after governance committed an admission and before its
/// access reached the file starts again admitting exactly what governance
/// does.
#[tokio::test]
async fn a_restart_between_the_commit_and_the_access_write_admits_what_governance_does() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Plans").await.unwrap();
    let (guest_key, guest) = guest_device();
    entry_join(
        &workspaces,
        &cert,
        &created.workspace,
        (&guest_key, &guest),
        42,
    )
    .await;
    // Another device of the owner, as a browser page signed in as them is.
    let (_, page) = device_cert(
        &SigningKey::from_bytes(&[1; 32]),
        "browser",
        DeviceCapabilities::member(),
    );
    let removed = guest.device_public_key;
    workspaces
        .remove_member(
            &created.workspace,
            &cert,
            RemoveMemberRequest {
                request_id: [43; 32],
                user_public_key: guest.user_public_key,
            },
        )
        .await
        .unwrap();
    let store = runtime(&workspaces, created.workspace_id)
        .await
        .lock()
        .await
        .store
        .clone();
    let listed: HashSet<[u8; 32]> = store.load_membership().unwrap().into_iter().collect();
    assert!(
        !listed.contains(&removed),
        "a removal is written before it is answered"
    );
    // Then governance admits `page`, and the host stops before that reaches
    // the access file.
    {
        let governed = workspaces.workspace(&created.workspace).await.unwrap();
        let mut governed = governed.lock().await;
        let now = SystemTime::now();
        let mut candidate = governed.peer().clone();
        assert!(
            candidate
                .admit_certificate(page.clone(), &workspaces.inner.host.key, now)
                .unwrap()
        );
        let state = governed.collaboration().clone();
        governed
            .commit(candidate, &workspaces.inner.host.key, now, |_, _| Ok(state))
            .unwrap();
    }
    let listed = store.load_membership().unwrap();
    assert!(!listed.contains(&page.device_public_key));
    drop(workspaces);
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let governed: HashSet<[u8; 32]> = {
        let state = workspaces.workspace(&created.workspace).await.unwrap();
        let state = state.lock().await;
        state
            .peer()
            .governance()
            .governed_access(SystemTime::now())
            .unwrap()
            .member_devices
            .into_iter()
            .collect()
    };
    assert!(governed.contains(&page.device_public_key));
    assert!(!governed.contains(&removed));
    let hosted = runtime(&workspaces, created.workspace_id).await;
    let admitted: HashSet<[u8; 32]> = crate::workspace_io::fresh_membership(&hosted)
        .await
        .unwrap()
        .membership
        .devices
        .iter()
        .copied()
        .collect();
    assert_eq!(
        admitted, governed,
        "the restarted host admits what governance does"
    );
    let turns = hosted.lock().await.authorization.clone();
    turns.settled().await;
    let store = hosted.lock().await.store.clone();
    let listed: HashSet<[u8; 32]> = store.load_membership().unwrap().into_iter().collect();
    assert_eq!(listed, governed, "and its file holds it");
    drop(workspaces);
    server.shutdown().await;
}
