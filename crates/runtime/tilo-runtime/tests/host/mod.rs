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

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
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
    // The test's endpoints and tasks go before the daemon, which is dropped on this thread,
    // outside any runtime, as a computer stops it.
    drop(runtime);
    drop(host);
}

/// Run `test` again, alone, in a process whose home, identity and profile are a new
/// directory's: what [`run`] does everywhere but in that process.
fn own_process(test: &str) {
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
    // The device relays through this machine's loopback relay: nothing reaches a relay or a
    // directory out on the internet.
    test_server::relay_through_loopback_in(&config, &data);
    let log = root.path().join("out");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads", "1"])
        .env(OWN, test)
        .env(PROFILE, place("p"))
        .env("HOME", place("h"))
        .env("XDG_CONFIG_HOME", &config)
        .env("XDG_DATA_HOME", &data)
        .env("XDG_CACHE_HOME", place("k"))
        .env("XDG_STATE_HOME", place("s"))
        .env("TILO_IDENTITY_ROOT", place("i"))
        .env(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
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
    root: tilo_enrollment::root::PersonSigner,
}

impl Host {
    fn start(profile: &Path) -> Self {
        tilo_runtime::device::services::found_a_person().unwrap();
        let daemon = Arc::new(Daemon::start_reporting(profile, &mut |_| {}).unwrap());
        let identities = tilo_identity::store::identity_dir_from_env().unwrap();
        let active = tilo_identity::store::resolve_active_identity(&identities)
            .expect("the person founded here");
        let root = tilo_enrollment::root::PersonSigner::new(
            tilo_identity::store::load_from_path(active).unwrap(),
        );
        Self {
            workspace: daemon.workspace().to_owned(),
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
        assert_eq!(started.workspace(), self.workspace, "the same workspace");
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

    /// An agent the person runs on this device (`tilo mcp`): the MCP session the daemon hands
    /// its client, in the founded workspace.
    #[must_use]
    pub fn agent(&self) -> Agent {
        let session = self.daemon().mcp_session_factory(self.workspace()).unwrap()().unwrap();
        Agent {
            session,
            calls: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Another device of the person's, `label`, entered in `workspace` and asking the daemon over
    /// the device link (`tilo-actions/1`), as their laptop or phone does.
    pub async fn member(&self, label: &str, workspace: &str) -> Member {
        let (key, cert) = self.certify(label, DeviceCapabilities::member());
        self.daemon()
            .workspaces()
            .enter(workspace, &cert)
            .await
            .unwrap();
        self.dial(key, workspace).await
    }

    /// A key no certificate of `workspace` names, asking the daemon over the device link: a
    /// page at the workspace's public entry. It reaches only what the workspace opens to any key.
    pub async fn visitor(&self, workspace: &str) -> Member {
        self.dial(random_key(), workspace).await
    }

    async fn dial(&self, key: SigningKey, workspace: &str) -> Member {
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
            _service: service,
            _endpoint: endpoint,
        }
    }
}

/// A key of the person's, calling the daemon's API in process, as the SDKs' native part does.
pub struct Key {
    direct: tilo_api::direct::Direct,
}

impl Key {
    /// `GET path`: the status and the JSON answered.
    pub async fn get(&self, path: &str) -> (u16, Value) {
        self.send("GET", path, b"").await
    }

    /// `POST path` with `body`: the status and the JSON answered.
    pub async fn post(&self, path: &str, body: &Value) -> (u16, Value) {
        self.send("POST", path, body.to_string().as_bytes()).await
    }

    async fn send(&self, method: &str, path: &str, body: &[u8]) -> (u16, Value) {
        let answer = self
            .direct
            .answer(
                method,
                path,
                &[("content-type".into(), "application/json".into())],
                body,
            )
            .await;
        let value = match answer.body {
            tilo_api::direct::AnswerBody::Whole(bytes) if bytes.is_empty() => Value::Null,
            tilo_api::direct::AnswerBody::Whole(bytes) => serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned())),
            tilo_api::direct::AnswerBody::Stream(_) => Value::Null,
        };
        (answer.status, value)
    }

    /// The MCP tool `name` with `arguments` (naming the `workspace` it acts in), as the key's
    /// agent calls it: its `tools/call` result.
    pub async fn tool(&self, name: &str, arguments: &Value) -> Value {
        let answer = self
            .direct
            .mcp(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": {"name": name, "arguments": arguments}}))
            .await
            .unwrap();
        answer["result"].clone()
    }
}

/// An agent run on the daemon: the MCP session `tilo mcp` serves its client.
pub struct Agent {
    session: tilo_mcp::McpServer<tilo_runtime::device::services::RuntimeActionProvider>,
    calls: std::sync::atomic::AtomicU64,
}

impl Agent {
    /// The MCP tool `name` with `arguments`: its `tools/call` result.
    pub async fn tool(&self, name: &str, arguments: &Value) -> Value {
        let answer = self
            .ask("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        answer["result"].clone()
    }

    /// The MCP request `method` with `params`, answered whole.
    pub async fn ask(&self, method: &str, params: Value) -> Value {
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
    _service: ActionService,
    _endpoint: iroh::Endpoint,
}

impl Member {
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
fn certified(
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
