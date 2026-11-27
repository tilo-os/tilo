//! The device's one daemon, started in this test's process with what the `tilo-runtime` program
//! mounts on it ([`tilo_daemon::capabilities`]), as a computer starts it: the person is founded in
//! an identity store of the test's own, and the daemon serves their workspace.
//!
//! Each test runs in a process of its own, with a home, identity store, runtime directory and
//! profile of its own: the daemon is the device, and a device is one per process. [`run`] starts
//! that process itself.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use tilo_runtime::RuntimeServices as _;
use tilo_runtime::device::services::Daemon;
use tilo_test_env::SpawnUnderEnvLock as _;

/// Names the test the process [`run`] starts runs, and where its profile is.
const OWN: &str = "TILO_DAEMON_TEST";
const PROFILE: &str = "TILO_DAEMON_TEST_PROFILE";

/// The daemon a test runs against.
pub struct Host {
    daemon: Arc<Daemon>,
    workspace: String,
}

impl Host {
    fn start(profile: &Path) -> Self {
        tilo_runtime::device::capabilities::mount(tilo_daemon::capabilities());
        // Founding signs as the device, which only the process that took its key does: this one
        // takes the computer's key for the founding, made here when the computer has none yet,
        // and lets go of it for the daemon it starts next.
        let keys = tilo_identity::provisioning::collaboration_identity_paths();
        tilo_device_identity::persist::load_or_create_device_key(&keys).unwrap();
        let taken = tilo_device_identity::persist::take_device_key(&keys).unwrap();
        tilo_runtime::device::services::found_account_person(
            &keys,
            "",
            tilo_runtime::device::services::this_account_uid(),
        )
        .unwrap();
        drop(taken);
        let daemon = Arc::new(Daemon::start_reporting(profile, &mut |_| {}).unwrap());
        Self {
            // The person's workspace is founded as the daemon starts, in the step that made
            // their root here.
            workspace: daemon
                .workspace()
                .expect("the person's workspace, founded with their root"),
            daemon,
        }
    }

    /// The daemon running.
    #[must_use]
    pub fn daemon(&self) -> Arc<Daemon> {
        Arc::clone(&self.daemon)
    }

    /// The workspace the daemon founded for its person.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }
}

/// Run `body` against a daemon of its own, in a process of its own: the test that calls it
/// passes when `body` returns.
///
/// # Panics
///
/// When the daemon does not start, or `body` fails.
pub fn run<F, Fut>(body: F)
where
    F: FnOnce(Arc<Host>) -> Fut,
    Fut: Future<Output = ()>,
{
    let test = std::thread::current()
        .name()
        .expect("libtest names a test's thread after the test")
        .to_owned();
    if std::env::var(OWN).ok().as_deref() != Some(test.as_str()) {
        own_process(&test);
        return;
    }
    let profile = PathBuf::from(std::env::var_os(PROFILE).expect("the profile is named"));
    let host = Arc::new(Host::start(&profile));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(body(Arc::clone(&host)));
    // The test's tasks go before the daemon, which is dropped on this thread, outside any
    // runtime, as a computer stops it.
    drop(runtime);
    drop(host);
}

/// Run `test` again, alone, in a process whose home, identity and profile are a new
/// directory's: what [`run`] does everywhere but in that process.
fn own_process(test: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    // A short path: the daemon's sockets are named below it.
    let root = tempfile::Builder::new()
        .prefix("tilo-d-")
        .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let place = |name: &str| {
        let dir = root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    // The login's runtime directory, where the daemon binds the sockets this account's own
    // processes reach (the account sign-ins among them): the test's own, never the login's.
    let runtime = place("r");
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    // The device relays through this machine's loopback relay: nothing reaches a relay or a
    // directory out on the internet.
    let (relay, relay_url) = test_server::loopback_relay();
    let log = root.path().join("out");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads", "1"])
        .env(OWN, test)
        .env(PROFILE, place("p"))
        .env("HOME", place("h"))
        .env("XDG_CONFIG_HOME", place("c"))
        .env("XDG_DATA_HOME", place("d"))
        .env("XDG_CACHE_HOME", place("k"))
        .env("XDG_STATE_HOME", place("s"))
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("TILO_IDENTITY_ROOT", place("i"))
        .env(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
        .env(relay, relay_url)
        .env_remove(runtime_bundle::RUNTIME_DIR_ENV)
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        // A daemon here is a person's computer, not a machine configured as a server.
        .env_remove("TILO_SERVER_CONFIG")
        .stdout(std::fs::File::create(&log).unwrap())
        .stderr(Stdio::inherit())
        .status_locked()
        .unwrap();
    let out = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        status.success() && out.contains("1 passed"),
        "{test} on its own daemon: {status}\n{out}"
    );
}
