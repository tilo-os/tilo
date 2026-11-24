//! A workspace this computer holds and governs, and the identities that hold it: the fixtures the
//! tests of a hosted workspace start from.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ed25519_dalek::{Signer as _, SigningKey};
use tilo_compositor::OutputConfig;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_device_identity::IdentityPaths;
use tilo_identity::provisioning::{FILE_SECRETS_ENV, IdentityStorage};
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::{self, testing};
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
/// identity root of the test's own.
pub fn this_computer(
    env: &mut tilo_test_env::ScopedEnv,
) -> (tempfile::TempDir, SigningKey, DeviceCert) {
    let root = tempfile::tempdir().expect("an identity root");
    let paths = IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = SigningKey::from_bytes(&[0x61; 32]);
    let device = SigningKey::from_bytes(&tilo_test_env::node_key());
    tilo_device_identity::persist::write_device_key(&paths.device_privkey, &device)
        .expect("this computer's device key");
    let cert = certified(&person, &device, "The laptop", DeviceCapabilities::member());
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    (root, device, cert)
}

/// This device's identity and the workspace it hosts, where the session reads them.
pub struct Fixture {
    pub device: SigningKey,
    pub cert: DeviceCert,
    /// The state directory the session keeps its workspaces under.
    pub state: PathBuf,
    // The identity's environment and files, restored and removed last: the session and the app read
    // them while they run.
    _env: tilo_test_env::ScopedEnv,
    pub root: tempfile::TempDir,
}

/// This device's own identity, provisioned as a session provisions it, holding [`WORKSPACE`].
pub fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("a directory for the identity");
    let xdg = root.path().join("config");
    let system = root.path().join("system");
    let state = root.path().join("state");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", &xdg)
        .set("TILO_IDENTITY_ROOT", &system)
        .set(FILE_SECRETS_ENV, "1");
    let personal = xdg.join("tilo/identity");
    let paths = IdentityPaths::under(&system);
    tilo_identity::provisioning::ensure_collaboration_identity(
        &personal,
        &paths,
        IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let _profile = tilo_identity::store::identity_for_workspace(WORKSPACE)
        .expect("the identity registry")
        .expect("the workspace is answered by this device's person");
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

/// This computer's copy of the workspace `workspace` under `state`, held as `cert`'s device in a
/// workspace the same device governs: the state a session started on this directory finds waiting
/// for it.
pub fn workspace_held_here(state: &Path, workspace: &str, cert: &DeviceCert, key: &SigningKey) {
    let owner = owner::open_file(
        &tilo_session_client::engine::owner_path(state).expect("this device's state directory"),
    )
    .expect("an owner for this device's workspace");
    let governor = testing::enroll_interactive_in(&owner, key, cert, workspace)
        .expect("this device governs the workspace it hosts");
    // The governor's checkpoint now names this device: the host admits it as the publication a
    // host's own workspace makes.
    let host = governor.device().device_public_key;
    let frames: Vec<Vec<u8>> = governor.outgoing().map(|(bytes, _)| bytes).collect();
    let id = WorkspaceId::parse(workspace).expect("a workspace id");
    for frame in frames {
        owner
            .receive(
                id,
                TransportOrigin::RetainedServerDelta(host),
                postcard::from_bytes(&frame).expect("a publication is a message"),
                SystemTime::now(),
            )
            .expect("the host admits the workspace's people");
    }
}

/// Found `workspace` under `name` on the workspace service this device's state keeps under `state`,
/// as a session founds a workspace the person makes here: the service holds it, this device's
/// person is its owner, and this device's own copy of it is enrolled in the owner store the session
/// opens. What the session asks of the workspace's governance — an invitation into it above all —
/// is answered by that service from then on. `then` runs on the service once the workspace is
/// founded there: what the person did in it before the session started.
pub fn workspace_founded_on_this_device_then(
    state: &Path,
    workspace: &str,
    name: &str,
    cert: &DeviceCert,
    key: &SigningKey,
    then: impl AsyncFnOnce(&tilo_workspace_service::service::WorkspaceService),
) {
    let root = state.join("tilo").join("workspace-service");
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
        // The service writes under a key of its own beside the device's, and its member copies
        // act as the device, as the session's own service does when it opens this store again.
        let signer: Arc<dyn collab_types::signer::DeviceSigner> = Arc::new(key.clone());
        let identity =
            tilo_workspace_service::service::ServiceIdentity::beside_device(&root, cert, &*signer)
                .await
                .expect("this device certifies its service's own key");
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

/// What an app presents to join the workspace's channel: the certificate, and the device's proof
/// that it is the certificate's.
pub fn presentation(device: &SigningKey, cert: &DeviceCert) -> Vec<u8> {
    runtime_collab::CertifiedJoinIdentity {
        workspace_id: WORKSPACE.to_owned(),
        device_certificate: serde_json::to_vec(cert).expect("a certificate is JSON"),
        proof: device
            .sign(&tilo_identity_client::collaboration_join_digest(WORKSPACE).unwrap())
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("a presentation encodes")
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

/// A session of this computer taking part in the peer network, keeping what it holds under
/// `state` , on the shell showing [`WORKSPACE`] owned by the person whose root key is `root`.
pub fn hosted_session(state: &Path, root: &[u8; 32]) -> Session {
    let mut session = super::headless_on(
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        Options {
            collab: true,
            person: "Ada".to_owned(),
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
    );
    session
        .handle()
        .replace_tree(layout(WORKSPACE, &actor(root)));
    for _ in 0..4 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    session
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
    let identity_dir = home.config().join("tilo").join("identity");
    tilo_identity::provisioning::ensure_collaboration_identity(
        &identity_dir,
        &tilo_device_identity::IdentityPaths::under(&root),
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("this computer's identity");
    home.env().set("TILO_IDENTITY_ROOT", &root);
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
