//! A device's one daemon, started in this test's process, and the people, keys and devices that
//! reach it the ways production reaches it.
//!
//! [`run`] starts the daemon (`tilo-runtime`'s [`Daemon`]) exactly as a computer starts it: the
//! person is founded in an identity store of the test's own, and the daemon governs their
//! workspace, answers its device's dispatch on its endpoint (`tilo-actions/1`), takes its
//! keys' sessions and runs its workspaces' agents and triggers. Everything a test does goes
//! through the paths a person, an SDK, an agent or another device uses:
//!
//! | In a test | What reaches the daemon |
//! |---|---|
//! | [`Host::workspace`] | the workspace founded on the daemon, its own workspace |
//! | [`Host::key`], then [`Key::get`] / [`Key::post`] | `/v1`, as the SDKs call it in process |
//! | [`Key::tool`] | the per-workspace MCP endpoint a key's agent calls |
//! | [`Host::agent`], then [`Agent::tool`] | `tilo mcp`: an agent run the device hands its person's MCP client |
//! | [`Host::member`], then [`Member::call`] | another device of the person, over `tilo-actions/1` |
//! | [`Host::visitor`] | a key the workspace has no certificate for ([`Caller::visitor`]) |
//! | [`stranger`], then [`endpoint`] | another person's computer, dialling through the relay |
//! | [`Host::restart`] | the computer restarting its daemon on the same home |
//! | [`Host::dispatch`] | the daemon's one dispatch, as the person's own call |
//!
//! Each test runs in a process of its own, with a home, identity store and profile of its own:
//! the daemon is the device, and a device is one per process. [`run`] starts that process
//! itself; under nextest, which runs each test in a process anyway, it costs one more start.
//!
//! # Porting a test
//!
//! A test that started a `tilo-server` with an agent supervisor and reached it through a
//! harness becomes one [`run`]:
//!
//! ```ignore
//! #[test]
//! fn a_key_lists_what_the_workspace_publishes() {
//!     host::run(|host| async move {
//!         let key = host.key("Visa backend");
//!         let (status, listed) = key
//!             .get(&format!("/v1/workspaces/{}/resources", host.workspace()))
//!             .await;
//!         assert_eq!(status, 200, "{listed}");
//!     });
//! }
//! ```
//!
//! - `start(&root, &device, state)` and `Hosted` go: [`run`] is the host, and its person is
//!   [`Host::person`].
//! - A page or a browser device is [`Host::member`]; one the workspace does not know is
//!   [`Host::visitor`].
//! - `approved_key` and a token on `api.dispatch` become [`Host::key`] and [`Key::get`] /
//!   [`Key::post`]; an `Mcp` session on `/mcp` becomes [`Key::tool`].
//! - `WorkspaceActions::new(..).attach(..)` and `invoked(..)` become [`Host::dispatch`] and
//!   [`call`].
//! - The test is a plain `#[test]`: the body is async, and runs on a runtime [`run`] makes.

pub mod apps;

mod app_actions;
mod messages_for_agents;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{
    APPLICATION_RESOURCE_PREFIX, APPLICATION_START_ACTION, ActionCallError, ActionProvider, Caller,
    CataloguePage, CatalogueQuery, InvokeArgument, InvokeRequest, MAX_CATALOGUE_PAGE, ReadAnswer,
    ReceiptStatus, RemoteActionTarget, ResourceReadRequest, answered, application_action_target,
    workspace_provider,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};
use tilo_peer_state::actions::{ActionService, NoActions, RemoteDevice};
use tilo_runtime::RuntimeServices as _;
use tilo_runtime::device::services::Daemon;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, StatementSigner as _};

/// Names the test the process [`run`] starts runs, and where its profile is.
const OWN: &str = "TILO_HOST_TEST";
const PROFILE: &str = "TILO_HOST_TEST_PROFILE";
/// How long a call over the device link is followed before a test fails on it.
const FOLLOW: Duration = Duration::from_secs(30);

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
    run_with(&[], body);
}

/// Run `body` in a process of its own, with a home, identity and profile of its own and its
/// person founded, but no daemon started: `body` starts the device's daemon itself, at the
/// profile it is handed.
///
/// # Panics
///
/// When the person cannot be founded, or `body` fails.
pub fn in_own_process(body: impl FnOnce(&Path)) {
    let test = std::thread::current()
        .name()
        .expect("libtest names a test's thread after the test")
        .to_owned();
    if std::env::var(OWN).ok().as_deref() != Some(test.as_str()) {
        own_process(&test, &[], &[]);
        return;
    }
    say_what_went_wrong();
    let profile = PathBuf::from(std::env::var_os(PROFILE).expect("the profile is named"));
    tilo_runtime::device::services::found_account_person(
        &tilo_identity::provisioning::collaboration_identity_paths(),
        "",
        tilo_runtime::device::services::this_account_uid(),
    )
    .unwrap();
    body(&profile);
}

/// [`run`], on a device that has the tilolang apps `apps` (directories under `apps/tl`)
/// installed, with the runtimes they run on ([`apps::install`]).
///
/// # Panics
///
/// As [`run`], and when an app does not build or install.
pub fn run_with<F, Fut>(apps: &[&str], body: F)
where
    F: FnOnce(Arc<Host>) -> Fut,
    Fut: Future<Output = ()>,
{
    run_with_packages(apps, &[], body);
}

/// [`run_with`], on a device that also has the Rust apps `rust` installed, each named by its
/// package of this tree's `apps/` and the entrypoint binary this run built for it
/// (`("terminal", "terminal")`): the daemon starts them the way a person's computer does.
///
/// # Panics
///
/// As [`run_with`], and when a package does not install.
pub fn run_with_packages<F, Fut>(apps: &[&str], rust: &[(&str, &str)], body: F)
where
    F: FnOnce(Arc<Host>) -> Fut,
    Fut: Future<Output = ()>,
{
    let test = std::thread::current()
        .name()
        .expect("libtest names a test's thread after the test")
        .to_owned();
    if std::env::var(OWN).ok().as_deref() != Some(test.as_str()) {
        own_process(&test, apps, rust);
        return;
    }
    say_what_went_wrong();
    let profile = PathBuf::from(std::env::var_os(PROFILE).expect("the profile is named"));
    // The language runtime this in-process daemon mounts, as the program mounts it: the built-in
    // apps' records are read and changed here as they are on a computer running the program.
    tilo_runtime::device::capabilities::mount(tilo_runtime::device::capabilities::Capabilities {
        tilolang: Some(Box::new(tilolang_records::host)),
        ..Default::default()
    });
    let host = Arc::new(Host::start(&profile));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(body(Arc::clone(&host)));
    // The test's endpoints and tasks go before the daemon, which is dropped on this thread,
    // outside any runtime, as a computer stops it.
    drop(runtime);
    drop(host);
}

/// Write the daemon's errors, and every line the apps it starts write to their standard streams,
/// to this process's error output, which the test's own log keeps: an app that ends at its start
/// says why there, as it does in a device's log. `RUST_LOG` adds to it.
pub fn say_what_went_wrong() {
    let directives =
        tilo_startup_metrics::log_directives(std::env::var("RUST_LOG").ok().as_deref());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(directives))
        .with_writer(std::io::stderr)
        .try_init();
}

/// Run `test` again, alone, in a process whose home, identity and profile are a new
/// directory's: what [`run`] does everywhere but in that process.
fn own_process(test: &str, apps: &[&str], rust: &[(&str, &str)]) {
    use std::os::unix::fs::PermissionsExt as _;
    // A short path: the daemon's sockets are named below it.
    let root = tempfile::Builder::new()
        .prefix("tilo-h-")
        .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let place = |name: &str| {
        let dir = root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    };
    let (config, data) = (place("c"), place("d"));
    // The login's runtime directory, where the daemon binds the sockets this account's own
    // processes reach (the account sign-ins among them): the test's own, never the login's.
    let runtime = place("r");
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    // The daemon's own profile is private to its person, as a login's is: a shell session it
    // starts is refused a profile anyone else could read.
    let profile = place("p");
    std::fs::set_permissions(&profile, std::fs::Permissions::from_mode(0o700)).unwrap();
    // The device relays through this machine's loopback relay: nothing reaches a relay or a
    // directory out on the internet.
    let (relay, relay_url) = test_server::loopback_relay();
    let installed = if apps.is_empty() && rust.is_empty() {
        Vec::new()
    } else {
        let installed = apps::install(&place("a"), &data, apps);
        for (package, bin) in rust {
            apps::install_rust(&place("a"), package, bin);
        }
        installed
    };
    let log = root.path().join("out");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .envs(installed)
        // The test is named exactly: run it even where `#[ignore]` keeps it out of an
        // ordinary run, as the check that names it runs it here.
        .args([
            "--exact",
            test,
            "--include-ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(OWN, test)
        .env(PROFILE, profile)
        .env("HOME", place("h"))
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data)
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

/// The daemon a test runs against, and its person.
pub struct Host {
    /// The daemon running now: [`Host::restart`] stops it and starts the next on the same home.
    daemon: Mutex<Option<Arc<Daemon>>>,
    profile: PathBuf,
    workspace: String,
    person: String,
    /// The person's root, kept here as the computer that founded them keeps it: what certifies
    /// their keys and devices.
    root: tilo_identity::store::SigningIdentity,
}

impl Host {
    fn start(profile: &Path) -> Self {
        tilo_runtime::device::services::found_account_person(
            &tilo_identity::provisioning::collaboration_identity_paths(),
            "",
            tilo_runtime::device::services::this_account_uid(),
        )
        .unwrap();
        let daemon = Arc::new(Daemon::start_reporting(profile, &mut |_| {}).unwrap());
        let root = tilo_identity::store::load_from_path(
            tilo_runtime::device::services::device_root_record().expect("the person founded here"),
        )
        .unwrap();
        Self {
            // The person's workspace is founded the first time it is asked for, as their own
            // `tilo` asks for it: nothing founds it at the daemon's start.
            workspace: daemon
                .workspace()
                .expect("the person's workspace, founded when first asked for"),
            person: daemon.actor().to_owned(),
            daemon: Mutex::new(Some(daemon)),
            profile: profile.to_owned(),
            root,
        }
    }

    /// The daemon running now.
    #[must_use]
    pub fn daemon(&self) -> Arc<Daemon> {
        Arc::clone(
            self.daemon
                .lock()
                .unwrap()
                .as_ref()
                .expect("a daemon runs between restarts"),
        )
    }

    /// Stop the daemon and start it again on the same home, as the computer does when it
    /// restarts: what it kept is read back. Keys and devices made before reach the stopped
    /// one, so the test drops them first and makes new ones.
    ///
    /// # Panics
    ///
    /// When something still holds the stopped daemon, or the next one does not start.
    pub async fn restart(&self) {
        let stopped = self.daemon.lock().unwrap().take().expect("a daemon runs");
        let stopped = Arc::try_unwrap(stopped)
            .unwrap_or_else(|_| panic!("a key or agent of the test still holds the daemon"));
        let profile = self.profile.clone();
        // A computer stops its daemon and starts the next outside any runtime of its own.
        let started = tokio::task::spawn_blocking(move || {
            drop(stopped);
            Daemon::start_reporting(&profile, &mut |_| {}).unwrap()
        })
        .await
        .unwrap();
        assert_eq!(
            started.workspace().expect("the workspace"),
            self.workspace,
            "the same workspace"
        );
        *self.daemon.lock().unwrap() = Some(Arc::new(started));
    }

    /// The workspace the daemon founded for its person.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// The person, by their id: the hex of their root.
    #[must_use]
    pub fn person(&self) -> &str {
        &self.person
    }

    /// The daemon's one dispatch in its founded workspace, as the person's own call: what the
    /// desktop, `tilo do` and the person's apps reach.
    #[must_use]
    pub fn dispatch(&self) -> Arc<dyn ActionProvider + Send + Sync> {
        Arc::new(self.daemon().action_provider().unwrap())
    }

    /// The person, as [`Self::dispatch`] admits them.
    #[must_use]
    pub fn caller(&self) -> Caller {
        Caller::local_person(self.person()).in_workspace(self.workspace())
    }

    /// A new key of the person's, `label`, certified by their root with `capabilities`.
    #[must_use]
    pub fn certify(
        &self,
        label: &str,
        capabilities: DeviceCapabilities,
    ) -> (SigningKey, DeviceCert) {
        certified(&self.root, self.root.public_key(), label, capabilities)
    }

    /// A key reaching all the workspaces of whoever `root` is, named `label`, calling this
    /// daemon's `/v1` and MCP as [`Self::key`] does.
    #[must_use]
    pub fn key_of(&self, root: &SigningKey, label: &str) -> Key {
        let (key, cert) = certified(
            root,
            root.verifying_key().to_bytes(),
            label,
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        );
        let direct = tilo_api::direct::Direct::here(
            self.daemon() as Arc<dyn tilo_api::session::Here>,
            key,
            cert,
            label,
        )
        .unwrap();
        Key { direct }
    }

    /// A key of the person's reaching all their workspaces, named `label`: the key an SDK or a
    /// backend keeps, calling `/v1` and MCP.
    #[must_use]
    pub fn key(&self, label: &str) -> Key {
        let (key, cert) = self.certify(
            label,
            DeviceCapabilities::agent_grant(AgentScope::AllWorkspaces),
        );
        let direct = tilo_api::direct::Direct::here(
            self.daemon() as Arc<dyn tilo_api::session::Here>,
            key,
            cert,
            label,
        )
        .unwrap();
        Key { direct }
    }

    /// A key of the person's reaching `workspace` alone, named `label`: what a backend keyed for
    /// one customer's workspace is.
    ///
    /// # Panics
    ///
    /// `workspace` is not a workspace id.
    #[must_use]
    pub fn key_reaching(&self, workspace: &str, label: &str) -> Key {
        let id: [u8; 16] = uuid::Uuid::parse_str(workspace)
            .expect("the workspace is named by its id")
            .into_bytes();
        let (key, cert) = self.certify(
            label,
            DeviceCapabilities::agent_grant(AgentScope::Workspace(id)),
        );
        let direct = tilo_api::direct::Direct::here(
            self.daemon() as Arc<dyn tilo_api::session::Here>,
            key,
            cert,
            label,
        )
        .unwrap();
        Key { direct }
    }

    /// One of the person's own devices, named `label`, as a key: what a page open on their
    /// workspace calls this daemon with, entered in the workspace as their device rather than
    /// as an agent of theirs.
    #[must_use]
    pub fn page_key(&self, label: &str) -> Key {
        let (key, cert) = self.certify(label, DeviceCapabilities::member());
        let direct = tilo_api::direct::Direct::here(
            self.daemon() as Arc<dyn tilo_api::session::Here>,
            key,
            cert,
            label,
        )
        .unwrap();
        Key { direct }
    }

    /// An agent the person runs on this device (`tilo mcp`): the MCP session the daemon hands
    /// its client, in the founded workspace.
    #[must_use]
    pub fn agent(&self) -> Agent {
        let session = self.daemon().mcp_session_factory(self.workspace()).unwrap()().unwrap();
        Agent {
            session,
            calls: std::sync::atomic::AtomicU64::new(1),
            opened: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Another device of the person's, `label`, entered in `workspace` and asking the daemon over
    /// the device link (`tilo-actions/1`), as their laptop or phone does.
    pub async fn member(&self, label: &str, workspace: &str) -> Member {
        let (key, cert) = self.certify(label, DeviceCapabilities::member());
        self.daemon()
            .workspaces()
            .enter(workspace, (&cert, &key))
            .await
            .unwrap();
        self.dial(key, workspace).await
    }

    /// A device of another person than this computer's, `label`, in `workspace` as an editor:
    /// the workspace's owner adds the person, and their device lists itself. Who a workspace
    /// shared with other people has among its members. Answered with their person's root, by
    /// which the owner names them.
    pub async fn other_person(&self, label: &str, workspace: &str) -> (Member, [u8; 32]) {
        use tilo_workspace_service::workspaces::{AddMemberRequest, MemberPlace};

        let (root, user) = tilo_user_identity::UserIdentity::generate(label, 1);
        let mut secret = [0; 32];
        getrandom::fill(&mut secret).unwrap();
        let key = SigningKey::from_bytes(&secret);
        let now = tilo_clock::unix_now_ns().saturating_sub(60 * 1_000_000_000);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: key.verifying_key().to_bytes(),
            device_label: label.into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now,
            expires_at_ns: Some(now.saturating_add(24 * 60 * 60 * 1_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let daemon = self.daemon();
        let workspaces = daemon.workspaces();
        // The owner's add-member action through the workspace's governance: a person is a
        // member before any device of theirs may enter.
        workspaces
            .add_member(
                workspace,
                workspaces.device_cert(),
                AddMemberRequest {
                    request_id: *blake3::hash(&user.public_key).as_bytes(),
                    user_public_key: user.public_key,
                    place: MemberPlace::Role(tilo_peer_state::join::Role::Editor),
                    name: Some(label.to_owned()),
                },
            )
            .await
            .expect("the workspace's owner adds the person");
        workspaces
            .enter(workspace, (&cert, &key))
            .await
            .expect("their device lists itself");
        (self.dial(key, workspace).await, user.public_key)
    }

    /// The workspace's owner removes the person `user` from `workspace`, and every device of
    /// theirs with them.
    pub async fn remove_person(&self, workspace: &str, user: [u8; 32]) {
        let daemon = self.daemon();
        let workspaces = daemon.workspaces();
        workspaces
            .remove_member(
                workspace,
                workspaces.device_cert(),
                tilo_workspace_service::workspaces::RemoveMemberRequest {
                    request_id: *blake3::hash(&[&user[..], b"removed"].concat()).as_bytes(),
                    user_public_key: user,
                },
            )
            .await
            .expect("the workspace's owner removes the person");
    }

    /// A key no certificate of `workspace` names, asking the daemon over the device link: a
    /// page at the workspace's public entry. It reaches only what the workspace opens to any key.
    pub async fn visitor(&self, workspace: &str) -> Member {
        self.dial(random_key(), workspace).await
    }

    /// A device of this person's, `label`, that serves `provider` beside the device's dispatcher:
    /// what another device of theirs answers with on the action link. It holds `workspace` as a
    /// member device does, as its state stands once the device entered it, so it decides what
    /// the daemon asks it back by the workspace's own roster.
    pub async fn member_serving(
        &self,
        label: &str,
        workspace: &str,
        provider: Arc<dyn ActionProvider + Send + Sync>,
    ) -> Member {
        let (key, cert) = self.certify(label, DeviceCapabilities::member());
        let daemon = self.daemon();
        let workspaces = daemon.workspaces();
        workspaces.enter(workspace, (&cert, &key)).await.unwrap();
        let held = workspaces
            .state(workspace)
            .await
            .expect("the daemon holds the workspace the device entered");
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let service = ActionService::new(key.clone(), Arc::new(NoActions));
        service.hold(tilo_peer_state::direct::Replica::new(held));
        service.add_provider(provider);
        let remote = Arc::new(service.remote(&endpoint, &self.daemon().endpoint_addr(), workspace));
        Member {
            remote,
            service,
            endpoint,
        }
    }

    /// `key`, whatever certifies it, asking the daemon over the device link for `workspace`.
    pub async fn dial(&self, key: SigningKey, workspace: &str) -> Member {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .bind()
            .await
            .unwrap();
        let service = ActionService::new(key.clone(), Arc::new(NoActions));
        let remote = Arc::new(service.remote(&endpoint, &self.daemon().endpoint_addr(), workspace));
        Member {
            remote,
            service,
            endpoint,
        }
    }
}

/// A key of the person's, calling the daemon's API in process.
pub struct Key {
    direct: tilo_api::direct::Direct,
}

impl Key {
    /// `GET path`: the status and the JSON answered.
    pub async fn get(&self, path: &str) -> (u16, Value) {
        self.request("GET", path, &[], b"").await
    }

    /// `POST path` with `body`: the status and the JSON answered.
    pub async fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        self.request("POST", path, &[], body.to_string().as_bytes())
            .await
    }

    /// `GET path`: the status and the bytes answered whole.
    pub async fn bytes(&self, path: &str) -> (u16, Vec<u8>) {
        let answer = self.direct.request("GET", path, &[], b"").await;
        let status = answer.status().as_u16();
        (status, whole(answer).await)
    }

    /// `method path` as a stream of server-sent events, a chunk at a time.
    pub async fn stream(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> Chunks {
        let mut all = vec![("accept".to_owned(), "text/event-stream".to_owned())];
        all.extend(
            headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
        );
        let answer = self.direct.request(method, path, &all, body).await;
        let streams = answer
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if !streams {
            let status = answer.status();
            panic!(
                "{status} is not a stream: {}",
                String::from_utf8_lossy(&whole(answer).await)
            );
        }
        Chunks(tokio::sync::Mutex::new(Some(answer.into_body())))
    }

    /// `method path` with `headers` and `body`: the status and the JSON answered.
    pub async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &[u8],
    ) -> (u16, Value) {
        let headers: Vec<(String, String)> = if headers.is_empty() {
            vec![("content-type".into(), "application/json".into())]
        } else {
            headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect()
        };
        json_of(self.direct.request(method, path, &headers, body).await).await
    }

    /// The MCP tool `name` with `arguments` (naming the `workspace` it acts in), as the key's
    /// agent calls it: its `tools/call` result.
    pub async fn tool(&self, name: &str, arguments: &Value) -> Value {
        self.tool_calling(name, arguments, Value::Null).await
    }

    /// [`Self::tool`], its typed answer: what the API's read of a resource answers from.
    pub async fn tool_structured(&self, name: &str, arguments: &Value) -> Value {
        self.tool_calling(
            name,
            arguments,
            json!({"so.tilo.mcp": {"structured": true}}),
        )
        .await
    }

    async fn tool_calling(&self, name: &str, arguments: &Value, meta: Value) -> Value {
        let mut params = json!({"name": name, "arguments": arguments});
        if !meta.is_null() {
            params["_meta"] = meta;
        }
        let answer = self
            .direct
            .mcp(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": params}))
            .await
            .unwrap();
        answer["result"].clone()
    }
}

/// An agent run on the daemon: the MCP session `tilo mcp` serves its client.
pub struct Agent {
    session: tilo_mcp::McpServer<tilo_runtime::device::services::RuntimeActionProvider>,
    calls: std::sync::atomic::AtomicU64,
    /// Whether the connection opened with its client's name, as `tilo mcp` opens it.
    opened: std::sync::atomic::AtomicBool,
}

impl Agent {
    /// The MCP tool `name` with `arguments`: its `tools/call` result.
    pub async fn tool(&self, name: &str, arguments: &Value) -> Value {
        self.tool_calling(name, arguments, Value::Null).await
    }

    /// [`Self::tool`], its typed answer: what the API's read of a resource answers from.
    pub async fn tool_structured(&self, name: &str, arguments: &Value) -> Value {
        self.tool_calling(
            name,
            arguments,
            json!({"so.tilo.mcp": {"structured": true}}),
        )
        .await
    }

    async fn tool_calling(&self, name: &str, arguments: &Value, meta: Value) -> Value {
        let mut params = json!({"name": name, "arguments": arguments});
        if !meta.is_null() {
            params["_meta"] = meta;
        }
        let answer = self.ask("tools/call", params).await;
        answer["result"].clone()
    }

    /// The MCP request `method` with `params`, answered whole. The connection opens with the
    /// client's `initialize` first, as every MCP client does, naming itself as the person's
    /// agent `Claude Code` unless the test's own `initialize` names it.
    pub async fn ask(&self, method: &str, params: Value) -> Value {
        if method == "initialize" {
            self.opened
                .store(true, std::sync::atomic::Ordering::Relaxed);
        } else if !self.opened.swap(true, std::sync::atomic::Ordering::Relaxed) {
            self.session
                .handle(json!({
                    "jsonrpc": "2.0",
                    "id": 0,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {},
                        "clientInfo": {"name": "Claude Code", "version": "2"},
                    },
                }))
                .await
                .unwrap();
        }
        let id = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.session
            .handle(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await
            .unwrap()
    }
}

/// Another device asking the daemon over the device link.
pub struct Member {
    remote: Arc<RemoteDevice>,
    service: ActionService,
    endpoint: iroh::Endpoint,
}

impl Member {
    /// This device's key, as its certificate names it: the one its requests are signed with, never
    /// the daemon's it asks.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.service.device()
    }

    /// This device's endpoint, as it dials a host's terminal streams.
    #[must_use]
    pub fn endpoint(&self) -> &iroh::Endpoint {
        &self.endpoint
    }

    /// `request` as it is, its signature included, followed until it settles.
    pub async fn send(&self, request: InvokeRequest) -> Result<InvocationUpdate, ActionCallError> {
        tilo_test_env::within(
            "the daemon to answer over the device link",
            Arc::clone(&self.remote).invoke_until_settled(request, FOLLOW),
        )
        .await
        .expect("the device link answers")
    }

    /// The action at `address` of `provider`, with `arguments`, followed until it settles.
    pub async fn call(
        &self,
        provider: &str,
        address: ActionAddress,
        arguments: &Value,
    ) -> Result<InvocationUpdate, ActionCallError> {
        tilo_test_env::within(
            "the daemon to answer over the device link",
            Arc::clone(&self.remote)
                .invoke_until_settled(call(provider, address, arguments), FOLLOW),
        )
        .await
        .expect("the device link answers")
    }
}

/// A device of another person's, `label`, whom no workspace here knows: one an invitation brings
/// in.
#[must_use]
pub fn stranger(label: &str) -> (SigningKey, DeviceCert) {
    let root = random_key();
    let person = root.verifying_key().to_bytes();
    certified(&root, person, label, DeviceCapabilities::member())
}

/// An endpoint of `key`'s, reached through the relays this machine's devices use: another
/// computer, dialling from a network of its own.
pub async fn endpoint(key: &SigningKey) -> iroh::Endpoint {
    iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Custom(
            tilo_endpoint_reachability::relays().relays,
        ))
        .bind()
        .await
        .unwrap()
}

fn random_key() -> SigningKey {
    let mut secret = [0; 32];
    getrandom::fill(&mut secret).unwrap();
    SigningKey::from_bytes(&secret)
}

/// A new key `label` of the person whose root `root` is, certified with `capabilities`.
pub fn certified(
    root: &dyn tilo_user_identity::StatementSigner,
    person: [u8; 32],
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = random_key();
    // Issued a minute ago: a check reads the clock in whole milliseconds, and a certificate
    // issued later than the check's own now is not valid yet.
    let now = tilo_clock::unix_now_ns().saturating_sub(60 * 1_000_000_000);
    let mut cert = DeviceCert {
        user_public_key: person,
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: now,
        expires_at_ns: Some(now.saturating_add(24 * 60 * 60 * 1_000_000_000)),
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).unwrap();
    (key, cert)
}

/// The target of `app`'s `action` on its own record in the daemon's workspace: the address an
/// agent's path, a view's button and `tilo do` all reach, whatever language the app is written in.
#[must_use]
pub fn app_action(host: &Host, app: &str, resource: &str, action: &str) -> RemoteActionTarget {
    application_action_target(host.workspace(), app, resource, action)
}

/// The daemon's workspace starts `app` with no window, as the workspace's own `start` does: the
/// answer names the instance that runs, and the app opens its own record there.
///
/// # Panics
///
/// When the dispatch does not answer at all.
pub async fn start_app(host: &Host, app: &str) -> Result<Value, ActionCallError> {
    let target = RemoteActionTarget {
        provider: workspace_provider(host.workspace()),
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{app}"),
            APPLICATION_START_ACTION,
        ),
    };
    output(host, target, Value::Null)
        .await
        .map(|output| output.value.unwrap_or_default())
}

/// What the person's own dispatch answers a call of `target` with `arguments` (an object, or
/// `null`), followed to its end: its whole output, or why it did not run.
///
/// # Panics
///
/// When the dispatch does not answer at all.
pub async fn output(
    host: &Host,
    target: RemoteActionTarget,
    arguments: Value,
) -> Result<ActionOutput, ActionCallError> {
    let caller = host.caller();
    let provider = host.dispatch();
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument: match &arguments {
            Value::Object(map) => InvokeArgument::Json(map.clone()),
            _ => InvokeArgument::None,
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let invocation = request.invocation;
    let receipt = provider
        .invoke(&caller, request)
        .await
        .expect("the dispatch answers")?;
    let status = receipt.status;
    let deadline = tilo_test_env::hang_deadline();
    while matches!(status, ReceiptStatus::Accepted) {
        assert!(
            std::time::Instant::now() < deadline,
            "the call never settled"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
        let Some(state) = provider.observe(&caller, &invocation) else {
            continue;
        };
        match state.update {
            InvocationUpdate::Succeeded { output, .. } => return Ok(output),
            InvocationUpdate::Failed { error, .. } => {
                return Err(ActionCallError::Refused {
                    reason: error.message,
                });
            }
            InvocationUpdate::Cancelled { .. } => {
                return Err(ActionCallError::Refused {
                    reason: "the call was cancelled".into(),
                });
            }
            _ => {}
        }
    }
    match status {
        ReceiptStatus::Completed { output } => Ok(output),
        ReceiptStatus::Failed { error } => Err(ActionCallError::Refused {
            reason: error.message,
        }),
        ReceiptStatus::Rejected { error } => Err(error),
        ReceiptStatus::Cancelled { .. } => Err(ActionCallError::Refused {
            reason: "the call was cancelled".into(),
        }),
        ReceiptStatus::Accepted => Err(ActionCallError::Unavailable {
            reason: "the call never settled".into(),
        }),
    }
}

/// The record `app` keeps at `record` in the daemon's workspace, read through the device's one
/// record reader as `tilo_read record:<app>:<record>` reads it: the record's entry with its
/// content under `value`, or `absent` when nothing was ever written to it.
///
/// # Panics
///
/// When the dispatch reads no records.
pub async fn record(host: &Host, app: &str, record: &str) -> Result<Value, ActionCallError> {
    let reads = host
        .dispatch()
        .resource_reads()
        .ok_or_else(|| ActionCallError::Unavailable {
            reason: "the dispatch reads no records".into(),
        })?;
    let read = ResourceReadRequest {
        workspace: host.workspace().to_owned(),
        app: app.to_owned(),
        record: record.to_owned(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: ReadAnswer::Reading,
    };
    let answer = answered(reads, &host.caller(), read).await?;
    Ok(answer["records"].get(0).cloned().unwrap_or(Value::Null))
}

/// The catalogue page the person's dispatch answers for `app`: the actions its running process
/// publishes now, and those its signed manifest offers while it is closed.
///
/// # Panics
///
/// When the dispatch does not answer at all.
pub async fn listed(host: &Host, app: &str) -> Result<CataloguePage, ActionCallError> {
    let query = CatalogueQuery {
        app_id: Some(app.to_owned()),
        max_results: MAX_CATALOGUE_PAGE,
        ..CatalogueQuery::default()
    };
    host.dispatch()
        .query(&host.caller(), query)
        .await
        .expect("the dispatch answers")
}

/// The record `app`'s running process opened in the daemon's workspace, as its live connection
/// names it: `Some("")` for the app's package-primary record, `None` when the app runs no record.
#[must_use]
pub fn running_record(page: &CataloguePage, workspace: &str, app: &str) -> Option<String> {
    page.providers
        .iter()
        .find(|provider| {
            provider.app_id == app
                && provider.workspace == workspace
                && provider.provider.parse::<u64>().is_ok()
        })
        .map(|provider| provider.document_id.clone())
}

/// A call of the action at `address` of `provider` with `arguments` (an object, or `null`).
#[must_use]
pub fn call(provider: &str, address: ActionAddress, arguments: &Value) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.to_owned(),
            address,
        },
        argument: match arguments {
            Value::Object(map) => InvokeArgument::Json(map.clone()),
            _ => InvokeArgument::None,
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// An answer of the API whole: its bytes, read to its end.
pub async fn whole(answer: tilo_api::Reply) -> Vec<u8> {
    use http_body_util::BodyExt as _;
    let Ok(whole) = answer.into_body().collect().await;
    whole.to_bytes().to_vec()
}

/// An answer of the API: its status, and the JSON it answered (`null` for none, a string for a
/// body that is no JSON, `null` for a stream of events).
pub async fn json_of(answer: tilo_api::Reply) -> (u16, Value) {
    let status = answer.status().as_u16();
    let streams = answer
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    if streams {
        return (status, Value::Null);
    }
    let bytes = whole(answer).await;
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, value)
}

/// A stream of server-sent events the API answered, its chunks of data in order.
pub struct Chunks(tokio::sync::Mutex<Option<tilo_api::Body>>);

impl Chunks {
    /// The next chunk of data, or `None` once the stream ended.
    pub async fn next(&self) -> Option<Vec<u8>> {
        use http_body_util::BodyExt as _;
        let mut body = self.0.lock().await;
        loop {
            let Some(frame) = body.as_mut()?.frame().await else {
                *body = None;
                return None;
            };
            let Ok(frame) = frame;
            if let Ok(data) = frame.into_data()
                && !data.is_empty()
            {
                return Some(data.to_vec());
            }
        }
    }
}
