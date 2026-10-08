//! A real helper in a root with no host /usr or system libraries. This
//! observes the dynamic loader failure that an argv-only test cannot see.
use std::path::{Path, PathBuf};

fn plan(root: &Path) -> std::io::Result<ush::guest::GuestPlan> {
    let runtime_bin = PathBuf::from(env!("CARGO_BIN_EXE_tilo-ush"));
    let helper_runtime = ush::helper_runtime::HelperRuntime::resolve(&runtime_bin)?;
    Ok(ush::guest::GuestPlan {
        images: Vec::new(),
        host_root: root.join("unexposed-host"),
        session_dir: root.join("session"),
        var_dir: root.join("var"),
        guest_home: root.join("home"),
        user: ush::guest::GuestUser::current(),
        environment: "loader test".into(),
        masked: Vec::new(),
        trusted_dirs: Vec::new(),
        readable_dirs: Vec::new(),
        grant_root: None,
        runtime_bin,
        helper_runtime,
        legacy_commands: Vec::new(),
        broker_socket: root.join("not-a-broker"),
        seccomp_blob: root.join("unused"),
        command: Vec::new(),
        shell: PathBuf::from("/usr/bin/sh"),
        term: "xterm".into(),
        env: Vec::new(),
        resolver: None,
        editor: None,
    })
}

#[test]
fn dynamic_helper_starts_from_private_closure_without_host_usr_and_keeps_shim_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let mut plan = plan(dir.path()).unwrap();
    std::fs::write(&plan.broker_socket, "").unwrap();
    plan.prepare().unwrap();
    assert!(
        plan.helper_runtime.is_some(),
        "this fixture must exercise a dynamic helper"
    );
    let helper = plan.helper_runtime.as_ref().unwrap();
    plan.command = helper.entry();
    plan.command.extend(["guest-init".into(), "--".into()]);
    let mut command = helper.entry();
    // The host loader must retain the shim's name rather than treating its
    // helper executable's filename as the application command.
    command[4] = "app".into();
    command.push("--help".into());
    plan.command.extend(command);
    let argv = ush::guest::build_argv(&plan, None);
    assert!(!argv.iter().any(|arg| arg == "LD_LIBRARY_PATH"));
    assert!(
        !argv
            .windows(3)
            .any(|args| args[0] == "--ro-bind" && args[1] == "/usr")
    );
    let output = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "private helper failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("app"), "shim help is observable: {stdout}");
    assert!(
        !stdout.contains("tilo-ush —"),
        "argv[0] must dispatch the app shim"
    );
}
