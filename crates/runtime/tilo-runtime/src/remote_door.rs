//! The agent door between the person's own devices.
//!
//! An agent on one of the person's devices (A) steers another of them (B) over
//! MCP as it would if it ran on B: A carries the agent's MCP lines over iroh,
//! on [`ALPN`], to B's agent door, where the person — or B's own setting —
//! admits it as an agent delegated by them, via A. Nothing else is new
//! authority: B serves the connection as it serves a program of its own login
//! ([`crate::session_door`]), through its broker's ordinary gate, in one
//! workspace, under an agent id that names A ([`agent_id`]).
//!
//! B knows who dials from the transport alone. The connection's endpoint id is
//! the dialing device's key, which the QUIC handshake proved; the certificate
//! the dialer presents first must name that key, be signed by the identity root
//! of B's person, hold now, not be a store node's, and not be revoked in the
//! person's own store ([`PersonalDevices`]). Anything else is refused, with the
//! reason, before anyone is asked and before anything is read of the MCP
//! stream.
//!
//! On A the connection is opened by A's host — the desktop session or a
//! standalone runtime, whichever holds A's device key and endpoint
//! ([`Doors`]). A `--connect --device NAME` client asks it with one
//! [`Forward`] line on its local door ([`ask_forward`]), reads whether B's door
//! opened, and from then on its stdio is B's door.
//!
//! The wire is newline-delimited JSON on one bi-directional stream: the
//! dialer's [`Hello`], B's [`Answer`], then MCP JSON-RPC both ways until either
//! side ends. Ending either side ends the other: the stream is the agent's
//! whole connection.

use std::collections::HashMap;
use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::protocol::{AcceptError, ProtocolHandler};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_peer_state::PersonalSync;
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// ALPN of the agent door between two of the person's devices.
pub const ALPN: &[u8] = b"tilo-agent-door/1";

/// The longest hello, answer or forward line either side reads.
const MAX_LINE: usize = 64 * 1024;
/// How long a connection is given to say who it is, and a dial to open.
const HELLO_WITHIN: Duration = Duration::from_secs(30);
/// How long asking whether a device answers waits for it.
pub const PROBE_WITHIN: Duration = Duration::from_secs(4);
/// How long the side that ends a connection waits for what it sent last to
/// be read before the connection goes.
const CLOSE_GRACE: Duration = Duration::from_secs(5);
/// The longest device name the person is shown, and a `--device` is matched
/// against.
const MAX_NAME: usize = 64;

/// What the dialing device says first.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    /// The dialing device's certificate, postcard-encoded, as hex.
    certificate: String,
    /// The workspace the agent asks to work in; the one in front of the
    /// person when it names none, or none they may grant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
    /// Only ask whether this door would open for the dialing device: nothing
    /// is admitted and nobody is asked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    probe: bool,
    /// Only read the person's workspace list, as this door's device holds
    /// it: nothing is admitted and nobody is asked.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    list: bool,
}

/// What a door answers a hello, and a host a local [`Forward`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "door", rename_all = "snake_case")]
pub enum Answer {
    /// The door is open: MCP follows.
    Open,
    /// The door would open for this device; a probe ends here.
    Here,
    /// The person's workspaces, as this device holds them, for a hello that
    /// asked only for them.
    List {
        list: tilo_relay_session::workspaces::PersonalList,
    },
    /// The door does not open, and why.
    Refused { reason: String },
}

/// Why a device's door refused a connection before anyone was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It said nothing this door reads.
    Unreadable,
    /// The certificate it showed names another device than the one that
    /// connected.
    NotTheDialer,
    /// The device is someone else's.
    OtherPerson,
    /// Its certificate does not hold now.
    NotCertified(String),
    /// It is a store node, which never acts for the person.
    StoreNode,
    /// The person removed it from their devices.
    Revoked,
    /// This device serves no agents now.
    Unserved(String),
    /// A key made for an agent asked for a workspace its scope does not
    /// reach, or named none.
    OutOfScope,
}

impl Refusal {
    /// The reason the dialing side is told.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Unreadable => {
                "the device door opens with the dialing device's certificate".to_owned()
            }
            Self::NotTheDialer => {
                "the certificate shown names another device than the one that connected".to_owned()
            }
            Self::OtherPerson => {
                "that device belongs to someone else; this door serves only its person's own \
                 devices"
                    .to_owned()
            }
            Self::NotCertified(why) => format!("that device's certificate does not hold: {why}"),
            Self::StoreNode => "a store node never acts for its person".to_owned(),
            Self::Revoked => {
                "that device was removed from its person's devices; it no longer reaches them"
                    .to_owned()
            }
            Self::Unserved(why) => why.clone(),
            Self::OutOfScope => {
                "this key reaches only the workspaces it was made for: name one of them".to_owned()
            }
        }
    }
}

/// The person's devices, as one of them knows them: who it is, which others
/// it knows, and whether a device that dials it is one of them now.
pub trait PersonsDevices: Send + Sync {
    /// This device's own certificate, which it shows the devices it dials.
    fn this_device(&self) -> DeviceCert;

    /// The person's devices this one knows that may act for them now, this
    /// one included.
    fn devices(&self) -> Vec<DeviceCert>;

    /// Whether `certificate`, shown on a connection the transport proved is
    /// from `dialer`, is one of the person's devices now.
    fn admit(&self, dialer: [u8; 32], certificate: &DeviceCert) -> Result<(), Refusal>;

    /// The revocations the person's identity root signed, as this device's
    /// store holds them: what a proof signed by one of the person's devices
    /// is checked against. Empty for a directory that holds none.
    fn revocations(&self) -> RevocationList {
        RevocationList::new()
    }
}

/// The person's devices as their own store holds them: the identity root it
/// opened on, the certificates it admitted, and the revocations that root
/// signed, which reach it from any of their devices.
#[derive(Clone)]
pub struct PersonalDevices(PersonalSync);

impl PersonalDevices {
    /// The devices `store` holds.
    #[must_use]
    pub fn new(store: PersonalSync) -> Self {
        Self(store)
    }
}

fn now_ns(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        })
}

impl PersonsDevices for PersonalDevices {
    fn this_device(&self) -> DeviceCert {
        self.0.read(|store| store.device().clone())
    }

    fn devices(&self) -> Vec<DeviceCert> {
        let now = SystemTime::now();
        self.0.read(|store| {
            store
                .devices()
                .into_iter()
                .filter(|device| {
                    holds(store.root(), device, now).is_ok()
                        && store.admits_device(&device.device_public_key, now).is_ok()
                })
                .collect()
        })
    }

    fn admit(&self, dialer: [u8; 32], certificate: &DeviceCert) -> Result<(), Refusal> {
        if certificate.device_public_key != dialer {
            return Err(Refusal::NotTheDialer);
        }
        let now = SystemTime::now();
        let revoked = self.revocations().refuses(certificate);
        self.0.read(|store| {
            holds(store.root(), certificate, now)?;
            // A grant a revoked grant minted ends with it.
            if revoked {
                return Err(Refusal::Revoked);
            }
            store
                .admits_device(&dialer, now)
                .map_err(|why| match why.as_str() {
                    "device revoked" => Refusal::Revoked,
                    _ => Refusal::NotCertified(why),
                })
        })
    }

    fn revocations(&self) -> RevocationList {
        let entries = self.0.read(tilo_peer_state::PersonalStore::revocations);
        let mut revocations = RevocationList::new();
        for entry in entries {
            // The store only ever holds entries its root signed.
            let _ = revocations.insert(entry);
        }
        revocations
    }
}

/// Whether `certificate` is one of `root`'s devices that acts for them now:
/// signed by it, current, and not a store node's.
fn holds(root: [u8; 32], certificate: &DeviceCert, now: SystemTime) -> Result<(), Refusal> {
    if certificate.user_public_key != root {
        return Err(Refusal::OtherPerson);
    }
    tilo_user_identity::verify_device_cert_at(certificate, now_ns(now))
        .map_err(|error| Refusal::NotCertified(error.to_string()))?;
    if certificate.capabilities.can_act_as_host {
        return Err(Refusal::StoreNode);
    }
    Ok(())
}

/// What a hello asks of a door.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    /// Open, and carry the agent's MCP.
    Open,
    /// Only whether it would open.
    Probe,
    /// Only the person's workspace list.
    List,
}

/// Where a connection comes from: the device that dialed, and the workspace
/// its agent asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub device: DeviceCert,
    pub workspace: Option<String>,
}

impl Origin {
    /// The dialing device's key.
    #[must_use]
    pub fn key(&self) -> [u8; 32] {
        self.device.device_public_key
    }

    /// The dialing device as the person reads it.
    #[must_use]
    pub fn name(&self) -> String {
        device_name(&self.device)
    }
}

/// A device as the person reads it: its label, or the start of its key when
/// it has none.
#[must_use]
pub fn device_name(device: &DeviceCert) -> String {
    let label: String = device
        .device_label
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_NAME)
        .collect();
    if label.is_empty() {
        short_id(&device.device_public_key)
    } else {
        label
    }
}

/// The first four bytes of a device key, as hex: enough to tell two devices
/// of one person with the same label apart.
fn short_id(key: &[u8; 32]) -> String {
    hex::encode(key.get(..4).unwrap_or_default())
}

/// The agent id a client admitted from another device works as: its own id
/// (`claude-code`), then `@` and the device it came from
/// (`claude-code@studio-laptop-3f2a9c1b`), so what it does is told apart from
/// what the same client does on this device, and from another device's.
#[must_use]
pub fn agent_id(client: &str, device: &DeviceCert) -> String {
    let mut slug = String::new();
    for character in device.device_label.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            slug.push(character);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let client: String = client.chars().take(64).collect();
    let short = short_id(&device.device_public_key);
    if slug.is_empty() {
        format!("{client}@{short}")
    } else {
        format!("{client}@{slug}-{short}")
    }
}

/// One connection the door admitted to be asked about: where it comes from,
/// and the local end of its stream, which carries the agent's MCP lines.
pub struct Arrival {
    pub origin: Origin,
    pub stream: UnixStream,
}

/// Where a door hands the connections it opened: the host's agent door.
pub trait Arrivals: Send + Sync {
    /// Take `arrival` to be asked about and served.
    ///
    /// # Errors
    /// Why this device serves it nothing; the dialer is told.
    fn arrive(&self, arrival: Arrival) -> Result<(), String>;

    /// The person's workspaces as this device holds them, each with its
    /// host's signed address: what a key of theirs that dials this door reads
    /// to find where to enter. `None` while this device holds no list.
    fn listed(&self) -> Option<tilo_relay_session::workspaces::PersonalList> {
        None
    }
}

/// The door one device answers the person's other devices' agents at.
#[derive(Clone)]
pub struct RemoteDoor {
    devices: Arc<dyn PersonsDevices>,
    arrivals: Arc<dyn Arrivals>,
}

impl std::fmt::Debug for RemoteDoor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteDoor").finish_non_exhaustive()
    }
}

impl RemoteDoor {
    /// A door admitting the devices `devices` says are the person's, handing
    /// what it opens to `arrivals`.
    #[must_use]
    pub fn new(devices: Arc<dyn PersonsDevices>, arrivals: Arc<dyn Arrivals>) -> Self {
        Self { devices, arrivals }
    }

    /// What this door says to a hello from `dialer`: whom it admits, or why
    /// not.
    fn decide(&self, dialer: [u8; 32], line: &[u8]) -> Result<(Origin, Asked), Refusal> {
        let hello: Hello = serde_json::from_slice(line).map_err(|_| Refusal::Unreadable)?;
        let bytes = hex::decode(&hello.certificate).map_err(|_| Refusal::Unreadable)?;
        let device: DeviceCert = postcard::from_bytes(&bytes).map_err(|_| Refusal::Unreadable)?;
        self.devices.admit(dialer, &device)?;
        let mut workspace = hello
            .workspace
            .filter(|workspace| uuid::Uuid::parse_str(workspace).is_ok());
        // A key made for an agent works where its scope reaches, as a relay
        // admits it (`tilo_room_host::workspace_access`): a key for one
        // workspace in that one alone, never the one in front of the person.
        if let Some(scope) = device.capabilities.agent
            && !hello.list
        {
            match (&workspace, scope) {
                (Some(asked), scope) if !scope.reaches(asked) => {
                    return Err(Refusal::OutOfScope);
                }
                (None, tilo_user_identity::AgentScope::Workspace(_)) => {
                    workspace = Some(scope.name());
                }
                (None, tilo_user_identity::AgentScope::AllWorkspaces) => {
                    return Err(Refusal::OutOfScope);
                }
                _ => {}
            }
        }
        let asked = if hello.list {
            Asked::List
        } else if hello.probe {
            Asked::Probe
        } else {
            Asked::Open
        };
        Ok((Origin { device, workspace }, asked))
    }

    /// The person's list as `device` reads it: the workspaces its scope
    /// reaches, for a key made for one of them.
    fn list_for(&self, device: &DeviceCert) -> Answer {
        let Some(mut list) = self.arrivals.listed() else {
            return Answer::Refused {
                reason: "this device holds no workspace list yet".to_owned(),
            };
        };
        if let Some(scope) = device.capabilities.agent {
            list.workspaces
                .retain(|workspace| scope.reaches(&workspace.workspace));
        }
        Answer::List { list }
    }

    async fn answer(&self, connection: &Connection) {
        let dialer = *connection.remote_id().as_bytes();
        let Ok(Ok((mut send, mut recv))) =
            tokio::time::timeout(HELLO_WITHIN, connection.accept_bi()).await
        else {
            return;
        };
        let Ok(Ok(line)) = tokio::time::timeout(HELLO_WITHIN, read_line(&mut recv)).await else {
            return;
        };
        let refused = match self.decide(dialer, &line) {
            Ok((_, Asked::Probe)) => Answer::Here,
            Ok((origin, Asked::List)) => self.list_for(&origin.device),
            Ok((origin, Asked::Open)) => match self.open(origin) {
                Ok(local) => {
                    if write_line(&mut send, &Answer::Open).await.is_err() {
                        return;
                    }
                    splice(send, recv, local).await;
                    connection.close(0_u32.into(), b"the agent door closed");
                    return;
                }
                Err(reason) => Answer::Refused { reason },
            },
            Err(refusal) => {
                tracing::info!(
                    device = %hex::encode(dialer),
                    reason = %refusal.reason(),
                    "agent door: another device's connection is refused"
                );
                Answer::Refused {
                    reason: refusal.reason(),
                }
            }
        };
        let _ = write_line(&mut send, &refused).await;
        let _ = send.finish();
        // The dialer closes once it read the answer.
        let _ = tokio::time::timeout(CLOSE_GRACE, connection.closed()).await;
    }

    /// Hand `origin`'s connection to the host's door: the local end of the
    /// stream the agent's lines are carried on, or why it serves none.
    fn open(&self, origin: Origin) -> Result<tokio::net::UnixStream, String> {
        let (ours, theirs) = UnixStream::pair().map_err(|error| error.to_string())?;
        let local = into_tokio(ours).map_err(|error| error.to_string())?;
        tracing::info!(
            device = %hex::encode(origin.key()),
            name = %origin.name(),
            "agent door: one of the person's devices connects"
        );
        self.arrivals.arrive(Arrival {
            origin,
            stream: theirs,
        })?;
        Ok(local)
    }
}

impl ProtocolHandler for RemoteDoor {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        // Everything a dialer is told is said on its stream.
        self.answer(&connection).await;
        Ok(())
    }
}

fn into_tokio(stream: UnixStream) -> io::Result<tokio::net::UnixStream> {
    stream.set_nonblocking(true)?;
    tokio::net::UnixStream::from_std(stream)
}

/// Read one line of at most [`MAX_LINE`] bytes, a byte at a time, so nothing
/// after it is consumed.
async fn read_line(recv: &mut RecvStream) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match recv.read(&mut byte).await {
            Ok(Some(0)) => {}
            Ok(Some(_)) => {
                let [byte] = byte;
                if byte == b'\n' {
                    return Ok(line);
                }
                if line.len() >= MAX_LINE {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the line is too long",
                    ));
                }
                line.push(byte);
            }
            Ok(None) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the other side ended before its line",
                ));
            }
            Err(error) => return Err(io::Error::other(error)),
        }
    }
}

async fn write_line(send: &mut SendStream, value: &(impl Serialize + Sync)) -> io::Result<()> {
    let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
    line.push(b'\n');
    send.write_all(&line).await.map_err(io::Error::other)
}

/// Carry bytes both ways between `send`/`recv` and `local` until both ends
/// are done: whatever ends on one side ends on the other, so a client that
/// leaves, a door that closes, or a connection that drops ends the whole
/// connection.
async fn splice(mut send: SendStream, mut recv: RecvStream, local: tokio::net::UnixStream) {
    let (mut from_local, mut to_local) = local.into_split();
    let inbound = async move {
        let mut buffer = vec![0_u8; 16 * 1024];
        while let Ok(Some(read)) = recv.read(&mut buffer).await {
            let Some(bytes) = buffer.get(..read) else {
                break;
            };
            if to_local.write_all(bytes).await.is_err() {
                break;
            }
        }
        // The local side reads its end; nothing more is read from the stream.
        let _ = to_local.shutdown().await;
        let _ = recv.stop(0_u32.into());
    };
    let outbound = async move {
        let mut buffer = vec![0_u8; 16 * 1024];
        loop {
            match from_local.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let Some(bytes) = buffer.get(..read) else {
                        break;
                    };
                    if send.write_all(bytes).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = send.finish();
        // What was sent last is read before the connection goes.
        let _ = tokio::time::timeout(CLOSE_GRACE, send.stopped()).await;
    };
    tokio::join!(inbound, outbound);
}

/// A connection to another device's door, open.
pub struct Dialed {
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
}

/// Open the door of the device at `addr` from `endpoint`, as the device
/// `own` certifies, asking to work in `workspace`; with `probe`, only ask
/// whether it would open.
pub async fn open(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    own: &DeviceCert,
    workspace: Option<&str>,
    probe: bool,
) -> Result<Dialed, String> {
    let dialing = async {
        let connection = endpoint
            .connect(addr, ALPN)
            .await
            .map_err(|error| format!("it does not answer: {error}"))?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| format!("it does not answer: {error}"))?;
        let certificate = postcard::to_allocvec(own).map_err(|error| error.to_string())?;
        let hello = Hello {
            certificate: hex::encode(certificate),
            workspace: workspace.map(str::to_owned),
            probe,
            list: false,
        };
        write_line(&mut send, &hello)
            .await
            .map_err(|error| format!("it does not answer: {error}"))?;
        let line = read_line(&mut recv)
            .await
            .map_err(|error| format!("its door does not answer: {error}"))?;
        let answer: Answer = serde_json::from_slice(&line)
            .map_err(|_| "its door answers in a way this device does not read".to_owned())?;
        match answer {
            Answer::Open | Answer::Here => Ok(Dialed {
                connection,
                send,
                recv,
            }),
            Answer::Refused { reason } => {
                connection.close(0_u32.into(), b"refused");
                Err(reason)
            }
            Answer::List { .. } => {
                connection.close(0_u32.into(), b"unasked");
                Err("its door answered something it was not asked".to_owned())
            }
        }
    };
    let within = if probe { PROBE_WITHIN } else { HELLO_WITHIN };
    tokio::time::timeout(within, dialing)
        .await
        .unwrap_or_else(|_| Err("it does not answer".to_owned()))
}

/// The person's workspace list as the device at `addr` holds it, read from
/// its door by `endpoint` as the device `own` certifies: each workspace with
/// its host's signed address, those a key for one workspace reaches alone.
///
/// # Errors
/// The device does not answer, refuses this one, or holds no list yet.
pub async fn read_list(
    endpoint: &Endpoint,
    addr: EndpointAddr,
    own: &DeviceCert,
) -> Result<tilo_relay_session::workspaces::PersonalList, String> {
    let reading = async {
        let connection = endpoint
            .connect(addr, ALPN)
            .await
            .map_err(|error| format!("it does not answer: {error}"))?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| format!("it does not answer: {error}"))?;
        let certificate = postcard::to_allocvec(own).map_err(|error| error.to_string())?;
        let hello = Hello {
            certificate: hex::encode(certificate),
            workspace: None,
            probe: false,
            list: true,
        };
        write_line(&mut send, &hello)
            .await
            .map_err(|error| format!("it does not answer: {error}"))?;
        let line = read_line(&mut recv)
            .await
            .map_err(|error| format!("its door does not answer: {error}"))?;
        connection.close(0_u32.into(), b"read");
        match serde_json::from_slice::<Answer>(&line) {
            Ok(Answer::List { list }) => Ok(list),
            Ok(Answer::Refused { reason }) => Err(reason),
            Ok(Answer::Open | Answer::Here) | Err(_) => {
                Err("its door answers in a way this device does not read".to_owned())
            }
        }
    };
    tokio::time::timeout(HELLO_WITHIN, reading)
        .await
        .unwrap_or_else(|_| Err("it does not answer".to_owned()))
}

impl Dialed {
    /// Carry the open door's stream to and from `local` until either side
    /// ends, then close the connection.
    pub async fn carry(self, local: tokio::net::UnixStream) {
        let Self {
            connection,
            send,
            recv,
        } = self;
        splice(send, recv, local).await;
        connection.close(0_u32.into(), b"the agent left");
    }

    /// The open door's stream as MCP lines this process reads and writes
    /// itself, rather than carries to a local client.
    #[must_use]
    pub fn into_lines(self) -> Lines {
        Lines {
            connection: self.connection,
            send: self.send,
            recv: self.recv,
        }
    }

    /// End a probe.
    pub fn close(self) {
        self.connection.close(0_u32.into(), b"probed");
    }
}

/// An open door's MCP, one JSON-RPC message a line each way.
pub struct Lines {
    connection: Connection,
    send: SendStream,
    recv: RecvStream,
}

impl Lines {
    /// Send `message` as one line.
    ///
    /// # Errors
    /// The door closed.
    pub async fn send(&mut self, message: &serde_json::Value) -> io::Result<()> {
        write_line(&mut self.send, message).await
    }

    /// The next message the door sends.
    ///
    /// # Errors
    /// The door closed, or sent what is no JSON.
    pub async fn next(&mut self) -> io::Result<serde_json::Value> {
        let line = read_line(&mut self.recv).await?;
        serde_json::from_slice(&line).map_err(io::Error::other)
    }
}

impl Drop for Lines {
    fn drop(&mut self) {
        let _ = self.send.finish();
        self.connection.close(0_u32.into(), b"the agent left");
    }
}

/// What a `--connect --device` client asks of its own host: carry this
/// connection to that device's agent door.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forward {
    /// The device, by its name or its endpoint id.
    pub device: String,
    /// The workspace on it to work in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForwardLine {
    forward: Forward,
}

/// The forward a connection's first line asks for, if that is what it is.
#[must_use]
pub fn forward_request(line: &[u8]) -> Option<Forward> {
    serde_json::from_slice::<ForwardLine>(line)
        .ok()
        .map(|line| line.forward)
}

/// Ask the host at the other end of `stream` to carry it to the door
/// `forward` names, and read whether that door opened. From `Ok` on, the
/// stream is that door's.
pub fn ask_forward(stream: &mut UnixStream, forward: &Forward) -> io::Result<()> {
    let mut line = serde_json::to_vec(&ForwardLine {
        forward: forward.clone(),
    })
    .map_err(io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let mut answer = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte)? {
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!(
                        "this device's host does not carry agents to another device: it closed \
                         without answering for {}",
                        forward.device
                    ),
                ));
            }
            _ if byte == *b"\n" => break,
            _ if answer.len() >= MAX_LINE => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "this device's host answered with a line too long to read",
                ));
            }
            _ => answer.extend_from_slice(&byte),
        }
    }
    match serde_json::from_slice::<Answer>(&answer) {
        Ok(Answer::Open) => Ok(()),
        Ok(Answer::Refused { reason }) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} did not let this agent in: {reason}", forward.device),
        )),
        Ok(Answer::Here | Answer::List { .. }) | Err(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "this device's host does not carry agents to another device: it answered {}",
                String::from_utf8_lossy(&answer)
            ),
        )),
    }
}

/// One of the person's devices, and whether its door answers this one now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reach {
    pub device: DeviceCert,
    /// It is the device asking.
    pub this: bool,
    /// Whether its door would open for this device now, or why not.
    pub answer: Result<(), String>,
}

/// The person's devices and whether each answers now, as an agent's tool
/// lists them.
pub trait DeviceDirectory: Send + Sync {
    /// Every device of the person this one knows, with whether it answers.
    fn reach(&self) -> std::pin::Pin<Box<dyn Future<Output = Vec<Reach>> + Send + '_>>;
}

/// The tool an agent lists the person's devices with.
pub const DEVICES_TOOL: &str = "tilo_devices";

/// [`DEVICES_TOOL`]'s definition.
#[must_use]
pub fn tool_definition() -> serde_json::Value {
    serde_json::json!({
        "name": DEVICES_TOOL,
        "description": "List your other devices and whether each answers now. An agent works on another device by connecting to it: `tilo agent connect claude --device NAME`, or `tilo-runtime-mcp --connect --device NAME`.",
        "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "annotations": {"readOnlyHint": true, "openWorldHint": true}
    })
}

/// The devices as the tool answers them: one line each, with its name, its
/// endpoint id and whether it answers.
#[must_use]
pub fn listing(reach: &[Reach]) -> String {
    let mut lines = Vec::with_capacity(reach.len().saturating_add(1));
    if reach.iter().all(|reach| reach.this) {
        lines.push("No other device of yours is known here.".to_owned());
    }
    for reach in reach {
        let state = match (&reach.answer, reach.this) {
            (_, true) => "this device".to_owned(),
            (Ok(()), false) => "reachable".to_owned(),
            (Err(why), false) => format!("not reachable: {why}"),
        };
        lines.push(format!(
            "{}  {}  {state}",
            device_name(&reach.device),
            hex::encode(reach.device.device_public_key)
        ));
    }
    lines.join("\n")
}

/// The doors of the person's other devices, as this device's host reaches
/// them: dialed from its endpoint, as the device its store certifies.
pub struct Doors {
    endpoint: Endpoint,
    devices: Arc<dyn PersonsDevices>,
    /// The runtime `endpoint` was bound on, where every dial runs.
    runtime: tokio::runtime::Handle,
    /// Where devices are that the endpoint's own lookup does not know.
    addresses: Mutex<HashMap<[u8; 32], EndpointAddr>>,
}

impl Doors {
    /// Dial from `endpoint`, bound on `runtime`, as the device `devices`
    /// names this one.
    #[must_use]
    pub fn new(
        endpoint: Endpoint,
        devices: Arc<dyn PersonsDevices>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            endpoint,
            devices,
            runtime,
            addresses: Mutex::default(),
        }
    }

    /// Where one device is, when the endpoint's lookup would not find it.
    pub fn learn(&self, addr: EndpointAddr) {
        self.addresses
            .lock_recover()
            .insert(*addr.id.as_bytes(), addr);
    }

    fn addr(&self, key: [u8; 32]) -> Result<EndpointAddr, String> {
        if let Some(addr) = self.addresses.lock_recover().get(&key) {
            return Ok(addr.clone());
        }
        EndpointId::from_bytes(&key)
            .map(EndpointAddr::new)
            .map_err(|_| "that is not a device key".to_owned())
    }

    /// The device `named` names: its endpoint id, or its name as the person
    /// reads it, matched without regard to case.
    pub fn resolve(&self, named: &str) -> Result<[u8; 32], String> {
        let named = named.trim();
        let own = self.devices.this_device().device_public_key;
        let known = self.devices.devices();
        let by_id = tilo_actions::remote::own_device::decode_device(&named.to_lowercase())
            .or_else(|| named.parse::<EndpointId>().ok().map(|id| *id.as_bytes()));
        let key = if let Some(key) = by_id {
            key
        } else {
            let matching: Vec<&DeviceCert> = known
                .iter()
                .filter(|device| device_name(device).eq_ignore_ascii_case(named))
                .collect();
            match matching.as_slice() {
                [device] => device.device_public_key,
                [] => {
                    return Err(format!(
                        "no device of yours is called {named}; yours: {}",
                        names(&known)
                    ));
                }
                _ => {
                    return Err(format!(
                        "more than one device of yours is called {named}; name it by its id: {}",
                        matching
                            .iter()
                            .map(|device| hex::encode(device.device_public_key))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        };
        if key == own {
            return Err(format!(
                "{named} is this device; connect without --device to work here"
            ));
        }
        Ok(key)
    }

    /// Every device of the person this one knows, and whether each one's
    /// door answers now: all are asked at once, and one that has not answered
    /// within [`PROBE_WITHIN`] is not reachable.
    pub async fn reach_now(&self) -> Vec<Reach> {
        let own = self.devices.this_device();
        let mut asks = Vec::new();
        for device in self.devices.devices() {
            if device.device_public_key == own.device_public_key {
                continue;
            }
            let endpoint = self.endpoint.clone();
            let addr = self.addr(device.device_public_key);
            let own = own.clone();
            let asking = self.runtime.spawn(async move {
                let addr = addr?;
                open(&endpoint, addr, &own, None, true)
                    .await
                    .map(Dialed::close)
            });
            asks.push((device, asking));
        }
        let mut reach = vec![Reach {
            device: own,
            this: true,
            answer: Ok(()),
        }];
        for (device, asking) in asks {
            let answer = asking
                .await
                .unwrap_or_else(|_| Err("asking it stopped".to_owned()));
            reach.push(Reach {
                device,
                this: false,
                answer,
            });
        }
        reach
    }

    /// Carry `local`, whose client asked for `forward`, to that device's
    /// door, telling the client whether it opened, until either side ends.
    /// Blocks the calling thread, which must not be one of the runtime's.
    pub fn serve_forward(&self, local: UnixStream, forward: &Forward) {
        let addr = match self.resolve(&forward.device).and_then(|key| self.addr(key)) {
            Ok(addr) => addr,
            Err(reason) => {
                refuse_forward(local, reason);
                return;
            }
        };
        let own = self.devices.this_device();
        let endpoint = self.endpoint.clone();
        let workspace = forward.workspace.clone();
        let carried = self.runtime.spawn(async move {
            let local = match into_tokio(local) {
                Ok(local) => local,
                Err(error) => {
                    tracing::debug!(%error, "agent door: a forwarded client's socket");
                    return;
                }
            };
            let dialed = open(&endpoint, addr, &own, workspace.as_deref(), false).await;
            let mut local = local;
            let answer = match &dialed {
                Ok(_) => Answer::Open,
                Err(reason) => Answer::Refused {
                    reason: reason.clone(),
                },
            };
            let told = match serde_json::to_vec(&answer) {
                Ok(mut line) => {
                    line.push(b'\n');
                    local.write_all(&line).await.is_ok()
                }
                Err(_) => false,
            };
            match dialed {
                Ok(dialed) if told => dialed.carry(local).await,
                Ok(dialed) => dialed.close(),
                Err(_) => {
                    let _ = local.shutdown().await;
                }
            }
        });
        let _ = self.runtime.block_on(carried);
    }
}

impl DeviceDirectory for Doors {
    fn reach(&self) -> std::pin::Pin<Box<dyn Future<Output = Vec<Reach>> + Send + '_>> {
        Box::pin(self.reach_now())
    }
}

/// Tell a forwarding client its device's door does not open, and why.
pub fn refuse_forward(mut local: UnixStream, reason: String) {
    let answer = Answer::Refused { reason };
    if let Ok(mut line) = serde_json::to_vec(&answer) {
        line.push(b'\n');
        let _ = local.set_write_timeout(Some(Duration::from_secs(1)));
        let _ = local.write_all(&line);
    }
    let _ = local.shutdown(std::net::Shutdown::Both);
}

fn names(devices: &[DeviceCert]) -> String {
    if devices.is_empty() {
        return "none is known here".to_owned();
    }
    devices
        .iter()
        .map(device_name)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn certificate(label: &str, key: u8) -> DeviceCert {
        DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [key; 32],
            device_label: label.into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        }
    }

    #[test]
    fn an_agent_from_another_device_is_named_by_it() {
        let laptop = certificate("Studio Laptop", 0x3f);
        assert_eq!(
            agent_id("claude-code", &laptop),
            "claude-code@studio-laptop-3f3f3f3f"
        );
        assert_eq!(
            agent_id("codex", &certificate("  ", 0xab)),
            "codex@abababab"
        );
        let id = agent_id(&"x".repeat(64), &certificate(&"Ü".repeat(200), 1));
        tilo_peer_state::workspace_membership::valid_agent_id(&id).unwrap();
    }

    #[test]
    fn only_a_forward_line_asks_for_a_forward() {
        assert_eq!(
            forward_request(br#"{"forward":{"device":"studio"}}"#),
            Some(Forward {
                device: "studio".into(),
                workspace: None
            })
        );
        assert_eq!(
            forward_request(br#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#),
            None
        );
        assert_eq!(forward_request(br#"{"op":"agent_seat"}"#), None);
        assert_eq!(
            forward_request(br#"{"forward":{"device":"studio","as":"person"}}"#),
            None,
            "nothing beyond the device and the workspace is read"
        );
        assert!(!crate::session_door::is_control_request(
            br#"{"forward":{"device":"studio"}}"#
        ));
    }

    #[test]
    fn the_listing_says_which_device_answers() {
        let text = listing(&[
            Reach {
                device: certificate("Here", 1),
                this: true,
                answer: Ok(()),
            },
            Reach {
                device: certificate("Build box", 2),
                this: false,
                answer: Ok(()),
            },
            Reach {
                device: certificate("Phone", 3),
                this: false,
                answer: Err("it does not answer".into()),
            },
        ]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].starts_with("Here  ") && lines[0].ends_with("this device"));
        assert!(lines[1].contains(&hex::encode([2_u8; 32])) && lines[1].ends_with("reachable"));
        assert!(lines[2].ends_with("not reachable: it does not answer"));
        assert!(
            listing(&[Reach {
                device: certificate("Here", 1),
                this: true,
                answer: Ok(()),
            }])
            .starts_with("No other device")
        );
    }
}
