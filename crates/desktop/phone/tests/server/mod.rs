//! The person's server and the apps it is given, as the phone's tests stand them up: a host on
//! this machine's loopback keeping the person's workspace — the server a phone's daemon lists the
//! person's workspaces from, and enters each where that list says it is kept — and Notes built
//! from the tree into a signed `.tapp`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failure here is the test's failure; each test uses part of it"
)]

use std::path::{Path, PathBuf};

use tilo_device_identity::login::Entry;
use tilo_peer_state::workspace_membership::{AppPayload, AppRelease, WorkspaceMembershipCommand};

use crate::support;

/// What the person calls the workspace the phone signs in to.
pub const WORKSPACE_NAME: &str = "Studio";

/// The person's server: a host on this machine's loopback keeping the person's workspace, which
/// they own, and the runtime the laptop that prints the phone's ticket runs on.
pub struct Server {
    pub runtime: tokio::runtime::Runtime,
    host: test_server::Host,
    /// The person's root key.
    pub person: ed25519_dalek::SigningKey,
    /// The person's workspace, by id.
    pub workspace: String,
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
        let host = test_server::Host::of(&person, WORKSPACE_NAME);
        let workspace = host.workspace().to_owned();
        // The person's own workspace, where their list of workspaces and the record of their
        // devices are kept, as a server keeps it for them; the workspace they signed the phone in
        // to is named there, as it reaches every device of theirs.
        runtime.block_on(async {
            tilo_workspace_service::workspaces::person_workspace::found(host.workspaces())
                .await
                .expect("the person's own workspace is founded on their server");
            host.workspaces()
                .list_known_for(&person.verifying_key().to_bytes())
                .await;
        });
        Self {
            runtime,
            host,
            person,
            workspace,
        }
    }

    /// Where this server keeps the person's workspaces: the person's own, where their devices are
    /// listed, among them.
    pub fn workspaces(&self) -> tilo_workspace_service::workspaces::Workspaces {
        self.host.workspaces().clone()
    }

    /// Where a device the person's laptop lets in enters: this host's workspace, at this host.
    pub fn entry(&self) -> Entry {
        Entry {
            workspace: self.workspace.clone(),
            workspace_name: Some(WORKSPACE_NAME.into()),
            server_cert: self.host.host_hint().host,
            server: tilo_api::daemon::enter::reached_of(&self.host.addr()),
            site: "https://tilo.test/".into(),
            host_origin: "https://server.tilo.test".into(),
        }
    }

    /// The relay this host answers through, which the phone's daemon is told to answer through
    /// too: every device of the flow is found there, on this machine's loopback.
    pub fn relay_url(&self) -> String {
        self.host
            .addr()
            .relay_urls()
            .next()
            .expect("the host names its packet relay")
            .to_string()
    }

    /// Install the app `tapp` holds in the person's workspace, as `tilo-app push` does: its file
    /// kept by the workspace's host, and the workspace's governance naming it as the version
    /// every member runs (`workspace/apps#install`). A tilolang app publishes no surface.
    pub fn install(&self, tapp: &Path) {
        let bytes = std::fs::read(tapp).unwrap();
        let read = tilo_app_package::tapp::read(
            std::io::BufReader::new(bytes.as_slice()),
            &mut tilo_app_package::blobs::DiscardBlobs,
        )
        .unwrap();
        let manifest = read.meta.manifest.package.clone();
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
                &self.host.host_hint().host,
                [3; 32],
                WorkspaceMembershipCommand::InstallApp(release),
            ))
            .unwrap_or_else(|error| panic!("the workspace takes {}: {error}", manifest.id));
    }
}

/// The tree's tilolang apps.
pub fn tl_apps() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl")
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
