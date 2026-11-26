//! Another person's computer, as the harness starts this one: its own daemon in a process of
//! its own, its own identity, and the `tilo` commands a person runs there. It joins a workspace
//! by an invitation the owner hands its person, and keeps it as a member copy.

use std::ffi::OsString;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use tilo_test_env::SpawnUnderEnvLock as _;

use super::Host;

/// A second computer of another person: its daemon, its identity, and its `tilo`.
pub struct Second {
    _root: tempfile::TempDir,
    daemon: Child,
    env: Vec<(String, OsString)>,
    log: PathBuf,
    home: PathBuf,
    /// The person, by their id: the hex of their root.
    pub person: String,
    /// The workspace the invitation brought it into.
    pub workspace: String,
}

impl Drop for Second {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

impl Second {
    /// A computer of a new person, `name`, that joins `host`'s workspace with `role` and has the
    /// tilolang apps `apps` and the Rust apps `rust` installed, as a person's computer has them.
    ///
    /// # Panics
    ///
    /// The computer does not start, the invitation is not made or redeemed, or an app does not
    /// install.
    pub async fn joined(
        host: &Host,
        name: &str,
        role: &str,
        apps: &[&str],
        rust: &[(&str, &str)],
    ) -> Self {
        let root = tempfile::Builder::new()
            .prefix("tilo-2-")
            .tempdir_in("/tmp")
            .expect("a directory for the second computer");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let place = |named: &str| {
            let dir = root.path().join(named);
            std::fs::create_dir_all(&dir).unwrap();
            dir
        };
        let (config, data) = (place("c"), place("d"));
        let runtime = place("r");
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
        let sockets = runtime.join("tilo");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&sockets)
            .unwrap();
        let home = place("h");
        let profile = place("p");
        std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env: Vec<(String, OsString)> = vec![
            ("HOME".into(), home.clone().into()),
            ("XDG_CONFIG_HOME".into(), config.into()),
            ("XDG_DATA_HOME".into(), data.clone().into()),
            ("XDG_CACHE_HOME".into(), place("k").into()),
            ("XDG_STATE_HOME".into(), place("s").into()),
            ("XDG_RUNTIME_DIR".into(), runtime.into()),
            ("TILO_IDENTITY_ROOT".into(), place("i").into()),
            (
                tilo_identity::provisioning::FILE_SECRETS_ENV.into(),
                "1".into(),
            ),
            (
                tilo_machine::DAEMON_PROFILE_VAR.into(),
                profile.clone().into(),
            ),
        ];
        // The apps this computer has, as the device's own env names them.
        for (name, value) in super::apps::install(&place("a"), &data, apps) {
            env.push((name.to_owned(), value));
        }
        for (package, bin) in rust {
            super::apps::install_rust(&place("a"), package, bin);
        }
        let log = root.path().join("daemon.log");
        let daemon = Command::new(tilo_test_env::sibling_binary("tilo-daemon", "tilo-runtime"))
            .envs(
                env.iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            )
            .env_remove("TILO_SERVER_CONFIG")
            .env_remove(runtime_bundle::RUNTIME_DIR_ENV)
            .env_remove("TILO_SVC_COLLAB_FD")
            .env_remove("TILO_SVC_IDENTITY_FD")
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(std::fs::File::create(root.path().join("daemon.err")).unwrap())
            .spawn_locked()
            .expect("the second computer's daemon starts");
        let mut second = Self {
            _root: root,
            daemon,
            env,
            log,
            home,
            person: String::new(),
            workspace: String::new(),
        };
        // The person is founded on this computer as a person founds theirs: the first command
        // the daemon answers asks it for one.
        let made = second.tilo_until(&["login", "--new-identity", "--json", "--name", name], 90);
        let answer: serde_json::Value = serde_json::from_str(made.trim()).unwrap_or_else(|error| {
            panic!("`tilo login --new-identity` answers its person ({error}): {made}")
        });
        let person = answer["user_public_key"]
            .as_str()
            .unwrap_or_else(|| panic!("the identity names its key: {made}"))
            .to_owned();
        // The owner invites them into the workspace, and the link carries the invitation their
        // daemon redeems.
        let link = invitation_for(host, role, &person).await;
        second.tilo_until(&["login", &link], 90);
        second.person = person;
        second.workspace = host.workspace().to_owned();
        second
    }

    /// [`Self::run`], asked again until it answers successfully or `within` seconds pass: a
    /// daemon started a moment ago answers its first commands when it serves.
    fn tilo_until(&self, args: &[&str], within: u64) -> String {
        let deadline = Instant::now() + Duration::from_secs(within);
        loop {
            let output = self.run(args);
            if output.status.success() {
                return String::from_utf8_lossy(&output.stdout).into_owned();
            }
            assert!(
                Instant::now() < deadline,
                "`tilo {}` on the second computer: {}\n{}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr),
                std::fs::read_to_string(&self.log).unwrap_or_default(),
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// `tilo` with `args` on this computer, its own env, as the person runs it.
    pub fn run(&self, args: &[&str]) -> Output {
        Command::new(tilo_test_env::sibling_binary("tilo-cli", "tilo"))
            .args(args)
            .envs(
                self.env
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            )
            .env_remove("TILO_SERVER_CONFIG")
            .env_remove(runtime_bundle::RUNTIME_DIR_ENV)
            .env_remove("TILO_SVC_COLLAB_FD")
            .env_remove("TILO_SVC_IDENTITY_FD")
            .stdin(Stdio::null())
            .output_locked()
            .expect("the second computer's tilo runs")
    }

    /// Mount this computer's workspace folder, as `tilo login` mounts it, and answer the folder
    /// it stands at: the files daemon serves the workspace's tree there, and what the person
    /// writes into it is this computer's record write.
    ///
    /// # Panics
    ///
    /// The mount does not come up, or the platform cannot mount a FUSE tree.
    pub fn mount(&self, name: &str) -> PathBuf {
        let point = self.home.join("Tilo").join(name);
        std::fs::create_dir_all(&point).expect("the folder to mount at");
        let cache = self.home.join(".cache").join("tilo-fs");
        std::fs::create_dir_all(&cache).expect("the mount's cache");
        let log = self.home.join("fs.log");
        Command::new(tilo_test_env::sibling_binary("tilo-fs", "fs"))
            .args([
                "serve",
                "--mount-point",
                point.to_str().expect("the mount point is text"),
                "--cache-dir",
                cache.to_str().expect("the cache is text"),
            ])
            .envs(
                self.env
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone())),
            )
            .env("TILO_FS_FUSE_REQUIRED", "1")
            .env_remove("TILO_SERVER_CONFIG")
            .env_remove(runtime_bundle::RUNTIME_DIR_ENV)
            .env_remove("TILO_SVC_COLLAB_FD")
            .env_remove("TILO_SVC_IDENTITY_FD")
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(&log).unwrap())
            .stderr(std::fs::File::create(self.home.join("fs.err")).unwrap())
            .spawn()
            .expect("the files daemon starts");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let mounted = std::fs::read_to_string("/proc/self/mountinfo")
                .unwrap_or_default()
                .lines()
                .any(|line| line.contains(point.to_str().expect("the mount point is text")));
            if mounted {
                return point;
            }
            assert!(
                Instant::now() < deadline,
                "the workspace's folder mounts: {}\n{}",
                std::fs::read_to_string(&log).unwrap_or_default(),
                std::fs::read_to_string(self.home.join("fs.err")).unwrap_or_default(),
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// `tilo do` the action at `path` with `args`, in the workspace this computer joined, as the
    /// person: the app instance runs here and its change is this computer's write.
    ///
    /// # Panics
    ///
    /// The action is refused.
    pub fn do_in(&self, path: &str, args: &[&str]) -> String {
        let workspace = self.workspace.clone();
        let mut argv = vec!["do", "-w", workspace.as_str(), path];
        argv.extend_from_slice(args);
        self.tilo_until(&argv, 60).trim().to_owned()
    }
}

/// The invitation link a person's `tilo login` takes, for `person` (hex) to join the host's
/// workspace with `role`: the owner's governance invitation, as the workspace's entry renders it.
async fn invitation_for(host: &Host, role: &str, person: &str) -> String {
    use tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER;
    use tilo_actions::remote::workspace_governance_address;

    let workspace = host.workspace().to_owned();
    let owner = host.member("its owner's computer", &workspace).await;
    let invited = owner
        .call(
            WORKSPACE_GOVERNANCE_PROVIDER,
            workspace_governance_address(&workspace, "invite"),
            &serde_json::json!({"role": role, "recipient": person}),
        )
        .await
        .expect("the owner invites the person");
    let tilo_actions::InvocationUpdate::Succeeded { output, .. } = &invited else {
        panic!("the invitation is issued: {invited:?}");
    };
    let answered = output.value.clone().unwrap_or_default();
    let signed = hex::decode(
        answered["invitation"]
            .as_str()
            .unwrap_or_else(|| panic!("the invitation is in the answer: {answered}")),
    )
    .expect("the invitation is hex");
    let text = crate::language::files_support::base64(&signed)
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_owned();
    format!("https://entry.example/#/w/{workspace}?invite={text}")
}
