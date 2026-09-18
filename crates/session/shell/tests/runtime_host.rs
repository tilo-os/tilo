//! The runtime must own a profile exclusively and preserve workspace identity.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

#[test]
fn profile_is_exclusive_and_workspace_survives_restart() {
    // Execute in a separate process: identity discovery uses process environment.
    if std::env::var_os("QOS_RUNTIME_HOST_TEST_CHILD").is_none() {
        let root = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_host_tests::profile_is_exclusive_and_workspace_survives_restart",
                "--nocapture",
            ])
            .env("QOS_RUNTIME_HOST_TEST_CHILD", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("TILO_IDENTITY_ROOT", root.path().join("identity"))
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let root = std::path::PathBuf::from(std::env::var_os("QOS_RUNTIME_HOST_TEST_CHILD").unwrap());
    let session = crate::runtime_host::Session::start(&root).unwrap();
    let room = session.room.clone();
    assert!(session.broker.capnp_socket().exists());
    assert!(crate::runtime_host::Session::start(&root).is_err());
    let invocation = tilo_actions::InvocationId::new();
    let membership = session.workspace_tree();
    let workspace_id = membership.read().workspaces[0].id;
    assert!(membership.write().append_workspace_effect_receipt(
        workspace_id,
        invocation,
        "agent:Scribe",
        tilo_actions::ActionContext::for_resource(
            "Publish",
            "document:brief",
            "document",
            "Release brief",
        ),
        tilo_actions::EffectReceiptStatus::Failed {
            code: "controlled_denial".into(),
        },
    ));
    drop(membership);
    drop(session);
    let reopened = crate::runtime_host::Session::start(&root).unwrap();
    assert_eq!(reopened.room, room);
    let membership = reopened.workspace_tree();
    let tree = membership.read();
    let receipts = &tree.workspaces[0].effect_receipts;
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].invocation, invocation);
    assert_eq!(receipts[0].actor, "agent:Scribe");
    assert_eq!(
        receipts[0].status,
        tilo_actions::EffectReceiptStatus::Failed {
            code: "controlled_denial".into(),
        }
    );
}
