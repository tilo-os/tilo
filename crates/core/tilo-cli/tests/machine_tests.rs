use super::*;

/// Without a session there is nowhere for a plan to go: the machines live in
/// the person's Servers workspace, which the running session holds, so `tilo
/// machine` says it needs one and leaves nothing behind on this computer.
#[test]
fn planning_a_machine_needs_the_session_that_holds_the_workspace() {
    let scratch = tempfile::tempdir().unwrap();
    let identity = scratch.path().join("config/tilo/identity");
    let tilo = |args: &[&str]| {
        let out = Command::new(tilo_test_env::package_binary!("tilo"))
            .args(args)
            .env("HOME", scratch.path())
            .env("XDG_CONFIG_HOME", scratch.path().join("config"))
            .env("TILO_IDENTITY_ROOT", scratch.path().join("identity-root"))
            .env("TILO_IDENTITY_FILE_SECRETS", "1")
            // No session answers here: `XDG_RUNTIME_DIR` names nothing.
            .env("XDG_RUNTIME_DIR", scratch.path().join("runtime"))
            .env_remove("TILO_TEST_CONTROL")
            .output_locked()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    };
    let (logged_in, _, _) = tilo(&["login", "--new-identity", "--name", "Ada"]);
    assert!(logged_in, "the identity the plan is minted under");

    for args in [
        vec!["machine", "add", "--name", "cupboard"],
        vec!["machine", "list"],
        vec!["machine", "show", "0123456789abcdef0123456789abcdef"],
    ] {
        let (done, said, told) = tilo(&args);
        assert!(!done, "tilo {args:?} does not pretend it worked");
        let both = format!("{said}{told}");
        assert!(
            both.to_lowercase().contains("session"),
            "tilo {args:?} says a session is needed: {both}"
        );
    }
    assert!(
        !identity.join("install-plans").exists(),
        "no plan or token file is left behind"
    );
}
