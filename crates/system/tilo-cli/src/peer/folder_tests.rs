//! A logged-in computer's synced folder runs on its own workspace service: the workspace's tree
//! is a record of the member copy that service holds, and the folder is a plain folder synced
//! both ways (no FUSE). A file the server holds appears in the folder, a file written in the
//! folder reaches the server, and both still hold after the computer's device restarts, an edit
//! made just before the restart included.

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
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};
use tilo_workspace_service::device_documents::ServiceWrites;
use tilo_workspace_service::workspaces::Workspaces;

use super::Peer;
use super::folders::{Folder, Folders};
use crate::own_device::OwnDevice;

/// A device `root` certified, and its key.
fn enrol(
    root: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
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
    server: &tilo_server::daemon::Server,
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
    let mut tree = tilo_fs::Namespace::new("home-server").unwrap();
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
        let keys = tilo_test_env::within("the device holds the tree's keys", async {
            loop {
                if let Some(Ok(keys)) = workspaces.own_route_keys(&route, true).await {
                    return keys;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        let mut record = tilo_peer_state::workspace_documents::DocumentWriting {
            app: FILES_APP,
            document: &route.document,
            readers: None,
            founding: !held.contains(&write.chunk),
            snapshot: !held.contains(&write.chunk),
            sealed: Some(&keys),
        };
        let writes = ServiceWrites::of(workspaces.clone());
        if record.founding {
            // The whole record first: the workspace must not hold it yet.
            writes
                .write(workspace, &automerge::AutoCommit::new().save(), record)
                .await
                .map_or_else(
                    |refusal| panic!("a tree record is founded: {refusal:?}"),
                    drop,
                );
            record.founding = false;
            record.snapshot = false;
        }
        writes
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
    server: &tilo_server::daemon::Server,
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

/// The computer as `tilo login` leaves it running: its own workspace service, the session its
/// login holds over that service's endpoint, and the peer showing the person's folders.
struct Computer {
    own: Arc<OwnDevice>,
    entered: tilo_session_client::device::Device,
    peer: Peer,
}

impl Computer {
    /// Start the computer kept below `dir`, its folders below `home`.
    async fn start(
        dir: &Path,
        home: &Path,
        (key, cert): (&SigningKey, &DeviceCert),
        login: &crate::login::Device,
    ) -> Self {
        let server = crate::login::server_of(login).await.unwrap();
        // The last run's service lets go of its store as its tasks end.
        let own = tilo_test_env::within("the computer's own service opens", async {
            loop {
                match OwnDevice::open(&dir.join("own"), key, cert, &server).await {
                    Ok(own) => return Arc::new(own),
                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
        })
        .await;
        let entered = tilo_test_env::within(
            "the login enters its workspace",
            crate::login::connect(key, login, Some(own.lent())),
        )
        .await
        .expect("the login enters");
        tilo_test_env::within("the service holds the workspace", own.hold(&entered)).await;
        let peer = Peer::open(dir, home, false).await.unwrap();
        tilo_test_env::within(
            "the folders are shown",
            peer.reconcile(&entered, Some(&own)),
        )
        .await;
        assert!(
            peer.failures().is_empty(),
            "every folder is shown: {:?}",
            peer.failures()
        );
        Self { own, entered, peer }
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
        self.peer.stop(Duration::from_secs(20)).await;
        self.peer.shutdown(Duration::from_secs(5)).await;
        drop(self.peer);
        self.entered.close();
        drop(self.entered);
        let own = Arc::try_unwrap(self.own)
            .unwrap_or_else(|_| panic!("nothing else holds the computer's service"));
        own.shutdown().await;
    }
}

/// The computer's login, as `tilo login` left it, naming `server`'s address: the device whose
/// certificate is `laptop`, in `workspace`.
fn login_to(
    server: &tilo_server::daemon::Server,
    workspace: &str,
    (laptop_cert, server_cert): (&DeviceCert, &DeviceCert),
) -> crate::login::Device {
    crate::login::Device {
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
async fn a_synced_folder_follows_the_server_both_ways_across_a_restart() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "home-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig {
            state_root: state.path().join("server"),
            ..Default::default()
        },
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server holds workspaces")
        .clone();
    let workspace = hosted.create("Studio").await.unwrap().workspace;
    hosted
        .enter(&workspace, &laptop_cert)
        .await
        .expect("a device of the person enters the workspace");
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

    let computer = Computer::start(&dir, &home, keys, &login).await;
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
            ["agenda.txt", "contract.txt", "late.txt", "minutes.txt"].map(str::to_owned)
        ),
        "one tree, holding every file once"
    );
    // Another writer on this computer changes a tree record directly: its write names the record,
    // so the folder reads it without a sweep.
    let (address, size) = server_file(&hosted, &workspace, "contract.txt")
        .await
        .expect("the server's tree names the file");
    put_in_tree(
        computer.own.workspaces(),
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

/// Call `action` of the ticket's provider on the device that issued `ticket`, as the computer
/// with `key` and no certificate, in `scope`: how it ended.
async fn ask_issuer(
    ticket: &tilo_device_identity::login::Ticket,
    key: &SigningKey,
    scope: &str,
    action: &str,
) -> Option<Result<tilo_actions::InvocationUpdate, tilo_actions::remote::ActionCallError>> {
    use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};
    use tilo_device_identity::login::{PROVIDER, RESOURCE};
    let dialed =
        tilo_server_client::ServerAddress::at(crate::login::address_of(&ticket.issuer).unwrap());
    let endpoint = dialed.bind(Some(key.to_bytes())).await.unwrap();
    let service = tilo_peer_state::actions::ActionService::new(
        key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    );
    let device = Arc::new(service.remote(&endpoint, &dialed.addr(), scope));
    let input = tilo_device_identity::login::CertifyInput {
        secret: ticket.secret_hex(),
        device_public_key: hex::encode(key.verifying_key().to_bytes()),
        agent: "Codex".into(),
        label: None,
    };
    let serde_json::Value::Object(argument) = serde_json::to_value(&input).unwrap() else {
        panic!("the input is an object");
    };
    let request = InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(RESOURCE, action),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let settled = device
        .invoke_until_settled(request, Duration::from_secs(10))
        .await;
    endpoint.close().await;
    settled
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one person adding a computer, from the ticket to a file read on it, read top to bottom"
)]
async fn a_computer_logged_in_with_a_ticket_reads_what_another_device_wrote() {
    use tilo_actions::remote::ActionCallError;
    use tilo_device_identity::login::{CERTIFY, SCOPE};

    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (server_key, server_cert) = enrol(&root, "home-server", DeviceCapabilities::member());
    let (laptop_key, laptop_cert) = enrol(&root, "laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let server = tilo_server::daemon::Server::start_with_relay_mode(
        tilo_server::ServerConfig {
            state_root: state.path().join("server"),
            ..Default::default()
        },
        tilo_server::daemon::ServerIdentity {
            user_public_key: root.verifying_key().to_bytes(),
            device_signing_key: server_key,
            device_cert: server_cert.clone(),
            revocations: RevocationList::new(),
        },
        iroh::RelayMode::Disabled,
    )
    .await
    .unwrap();
    let hosted = server
        .workspaces()
        .expect("the server holds workspaces")
        .clone();
    let workspace = hosted.create("Studio").await.unwrap().workspace;
    hosted
        .enter(&workspace, &laptop_cert)
        .await
        .expect("a device of the person enters the workspace");

    // The laptop holds the person's root and writes a file in its folder.
    let laptop_login = login_to(&server, &workspace, (&laptop_cert, &server_cert));
    let (laptop_dir, laptop_home) = (
        state.path().join("laptop"),
        state.path().join("laptop-home"),
    );
    let laptop_folder = folder_at(&laptop_dir, &laptop_home, &workspace);
    let laptop = Computer::start(
        &laptop_dir,
        &laptop_home,
        (&laptop_key, &laptop_cert),
        &laptop_login,
    )
    .await;
    let notes = b"Ship on Friday.\n";
    std::fs::write(laptop_folder.join("notes.txt"), notes).unwrap();
    reaches_server(
        &server,
        &workspace,
        "notes.txt",
        notes,
        "the laptop's file reaches the server",
    )
    .await;

    // `tilo device add` on the laptop: a ticket its root approves.
    let issued = crate::device::issue(
        &laptop_login,
        Arc::new(crate::device::RootApprover {
            root: root.clone(),
            device: laptop_key.clone(),
            cert: laptop_cert.clone(),
        }),
    )
    .await
    .unwrap();
    let text = issued.ticket.encode();
    let ticket = tilo_device_identity::login::Ticket::parse(&text).unwrap();
    let desk_key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));

    // A computer with no certificate gets nothing but the certify action.
    let other = ask_issuer(&ticket, &desk_key, SCOPE, "list").await;
    assert!(
        matches!(other, Some(Err(ActionCallError::UnknownAction))),
        "another action of the ticket's provider is refused: {other:?}"
    );
    let in_workspace = ask_issuer(&ticket, &desk_key, &workspace, CERTIFY).await;
    assert!(
        matches!(
            in_workspace,
            Some(Err(
                ActionCallError::NotPermitted | ActionCallError::Refused { .. }
            ))
        ),
        "a call in the person's workspace is refused to a device it does not know: {in_workspace:?}"
    );
    // A wrong secret is refused.
    let mut wrong = ticket.clone();
    wrong.secret = [7; 32];
    let refused = crate::login::certified_with(&wrong, &desk_key, "Codex", "Codex on desk")
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("didn't take this ticket"), "{refused}");

    // `tilo login <ticket>` on the desk.
    let answer = crate::login::certified_with(&ticket, &desk_key, "Codex", "Codex on desk")
        .await
        .expect("the laptop certifies the desk");
    assert!(answer.cert.is_root_signed_device());
    assert_eq!(answer.cert.user_public_key, root.verifying_key().to_bytes());
    assert_eq!(
        answer.cert.device_public_key,
        desk_key.verifying_key().to_bytes()
    );
    assert_eq!(answer.enter.workspace, workspace);
    let given = crate::device::certified(&issued).await.unwrap();
    assert_eq!(
        given, answer.cert,
        "the laptop says which computer it added"
    );
    // The ticket is spent.
    let spent_key = SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(getrandom::SysRng));
    let spent = crate::login::certified_with(&ticket, &spent_key, "Codex", "Codex on desk")
        .await
        .unwrap_err()
        .to_string();
    assert!(spent.contains("didn't take this ticket"), "{spent}");
    issued.end().await;

    // The desk enters as the person's device: it lists their workspaces and holds the laptop's
    // file in its own folder.
    let desk_login = crate::login::device_of(answer.clone(), "Codex");
    let (desk_dir, desk_home) = (state.path().join("desk"), state.path().join("desk-home"));
    let desk_folder = folder_at(&desk_dir, &desk_home, &workspace);
    let desk = Computer::start(
        &desk_dir,
        &desk_home,
        (&desk_key, &answer.cert),
        &desk_login,
    )
    .await;
    let listed = desk
        .entered
        .personal_list(
            &tilo_server_session::workspaces::WorkspaceLabels::new(),
            None,
        )
        .await
        .expect("the desk reads the person's workspaces");
    assert!(
        listed
            .workspaces
            .iter()
            .any(|known| known.workspace == workspace),
        "the desk lists the person's workspace: {:?}",
        listed.workspaces
    );
    holds(
        &desk_folder.join("notes.txt"),
        notes,
        "the laptop's file appears on the desk",
    )
    .await;

    desk.stop().await;
    laptop.stop().await;
    server.shutdown().await;
}
