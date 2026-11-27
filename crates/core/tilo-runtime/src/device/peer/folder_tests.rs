//! A logged-in Linux computer's FUSE folder, both ways, across a restart.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; the server and the device stay up until it ends"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use fs_protocol::TreeChunk;
use fs_protocol::tree::{FILES_APP, ROOT_ID, Tree};
use tilo_peer_state::workspace_documents::DeviceWrites as _;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList};
use tilo_workspace_service::device_documents::ServiceWrites;
use tilo_workspace_service::workspaces::Workspaces;

use super::Peer;
use crate::device::account::Account;
use crate::device::workspaces::ServiceWorkspaces;
use tilo_api::daemon::folders::{Folder, Folders};

/// A device `root` certified, and its key.
fn enrol(
    root: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let cert = tilo_peer_state::fixture::labelled(root, &key, label, capabilities);
    (key, cert)
}

/// The route record `chunk` of `workspace`'s tree is kept under.
fn tree_route(workspace: &str, chunk: TreeChunk) -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: FILES_APP.into(),
        cluster_id: workspace.to_owned(),
        document: fs_protocol::tree::chunk_document_id(chunk),
    }
}

/// The records of `workspace`'s tree the server keeps, by chunk, each saved whole.
async fn server_records(server: &Workspaces, workspace: &str) -> BTreeMap<TreeChunk, Vec<u8>> {
    let mut records = BTreeMap::new();
    let Some(index) = server
        .resource_document(workspace, FILES_APP, &fs_protocol::tree::document_id())
        .await
        .unwrap()
    else {
        return records;
    };
    let listed = fs_protocol::tree::listed_chunks(&automerge::AutoCommit::load(&index).unwrap());
    records.insert(0, index);
    for chunk in listed {
        if let Some(record) = server
            .resource_document(
                workspace,
                FILES_APP,
                &fs_protocol::tree::chunk_document_id(chunk),
            )
            .await
            .unwrap()
        {
            records.insert(chunk, record);
        }
    }
    records
}

/// The tree the server keeps, as its records read.
async fn server_tree(server: &Workspaces, workspace: &str) -> Tree {
    let mut tree = fs_protocol::TreeChunks::default();
    for (chunk, record) in server_records(server, workspace).await {
        let record = automerge::AutoCommit::load(&record).unwrap();
        if chunk == 0 {
            tree.read_index(&record);
        } else {
            tree.read_chunk(chunk, &record);
        }
    }
    tree.tree()
}

/// The content address and size the server's tree names `name` at the top of the folder by.
async fn server_file(server: &Workspaces, workspace: &str, name: &str) -> Option<(String, u64)> {
    let tree = server_tree(server, workspace).await;
    tree.children(ROOT_ID)
        .into_iter()
        .find(|(_, node)| node.name.as_bytes() == name.as_bytes())
        .and_then(|(_, node)| Some((node.hash.clone()?, node.size)))
}

/// Put a file called `name` holding `bytes` into the server's tree, as another member's device
/// would: its content kept in the server's store, chunked as the tree names files, and the
/// tree's records changed by the server, each sealed with the keys of the tree's group.
async fn put_on_server(
    server: &crate::device::keep::server::Server,
    workspace: &str,
    name: &str,
    bytes: &[u8],
) {
    let store = server.content().get(workspace).unwrap();
    let (manifest, _) = tilo_fs::content::build(bytes, None);
    for (index, chunk) in manifest.chunks().iter().enumerate() {
        let range = manifest.chunk_range(index).unwrap();
        let piece =
            &bytes[usize::try_from(range.start).unwrap()..usize::try_from(range.end).unwrap()];
        assert_eq!(store.put_public(piece).unwrap(), hex::encode(chunk.hash));
    }
    let address = store.put_public(&manifest.encode()).unwrap();
    put_in_tree(
        server.workspaces().unwrap(),
        workspace,
        name,
        (&address, bytes.len() as u64),
    )
    .await;
}

/// Add a file `name` at content `(address, size)` to `workspace`'s tree through `workspaces`, as
/// this device would: each of the tree's records changed, sealed with the keys of its group.
async fn put_in_tree(
    workspaces: &Workspaces,
    workspace: &str,
    name: &str,
    (address, size): (&str, u64),
) {
    let records = server_records(workspaces, workspace).await;
    let held: BTreeSet<TreeChunk> = records.keys().copied().collect();
    // The server is a device of the person's, and a mount acts as the person who mounted it:
    // every change of the tree names them.
    let device = workspaces.device_cert();
    let mut tree = tilo_fs::Namespace::new(tilo_fs::namespace::MountAuthor::person(
        workspace,
        hex::encode(device.device_public_key),
        tilo_actions::MemberId::person(tilo_actions::PersonId::from_root(device.user_public_key)),
    ))
    .unwrap();
    if !records.is_empty() {
        tree.take_documents(records.into_iter().collect()).unwrap();
    }
    let known = if held.is_empty() {
        BTreeMap::new()
    } else {
        tree.document_heads()
    };
    let hash: iroh_blobs::Hash = address.parse().unwrap();
    tree.create_file(tilo_fs::namespace::ROOT_INO, name, Some(&hash), size, 1)
        .unwrap();
    let writes = tree.document_writes(&known);
    let mut ordered: Vec<_> = writes
        .iter()
        .filter(|write| !write.changes.is_empty())
        .collect();
    // The chunks first, the tree's own record that names them last.
    ordered.sort_by_key(|write| write.chunk == 0);
    for write in ordered {
        let route = tree_route(workspace, write.chunk);
        let record = tilo_peer_state::workspace_documents::DocumentWriting {
            app: FILES_APP,
            kind: Some(fs_protocol::tree::TREE_KIND),
            record: &route.document,
        };
        ServiceWrites::of(workspaces.clone())
            .write(workspace, &write.changes, record)
            .await
            .map_or_else(|refusal| panic!("the tree changes: {refusal:?}"), drop);
    }
}

/// Wait until the file at `path` holds `bytes`.
async fn holds(path: &Path, bytes: &[u8], what: &str) {
    tilo_test_env::within(what, async {
        loop {
            if std::fs::read(path).is_ok_and(|held| held == bytes) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
}

/// Wait until the server's tree names `name` with `bytes`' size and its store holds the file's
/// content: its manifest and every chunk it lists.
async fn reaches_server(
    server: &crate::device::keep::server::Server,
    workspace: &str,
    name: &str,
    bytes: &[u8],
    what: &str,
) {
    let workspaces = server.workspaces().unwrap();
    let store = server.content().get(workspace).unwrap();
    tilo_test_env::within(what, async {
        loop {
            if let Some((address, size)) = server_file(workspaces, workspace, name).await
                && size == bytes.len() as u64
                && let Some(manifest) = store.read(&address)
                && let Ok(manifest) = tilo_fs::content::Manifest::decode(&manifest)
                && manifest.total_len() == bytes.len() as u64
                && manifest
                    .chunks()
                    .iter()
                    .all(|chunk| store.read(&hex::encode(chunk.hash)).is_some())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
}

/// The computer as `tilo login` leaves it running: its own workspace service, which the login's
/// session entered the workspace through, the person's account at their server, and the peer
/// keeping the person's folders with them.
struct Computer {
    own: Arc<ServiceWorkspaces>,
    entered: tilo_session_client::device::Device,
    peer: Arc<Peer>,
    keeping: tokio::task::JoinHandle<()>,
}

impl Computer {
    /// Start the computer kept below `dir`, its folders below `home`.
    async fn start(
        dir: &Path,
        home: &Path,
        (key, cert): (&SigningKey, &DeviceCert),
        login: &tilo_device_identity::login::Device,
    ) -> Self {
        let server = tilo_api::daemon::enter::server_of(login).await.unwrap();
        // The last run's service lets go of its store as its tasks end.
        let own = tilo_test_env::within("the computer's own service opens", async {
            loop {
                match ServiceWorkspaces::open_for_test(&dir.join("own"), key, cert, &server).await {
                    Ok(own) => return own,
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        })
        .await;
        let lent = Some(own.lent());
        let entered = tilo_api::daemon::enter::connect(key, login, lent)
            .await
            .unwrap();
        let address = login.server_address.as_ref().unwrap();
        let account = Arc::new(Account::at(&own, &address.endpoint_addr, &login.workspace));
        let peer = Arc::new(Peer::open(dir, home).await.unwrap());
        let login = Arc::new(crate::device::home::Home::new(
            Arc::clone(&own),
            cert.clone(),
            Some(login.clone()),
        ));
        let keeping = tokio::spawn(Arc::clone(&peer).keep(
            Arc::clone(&own),
            move || -> crate::device::peer::Accounts {
                let account = Arc::clone(&account);
                Box::pin(async move { Some(Some(account)) })
            },
            login,
        ));
        Self {
            own,
            entered,
            peer,
            keeping,
        }
    }

    /// Whether the folder's tree on this computer names `name` at its top.
    async fn names(&self, name: &str) -> bool {
        let held = self.peer.held.lock().await;
        held.iter().any(|shown| {
            shown
                .replica
                .namespace
                .lock()
                .unwrap()
                .lookup(tilo_fs::namespace::ROOT_INO, name)
                .is_some()
        })
    }

    /// Stop as the holder stops: the folders kept on disk, the store and the service let go.
    async fn stop(self) {
        drop(self.entered);
        self.keeping.abort();
        let _ = self.keeping.await;
        self.peer.stop(Duration::from_secs(20)).await;
        self.peer.shutdown(Duration::from_secs(5)).await;
        drop(self.peer);
        self.own.shutdown().await;
    }
}

/// The computer's login, as `tilo login` left it, naming `server`'s address: the device whose
/// certificate is `laptop`, in `workspace`.
fn login_to(
    server: &crate::device::keep::server::Server,
    workspace: &str,
    (laptop_cert, server_cert): (&DeviceCert, &DeviceCert),
) -> tilo_device_identity::login::Device {
    tilo_device_identity::login::Device {
        site: "https://tilo.test/".into(),
        host_origin: "https://eu1.tilo.test".into(),
        workspace: workspace.to_owned(),
        workspace_name: Some("Studio".into()),
        agent: "Claude Code".into(),
        cert: laptop_cert.clone(),
        server_cert: server_cert.clone(),
        server_address: Some(tilo_server_protocol::web::ServerAddress {
            endpoint_addr: server.node_addr(),
            relay_url: None,
            web: None,
        }),
    }
}

/// Give `workspace` a folder below `home`, as `tilo folders` keeps it in the device's directory
/// `dir`: where it is.
fn folder_at(dir: &Path, home: &Path, workspace: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let folder = home.join("Studio");
    Folders {
        workspaces: BTreeMap::from([(
            workspace.to_owned(),
            Folder {
                name: "Studio".into(),
                path: folder.clone(),
                paused: false,
                removed: false,
            },
        )]),
        ..Folders::default()
    }
    .save(dir)
    .unwrap();
    folder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one person's folder from its first sync through a restart, read top to bottom"
)]
async fn a_mounted_folder_follows_the_server_both_ways_across_a_restart() {
    tilo_test_env::require_capability!(
        Fuse,
        tilo_fs::fuse_session::mountable(
            Path::new("/dev/fuse"),
            std::env::var_os("PATH").as_deref()
        ),
        "a workspace's folder is a FUSE mount"
    );
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "home-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = crate::device::keep::server::KeptServer::start(
        test_server::on_loopback_relay(tilo_server_protocol::config::ServerConfig::loopback(
            state.path().join("server"),
        )),
        crate::device::keep::server::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server holds workspaces")
        .clone();
    let workspace = hosted.create("Studio").await.unwrap().workspace;
    // The server holds a file before the computer ever syncs.
    put_on_server(
        &server,
        &workspace,
        "contract.txt",
        b"Net 30, signed by both.\n",
    )
    .await;

    let login = login_to(&server, &workspace, (&laptop_cert, &server_cert));
    let (dir, home) = (state.path().join("laptop"), state.path().join("home"));
    let folder = folder_at(&dir, &home, &workspace);
    let keys = (&laptop_key, &laptop_cert);
    // The person writes into the folder before the workspace is ever shown there.
    std::fs::create_dir_all(folder.join("drafts")).unwrap();
    std::fs::write(folder.join("before.txt"), b"Written before the mount\n").unwrap();
    std::fs::write(folder.join("drafts/outline.txt"), b"1. Intro\n").unwrap();

    let computer = Computer::start(&dir, &home, keys, &login).await;
    // What was there is in the workspace, through the mount and on the server, never hidden
    // underneath it.
    holds(
        &folder.join("drafts/outline.txt"),
        b"1. Intro\n",
        "a file written into a folder of it before the mount is in the mounted folder",
    )
    .await;
    reaches_server(
        &server,
        &workspace,
        "before.txt",
        b"Written before the mount\n",
        "a file written before the mount reaches the server",
    )
    .await;
    // Server to folder.
    holds(
        &folder.join("contract.txt"),
        b"Net 30, signed by both.\n",
        "the server's file appears in the folder",
    )
    .await;
    // Folder to server.
    std::fs::write(folder.join("agenda.txt"), b"1. Budget\n2. Hiring\n").unwrap();
    reaches_server(
        &server,
        &workspace,
        "agenda.txt",
        b"1. Budget\n2. Hiring\n",
        "a file written in the folder reaches the server",
    )
    .await;
    let written_by = hex::encode(root.verifying_key().to_bytes());
    let agenda = server_tree(&hosted, &workspace)
        .await
        .children(ROOT_ID)
        .into_iter()
        .find(|(_, node)| node.name.as_bytes() == b"agenda.txt")
        .map(|(_, node)| node.author.clone())
        .expect("the server's tree holds the file the folder wrote");
    assert_eq!(
        agenda.as_deref(),
        Some(written_by.as_str()),
        "history names its writer"
    );
    // An edit the computer took in just before it stops, which need not have gone out yet.
    std::fs::write(
        folder.join("late.txt"),
        b"Written just before the restart\n",
    )
    .unwrap();
    tilo_test_env::within("the folder's tree takes in the late edit", async {
        while !computer.names("late.txt").await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    computer.stop().await;
    // While the workspace is not shown, the folder refuses a write rather than keeping it where
    // the next mount would hide it. Root writes whatever a folder's mode says.
    if rustix::process::geteuid().as_raw() != 0 {
        let refused = std::fs::write(folder.join("unmounted.txt"), b"lost?\n")
            .expect_err("the folder refuses a write while the workspace is not shown");
        assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    }

    let computer = Computer::start(&dir, &home, keys, &login).await;
    reaches_server(
        &server,
        &workspace,
        "late.txt",
        b"Written just before the restart\n",
        "the edit made before the restart reaches the server",
    )
    .await;
    reaches_server(
        &server,
        &workspace,
        "agenda.txt",
        b"1. Budget\n2. Hiring\n",
        "the folder's file is still the server's after the restart",
    )
    .await;
    holds(
        &folder.join("contract.txt"),
        b"Net 30, signed by both.\n",
        "the server's file is still in the folder after the restart",
    )
    .await;
    // And the server's changes still arrive.
    put_on_server(&server, &workspace, "minutes.txt", b"Approved.\n").await;
    holds(
        &folder.join("minutes.txt"),
        b"Approved.\n",
        "a file the server holds after the restart appears in the folder",
    )
    .await;
    let tree = server_tree(&hosted, &workspace).await;
    let names: BTreeSet<String> = tree
        .children(ROOT_ID)
        .into_iter()
        .map(|(_, node)| String::from_utf8_lossy(node.name.as_bytes()).into_owned())
        .collect();
    assert_eq!(
        names,
        BTreeSet::from(
            [
                "agenda.txt",
                "before.txt",
                "contract.txt",
                "drafts",
                "late.txt",
                "minutes.txt"
            ]
            .map(str::to_owned)
        ),
        "one tree, holding every file once"
    );
    // Another writer on this computer changes a tree record directly: its write names the record,
    // so the folder reads it without a sweep.
    let (address, size) = server_file(&hosted, &workspace, "contract.txt")
        .await
        .expect("the server's tree names the file");
    put_in_tree(
        computer.own.copies(),
        &workspace,
        "copy.txt",
        (&address, size),
    )
    .await;
    holds(
        &folder.join("copy.txt"),
        b"Net 30, signed by both.\n",
        "another local writer's change reaches the folder",
    )
    .await;

    computer.stop().await;
    server.shutdown().await;
}

/// A workspace made on another device of the person's, after this computer started, reaches it
/// through their server's account: the computer enters it where the server says it is kept,
/// gives it a folder, and the folder syncs both ways.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_workspace_made_elsewhere_gets_a_folder_that_syncs() {
    tilo_test_env::require_capability!(
        Fuse,
        tilo_fs::fuse_session::mountable(
            Path::new("/dev/fuse"),
            std::env::var_os("PATH").as_deref()
        ),
        "a workspace's folder is a FUSE mount"
    );
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "home-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = crate::device::keep::server::KeptServer::start(
        test_server::on_loopback_relay(tilo_server_protocol::config::ServerConfig::loopback(
            state.path().join("server"),
        )),
        crate::device::keep::server::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server holds workspaces")
        .clone();
    // The workspace the computer logged in to, which its login entered.
    let studio = hosted.create("Studio").await.unwrap().workspace;
    let login = login_to(&server, &studio, (&laptop_cert, &server_cert));
    let (dir, home) = (state.path().join("laptop"), state.path().join("home"));
    let computer = Computer::start(&dir, &home, (&laptop_key, &laptop_cert), &login).await;
    put_on_server(&server, &studio, "brief.txt", b"Studio brief\n").await;
    holds(
        &home.join("Studio").join("brief.txt"),
        b"Studio brief\n",
        "the workspace logged in to gets a folder",
    )
    .await;
    // Another device of the person's makes a workspace at their server.
    let garden = hosted.create("Garden").await.unwrap().workspace;
    put_on_server(&server, &garden, "seeds.txt", b"Tomatoes, basil\n").await;
    // The person's own workspace, which holds their list of workspaces, is founded on their
    // device when their identity is made, and that device hands it to the server it logs in at
    // without a ticket. The server writes nothing of theirs: it answers the workspaces it keeps
    // for them, and the laptop lists those under its own key, where its account reads them.
    let person = tilo_test_env::within(
        "the laptop founds the person's own workspace",
        tilo_workspace_service::workspaces::person_workspace::found(computer.own.copies()),
    )
    .await
    .expect("the person's own workspace");
    tilo_test_env::within(
        "the server takes the person's own workspace",
        computer.own.copies().attach_server(
            &person,
            server_cert.device_public_key,
            server.node_addr(),
            tilo_peer_state::CopyRole::KeyHolder,
        ),
    )
    .await
    .expect("the server keeps the person's own workspace");
    holds(
        &home.join("Garden").join("seeds.txt"),
        b"Tomatoes, basil\n",
        "the workspace made elsewhere gets a folder holding its files",
    )
    .await;
    assert!(
        computer
            .own
            .keeps(tilo_server_protocol::WorkspaceId::parse(&garden).unwrap())
            .await,
        "the computer's own service keeps the new workspace"
    );
    std::fs::write(home.join("Garden").join("rows.txt"), b"Three rows\n").unwrap();
    reaches_server(
        &server,
        &garden,
        "rows.txt",
        b"Three rows\n",
        "a file written in the new folder reaches the server",
    )
    .await;

    computer.stop().await;
    server.shutdown().await;
}
