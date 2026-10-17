//! Runs the real Linux startup entry point in an isolated test subprocess.
//! Included by linux_main so this can exercise run() without a display or a
//! second, test-only implementation of startup ordering.
#![allow(
    clippy::expect_used,
    clippy::exit,
    reason = "subprocess regression fixture must fail on setup errors and return the startup exit code"
)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::process::Command;
use std::time::Duration;

#[test]
fn firstboot_rejects_settings_symlink_before_host_import() {
    const CHILD: &str = "TILO_FIRSTBOOT_STARTUP_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::process::exit(if super::run() == std::process::ExitCode::SUCCESS {
            0
        } else {
            1
        });
    }

    let root = tempfile::tempdir().expect("private startup fixture");
    let home = root.path().join("home");
    let config = home.join(".config");
    let session = config.join("tilo/session");
    let bin = root.path().join("bin");
    fs::create_dir_all(&session).expect("settings directory");
    fs::create_dir_all(&bin).expect("fake host tools");
    let queried = root.path().join("host-queried");
    let gsettings = bin.join("gsettings");
    // A host value that forces the old unchecked import to write, replacing
    // the symlink with an ordinary copy and hiding it from checked startup.
    fs::write(
        &gsettings,
        b"#!/bin/sh\nprintf queried > \"$HOST_QUERIED\"\nprintf 'false\\n'\n",
    )
    .expect("deterministic host input provider");
    fs::set_permissions(&gsettings, fs::Permissions::from_mode(0o755))
        .expect("executable host tool");
    let target = root.path().join("untrusted.json");
    let original = b"{\"untrusted\":{\"sentinel\":true}}\n";
    fs::write(&target, original).expect("symlink target");
    let copy = session.join("settings.json");
    symlink(&target, &copy).expect("redirected settings copy");

    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command
        .args([
            "--exact",
            "linux_main::firstboot_tests::firstboot_rejects_settings_symlink_before_host_import",
            "--nocapture",
            // libtest consumes these as skip filters; the production argument
            // parser still sees the exact session flags in std::env::args().
            "--skip",
            "--firstboot",
            "--skip",
            "--host-desktop-session",
        ])
        .env_clear()
        .env(CHILD, "1")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_CACHE_HOME", root.path().join("cache"))
        .env("XDG_RUNTIME_DIR", root.path().join("runtime"))
        .env("PATH", &bin)
        .env("HOST_QUERIED", &queried)
        .env("TILO_SHADER_SEED_PREPARED", "1")
        .env("TILO_STARTUP_WARM", "off")
        .env("RUST_LOG", "shell=info");
    let output = bounded_command::run_prepared(command, None, 64 * 1024, Duration::from_secs(10))
        .expect("startup must terminate before entering the compositor");
    assert!(
        !output.status.success(),
        "unsafe settings must refuse startup"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("cannot safely open session settings")
            || stderr.contains("cannot safely open session settings"),
        "must fail at checked settings startup, not an unrelated error: status={}\nstdout: {stdout}\nstderr: {stderr}",
        output.status
    );
    assert_eq!(
        fs::read_link(&copy).expect("copy must remain a symlink"),
        target
    );
    assert_eq!(fs::read(&target).expect("unchanged target"), original);
    assert!(
        !queried.exists(),
        "host import must not run before validation"
    );
    assert!(
        !config.join("tilo/records").exists(),
        "untrusted copy must not seed settings records"
    );
    assert!(
        config.join("tilo-desktop").is_dir(),
        "firstboot setup must still precede checked settings startup"
    );
}
