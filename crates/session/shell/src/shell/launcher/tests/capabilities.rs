//! The capabilities a workspace publishes reach only the app it launches:
//! filtered to that app, bound read-only, and revoked with the file.

use super::*;

#[test]
#[serial_test::serial(process_env)]
fn published_capabilities_are_filtered_bound_read_only_and_revoked() {
    fn published_capabilities_are_filtered_to_the_launching_app() {
        set_workspace_capabilities(LaunchGrants {
            authors: vec!["so.tilo.books".into(), "so.tilo.other".into()],
            sandbox: vec![
                fs_grant("so.tilo.books", "/var/workspace/books", Access::Read),
                fs_grant("so.tilo.other", "/var/workspace/secrets", Access::Write),
                // A person's share is a capability in the same list but
                // not an app grant, and must not travel.
                CapOverride::grant(
                    Subject::person("alice"),
                    Scope::FsPath("/var/workspace/private".into()),
                    Access::Write,
                ),
            ],
        });
        let mine = capabilities_for_app("so.tilo.books");
        clear_workspace_capabilities();

        assert_eq!(
            mine.sandbox.len(),
            1,
            "only this app's grant travels: {mine:?}"
        );
        assert_eq!(
            mine.sandbox[0].scope,
            Scope::FsPath("/var/workspace/books".into())
        );
        assert_eq!(mine.sandbox[0].subject, Subject::app("so.tilo.books"));
        assert_eq!(
            mine.authors,
            vec!["so.tilo.books".to_owned()],
            "only this app's authorship travels"
        );
    }

    fn nothing_published_means_nothing_granted() {
        clear_workspace_capabilities();
        assert!(
            capabilities_for_app("so.tilo.books").is_empty(),
            "with no workspace published, an app is granted nothing"
        );
    }

    /// The whole handoff end to end: publish a capability, write the
    /// file a launch would write, then read it back with `tilo-launcher`'s
    /// **own** loader and planner and check the bind that comes out.
    fn a_granted_path_survives_the_handoff_and_becomes_a_read_only_bind() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", tmp.path());
        set_workspace_capabilities(sandbox(vec![
            fs_grant("so.tilo.books", "/var/workspace/books", Access::Read),
            fs_grant("so.tilo.other", "/var/workspace/secrets", Access::Write),
        ]));

        let caps = capabilities_for_app("so.tilo.books");
        let path = write_capabilities_file("so.tilo.books", &caps)
            .expect("write capability file")
            .expect("a granted app gets a file");

        // readable only by the user: this file says what one app may reach
        let mode = std::fs::metadata(path.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "capability file must not be world-readable");

        let records = launcher::capabilities::load_capability_file(path.path());
        let plan = launcher::capabilities::CapabilityPlan::for_app(
            "so.tilo.books",
            &records.sandbox,
            Some(tilo_clock::unix_now()),
        );

        clear_workspace_capabilities();
        env.remove("XDG_RUNTIME_DIR");

        assert_eq!(
            plan.ro_paths,
            vec![PathBuf::from("/var/workspace/books")],
            "the granted path arrives at the launcher as a read-only bind: {plan:?}"
        );
        assert!(
            plan.rw_paths.is_empty(),
            "a Read grant must not cross as writable: {plan:?}"
        );
        assert!(
            !plan.allow_network,
            "no network capability was granted: {plan:?}"
        );
    }

    /// Every launch has its own record. A reduced-policy launch cannot
    /// reuse the previous file; the supervisor removes that file on exit.
    fn revoking_the_last_capability_removes_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", tmp.path());

        let granted = sandbox(vec![fs_grant(
            "so.tilo.books",
            "/var/workspace/books",
            Access::Read,
        )]);
        let path = write_capabilities_file("so.tilo.books", &granted)
            .unwrap()
            .expect("file written while granted");
        let old_path = path.path().to_path_buf();
        assert!(old_path.exists());

        let after_revoke =
            write_capabilities_file("so.tilo.books", &LaunchGrants::default()).unwrap();
        assert!(
            after_revoke.is_none(),
            "revoked relaunch must have no capability file"
        );
        let other = write_capabilities_file(
            "so.tilo.books",
            &sandbox(vec![fs_grant(
                "so.tilo.books",
                "/another-room",
                Access::Write,
            )]),
        )
        .unwrap()
        .unwrap();
        assert_ne!(old_path, other.path());
        assert_eq!(
            launcher::capabilities::load_capability_file(&old_path),
            granted,
            "a concurrent same-package launch must not overwrite this snapshot"
        );
        assert_eq!(
            launcher::capabilities::load_capability_file(other.path()).sandbox[0].scope,
            Scope::FsPath("/another-room".into())
        );
        drop(path);
        assert!(
            !old_path.exists(),
            "the ended launch's snapshot must be removed"
        );
        let other_path = other.path().to_path_buf();
        assert!(other_path.exists(), "another launch's snapshot must remain");
        drop(other);
        assert!(
            !other_path.exists(),
            "each supervisor removes only its own snapshot"
        );
        env.remove("XDG_RUNTIME_DIR");
    }

    published_capabilities_are_filtered_to_the_launching_app();
    nothing_published_means_nothing_granted();
    a_granted_path_survives_the_handoff_and_becomes_a_read_only_bind();
    revoking_the_last_capability_removes_the_file();
}
