//! Fixtures for the flows `docs/architecture/flows.md` describes: several devices of one
//! person, driven and read through published actions and views alone.
//!
//! A device here is what any device is to a host: a certificate the person's root signed, a
//! session to the host, and the actions the host serves it. The host is a real one, on this
//! machine's loopback, as a server of the person's or one at tilo.sh is.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    dead_code,
    reason = "in a test these are the assertion, not a hazard; each flow binary reads a different subset"
)]

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT, ReadDoc as _};
use collab_types::DocumentMutation;
use serde_json::{Map, Value};
use tilo_actions::remote::RemoteActionTarget;
use tilo_actions::{ActionAddress, InvocationId};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_server_protocol::WorkspaceId;
use tilo_session_client::device::{Device, DeviceRefusal};
use tilo_session_client::{Cmd, Link, Outbox as _};
use tilo_user_identity::{AgentScope, DeviceCapabilities, DeviceCert, sign_device_cert};

use super::{Device as Certified, TestHost, admitted_to, record_of, start_peer};

/// The app whose document is a workspace's shared desktop, and that document.
pub const DESKTOP_APP: &str = tilo_desktop_model::ownership::DESKTOP_APP;
pub const DESKTOP_DOCUMENT: &str = tilo_desktop_model::ownership::DESKTOP_DOCUMENT;

/// The leaf of the person's settings that arranges their displays across devices.
pub const PLACEMENT_KEY: &str = "tilo.display_person.placement";

/// A session of `device` that enters `workspace` through its entry, as a device opening the
/// workspace does: the host certifies the device's own key for the person `root`, and the
/// session enters on its handshake, which is what hands it the workspace's records. This is
/// how `layout_views`' pages read and write a workspace's records.
pub async fn session_entered(
    host: &TestHost,
    root: &ed25519_dalek::SigningKey,
    device: &Certified,
    server_cert: &DeviceCert,
    workspace: &str,
    label: &str,
) -> Link {
    let (cert, entry) = tilo_session_client::device::entry_request(
        server_cert,
        workspace,
        &device.key,
        tilo_session_client::device::Entering {
            held: None,
            root: Some(root),
            label,
            name: "",
            token: None,
        },
    )
    .await
    .expect("the host certifies the device's key for the person");
    // A page has no UDP socket of its own: it reaches the host through the relay the host
    // runs, as `layout_views`' pages do.
    let addr = host.server.node_addr();
    let dialed = tilo_server_client::ServerAddress::through(
        iroh::EndpointAddr::new(addr.id),
        addr.relay_urls().next().cloned(),
    );
    let peer = super::enter_page(
        dialed,
        cert,
        server_cert.clone(),
        device.key.clone(),
        std::sync::Arc::new(tilo_session_client::MemoryPeerStore::default()),
        entry,
    );
    admitted_to(
        &peer,
        WorkspaceId::parse(workspace).expect("a workspace's id"),
    )
    .await;
    peer
}

/// A session of `device` with `host`, as the device dials it.
#[must_use]
pub fn session(host: &TestHost, device: &Certified, server_cert: &DeviceCert) -> Link {
    start_peer(
        host.iroh(),
        device.cert.clone(),
        server_cert.clone(),
        device.key.clone(),
    )
}

/// Join `peer` to `workspace` (its UUID) as a device of the workspace's person, and wait for
/// its host to admit it: the entry that hands the device its record channels, as every device
/// of a workspace's person reaches them.
pub async fn join(peer: &Link, workspace: &str, name: &str) -> WorkspaceId {
    let id = WorkspaceId::parse(workspace).expect("a workspace's id");
    peer.join_placed(workspace, name)
        .expect("the session is open");
    admitted_to(peer, id).await;
    id
}

/// Join `peer` to `workspace` (its UUID) through its entry, as someone a link let in does, and
/// wait for its host to admit it.
pub async fn join_entered(peer: &Link, workspace: &str) -> WorkspaceId {
    let id = WorkspaceId::parse(workspace).expect("a workspace's id");
    peer.send(Cmd::Join(id)).expect("the session is open");
    admitted_to(peer, id).await;
    id
}

/// `device`'s published actions on the host, as the device `peer` is.
#[must_use]
pub fn client(peer: &Link, device: &Certified, server_cert: &DeviceCert, label: &str) -> Device {
    Device::of(
        peer.clone(),
        device.cert.clone(),
        server_cert.clone(),
        label,
    )
}

/// Ask the host `action` of `provider`'s `resource` with `arguments`, in `workspace`, as
/// `device`.
pub async fn ask(
    device: &Device,
    workspace: WorkspaceId,
    provider: &str,
    resource: &str,
    action: &str,
    arguments: Value,
) -> Result<Value, DeviceRefusal> {
    let target = RemoteActionTarget {
        provider: provider.to_owned(),
        address: ActionAddress::new(resource, action),
    };
    let arguments: Map<String, Value> = match arguments {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => panic!("an action's arguments are an object: {other}"),
    };
    device
        .ask_server_as(workspace, target, arguments, InvocationId::new())
        .await
}

/// Ask `action` of a child of `workspace`'s governance tree (`workspace/<child>#<action>`) as
/// `device`, from the workspace `in_workspace` its session is in.
pub async fn ask_permissions_child(
    device: &Device,
    in_workspace: WorkspaceId,
    workspace: &str,
    child: &str,
    action: &str,
    arguments: Value,
) -> Result<Value, DeviceRefusal> {
    let target = RemoteActionTarget {
        provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(
            tilo_actions::remote::workspace_governance_child(workspace, Some(child)),
            action,
        ),
    };
    let arguments: Map<String, Value> = match arguments {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => panic!("an action's arguments are an object: {other}"),
    };
    device
        .ask_server_as(in_workspace, target, arguments, InvocationId::new())
        .await
}

/// Ask `workspace`'s governance `action` (`workspace/<workspace>/<...>#<action>`) as `device`,
/// from the workspace `in_workspace` its session is in.
pub async fn ask_permissions(
    device: &Device,
    in_workspace: WorkspaceId,
    workspace: &str,
    action: &str,
    arguments: Value,
) -> Result<Value, DeviceRefusal> {
    let target = RemoteActionTarget::workspace_governance(workspace, action);
    let arguments: Map<String, Value> = match arguments {
        Value::Object(map) => map,
        Value::Null => Map::new(),
        other => panic!("an action's arguments are an object: {other}"),
    };
    device
        .ask_server_as(in_workspace, target, arguments, InvocationId::new())
        .await
}

/// Write `mutation` to `route` as `peer`'s device, answering the refusal instead of failing
/// the test where a step proves what a write may not do. The write waits for its record's keys,
/// as a device of the workspace's person writes.
pub async fn try_written(
    peer: &Link,
    route: &DocumentRoute,
    mutation: &DocumentMutation,
) -> Result<(), String> {
    peer.send(Cmd::OpenRoute(route.clone()))?;
    write_and_wait(peer, route, mutation).await
}

/// [`try_written`] for a page that entered a workspace through a link, which the host admits
/// to a record's channel as it opens the record.
pub async fn try_written_entered(
    peer: &Link,
    route: &DocumentRoute,
    mutation: &DocumentMutation,
) -> Result<(), String> {
    peer.send(Cmd::OpenRoute(route.clone()))?;
    write_and_wait(peer, route, mutation).await
}

/// Write `mutation` to `route` and answer once the host holds it, or why it did not.
async fn write_and_wait(
    peer: &Link,
    route: &DocumentRoute,
    mutation: &DocumentMutation,
) -> Result<(), String> {
    peer.write(route.clone(), mutation)?;
    // Only this record's writes: a workspace's own membership writes, which another
    // workspace of the same session may still owe its host, say nothing about this one.
    let receipt = peer.barrier_for(std::slice::from_ref(route))?;
    let answered = tokio::time::timeout(Duration::from_secs(30), receipt).await;
    match answered {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(refusal))) => Err(refusal),
        Ok(Err(_)) => Err("the session let go of the barrier unanswered".to_owned()),
        Err(_) => Err(format!(
            "the host did not answer the write; channels:\n{}\nfailures: {:?}",
            peer.status().key_state,
            peer.status().failures
        )),
    }
}

/// The route of one record of `workspace`'s folder tree (`fs_protocol::tree`), as a
/// computer's folder writes it through its session.
#[must_use]
pub fn tree_route(workspace: &str, chunk: fs_protocol::TreeChunk) -> DocumentRoute {
    DocumentRoute {
        app_id: fs_protocol::tree::FILES_APP.into(),
        cluster_id: workspace.into(),
        document: fs_protocol::tree::chunk_document_id(chunk),
    }
}

/// Write `name` with `bytes` into `workspace`'s folder as `peer`'s device, as a computer's
/// folder writes its tree through its session: the file is a real entry of the workspace's
/// file tree, never an app's document.
pub async fn put_file(
    peer: &Link,
    workspace: &str,
    name: &str,
    bytes: &[u8],
) -> Result<(), String> {
    // The folder of the person's certified device: every change it writes names them, as the
    // computer's own mount does.
    let cert = &peer.engine().identity().device_cert;
    let mount = tilo_fs::namespace::MountAuthor::person(
        hex::encode(cert.device_public_key),
        tilo_actions::MemberId::person(tilo_actions::PersonId::from_root(cert.user_public_key)),
    );
    let mut namespace =
        tilo_fs::namespace::Namespace::new(mount).map_err(|error| error.to_string())?;
    let content = iroh_blobs::Hash::from_bytes(blake3::hash(bytes).into());
    namespace
        .create_file(
            tilo_fs::namespace::ROOT_INO,
            name,
            Some(&content),
            bytes.len() as u64,
            tilo_clock::unix_now(),
        )
        .ok_or_else(|| "the folder refused the file".to_owned())?;
    let mut sent: std::collections::BTreeMap<fs_protocol::TreeChunk, Vec<automerge::ChangeHash>> =
        std::collections::BTreeMap::new();
    for write in namespace.document_writes(&sent) {
        let route = tree_route(workspace, write.chunk);
        if !sent.contains_key(&write.chunk) {
            peer.send(Cmd::OpenRoute(route.clone()))?;
            peer.write(
                route.clone(),
                &DocumentMutation::Snapshot(AutoCommit::new().save()),
            )?;
        }
        peer.write(route, &DocumentMutation::Incremental(write.changes))?;
        sent.insert(write.chunk, write.heads);
    }
    let routes: Vec<DocumentRoute> = sent
        .keys()
        .map(|chunk| tree_route(workspace, *chunk))
        .collect();
    let receipt = peer.barrier_for(&routes)?;
    let answered = tokio::time::timeout(Duration::from_secs(30), receipt).await;
    match answered {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(refusal))) => Err(refusal),
        Ok(Err(_)) => Err("the session let go of the barrier unanswered".to_owned()),
        Err(_) => Err(format!(
            "the host did not answer the folder's writes; channels:\n{}\nfailures: {:?}",
            peer.status().key_state,
            peer.status().failures
        )),
    }
}

/// Read one record of `workspace`'s folder tree into `docs` and `tree`.
fn read_tree_record(
    peer: &Link,
    workspace: &str,
    chunk: fs_protocol::TreeChunk,
    docs: &mut std::collections::BTreeMap<fs_protocol::TreeChunk, AutoCommit>,
    tree: &mut fs_protocol::TreeChunks,
) {
    let route = tree_route(workspace, chunk);
    let Ok(super::DocumentRead::Whole { bytes, .. } | super::DocumentRead::Changes { bytes, .. }) =
        super::reading_of(peer, &route, None)
    else {
        return;
    };
    let doc = docs.entry(chunk).or_default();
    tilo_automerge::ApplyIncoming::load_incoming(doc, &bytes).unwrap();
    if chunk == 0 {
        tree.read_index(doc);
    } else {
        tree.read_chunk(chunk, doc);
    }
}

/// Open the folder `workspace` on `peer` and wait until its device reads the folder's index
/// listing a chunk. A device that refuses the writes it opened fails at once, naming the
/// refusal; a timeout says what the device reads of the index, and its status.
pub async fn folder_index(peer: &Link, workspace: &str, what: &str) {
    let index = tree_route(workspace, 0);
    open(peer, &index);
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut tree = fs_protocol::TreeChunks::default();
        let mut docs = std::collections::BTreeMap::new();
        read_tree_record(peer, workspace, 0, &mut docs, &mut tree);
        if !tree.listed().is_empty() {
            return;
        }
        let read = super::reading_of(peer, &index, None);
        if let Err(refusal) = &read
            && refusal.contains("document change forbidden")
        {
            panic!(
                "{what}: the device refuses the folder's writes it opened: {refusal}. A \
                 computer's folder writes its tree as the person at that device \
                 (`tilo_fs::namespace::acting_as(None)`), whose history entry names no \
                 invoker, and every device that opens such a sealed write refuses it \
                 (`StatePeer::callers_named_in`, tilo-peer-state state/authority.rs)"
            );
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}; the device reads {:?}; status: {:?}",
            read.map(|read| match read {
                super::DocumentRead::Whole { bytes, .. } => format!("whole, {} bytes", bytes.len()),
                other => format!("{other:?}"),
            }),
            peer.status(),
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// How many files the folder `workspace` holds as `peer`'s device reads it: the tree's index,
/// and each chunk it lists, opened as the device's folder opens them: a workspace sends a
/// device the records it opens, and a chunk is one of them.
pub fn file_count(peer: &Link, workspace: &str) -> usize {
    let mut tree = fs_protocol::TreeChunks::default();
    let mut docs = std::collections::BTreeMap::new();
    read_tree_record(peer, workspace, 0, &mut docs, &mut tree);
    for chunk in tree.listed() {
        open(peer, &tree_route(workspace, chunk));
        read_tree_record(peer, workspace, chunk, &mut docs, &mut tree);
    }
    tree.listed()
        .into_iter()
        .filter_map(|chunk| tree.chunk(chunk))
        .flat_map(|nodes| nodes.values())
        .filter(|node| node.kind == fs_protocol::FileNodeKind::File)
        .count()
}

/// The route of the person's settings record, which their devices write and read as a
/// resource of the person's own workspace.
#[must_use]
pub fn settings_route(person: [u8; 32]) -> DocumentRoute {
    DocumentRoute {
        app_id: "so.tilo.settings".into(),
        cluster_id: tilo_server_protocol::servers::person_workspace_id(&person),
        document: "person".into(),
    }
}

/// Write `placement` as the person's display arrangement: the leaf every device of theirs
/// reads.
pub async fn arrange_displays(
    peer: &Link,
    person: [u8; 32],
    placement: &Value,
) -> Result<(), String> {
    let mut doc = AutoCommit::new();
    doc.put(
        ROOT,
        PLACEMENT_KEY,
        serde_json::to_string(placement).expect("the placement is JSON"),
    )
    .unwrap();
    super::commit_change(&mut doc);
    try_written(
        peer,
        &settings_route(person),
        &DocumentMutation::Snapshot(doc.save()),
    )
    .await
}

/// The person's display arrangement as `peer`'s replica holds it.
#[must_use]
pub fn displays(peer: &Link, person: [u8; 32]) -> Option<Value> {
    let bytes = record_of(peer, &settings_route(person))?;
    let doc = AutoCommit::load(&bytes).ok()?;
    let (value, _) = doc.get(ROOT, PLACEMENT_KEY).ok().flatten()?;
    serde_json::from_str(&value.into_string().ok()?).ok()
}

/// Open `route` on `peer` and answer what the workspace holds of it there: every write
/// another device made of the record.
pub fn open(peer: &Link, route: &DocumentRoute) {
    peer.send(Cmd::OpenRoute(route.clone()))
        .expect("the session is open");
    // The keys of the record's channel come with the workspace the device placed, as a
    // session's own admission carries them; a read waits on what the replica holds.
}

/// An agent's device of `root`'s person, certified for `scope`: a grant that acts for them in
/// the workspaces it names and nowhere else, and expires.
#[must_use]
pub fn agent_device(root: &ed25519_dalek::SigningKey, label: &str, scope: AgentScope) -> Certified {
    let key = ed25519_dalek::SigningKey::generate(&mut getrandom::rand_core::UnwrapErr(
        getrandom::SysRng,
    ));
    let now = tilo_clock::unix_now_ns();
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::agent_grant(scope),
        // Issued a moment ago: an agent's grant lives at most its bounded lifetime from issue.
        issued_at_ns: now.saturating_sub(1_000_000_000),
        expires_at_ns: Some(
            now.checked_add(1_800_000_000_000)
                .expect("half an hour from now"),
        ),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).expect("the root signs the agent's grant");
    Certified { key, cert }
}

/// A device's key, as a host's documents name it.
#[must_use]
pub fn key_of(cert: &DeviceCert) -> String {
    hex::encode(cert.device_public_key)
}

/// A person's display arrangement for `displays`, as `person_displays` stores it: each the
/// device that drives it, the device's own output, and its place in the one space.
#[must_use]
pub fn placements(displays: &[(String, String, i32, i32)]) -> Value {
    let placed: Vec<Value> = displays
        .iter()
        .enumerate()
        .map(|(order, (device, output, x, y))| {
            serde_json::json!({
                "device": device,
                "output": output,
                "order": i64::try_from(order).unwrap_or(i64::MAX),
                "placed": true,
                "x": x,
                "y": y,
                "rev": 1,
            })
        })
        .collect();
    serde_json::json!({ "displays": placed })
}

/// The devices in `placement`, by key.
#[must_use]
pub fn arranged_devices(placement: &Value) -> BTreeMap<String, usize> {
    let mut counted = BTreeMap::new();
    if let Some(displays) = placement.get("displays").and_then(Value::as_array) {
        for display in displays {
            if let Some(device) = display.get("device").and_then(Value::as_str) {
                *counted.entry(device.to_owned()).or_default() += 1;
            }
        }
    }
    counted
}

/// A `GET` of `path` from `addr`, as a browser opening it makes it: the status and the body.
pub fn get(addr: SocketAddr, path: &str) -> std::io::Result<(u16, String)> {
    // A browser opening the address asks for a page it can read.
    fetch(addr, "GET", path, "text/html", &[])
}

/// A `POST` of `body` to `path` from `addr`: the status and the body.
pub fn post(addr: SocketAddr, path: &str, body: &[u8]) -> std::io::Result<(u16, String)> {
    fetch(addr, "POST", path, "*/*", body)
}

/// An HTTP/1.1 request from `addr`, as a browser at a host with no certificate to trust yet
/// makes it: the status and the body.
pub fn fetch(
    addr: SocketAddr,
    method: &str,
    path: &str,
    accept: &str,
    body: &[u8],
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: {accept}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map_or(raw.len(), |at| at + 4);
    let (head, body) = raw.split_at(split);
    let head = String::from_utf8_lossy(head).into_owned();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let chunked = head.lines().any(|line| {
        line.to_ascii_lowercase()
            .starts_with("transfer-encoding: chunked")
    });
    let body = if chunked {
        decode_chunks(body)
    } else {
        String::from_utf8_lossy(body).into_owned()
    };
    Ok((status, body))
}

/// `body` with its chunked framing removed, as a browser reads it.
fn decode_chunks(body: &[u8]) -> String {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(end) = rest.windows(2).position(|window| window == b"\r\n") {
        let size = String::from_utf8_lossy(&rest[..end]);
        let size =
            usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16).unwrap_or(0);
        rest = &rest[end + 2..];
        if size == 0 || size > rest.len() {
            break;
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2.min(rest.len() - size)..];
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The address a plain listener's origin names: its host resolved as this machine reads it.
#[must_use]
pub fn address_of(origin: &str) -> SocketAddr {
    let rest = origin
        .strip_prefix("http://")
        .unwrap_or(origin)
        .trim_end_matches('/');
    if let Ok(addr) = rest.parse() {
        return addr;
    }
    let (host, port) = rest
        .rsplit_once(':')
        .expect("a plain listener's host and port");
    let host = match host {
        "localhost" => "127.0.0.1",
        other => other,
    };
    format!("{host}:{port}")
        .parse()
        .expect("a plain listener's address")
}
