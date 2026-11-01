//! The app sandbox's own recipe ([`Container::build_command`]) starts an
//! app with the system visible and the host's devices passed through.

use shell::security::container::Container;
use tilo_test_env::SpawnUnderEnvLock as _;

/// Run `app_command` in the sandbox the shell starts apps in.
fn run_in_sandbox(app_command: &str) -> std::process::Output {
    let project = tempfile::tempdir().expect("project directory");
    let container = Container::new(project.path().to_str().expect("utf-8 temporary path"));
    container
        .build_command(app_command, "wayland-sandbox-check")
        .output_locked()
        .expect("start bwrap")
}

#[test]
fn an_app_starts_in_the_sandbox_with_the_system_visible() {
    tilo_test_env::require_capability!(
        UserNamespace,
        tilo_test_env::bubblewrap(),
        "the app sandbox starts bubblewrap with namespaces of its own"
    );
    let output = run_in_sandbox("ls /usr/bin/sh");
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("/usr/bin/sh"),
        "the sandboxed app did not run or could not see /usr: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The sandbox must pass device nodes through rather than hiding them.
///
/// A GPU-less machine cannot prove that about `/dev/dri`, so the device under
/// test is whichever one the host has: `/dev/dri` where there is a render
/// node, `/dev/null` (from the sandbox's own `/dev`) otherwise. Either way the
/// assertion runs.
#[test]
fn the_sandbox_passes_the_hosts_render_node_through() {
    tilo_test_env::require_capability!(
        UserNamespace,
        tilo_test_env::bubblewrap(),
        "the app sandbox starts bubblewrap with namespaces of its own"
    );
    let host_has_render_node = std::fs::read_dir("/dev/dri")
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("renderD"));
    let node = if host_has_render_node {
        "/dev/dri"
    } else {
        "/dev/null"
    };
    let output = run_in_sandbox(&format!("ls {node}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{node} is not visible inside the sandbox: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    // Visible is not enough where a real GPU is present: the bind has to
    // carry the render node itself, not just an empty directory.
    assert!(
        !host_has_render_node || stdout.contains("renderD"),
        "sandbox /dev/dri contains no render node: {stdout}"
    );
}
