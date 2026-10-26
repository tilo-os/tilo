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

/// An agent's effect receipt, recorded in the session's hosted workspace,
/// survives a restart at either end of its way to the room's history: still
/// waiting in the outbox for the session's own host, and once the host
/// committed it and the outbox let it go.
#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "each session is held for the whole of its step on purpose"
)]
fn an_agent_receipt_survives_a_restart_before_and_after_its_host_commits_it() {
    const TEST: &str = "runtime_host_tests::an_agent_receipt_survives_a_restart_before_and_after_its_host_commits_it";
    const CHILD: &str = "TILO_RUNTIME_HOST_RECEIPT_TEST_CHILD";
    // Execute in a separate process: identity discovery uses process environment.
    if std::env::var_os(CHILD).is_none() {
        let root = tempfile::tempdir().unwrap();
        let log_dir = tempfile::tempdir().unwrap();
        let log = log_dir.path().join("child.stdout");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env("TILO_IDENTITY_ROOT", root.path().join("identity"))
            .stdout(std::fs::File::create(&log).unwrap())
            .status()
            .unwrap();
        let stdout = std::fs::read_to_string(&log).unwrap();
        assert!(status.success(), "{stdout}");
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "the child ran no test: {stdout}"
        );
        return;
    }
    let root = std::path::PathBuf::from(std::env::var_os(CHILD).unwrap());
    let record = |session: &crate::runtime_host::Session| {
        let invocation = tilo_actions::InvocationId::new();
        let tree = session.workspace_tree();
        let workspace = tree.read().desktop.workspaces[0].id;
        assert!(tree.write().append_workspace_effect_receipt(
            workspace,
            invocation,
            "agent:Scribe",
            tilo_actions::ActionContext::for_resource("document:brief", "document", "Brief"),
            tilo_actions::EffectReceiptStatus::Succeeded,
        ));
        invocation
    };
    let held = |session: &crate::runtime_host::Session| {
        let tree = session.workspace_tree();
        let tree = tree.read();
        assert!(
            tree.governance_refusals.is_empty(),
            "the session's own host refused an effect of its agent: {:?}",
            tree.governance_refusals
        );
        tree.desktop.workspaces[0]
            .effect_receipts
            .iter()
            .map(|receipt| receipt.invocation)
            .collect::<Vec<_>>()
    };

    // Recorded and restarted at once: the outbox still holds it.
    let session = crate::runtime_host::Session::start(&root).unwrap();
    let waiting = record(&session);
    drop(session);
    let session = crate::runtime_host::Session::start(&root).unwrap();
    assert_eq!(held(&session), [waiting], "a waiting receipt came back");

    // Recorded, committed by the host and let go by the outbox; restarted
    // right after.
    let committed = record(&session);
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        let settled = {
            let tree = session.workspace_tree();
            let tree = tree.read();
            tree.room_outbox.is_empty()
                && tree.room_docs.values().any(|document| {
                    let receipts =
                        tilo_desktop_model::projection::workspace_effect_receipts_of(document);
                    [waiting, committed]
                        .iter()
                        .all(|invocation| receipts.iter().any(|r| r.invocation == *invocation))
                })
        };
        if settled {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the session's own host never committed the receipts: {:?}",
            held(&session)
        );
        // The session's network turns when woken, as by any change.
        (session.network().loop_waker())();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(session);
    let session = crate::runtime_host::Session::start(&root).unwrap();
    let mut receipts = held(&session);
    receipts.sort_by_key(|invocation| invocation.0);
    let mut expected = vec![waiting, committed];
    expected.sort_by_key(|invocation| invocation.0);
    assert_eq!(receipts, expected, "committed receipts came back");
    assert!(session.workspace_tree().read().room_outbox.is_empty());
}
