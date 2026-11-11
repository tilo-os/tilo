//! One person's own identity and session, as a person record's test needs
//! them: the device this machine is, another of their devices certified under
//! their root, a session that serves the person's own store, and a client that
//! speaks on that session's plane as an app does.
//!
//! A person's own records route on the cluster their store derives from their
//! root ([`collab_types::personal_cluster`]), and an app they launch joins a
//! record of theirs there by the kind its surface declares. That cluster is
//! the person's store and no workspace: no device holds or governs one under
//! its id, so a session seals nothing an app writes there, as it would in a
//! workspace it holds.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer as _, SigningKey};
use runtime_collab::CollabClient;
use tilo_identity_host::provisioning::FILE_SECRETS_ENV;
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, Launches, Options, Session};
use tilo_user_identity::{DeviceCert, IdentityPaths};

use super::people::Person;

/// The person's keys and the environment a session finds them in: this
/// machine's identity, as setup provisioned it.
pub struct Personal {
    /// The person, whose root certified this machine and every other device of
    /// theirs.
    pub person: Person,
    /// Where this machine's device key and certificate live.
    pub paths: IdentityPaths,
    /// This machine's key, as the identity store minted it.
    pub device: SigningKey,
    /// This machine's certificate, signed by the person's root.
    pub cert: DeviceCert,
    /// The socket this machine's session serves its plane on.
    socket: PathBuf,
    /// The environment the identity was provisioned in, restored before the
    /// directories it names are removed.
    _env: tilo_test_env::ScopedEnv,
    _runtime: tempfile::TempDir,
}

impl Personal {
    /// The person `name`, on this machine, as their computer's first use
    /// provisions them.
    pub fn new(name: &str) -> Self {
        let person = Person::new(name);
        let runtime = tempfile::tempdir().expect("a runtime directory for this machine");
        // The session serves its socket from a directory no one else may enter,
        // as the runtime directory is.
        let services = runtime.path().join("tilo/services");
        std::fs::create_dir_all(&services).expect("the session's services directory");
        std::fs::set_permissions(&services, std::fs::Permissions::from_mode(0o700))
            .expect("the services directory is this machine's alone");
        std::fs::create_dir_all(person.home()).expect("this machine's home");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_CONFIG_HOME", person.config_home())
            .set("TILO_IDENTITY_ROOT", person.identity_root())
            .set("XDG_RUNTIME_DIR", runtime.path())
            .set("FILE_SECRETS_ENV", FILE_SECRETS_ENV)
            .set("HOME", person.home());
        let paths = IdentityPaths::under(person.identity_root());
        let device =
            tilo_user_identity::persist::load_device_key(&paths).expect("this device's key");
        let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
            .expect("this device's certificate");
        let socket = tilo_paths::xdg::service_socket("collab");
        Self {
            person,
            paths,
            device,
            cert,
            socket,
            _env: env,
            _runtime: runtime,
        }
    }

    /// Another of the person's devices, certified under their root:
    /// [`Other::new`], and recorded in this machine's fleet as the device the
    /// person admitted, so this machine's session answers it as one of theirs.
    pub fn second(&self, label: &str) -> Other {
        let other = Other::new(&self.person, label);
        tilo_identity_host::device_fleet::DeviceFleetStore::from_env()
            .expect("this machine's device fleet")
            .remember_certificate(&hex::encode(other.cert.user_public_key), other.cert.clone())
            .expect("the person's other device is enrolled on this machine");
        other
    }

    /// The cluster the person's own records route on, which their store
    /// carries.
    pub fn cluster(&self) -> String {
        collab_types::personal_cluster(&self.cert.user_public_key).to_string()
    }

    /// The socket this machine's session serves its plane on.
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

/// Another of the person's devices, certified under their root: its own keys,
/// its own certificate, and nothing of this machine's.
pub struct Other {
    /// Where the device's key and certificate live.
    pub paths: IdentityPaths,
    /// The device's key.
    pub device: SigningKey,
    /// The device's certificate, signed by the person's root.
    pub cert: DeviceCert,
    _root: tempfile::TempDir,
}

impl Other {
    /// Certify a device key drawn for `label` under `person`'s root, as the
    /// person admits a device of theirs.
    pub fn new(person: &Person, label: &str) -> Self {
        let root = tempfile::tempdir().expect("a directory for the device's identity");
        let paths = IdentityPaths::under(root.path());
        let cert = person.certify(&paths, label);
        let device =
            tilo_user_identity::persist::load_device_key(&paths).expect("the device's key");
        Self {
            paths,
            device,
            cert,
            _root: root,
        }
    }
}

/// What an app presents to join `cluster`'s channel: the certificate, and the
/// device's proof that it is the certificate's.
pub fn presentation(device: &SigningKey, cert: &DeviceCert, cluster: &str) -> Vec<u8> {
    runtime_collab::CertifiedJoinIdentity {
        workspace_id: cluster.to_owned(),
        device_certificate: serde_json::to_vec(cert).expect("a certificate is JSON"),
        proof: device
            .sign(&runtime_identity::collaboration_join_signing_bytes(cluster))
            .to_bytes()
            .to_vec(),
    }
    .encode()
    .expect("a presentation encodes")
}

/// A session serving `app` on `socket`, on this computer's certified device,
/// with its state at `state`: the desk is the session's own, and the person's
/// records are in no workspace on it. `windowless` records the app's process
/// as an endpoint no window serves, as the files daemon's is.
pub fn session(
    socket: &Path,
    who: &str,
    cluster: &str,
    state: &Path,
    app: &str,
    windowless: bool,
) -> Session {
    // The app speaks from this test's own process, so the launch the session
    // would have recorded for it names that process.
    let launches = std::sync::Arc::new(Launches::default());
    if windowless {
        launches.windowless(std::process::id(), app, cluster);
    } else {
        launches.started(std::process::id(), app, cluster, true);
    }
    let mut session = super::headless(Options {
        collab: true,
        wake: LoopWake::default(),
        action_plane: ActionPlane::At(socket.to_path_buf()),
        person: who.to_owned(),
        launches,
        state: Some(state.to_path_buf()),
        ..Options::default()
    });
    for _ in 0..4 {
        session
            .step(std::time::Duration::from_millis(1))
            .expect("step");
    }
    session
}

/// An app connected to the session's plane as `app` in `cluster`, as the
/// launcher hands it the connection.
pub fn connect(socket: &Path, app: &str, cluster: &str) -> CollabClient {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("the plane's socket");
    launcher::services::write_collab_handoff(&mut stream, app, cluster)
        .expect("the launcher names the app on the connection");
    stream
        .set_nonblocking(true)
        .expect("the connection is ready for the runtime");
    let stream = tokio::net::UnixStream::from_std(stream).expect("a runtime stream");
    let (reader, writer) = stream.into_split();
    let (client, carrying) = CollabClient::over(reader, writer);
    tokio::spawn(carrying);
    client
}

/// Where the session keeps the person's own documents.
pub fn store_path(state: &Path) -> PathBuf {
    state.join("tilo").join("personal").join("documents")
}

/// The channel key of the person's own record of `kind`, as `app` keeps it.
pub fn channel(app: &str, cluster: &str, kind: &str) -> String {
    tilo_action_broker::route::CrdtRoute::record(app, cluster, record(kind)).channel_key()
}

/// The record of the person's own that `kind` names.
pub fn record(kind: &str) -> String {
    collab_types::person_record_id(kind)
}
