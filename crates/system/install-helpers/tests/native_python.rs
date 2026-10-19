//! Keep the full Python regression/oracle suites, but let Cargo provide every
//! native executable. No suite depends on a parallel build's target/debug files.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

/// Report a fixture namespace the host cannot create, or fail under
/// `TILO_REQUIRE_USERNS=1`. Returns whether the caller should skip.
fn namespace_unavailable(
    capability: tilo_test_env::Capability,
    probe: std::io::Result<()>,
    reason: &str,
) -> bool {
    match probe {
        Ok(()) => false,
        Err(error) => {
            tilo_test_env::capability_unavailable(capability, format_args!("{reason}: {error}"));
            true
        }
    }
}

fn suite(script: &str, args: &[&str]) -> std::io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    // The staging oracle's injection seam deliberately refuses uid 0. Map the
    // current owner to a non-root namespace uid, keeping build and scratch
    // files accessible without chmod/chown of shared runner directories. A
    // capability-less job sandbox refuses the namespace; the micro-VM check
    // sets TILO_REQUIRE_USERNS=1 and runs it as real root.
    if script == "os/tests/oracles/staging_handoff_test.py"
        && rustix::process::geteuid().is_root()
        && namespace_unavailable(
            tilo_test_env::Capability::UserNamespace,
            tilo_test_env::user_namespace(),
            "the staging oracle refuses uid 0 and needs a user namespace",
        )
    {
        return Ok(());
    }
    // The native seed driver re-enters a private mount namespace itself, and
    // fails closed when it cannot. Skip with that reason where the sandbox
    // refuses namespaces, and require it in the micro-VM lane.
    if script == "os/tests/test_firstboot_home_seeds_native.py"
        && namespace_unavailable(
            tilo_test_env::Capability::UserNamespace,
            tilo_test_env::mount_namespace(),
            "the native seed test needs a private mount namespace",
        )
    {
        return Ok(());
    }
    // Bound each suite so a wedged script fails the lane instead of stalling
    // it; timeout exits 124, which the status check below reports.
    let mut command = Command::new("timeout");
    command.args(["--kill-after=10", "600"]);
    if script == "os/tests/oracles/staging_handoff_test.py" && rustix::process::geteuid().is_root()
    {
        command.arg("unshare");
        command.args(["--user", "--map-user=19991", "--map-group=19992", "python3"]);
    } else {
        command.arg("python3");
    }
    let output = command
        .arg(root.join(script))
        .args(args)
        .env(
            "TILO_PREPARED_EXTRACT_BINARY",
            env!("CARGO_BIN_EXE_staging-handoff-native-extractor"),
        )
        .env(
            "TILO_FIRSTBOOT_HOME_SEEDS_BINARY",
            env!("CARGO_BIN_EXE_install-test-home-seeds"),
        )
        .env(
            "TILO_FIRSTBOOT_FILES_BINARY",
            env!("CARGO_BIN_EXE_install-test-firstboot-files"),
        )
        .output()?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "{script}: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

#[test]
fn firstboot_transaction() -> std::io::Result<()> {
    suite("os/tests/test_firstboot_transaction.py", &[])
}

#[test]
fn credential_transport() -> std::io::Result<()> {
    suite("os/tests/test_firstboot_credential_transport.py", &[])
}

#[test]
fn installed_runtime() -> std::io::Result<()> {
    suite("os/tests/test_installer_firstboot_runtime.py", &[])
}

#[test]
fn prepared_binding() -> std::io::Result<()> {
    suite("os/tests/test_prepared_first_use_binding.py", &[])
}

#[test]
fn native_home_seeds() -> std::io::Result<()> {
    suite(
        "os/tests/test_firstboot_home_seeds_native.py",
        &["--binary", env!("CARGO_BIN_EXE_install-test-home-seeds")],
    )
}

#[test]
fn staging_handoff_oracle() -> std::io::Result<()> {
    suite("os/tests/oracles/staging_handoff_test.py", &[])
}
