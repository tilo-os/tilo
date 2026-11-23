//! Answering a governed workspace by its id, however many the host serves.
#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;

/// Every governed workspace answers for its own id, whatever else this host
/// governs: the lookup is by id, not by looking through the rest. A
/// workspace deleted here answers nothing by its id, and the ones left still
/// answer theirs.
#[tokio::test]
async fn a_host_answers_each_governed_workspace_by_its_own_id() {
    use tilo_actions::remote::Caller;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    let caller = Caller::authenticated(member.user_public_key, member.device_public_key);
    let mut made = Vec::new();
    for index in 0..24 {
        let created = workspaces
            .create(&format!("Applicant {index}"))
            .await
            .unwrap();
        admit_view_reader(&workspaces, &created.workspace, &key, &member).await;
        made.push(created);
    }
    for created in &made {
        let policy = workspaces
            .resource_view_policy(created.workspace_id, &caller, "so.tilo.chat", "")
            .await
            .expect("the workspace answered for its own id");
        assert_eq!(policy.workspace, created.workspace);
    }
    let gone = made.pop().unwrap();
    workspaces.delete(&gone.workspace).await.unwrap();
    assert!(
        workspaces
            .resource_view_policy(gone.workspace_id, &caller, "so.tilo.chat", "")
            .await
            .is_none(),
        "a workspace deleted here answers nothing by its id"
    );
    for created in &made {
        let policy = workspaces
            .resource_view_policy(created.workspace_id, &caller, "so.tilo.chat", "")
            .await
            .expect("the workspaces left still answer theirs");
        assert_eq!(policy.workspace, created.workspace);
    }
    drop(workspaces);
    server.shutdown().await;
}
