//! The recovery guard must reject before any production identity or transport
//! startup can create persistent state. These tests invoke the shipped CLI.

use std::process::Command;
use tilo_test_env::SpawnUnderEnvLock as _;

tilo_test_env::scenario! {
    fn recovery_and_status_launches_create_nothing;

    phase recovery_only_launch_does_not_create_a_workspace_or_identity;
    {
        let dir = tempfile::tempdir().expect("isolated fixture");
        let cache = dir.path().join("cache");
        let mount = dir.path().join("mount");
        let identity = dir.path().join("identity");
        let output = Command::new(tilo_test_env::package_binary!("fs"))
            .env("TILO_IDENTITY_ROOT", &identity)
            .args(["serve", "--require-existing-workspace", "--cache-dir"])
            .arg(&cache)
            .arg("--mount-point")
            .arg(&mount)
            .output_locked()
            .expect("run production Files CLI");
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("no saved Files tree"));
        assert!(!cache.exists());
        assert!(!mount.exists());
        assert!(!identity.exists());
    }

    phase status_is_read_only_and_does_not_require_identity;
    {
        let dir = tempfile::tempdir().expect("isolated fixture");
        let cache = dir.path().join("cache");
        let output = Command::new(tilo_test_env::package_binary!("fs"))
            .env("TILO_IDENTITY_ROOT", dir.path().join("identity"))
            .args(["workspace-status", "--cache-dir"])
            .arg(&cache)
            .output_locked()
            .expect("run production Files CLI");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let status: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("public status JSON");
        assert_eq!(status["can_resume_existing"], false);
        assert_eq!(status["workspace_service"], false);
        assert_eq!(status["saved_namespace"], false);
        assert!(!cache.exists());
        assert!(!dir.path().join("identity").exists());
    }
}

struct Profile {
    dir: tempfile::TempDir,
}

impl Profile {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("isolated fixture"),
        }
    }

    fn path(&self, relative: &str) -> std::path::PathBuf {
        self.dir.path().join(relative)
    }

    fn cache(&self) -> std::path::PathBuf {
        self.path("state/tilo/files")
    }

    fn save_namespace(&self) {
        std::fs::create_dir_all(self.cache().join("namespace")).expect("create kept tree");
        std::fs::write(
            self.cache().join("namespace/0.automerge"),
            b"retained namespace",
        )
        .expect("write a kept document");
    }

    /// A certified device of a person at `identity`, as a machine that joined
    /// leaves it: the production check the launch runs reads these.
    fn identity(&self) {
        Self::identity_at(&self.path("identity"));
    }

    fn identity_at(root: &std::path::Path) {
        use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
        let root_key = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
        let paths = tilo_device_identity::IdentityPaths::under(root);
        paths.ensure_directories().expect("identity directories");
        let person = root_key.verifying_key().to_bytes();
        tilo_device_identity::persist::write_public_key(&paths.user_pubkey, &person)
            .expect("write person");
        let mut cert = DeviceCert {
            user_public_key: person,
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "fixture".into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root_key).expect("sign the device");
        tilo_device_identity::persist::write_device_cert(
            &paths.device_cert_for_user(&person),
            &cert,
        )
        .expect("write the certificate");
    }

    /// The launch's runtime directory, as the session gives it: this account's own.
    fn runtime(&self) -> std::path::PathBuf {
        use std::os::unix::fs::DirBuilderExt as _;
        let run = self.path("run");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&run)
            .expect("the runtime directory");
        run
    }

    /// What a launch left in its runtime directory beside its device daemon's sockets.
    fn made_in_runtime(&self) -> Vec<std::path::PathBuf> {
        let run = self.path("run");
        let daemon = std::fs::read_dir(run.join("tilo"))
            .into_iter()
            .flatten()
            .map(|entry| entry.expect("runtime entry").path())
            .filter(|path| {
                !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("profile-"))
            });
        std::fs::read_dir(&run)
            .expect("the runtime directory")
            .map(|entry| entry.expect("runtime entry").path())
            .filter(|path| *path != run.join("tilo"))
            .chain(daemon)
            .collect()
    }

    fn launch(&self, overrides: &[(&str, &str)]) -> std::process::Output {
        // The launch asks its device's daemon where every client of the profile does: among this
        // account's sockets in its runtime directory.
        let sockets = {
            let mut scoped = tilo_test_env::ScopedEnv::new();
            scoped.set("XDG_RUNTIME_DIR", self.runtime());
            tilo_device_identity::login::daemon::profile_sockets(
                self.dir.path(),
                &self.path("daemon"),
            )
            .expect("the daemon's sockets")
        };
        let daemon = DeviceDaemon::start(tilo_device_identity::login::daemon::native_in(&sockets));
        let mut command = Command::new(tilo_test_env::package_binary!("fs"));
        command
            .args(["serve-primary", "--dry-run"])
            .env("HOME", self.dir.path())
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("TILO_IDENTITY_ROOT", self.path("identity"))
            .env("TILO_RUNTIME_PROFILE", self.path("daemon"));
        for (name, value) in overrides {
            command.env(name, value);
        }
        let output = command.output_locked().expect("run production Files CLI");
        drop(daemon);
        output
    }
}

/// The device's daemon stand-in a launch reaches: every `info` call is answered with the
/// fixture device's public key, and it stops when dropped.
struct DeviceDaemon {
    socket: std::path::PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DeviceDaemon {
    fn start(socket: std::path::PathBuf) -> Self {
        use std::io::{BufRead as _, Write as _};
        let device = ed25519_dalek::SigningKey::from_bytes(&[52; 32])
            .verifying_key()
            .to_bytes();
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("daemon socket");
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = std::sync::Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            use std::sync::atomic::Ordering;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                if stopping.load(Ordering::Relaxed) {
                    return;
                }
                let mut line = String::new();
                let _ = std::io::BufReader::new(stream.try_clone().expect("daemon stream"))
                    .read_line(&mut line);
                let answer = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "content": [{"type": "text", "text": ""}],
                        "structuredContent": {
                            "status": "ok",
                            "result": {"device": hex::encode(device)},
                        },
                    },
                });
                let _ = writeln!(stream, "{answer}");
            }
        });
        Self {
            socket,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for DeviceDaemon {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::Relaxed);
        let _ = std::os::unix::net::UnixStream::connect(&self.socket);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
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

tilo_test_env::scenario! {
    fn a_primary_launch_checks_its_identity_before_it_serves;

    phase primary_launch_without_a_saved_namespace_creates_nothing;
    {
        let profile = Profile::new();
        let output = profile.launch(&[]);
        assert_not_ready(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains("no saved Files tree"));
        assert!(!profile.path("state").exists());
        assert_eq!(profile.made_in_runtime(), Vec::<std::path::PathBuf>::new());
        assert!(!profile.path("identity").exists());
    }

    phase primary_launch_resumes_from_the_workspace_service_without_a_kept_tree;
    {
        let profile = Profile::new();
        profile.identity();
        // The device's daemon serves the trees at `files_tree_socket` of the account: among its
        // sockets, `$XDG_RUNTIME_DIR/tilo` ([`tilo_machine::daemon_sockets`]).
        {
            use std::os::unix::fs::DirBuilderExt as _;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(profile.path("run/tilo"))
                .expect("create the daemon's sockets dir");
        }
        let _service =
            std::os::unix::net::UnixListener::bind(profile.path("run/tilo/files-tree.sock"))
                .expect("stand in for the workspace service");
        let output = profile.launch(&[]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !profile.cache().join("namespace").exists(),
            "the tree arrives with the first mount, from the workspace service"
        );
    }

    phase primary_launch_refused_by_the_identity_check_never_mounts;
    {
        let profile = Profile::new();
        profile.save_namespace();
        let output = profile.launch(&[]);
        assert_not_ready(&output);
        assert!(!profile.cache().join("mount").exists());
        assert_eq!(profile.made_in_runtime(), Vec::<std::path::PathBuf>::new());
        assert_eq!(
            std::fs::read(profile.cache().join("namespace/0.automerge")).expect("namespace kept"),
            b"retained namespace"
        );
    }

    phase primary_launch_refuses_a_kept_tree_that_is_not_a_directory;
    {
        let profile = Profile::new();
        std::fs::create_dir_all(profile.cache()).expect("create cache");
        std::fs::write(profile.path("elsewhere"), b"namespace").expect("write target");
        std::os::unix::fs::symlink(profile.path("elsewhere"), profile.cache().join("namespace"))
            .expect("symlink the kept tree");
        assert_not_ready(&profile.launch(&[]));
        assert!(!profile.cache().join("mount").exists());
        assert_eq!(profile.made_in_runtime(), Vec::<std::path::PathBuf>::new());
    }

    phase primary_launch_rejects_relative_paths;
    {
        let profile = Profile::new();
        profile.save_namespace();
        let output = profile.launch(&[("XDG_STATE_HOME", "relative")]);
        assert_not_ready(&output);
        assert!(String::from_utf8_lossy(&output.stderr).contains("must be absolute"));
    }

    phase primary_launch_gives_the_daemon_the_paths_the_identity_check_saw;
    {
        use std::os::unix::fs::PermissionsExt as _;
        let profile = Profile::new();
        profile.identity();
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
            std::fs::canonicalize(tilo_test_env::package_binary!("fs")).expect("fs path"),
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
                "--backend".into(),
                "fuse".into(),
                "--storage-sock".into(),
                text(profile.path("run/tilo/files-storage/storage.sock")),
                "--blob-sock".into(),
                text(run.join("fs-blobs.sock")),
                "--workspace-key-socket".into(),
                text(profile.path("run/tilo/services/identity.sock")),
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
            std::fs::read_dir(profile.path("identity/device/certs"))
                .unwrap()
                .next()
                .is_some(),
            "the launch reads the identity it was given and mints none"
        );
        let defaults = Profile::new();
        Profile::identity_at(&defaults.path(".local/share/tilo"));
        std::fs::create_dir_all(defaults.path(".local/state/tilo/files")).expect("create cache");
        std::fs::create_dir_all(defaults.path(".local/state/tilo/files/namespace"))
            .expect("create kept tree");
        std::fs::write(
            defaults.path(".local/state/tilo/files/namespace/0.automerge"),
            b"saved",
        )
        .expect("write a kept document");
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
        let argv: Vec<String> = serde_json::from_slice(&output.stdout).expect("argv JSON");
        assert_eq!(
            argv[argv.iter().position(|a| a == "--acl").expect("acl") + 1],
            text(defaults.path(".config/tilo/fs-capabilities.json"))
        );
    }
}
