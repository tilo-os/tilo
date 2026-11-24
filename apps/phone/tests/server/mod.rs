//! The person's server and the apps it is given, as the phone's tests stand them up: a
//! workspace host on this machine's loopback keeping the person's workspace, and Notes built
//! from the tree into a signed `.tapp`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failure here is the test's failure; each test uses part of it"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use iroh::Endpoint;
use iroh::protocol::Router;
use tilo_device_identity::login::Entry;
use tilo_peer_state::workspace_membership::{AppPayload, AppRelease, WorkspaceMembershipCommand};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::service::{ServiceConfig, ServiceIdentity, WorkspaceService};

use crate::support;

/// What the person calls the workspace the phone signs in to.
pub const WORKSPACE_NAME: &str = "Studio";

/// The person's server: a workspace host on this machine's loopback keeping the person's
/// workspace, which they own.
pub struct Server {
    pub runtime: tokio::runtime::Runtime,
    host: Arc<WorkspaceService>,
    cert: DeviceCert,
    endpoint: Endpoint,
    /// The person's root key.
    pub person: ed25519_dalek::SigningKey,
    /// The person's workspace, by id.
    pub workspace: String,
    _serving: (Router, tempfile::TempDir),
}

impl Server {
    pub fn start() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("person-server")
            .enable_all()
            .build()
            .unwrap();
        let person = support::key(7);
        let (host, cert, endpoint, serving, workspace) = runtime.block_on(async {
            let server_key = support::key(42);
            let cert = support::certified(&person, &server_key, "the server", false);
            let endpoint = support::endpoint(&server_key).await;
            let state = tempfile::tempdir().unwrap();
            let host = Arc::new(
                WorkspaceService::open(
                    ServiceConfig {
                        reaching: tilo_workspace_service::Reaching::OnItsOwn,
                        ..ServiceConfig::new(
                            state.path().join("server"),
                            tilo_server_protocol::WorkSetting::own_person(),
                        )
                    },
                    ServiceIdentity {
                        user_public_key: person.verifying_key().to_bytes(),
                        device_signing_key: server_key,
                        device_cert: cert.clone(),
                        revocations: tilo_user_identity::RevocationList::new(),
                    },
                    endpoint.clone(),
                    None,
                )
                .await
                .unwrap(),
            );
            let router = host
                .protocols()
                .into_iter()
                .fold(
                    Router::builder(endpoint.clone()),
                    |router, (alpn, handler)| router.accept(alpn, handler),
                )
                .spawn();
            let workspaces = host.workspaces();
            let made = workspaces.create(WORKSPACE_NAME).await.unwrap();
            workspaces
                .declare_owner(&made.workspace, person.verifying_key().to_bytes(), None)
                .await
                .unwrap();
            (host, cert, endpoint, (router, state), made.workspace)
        });
        Self {
            runtime,
            host,
            cert,
            endpoint,
            person,
            workspace,
            _serving: serving,
        }
    }

    /// Where a device the person's laptop lets in enters: this server's workspace.
    pub fn entry(&self) -> Entry {
        Entry {
            workspace: self.workspace.clone(),
            workspace_name: Some(WORKSPACE_NAME.into()),
            server_cert: self.cert.clone(),
            server: tilo_api::daemon::enter::reached_of(&self.endpoint.addr()),
            site: "https://tilo.test/".into(),
            host_origin: "https://server.tilo.test".into(),
        }
    }

    /// Install the app `tapp` holds in the person's workspace, as `tilo-app push` does: its
    /// file kept by the workspace's host, and the workspace's governance naming it as the version
    /// every member runs (`workspace/apps#install`). A tilolang app publishes no surface.
    pub fn install(&self, tapp: &Path) {
        let bytes = std::fs::read(tapp).unwrap();
        let read = tilo_app_package::tapp::read(
            std::io::BufReader::new(bytes.as_slice()),
            &mut tilo_app_package::blobs::DiscardBlobs,
        )
        .unwrap();
        let manifest = &read.meta.manifest.package;
        let workspaces = self.host.workspaces();
        let file = workspaces.keep_app_file(&self.workspace, &bytes).unwrap();
        let release = AppRelease {
            app: manifest.id.clone(),
            version: manifest.version.clone(),
            surface: String::new(),
            package: hex::encode(read.meta.id.0),
            publisher: hex::encode(read.meta.root.publisher_identity),
            file: Some(file),
            dev: false,
            pinned: false,
            payload: AppPayload::Tapp,
        };
        self.runtime
            .block_on(workspaces.govern_app(
                &self.workspace,
                &self.cert,
                [3; 32],
                WorkspaceMembershipCommand::InstallApp(release),
            ))
            .unwrap_or_else(|error| panic!("the workspace takes {}: {error}", manifest.id));
    }
}

/// The tree's tilolang apps.
pub fn tl_apps() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tl")
}

/// Notes, `apps/tl/notes`, built into a signed `.tapp` under `scratch`: the file a person installs.
pub fn notes_tapp(scratch: &Path) -> PathBuf {
    use tilolang_cli::package::{self, BuildOptions};
    // A build writes each package's ids lock and source catalogue: it builds a copy, never the
    // tree it runs from.
    let source = scratch.join("tl");
    let dirs = tilolang_cli::package_dirs(&tl_apps().join("notes"), &[tl_apps()]).unwrap();
    for dir in &dirs {
        copy_tree(dir, &source.join(dir.file_name().unwrap()));
    }
    let publisher = tilolang_cli::identity_at(&scratch.join("publisher.json")).unwrap();
    let built = package::build(
        &BuildOptions {
            dir: source.join("notes"),
            search: vec![source.clone(), tl_apps()],
            out: scratch.join("tapps"),
            tilo_runtime: package::DEFAULT_TILO_RUNTIME.to_owned(),
            tilolang_runtime: package::DEFAULT_TILOLANG_RUNTIME.to_owned(),
            version: "0.1.0".to_owned(),
            locked: false,
        },
        &publisher,
    )
    .unwrap_or_else(|error| panic!("Notes builds: {error}"));
    // The build answers the packages the app is made with first and the app last: the app is
    // what a person installs.
    built.last().expect("Notes is built").tapp.clone()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
