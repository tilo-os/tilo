//! The recovery guard must reject before any production identity or transport
//! startup can create persistent state. These tests invoke the shipped CLI.

#![allow(
    clippy::expect_used,
    reason = "in a test fixture helper this is the assertion, not a hazard"
)]
use std::process::Command;

fn fs_binary() -> std::path::PathBuf {
    // Archived nextest runs relocate the production binary away from Cargo's build path.
    std::env::var_os("NEXTEST_BIN_EXE_fs")
        .map_or_else(|| env!("CARGO_BIN_EXE_fs").into(), Into::into)
}

#[test]
fn recovery_only_launch_does_not_create_a_workspace_or_identity() {
    let dir = tempfile::tempdir().expect("isolated fixture");
    let cache = dir.path().join("cache");
    let mount = dir.path().join("mount");
    let identity = dir.path().join("identity");
    let output = Command::new(fs_binary())
        .env("TILO_IDENTITY_ROOT", &identity)
        .args(["serve", "--require-existing-workspace", "--cache-dir"])
        .arg(&cache)
        .arg("--mount-point")
        .arg(&mount)
        .output()
        .expect("run production Files CLI");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no saved Files namespace"));
    assert!(!cache.exists());
    assert!(!mount.exists());
    assert!(!identity.exists());
}

#[test]
fn status_is_read_only_and_does_not_require_identity() {
    let dir = tempfile::tempdir().expect("isolated fixture");
    let cache = dir.path().join("cache");
    let output = Command::new(fs_binary())
        .env("TILO_IDENTITY_ROOT", dir.path().join("identity"))
        .args(["workspace-status", "--cache-dir"])
        .arg(&cache)
        .output()
        .expect("run production Files CLI");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let status: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("public status JSON");
    assert_eq!(status["can_resume_existing"], false);
    assert_eq!(status["saved_namespace"], false);
    assert!(!cache.exists());
    assert!(!dir.path().join("identity").exists());
}

/// An isolated XDG profile plus a stand-in for `tilo-relay-admin` that
/// records its argv and prints readiness output the service must not expose.
struct Profile {
    dir: tempfile::TempDir,
    admin: std::path::PathBuf,
}

impl Profile {
    fn new(admin_exit: u8) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("isolated fixture");
        let admin = dir.path().join("admin");
        std::fs::write(
            &admin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$0.argv\"\necho readiness-stdout-must-stay-private\nexit {admin_exit}\n"
            ),
        )
        .expect("write admin stand-in");
        std::fs::set_permissions(&admin, std::fs::Permissions::from_mode(0o755))
            .expect("make admin stand-in executable");
        Self { dir, admin }
    }

    fn path(&self, relative: &str) -> std::path::PathBuf {
        self.dir.path().join(relative)
    }

    fn cache(&self) -> std::path::PathBuf {
        self.path("state/tilo/files")
    }

    fn save_namespace(&self) {
        std::fs::create_dir_all(self.cache()).expect("create cache");
        std::fs::write(self.cache().join("namespace.bin"), b"retained namespace")
            .expect("write namespace");
    }

    fn admin_argv(&self) -> Option<Vec<String>> {
        let recorded = std::fs::read_to_string(self.dir.path().join("admin.argv")).ok()?;
        Some(recorded.lines().map(str::to_string).collect())
    }

    fn launch(&self, overrides: &[(&str, &str)]) -> std::process::Output {
        let mut command = Command::new(fs_binary());
        command
            .args(["serve-primary", "--dry-run", "--admin-binary"])
            .arg(&self.admin)
            .env("HOME", self.dir.path())
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("TILO_IDENTITY_ROOT", self.path("identity"));
        for (name, value) in overrides {
            command.env(name, value);
        }
        command.output().expect("run production Files CLI")
    }
}

fn assert_not_ready(output: &std::process::Output) {
    assert_eq!(
        output.status.code(),
        Some(78),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).starts_with("Files service is not ready: "));
}

#[test]
fn primary_launch_without_a_saved_namespace_creates_nothing() {
    let profile = Profile::new(0);
    let output = profile.launch(&[]);
    assert_not_ready(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("no saved Files namespace"));
    assert!(
        profile.admin_argv().is_none(),
        "no identity check without saved state"
    );
    assert!(!profile.path("state").exists());
    assert!(!profile.path("run").exists());
    assert!(!profile.path("identity").exists());
}

#[test]
fn primary_launch_refused_by_the_authority_check_never_mounts() {
    let profile = Profile::new(1);
    profile.save_namespace();
    let output = profile.launch(&[]);
    assert_not_ready(&output);
    assert!(profile.admin_argv().is_some());
    assert!(!profile.cache().join("mount").exists());
    assert!(!profile.path("run").exists());
    assert_eq!(
        std::fs::read(profile.cache().join("namespace.bin")).expect("namespace kept"),
        b"retained namespace"
    );
}

#[test]
fn primary_launch_refuses_a_namespace_that_is_not_a_regular_file() {
    let profile = Profile::new(0);
    std::fs::create_dir_all(profile.cache()).expect("create cache");
    std::fs::write(profile.path("elsewhere"), b"namespace").expect("write target");
    std::os::unix::fs::symlink(
        profile.path("elsewhere"),
        profile.cache().join("namespace.bin"),
    )
    .expect("symlink namespace");
    assert_not_ready(&profile.launch(&[]));
    assert!(!profile.cache().join("mount").exists());
    assert!(!profile.path("run").exists());
}

#[test]
fn primary_launch_rejects_relative_paths() {
    let profile = Profile::new(0);
    profile.save_namespace();
    let output = profile.launch(&[("XDG_STATE_HOME", "relative")]);
    assert_not_ready(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be absolute"));
    assert!(profile.admin_argv().is_none());
}

#[test]
fn primary_launch_gives_the_daemon_the_paths_the_authority_check_saw() {
    use std::os::unix::fs::PermissionsExt as _;
    let profile = Profile::new(0);
    profile.save_namespace();
    let output = profile.launch(&[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 argv");
    assert!(!stdout.contains("readiness-stdout-must-stay-private"));
    assert!(
        !String::from_utf8_lossy(&output.stderr).contains("readiness-stdout-must-stay-private")
    );
    let argv: Vec<String> = serde_json::from_str(&stdout).expect("argv JSON");
    let cache = profile.cache();
    let run = profile.path("run/tilo/files-workspace");
    let text = |path: std::path::PathBuf| path.to_str().expect("UTF-8 fixture").to_string();
    assert_eq!(
        std::fs::canonicalize(&argv[0]).expect("fs path"),
        std::fs::canonicalize(fs_binary()).expect("fs path"),
        "the daemon is this same fs executable"
    );
    assert_eq!(
        argv[1..],
        [
            "serve".to_string(),
            "--require-existing-workspace".into(),
            "--cache-dir".into(),
            text(cache.clone()),
            "--mount-point".into(),
            text(cache.join("mount")),
            "--acl".into(),
            text(profile.path("config/tilo/fs-capabilities.json")),
            "--data-plane".into(),
            "fuse".into(),
            "--storage-sock".into(),
            text(run.join("fs-storage.sock")),
            "--blob-sock".into(),
            text(run.join("fs-blobs.sock")),
        ]
    );
    assert_eq!(
        profile.admin_argv().expect("authority check ran"),
        [
            "files-workspace-check".to_string(),
            "--identity-root".into(),
            text(profile.path("identity")),
            "--cache-dir".into(),
            text(cache.clone()),
        ]
    );
    for dir in [cache.join("mount"), run] {
        let mode = std::fs::metadata(&dir)
            .expect("private dir")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
    }
    assert!(
        !profile.path("identity").exists(),
        "launch never mints identity"
    );
    // Empty XDG variables count as unset and resolve to the same defaults.
    let defaults = Profile::new(0);
    std::fs::create_dir_all(defaults.path(".local/state/tilo/files")).expect("create cache");
    std::fs::write(
        defaults.path(".local/state/tilo/files/namespace.bin"),
        b"saved",
    )
    .expect("write namespace");
    let blank = [
        ("XDG_STATE_HOME", ""),
        ("XDG_DATA_HOME", ""),
        ("XDG_CONFIG_HOME", ""),
        ("TILO_IDENTITY_ROOT", ""),
    ];
    let output = defaults.launch(&blank);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        defaults.admin_argv().expect("authority check ran")[2],
        text(defaults.path(".local/share/tilo"))
    );
    let argv: Vec<String> = serde_json::from_slice(&output.stdout).expect("argv JSON");
    assert_eq!(
        argv[argv.iter().position(|a| a == "--acl").expect("acl") + 1],
        text(defaults.path(".config/tilo/fs-capabilities.json"))
    );
}
