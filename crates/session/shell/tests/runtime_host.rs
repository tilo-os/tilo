//! The runtime must own a profile exclusively and preserve workspace identity.

#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the session and broker handles are held for the whole scenario on purpose"
)]
fn profile_is_exclusive_and_workspace_survives_restart() {
    // Execute in a separate process: identity discovery uses process environment.
    if std::env::var_os("TILO_RUNTIME_HOST_TEST_CHILD").is_none() {
        let root = tempfile::tempdir().unwrap();
        // Outside the profile root: the child owns that directory.
        let log_dir = tempfile::tempdir().unwrap();
        let log = log_dir.path().join("child.stdout");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_host_tests::profile_is_exclusive_and_workspace_survives_restart",
                "--nocapture",
            ])
            .env("TILO_RUNTIME_HOST_TEST_CHILD", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("TILO_IDENTITY_ROOT", root.path().join("identity"))
            // A file, not a pipe: a lingering broker descendant cannot block the read.
            .stdout(std::fs::File::create(&log).unwrap())
            .status()
            .unwrap();
        let stdout = std::fs::read_to_string(&log).unwrap();
        assert!(status.success(), "{stdout}");
        // libtest exits 0 when `--exact` selects nothing; require the child ran.
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "the child ran no test: {stdout}"
        );
        return;
    }
    let root = std::path::PathBuf::from(std::env::var_os("TILO_RUNTIME_HOST_TEST_CHILD").unwrap());
    let session = crate::runtime_host::Session::start(&root).unwrap();
    let room_id = session.room.clone();
    assert!(session.broker.capnp_socket().exists());
    assert!(crate::runtime_host::Session::start(&root).is_err());
    let invocation = tilo_actions::InvocationId::new();
    let membership = session.workspace_tree();
    let workspace_id = membership.read().desktop.workspaces[0].id;
    assert!(membership.write().append_workspace_effect_receipt(
        workspace_id,
        invocation,
        "agent:Scribe",
        tilo_actions::ActionContext::for_resource("document:brief", "document", "Release brief",),
        tilo_actions::EffectReceiptStatus::Failed {
            code: "controlled_denial".into(),
        },
    ));
    drop(membership);
    drop(session);
    let reopened = crate::runtime_host::Session::start(&root).unwrap();
    assert_eq!(reopened.room, room_id);
    let membership = reopened.workspace_tree();
    let tree = membership.read();
    let receipts = &tree.desktop.workspaces[0].effect_receipts;
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].invocation, invocation);
    assert_eq!(receipts[0].actor, "agent:Scribe");
    assert_eq!(
        receipts[0].status,
        tilo_actions::EffectReceiptStatus::Failed {
            code: "controlled_denial".into(),
        }
    );
    drop(tree);
}
