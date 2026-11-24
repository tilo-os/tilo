//! A workspace this computer holds and governs, and the identities that hold it: the fixtures the
//! tests of a hosted workspace start from.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ed25519_dalek::SigningKey;
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_peer_state::owner;
use tilo_runtime::daemon::Running;
use tilo_session::{Options, Session};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::{HEIGHT, WIDTH};

/// The workspace this computer holds.
pub const WORKSPACE: &str = "0b6a8f2c-3d4e-4f5a-8b9c-0d1e2f3a4b5c";

/// `key` 's certificate as a device of the person whose root key is `root` .
pub fn certified(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities,
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("a certified device");
    cert
}

/// This computer's person: a device of theirs, certified for their own root key, kept in an
/// identity root of the test's own and named by the account's identity store, as a device its
/// daemon starts the person's services as.
pub fn this_computer(
    env: &mut tilo_test_env::ScopedEnv,
) -> (tempfile::TempDir, SigningKey, DeviceCert) {
    let root = tempfile::tempdir().expect("an identity root");
    let paths = IdentityPaths::under(root.path());
    tilo_identity::sources::test_trust::install();
    let personal =
        tilo_identity::store::account_identity_dir_from_env().expect("the test's identity store");
    tilo_identity::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    env.set("TILO_IDENTITY_ROOT", root.path());
    let device = tilo_device_identity::persist::load_device_key(&paths).expect("this device's key");
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    (root, device, cert)
}

/// This device's identity and the workspace it hosts, where its daemon reads them.
pub struct Fixture {
    pub device: SigningKey,
    pub cert: DeviceCert,
    /// The state directory the device's daemon keeps its workspaces under.
    pub state: PathBuf,
    // The identity's environment and files, restored and removed last: the daemon and the apps
    // read them while they run.
    _env: tilo_test_env::ScopedEnv,
    pub root: tempfile::TempDir,
}

/// This device's own identity, provisioned as a session provisions it, holding [`WORKSPACE`] in
/// the store its daemon serves.
pub fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let home = root.path().join("home");
    let xdg = root.path().join("config");
    let data = root.path().join("data");
    let cache = root.path().join("cache");
    let runtime = root.path().join("runtime");
    let system = root.path().join("system");
    let state = root.path().join("state");
    for directory in [&home, &xdg, &data, &cache, &state, &runtime] {
        std::fs::create_dir_all(directory).expect("a directory for the device");
    }
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))
        .expect("the runtime directory is this user's");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &home)
        .set("XDG_CONFIG_HOME", &xdg)
        .set("XDG_DATA_HOME", &data)
        .set("XDG_CACHE_HOME", &cache)
        .set("XDG_STATE_HOME", &state)
        .set("XDG_RUNTIME_DIR", &runtime)
        .set("TILO_IDENTITY_ROOT", &system)
        .set(FILE_SECRETS_ENV, "1");
    tilo_identity::sources::test_trust::install();
    let personal = tilo_identity::store::account_identity_dir(&xdg.join("tilo/identity"));
    let paths = IdentityPaths::under(&system);
    tilo_identity::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let device = tilo_device_identity::persist::load_device_key(&paths).expect("this device's key");
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this device's certificate");
    workspace_held_here(&state, WORKSPACE, &cert, &device);
    Fixture {
        device,
        cert,
        state,
        _env: env,
        root,
    }
}

/// Where the device's daemon keeps the service that holds its workspaces, below its state
/// directory: the state root its service is opened on when it starts
/// (`tilo_runtime::device::workspaces`).
pub fn daemon_service_root(state: &Path) -> PathBuf {
    state
        .join("tilo")
        .join("device")
        .join("own")
        .join("service")
}

/// This computer's copy of the workspace `workspace` under `state`, held as `cert`'s device in a
/// workspace the same device governs: the state the daemon started on this directory finds
/// waiting for it.
pub fn workspace_held_here(state: &Path, workspace: &str, cert: &DeviceCert, key: &SigningKey) {
    workspace_founded_on_this_device_then(
        state,
        workspace,
        "Shared browser workspace",
        cert,
        key,
        async |_| {},
    );
}

/// Found `workspace` under `name` in the workspace service this device's daemon serves from
/// `state`, as the daemon founds a workspace its person makes: the service holds and governs it,
/// this device's person is its owner, and this device is enrolled in it. What a session asks of
/// the workspace's governance — an invitation into it above all — is answered by that service
/// through the daemon from then on. `then` runs on the service once the workspace is founded
/// there: what the person did in it before the daemon started.
pub fn workspace_founded_on_this_device_then(
    state: &Path,
    workspace: &str,
    name: &str,
    cert: &DeviceCert,
    key: &SigningKey,
    then: impl AsyncFnOnce(&tilo_workspace_service::service::WorkspaceService),
) {
    let root = daemon_service_root(state);
    let workspace_id = WorkspaceId::parse(workspace).expect("a workspace id");
    let owner = owner::open_file(
        &tilo_session_client::engine::owner_path(state).expect("this device's state directory"),
    )
    .expect("an owner for this device's workspace");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime for this device's own workspace service");
    runtime.block_on(async {
        let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
            .bind()
            .await
            .expect("this device's endpoint");
        // The service writes as the device, and its member copies act as it, as the session's
        // own service does when it opens this store again.
        let signer: Arc<dyn collab_types::signer::DeviceSigner> = Arc::new(key.clone());
        let identity = tilo_workspace_service::service::ServiceIdentity {
            user_public_key: cert.user_public_key,
            device_signing_key: key.clone(),
            device_cert: cert.clone(),
            revocations: tilo_user_identity::RevocationList::new(),
        };
        let service = tilo_workspace_service::service::WorkspaceService::open(
            tilo_workspace_service::service::ServiceConfig {
                // The service names no address of its own until something needs it reachable, as
                // the session's own service does: nothing here waits for a server to name it.
                reaching: tilo_endpoint_reachability::Reaching::WhenNeeded,
                member: Some(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
                    cert: cert.clone(),
                    signer,
                })),
                ..tilo_workspace_service::service::ServiceConfig::new(
                    root,
                    tilo_server_protocol::WorkSetting::none(),
                )
            },
            identity,
            endpoint.clone(),
            None,
        )
        .await
        .expect("this device's own workspace service opens");
        let invitation = service
            .place_own_workspace(workspace, name, cert)
            .await
            .expect("this device's service places the workspace");
        let enrollment = tilo_peer_state::owner::enroll::prepare_and_complete(
            tilo_peer_state::join_transport::WorkspaceJoinClient::local(
                service.join_service(),
                cert.device_public_key,
            ),
            tilo_identity::identity_store::DeviceIdentity::kept(cert.clone(), key.clone()),
            workspace_id,
            None,
            invitation,
            owner.clone(),
        )
        .await
        .expect("this device's service admits it");
        owner
            .install(&enrollment, SystemTime::now())
            .expect("the founding installs here");
        then(&service).await;
        drop(service);
        endpoint.close().await;
    });
}

/// The record `key` names on `client`'s connection, once the session admitted the app there: as
/// the device's person, whom every app the session launched acts as.
pub async fn opened(
    client: &runtime_collab::CollabClient,
    key: &str,
) -> Result<runtime_collab::RecordHandle, runtime_portal::NotAvailable> {
    let record = client.record(key, "");
    record.sealed().await?;
    Ok(record)
}

/// The person `root` certifies, as a workspace names its owner.
pub fn actor(root: &[u8; 32]) -> String {
    hex::encode(root)
}

/// A shell showing the workspace `workspace`, known by its id, owned by `person`.
pub fn layout(workspace: &str, person: &str) -> Layout {
    let id = WorkspaceId::parse(workspace).expect("a workspace id");
    let mut held = Workspace::new(id, "Shell");
    held.policy.set_owner(person);
    Layout {
        workspaces: vec![held.clone()],
        active: held.id(),
        ..Layout::default()
    }
}

/// A session running as the shell of a device daemon a test started, over a profile of the
/// test's own: the daemon stops after the session ends.
pub struct Hosted {
    pub session: Session,
    // The daemon keeps the session's person's services, and stops once nothing holds it: after
    // the session, which runs on the link it made.
    _daemon: Running,
    /// The profile the daemon serves, when the test did not keep it.
    _profile: Option<tempfile::TempDir>,
    /// What a test set for the daemon to read its state from, restored after it.
    _env: Option<tilo_test_env::ScopedEnv>,
    /// Whatever else the test's daemon reads, kept as long as the session it serves.
    _kept: Vec<tempfile::TempDir>,
}

impl Hosted {
    /// Keep `directory` as long as the session: what the daemon read as it started.
    #[must_use]
    pub fn keeping(mut self, directory: tempfile::TempDir) -> Self {
        self._kept.push(directory);
        self
    }
}

impl std::ops::Deref for Hosted {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        &self.session
    }
}

impl std::ops::DerefMut for Hosted {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.session
    }
}

/// A private directory for a daemon's profile, which only this user reaches.
fn private_profile() -> tempfile::TempDir {
    let profile = tempfile::tempdir().expect("a profile for the daemon");
    std::fs::set_permissions(profile.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the profile is this user's");
    profile
}

/// A session as the shell of a device daemon over `profile`, waiting up to the suite's hang
/// bound for the person's services to run: what a person's computer runs.
///
/// # Panics
/// The daemon or the session does not start.
#[track_caller]
pub fn hosted_in(profile: &Path, options: Options, outputs: &[OutputConfig]) -> Hosted {
    let (daemon, session) = tilo_session::shell_session::start_in_process(
        profile,
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            ..options
        },
        outputs,
        tilo_test_env::hang_bound(),
    )
    .expect("the daemon starts and the session runs as its shell");
    Hosted {
        session,
        _daemon: daemon,
        _profile: None,
        _env: None,
        _kept: Vec::new(),
    }
}

/// A session as the shell of a device daemon of the test's own, on a profile of its own: the
/// daemon takes the person's services from the environment, and stops after the session ends.
///
/// # Panics
/// The daemon or the session does not start.
#[track_caller]
pub fn hosted(options: Options, outputs: &[OutputConfig]) -> Hosted {
    let profile = private_profile();
    let mut hosted = hosted_in(profile.path(), options, outputs);
    // The profile the daemon serves lives as long as the session it serves.
    hosted._profile = Some(profile);
    hosted
}

/// [`hosted`], with `env` kept set for as long as the session runs: what the daemon reads its
/// relays and other settings from as it starts.
///
/// # Panics
/// The daemon or the session does not start.
#[track_caller]
pub fn hosted_in_env(
    env: tilo_test_env::ScopedEnv,
    options: Options,
    outputs: &[OutputConfig],
) -> Hosted {
    let mut hosted = hosted(options, outputs);
    hosted._env = Some(env);
    hosted
}

/// A session of this computer taking part in the peer network, on the shell showing
/// [`WORKSPACE`] owned by the person whose root key is `root`, as the shell of the device daemon
/// started on the state `state` holds.
pub fn hosted_session(state: &Path, root: &[u8; 32]) -> Hosted {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_STATE_HOME", state);
    let mut hosted = hosted(
        Options {
            collab: true,
            person: "Ada".to_owned(),
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    );
    hosted._env = Some(env);
    hosted
        .session
        .handle()
        .replace_tree(layout(WORKSPACE, &actor(root)));
    for _ in 0..4 {
        hosted
            .session
            .step(Duration::from_millis(1))
            .expect("a frame");
    }
    hosted
}

/// This computer's person: an identity of its own in `home` 's configuration, and a device
/// certified for it under a key root, kept in `home` too so it lives as long as the test. Returns
/// the root to hold, and the person's key.
pub fn this_computer_with_an_identity(
    home: &mut tilo_test_env::TestHome,
) -> (std::path::PathBuf, [u8; 32]) {
    // The keys live under the test's own home, not a second temporary
    // directory: the session's renewal resolves them from the environment,
    // and nothing may delete them while it runs.
    let root = home.root().join("system");
    // Named before the person is made: their root is sealed with the device key kept there.
    home.env().set("TILO_IDENTITY_ROOT", &root);
    tilo_identity::sources::test_trust::install();
    let identity_dir =
        tilo_identity::store::account_identity_dir(&home.config().join("tilo").join("identity"));
    tilo_identity::provisioning::ensure_collaboration_identity(
        &identity_dir,
        &tilo_device_identity::IdentityPaths::under(&root),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let person = tilo_identity::store::list_identities(&identity_dir)
        .expect("this computer's identities")
        .into_iter()
        .next()
        .expect("an identity")
        .public_key;
    // The name they go by, which the site's entry learns of them: never their device's.
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: person,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
            device_public_key: None,
        },
    ))));
    (root, person)
}
