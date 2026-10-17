//! Keep the full Python regression/oracle suites, but let Cargo provide every
//! native executable. No suite depends on a parallel build's target/debug files.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

fn suite(script: &str, args: &[&str]) -> std::io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    // The staging oracle's injection seam deliberately refuses uid 0. Map the
    // current owner to a non-root namespace uid, keeping build and scratch
    // files accessible without chmod/chown of shared runner directories.
    let mut command = if script == "os/tests/oracles/staging_handoff_test.py"
        && rustix::process::geteuid().is_root()
    {
        let mut command = Command::new("unshare");
        command.args(["--user", "--map-user=19991", "--map-group=19992", "python3"]);
        command
    } else {
        Command::new("python3")
    };
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
