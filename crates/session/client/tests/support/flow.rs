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
    doc.commit();
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
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::agent_grant(scope),
        issued_at_ns: 1,
        expires_at_ns: Some(
            tilo_clock::unix_now_ns()
                .checked_add(1_800_000_000_000)
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
