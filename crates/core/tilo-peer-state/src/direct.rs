//! Direct sync of one workspace between two of its devices, with no
//! workspace host in between, over whatever carries the link between them: a
//! native device's iroh connection ([`iroh::endpoint::Connection`]) or a
//! page's WebTransport session ([`tilo_server_client::link::Link`], both
//! [`Transport`]).
//!
//! The transport proves the remote device key, so a device that is not a
//! certified, unrevoked device of the workspace's members is refused before a
//! frame is read, and everything it sends is admitted as its own live
//! traffic through [`StatePeer::receive_from`], the same admission a workspace
//! host's relayed delta takes.
//!
//! A link is three streams. Each side opens one unidirectional stream of
//! length-prefixed postcard [`NeighbourMsg`] frames: what the other may lack
//! first, then every message it publishes while the link lasts. A
//! bidirectional control stream carries the dialling device's ask for the
//! other device's retained history ([`DirectSync::with_store`]) and the frames
//! its store holds, so a device that carries a workspace's history serves a
//! member the same way a host does.
//!
//! A frame can come ahead of what it builds on, or what it builds on can be
//! lost: a workspace's changes reach a member both as the host forwards
//! another member's frame and as the host publishes its own governance. A
//! device that then has a change it cannot apply asks the device
//! that sent it, over the same link, for the state that device holds
//! ([`NeighbourMsg::SyncReq`]), and that device answers on that link alone
//! with what a link starts with ([`DirectSync::catch_up`]): its certificate
//! and, from a device keeping the workspace for its person, its governance and
//! a checkpoint of every record. What the waiting change builds on comes with
//! it, wherever the change came from.
//!
//! A device that takes the workspace's records only as it opens them — a page,
//! a browser environment — says so when it names the workspace
//! ([`DirectSync::when_opened`]): it is replayed the records every device is
//! sent and each chain's newest part, and the history of any other record once
//! it opens it ([`DirectSync::open_record`]). A link from such a device carries
//! only the frames such a device sends.
//!
//! A host of several workspaces answers them all on [`DIRECT_ALPN`] through
//! one [`Serving`], which dispatches each connection to the workspace its
//! first frame names and admits the device at its other end before the link
//! listens ([`Serving::admitting`]).
//!
//! Each end of a link answers the other's own writes it took in, or refused
//! for good, on a unidirectional stream of its own opened after its frames'
//! ([`Answered`]); a device that takes the workspace's records only as it opens
//! them answers none, as it keeps no copy of the workspace. A store device
//! keeps which of its own writes no other device keeping the workspace has
//! answered yet ([`crate::reach`]), and sends them again on each link that comes
//! up, so a write made while every other device was away is answered once one
//! links.

#[cfg(not(target_arch = "wasm32"))]
use crate::frames::Budget;
use crate::frames::{
    Cursor, ENDINGS, MAX_FRAME_BYTES, OUTGOING, REFUSED, advance, author_of, bump,
    cursors as cursors_of, from_device,
};
use crate::join::WorkspaceId;
use crate::peer_handle;
pub use crate::replica::Replica;
use crate::retained::Frame;
#[cfg(not(target_arch = "wasm32"))]
use crate::store::{Store, StoreError};
use crate::transport::{StreamRecv, StreamSend, Transport};
use crate::{NeighbourMsg, StatePeer, TransportOrigin};
use collab_types::signer::DeviceSigner;
#[cfg(not(target_arch = "wasm32"))]
use iroh::endpoint::Connection;
#[cfg(not(target_arch = "wasm32"))]
use iroh::protocol::{AcceptError, ProtocolHandler};
#[cfg(not(target_arch = "wasm32"))]
use iroh::{Endpoint, EndpointAddr};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tilo_server_protocol::refusal::Refusal;
use tokio::sync::{broadcast, watch};
use web_time::SystemTime;

/// ALPN of a direct link between two devices of one workspace, on which each
/// answers the other's writes ([`Answered`]).
pub use tilo_server_protocol::transport::DIRECT_ALPN;

/// What the device at a link's other end made of one of this device's own
/// writes, named by the hash of the write's frame as the writer sent it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Answered {
    /// The hash of the write's frame.
    pub write: [u8; 32],
    /// The write's own sequence number, as its writer signed it.
    pub seq: u64,
    /// Whether the workspace keeps it.
    pub kept: Kept,
}

/// Whether the workspace keeps a write ([`Answered`]).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Kept {
    /// The other device took it in.
    Durable,
    /// The other device refused it for good: no later attempt of it is
    /// admitted. Its verdict reaches the writer in the workspace as before,
    /// and `reason` is why, in the refusing workspace's own words: what the
    /// writer answers a wait for that write with.
    Refused { reason: String },
}

/// One frame of a direct link's control stream.
///
/// A dialling device names its workspace first, and whether it takes the
/// workspace's records only as it opens them; the other device answers that
/// it is listening, then the dialling one asks for the retained history to
/// continue from its own cursors, which the other device answers with the
/// frames its store holds. A device that opens a record asks for it by its
/// channel. The workspace's live messages travel on their own streams, one
/// per direction.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
enum LinkFrame {
    /// The workspace the dialling device wants, named before anything else,
    /// and whether it takes the workspace's records only once it opens them.
    Workspace {
        /// The workspace the dialling device wants.
        id: WorkspaceId,
        /// Whether the dialling device takes the records every device is not
        /// replayed only once it opens them, as a page does.
        opens: bool,
    },
    /// The retained history the dialling device asks for, from its cursors.
    Cursors(Vec<Cursor>),
    /// Retained history after the asker's cursors, and whether more is left.
    Frames {
        /// The frames the other device holds after the asker's cursors, each
        /// with the device that authored it: a round opens with the other
        /// device's own state, which is history too, and a frame is judged
        /// under the device that wrote it, as the live stream's are.
        frames: Vec<([u8; 32], NeighbourMsg)>,
        /// Whether a further ask carries more.
        more: bool,
    },
    /// The dialled device listens now: whatever the dialling one publishes
    /// from this point reaches it, live.
    Ready,
    /// The workspace whose store the dialling device takes, named instead of
    /// [`Self::Workspace`]: the dialled device answers what its store keeps
    /// and sends nothing live ([`DirectSync::take_store`]).
    Take(WorkspaceId),
    /// One record the dialling device opens, by its channel, from its
    /// cursors: its history follows, and every write of it from then on.
    Open {
        /// The channel of the record, as [`neighbour_wire::app_document_group`]
        /// names it; a chain's head (`neighbour_wire::chain_head`) names
        /// whichever part is the chain's newest.
        channel: [u8; 32],
        /// Where the dialling device stands in what it was sent of the record.
        cursors: Vec<Cursor>,
    },
}

/// Ask the other device's store for the frames after `cursors`, round by round
/// while it says more are left, and answer them, each with its author, once
/// it says no more are.
async fn fetch_store(
    control: &mut Control,
    mut cursors: Vec<Cursor>,
) -> Result<Vec<([u8; 32], NeighbourMsg)>, String> {
    let mut fetched = Vec::new();
    write_link(&mut control.send, &LinkFrame::Cursors(cursors.clone())).await?;
    while let Some(frame) = read_link(&mut control.recv, MAX_FRAME_BYTES).await? {
        let LinkFrame::Frames { frames, more } = frame else {
            return Err("the device answered something else than retained frames".into());
        };
        let messages = frames
            .iter()
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>();
        advance(&mut cursors, &messages);
        fetched.extend(frames);
        if !more {
            return Ok(fetched);
        }
        write_link(&mut control.send, &LinkFrame::Cursors(cursors.clone())).await?;
    }
    Err("the device's store ended the history before the last frame".into())
}

/// Whether `device` is one of the devices of the readers the reader set
/// `readers` names, as `peer`'s governance has them now: a document sealed to
/// that set reaches no other device over a link.
fn reads_named(peer: &StatePeer, readers: &str, device: [u8; 32]) -> bool {
    let Some(named) = collab_types::Readers::parse(readers).and_then(|set| set.named()) else {
        return false;
    };
    let now = tilo_clock::wall();
    peer.certificates().iter().any(|certificate| {
        certificate.device_public_key == device
            && peer
                .governance()
                .reads_named(&named, &certificate.user_public_key, now)
    })
}

/// The certificate `device` takes part in `peer`'s workspace under: its own
/// in the member list, or, for a device the workspace names among those
/// keeping it, the one its signed address carries. A device keeping the
/// workspace for another person is no member, and takes the store of a
/// device keeping it all the same ([`DirectSync::take_store`]).
fn certified(peer: &StatePeer, device: [u8; 32]) -> Option<tilo_user_identity::DeviceCert> {
    peer.certificates()
        .into_iter()
        .chain(
            peer.governance()
                .keeping_devices()
                .into_iter()
                .map(|address| address.host),
        )
        .find(|certificate| certificate.device_public_key == device)
}

/// Where `store` stands on `workspace`: a cursor per author's line, at the
/// latest frame it keeps there.
#[cfg(not(target_arch = "wasm32"))]
fn standing(store: &Store, workspace: WorkspaceId) -> Vec<Cursor> {
    store
        .stands(workspace)
        .into_iter()
        .map(|(cursor, _)| cursor)
        .collect()
}

/// What this device's owner decided of `message`, `author`'s frame, as the
/// link hands it on: landed when it changed what the workspace holds.
fn heard_of(
    author: [u8; 32],
    message: NeighbourMsg,
    report: Result<crate::AdmissionReport, crate::AdmissionError>,
    retained: bool,
) -> Heard {
    let (landed, report) = match report {
        Ok(report) => (report.changed || report.metadata_changed, Ok(report)),
        Err(error) if error.is_device_failure() => {
            tracing::warn!(
                %error,
                "this device could not decide a link's frame; nothing was refused"
            );
            (false, Err(error))
        }
        Err(error) => {
            tracing::warn!(%error, "a direct link carried a frame the workspace refused");
            (false, Err(error))
        }
    };
    Heard {
        author,
        message,
        landed,
        retained,
        report,
        fresh: true,
    }
}

/// Write `frame` as the control stream's next record.
async fn write_link<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &LinkFrame,
) -> Result<(), String> {
    let bytes = postcard::to_allocvec(frame).map_err(|error| error.to_string())?;
    crate::frames::write_frame(writer, &bytes).await
}

/// The next control frame, or `None` once the stream ended cleanly before one started.
async fn read_link<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<LinkFrame>, String> {
    match crate::frames::read_frame(reader, max).await? {
        Some(bytes) => LinkFrame::decode(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        None => Ok(None),
    }
}

impl LinkFrame {
    /// The control frame `bytes` holds, whole.
    fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        match postcard::take_from_bytes(bytes)? {
            (frame, []) => Ok(frame),
            _ => Err(postcard::Error::DeserializeBadEncoding),
        }
    }
}

/// What one link may do beyond the workspace's own rules, as the host admits
/// the device at its other end ([`Serving::admitting`]).
///
/// Nothing here wards a device off the workspace's admission: every frame a
/// link brings is still admitted as its author's. A scope narrows what a link
/// is sent and what it may pass on, for a device the workspace lets in but for
/// less than a member device.
/// A scope that names nothing admits nothing beyond the workspace's own
/// admission: a host says what a link reaches, and says it fails closed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkScope {
    /// The one record channel the link's device reaches, when it reaches no
    /// more: it is sent frames of that record and frames of no record, and
    /// nothing of any other, and an open of another record is answered with
    /// nothing.
    pub reads: Option<[u8; 32]>,
    /// Whether the link carries only frames its own device authored: a device
    /// that may not pass another device's frame on, nor name another as the
    /// source of one. Every link from a device that does not keep the
    /// workspace carries its own device's frames alone, whatever the hook
    /// says.
    pub own_frames_only: bool,
    /// Whether the link's device is admitted for the workspace's channel keys:
    /// it joins each channel's group and may publish the commits that settle
    /// it. A link without it may ask a channel to admit it
    /// ([`NeighbourMsg::MlsAccessRequest`]) but may not publish a group's
    /// commit, so a visitor's many links cannot churn the workspace's channels.
    pub key_access: bool,
}

impl LinkScope {
    /// The scope of a device a host admits to everything its membership
    /// allows: every record, its own frames, and the workspace's channel keys.
    /// A host with no admission hook admits its members' devices so.
    #[cfg(not(target_arch = "wasm32"))]
    fn admitting() -> Self {
        Self {
            reads: None,
            own_frames_only: false,
            key_access: true,
        }
    }
}

/// What a link may send and be sent, beyond the workspace's admission: a
/// scope for what it is sent, and the checks on what it may send.
#[derive(Clone, Copy, Debug)]
struct LinkAdmit {
    /// The scope of what the link is sent ([`LinkScope::reads`]).
    #[cfg(not(target_arch = "wasm32"))]
    scope: LinkScope,
    /// Whether the link carries only frames its own device authored: the
    /// scope's own flag, or a device that does not keep the workspace.
    own_frames_only: bool,
    /// Whether the link may carry only the frames a device takes the
    /// workspace's records with, and nothing a member device alone may send:
    /// a link from a device that said it takes records only as it opens them.
    records_only: bool,
    /// Whether the link's device may publish a channel's commits
    /// ([`LinkScope::key_access`]); asking a channel to admit it is allowed
    /// either way.
    key_access: bool,
}

impl LinkAdmit {
    /// What a link from `remote` is limited to: the scope the host admitted it
    /// with, and, for a device that does not keep the workspace, its own
    /// frames alone.
    #[cfg(not(target_arch = "wasm32"))]
    fn of(link: &DirectSync, remote: [u8; 32], scope: LinkScope, records_only: bool) -> Self {
        Self {
            scope,
            own_frames_only: scope.own_frames_only || !link.keeps_workspace(remote),
            records_only,
            key_access: scope.key_access,
        }
    }

    /// What the side that dialled holds its link to: nothing beyond the
    /// workspace's admission, since it dialled the device it meant to reach.
    fn dialling() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            scope: LinkScope::admitting(),
            own_frames_only: false,
            records_only: false,
            key_access: true,
        }
    }

    /// Whether a frame from the link's device may be admitted: its own
    /// authorship ([`Self::own_frames_only`]), the frames a device that does
    /// not keep the workspace may send at all ([`Self::records_only`]), and a
    /// channel's commit only from a device admitted for its keys
    /// ([`Self::key_access`]).
    fn permits(self, own: [u8; 32], remote: [u8; 32], authored: &Authored) -> bool {
        // A device the host confined to one record writes that record and no
        // other: what it sends of another is refused here, as the same scope
        // refuses to send it another's frames. A hosted environment's device is
        // the one a host admits this way, and its own record is all it has.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(only) = self.scope.reads
            && matches!(&authored.message, NeighbourMsg::AppDocument { .. })
            && record_of(&authored.message).is_some_and(|record| record != only)
        {
            return false;
        }
        if self.records_only && !records_frame(&authored.message) {
            return false;
        }
        if !self.key_access && matches!(authored.message, NeighbourMsg::MlsGovernance { .. }) {
            return false;
        }
        !self.own_frames_only || by_remote(&authored.message, authored.from, remote, own)
    }
}

/// Whether `message` is one a device that takes the workspace's records only
/// as it opens them sends: a record's write, a channel's governance, the
/// workspace's own state, its identity, and what it asks for and answers about
/// a record's content. Anything a member device alone sends — a shared
/// screen's frames, remote input, a permission grant — is not.
fn records_frame(message: &NeighbourMsg) -> bool {
    matches!(
        message,
        NeighbourMsg::AppDocument { .. }
            | NeighbourMsg::SyncResp { .. }
            | NeighbourMsg::IdentityHello { .. }
            | NeighbourMsg::MlsGovernance { .. }
            | NeighbourMsg::MlsAccessRequest { .. }
            | NeighbourMsg::DocumentContentWant { .. }
            | NeighbourMsg::DocumentContentPiece { .. }
            | NeighbourMsg::DocumentContentLacks { .. }
            | NeighbourMsg::DocumentContentRefused { .. }
    )
}

/// Whether `message`, which the link's stream says `from` wrote, is the frame
/// of `remote`, the device the transport proved, and names no other device in
/// the places a frame carries one: a write's signed author, a record's
/// requester and responder, a channel frame's signer.
fn by_remote(message: &NeighbourMsg, from: [u8; 32], remote: [u8; 32], own: [u8; 32]) -> bool {
    if from != remote {
        return false;
    }
    match message {
        NeighbourMsg::AppDocument { peer, envelope, .. } => {
            envelope.author.as_bytes() == &remote && *peer == peer_handle(&remote)
        }
        NeighbourMsg::IdentityHello { cert, .. } => cert.device_public_key == remote,
        NeighbourMsg::DocumentContentWant { requester, .. } => *requester == peer_handle(&remote),
        NeighbourMsg::DocumentContentPiece {
            responder,
            requester,
            ..
        }
        | NeighbourMsg::DocumentContentLacks {
            responder,
            requester,
            ..
        }
        | NeighbourMsg::DocumentContentRefused {
            responder,
            requester,
            ..
        } => *responder == peer_handle(&remote) && *requester == peer_handle(&own),
        NeighbourMsg::MlsGovernance { frame, .. } => frame.signed_by.as_bytes() == &remote,
        NeighbourMsg::MlsAccessRequest { request, .. } => request.device.as_bytes() == &remote,
        _ => true,
    }
}

/// The records a link that takes them only as it opens them is replayed: every
/// record every device is sent, and each chain's newest part. Read once per
/// answer, from what this device holds ([`neighbour_wire::document_sync`]).
#[cfg(not(target_arch = "wasm32"))]
struct EagerRecords {
    /// The newest part each chain this device holds has, by its app and base:
    /// its number, and its channel.
    newest: BTreeMap<(String, String), (u64, [u8; 32])>,
}

#[cfg(not(target_arch = "wasm32"))]
impl EagerRecords {
    /// What `link` is replayed of the records its device does not open.
    fn of(link: &DirectSync) -> Self {
        let mut newest = BTreeMap::new();
        for (app, record) in link.listing() {
            let neighbour_wire::DocumentSync::Part { base, index } =
                neighbour_wire::document_sync(&record)
            else {
                continue;
            };
            let channel = neighbour_wire::app_document_group(&app, &record);
            let entry = newest
                .entry((app, base.to_owned()))
                .or_insert((index, channel));
            if index >= entry.0 {
                *entry = (index, channel);
            }
        }
        Self { newest }
    }

    /// The channels a link replayed these records follows: each chain's newest
    /// part, and the chain it belongs to, so a write to the part after reaches
    /// it ([`opened_admits`]).
    fn followed(&self) -> Vec<[u8; 32]> {
        let mut channels = Vec::with_capacity(self.newest.len().saturating_mul(2));
        for ((app, base), (_, part)) in &self.newest {
            channels.push(*part);
            channels.push(neighbour_wire::app_document_group(
                app,
                &neighbour_wire::chain_head(base),
            ));
        }
        channels
    }

    /// Whether `message`, a frame of this device's history, is one the link is
    /// replayed: a record every device is sent, or a chain's newest part.
    /// Governance, identity and asks carry no record and are replayed.
    fn admits(&self, message: &NeighbourMsg) -> bool {
        let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            ..
        } = message
        else {
            return true;
        };
        match neighbour_wire::document_sync(document_id) {
            neighbour_wire::DocumentSync::Eager => true,
            neighbour_wire::DocumentSync::Part { base, index } => self
                .newest
                .get(&(app_id.clone(), base.to_owned()))
                .is_none_or(|(newest, _)| index >= *newest),
            neighbour_wire::DocumentSync::OnDemand
            | neighbour_wire::DocumentSync::Chunk { .. }
            | neighbour_wire::DocumentSync::ChainHead { .. } => false,
        }
    }
}
/// The record a frame belongs to, as a link's device counts what it reaches:
/// a record's write, its channel's governance. `None` for a frame of no record
/// — the workspace's own document, an identity, an ask, and content, which is
/// answered to the device that asked for it and reaches no other — which every
/// link of the workspace carries.
fn record_of(message: &NeighbourMsg) -> Option<[u8; 32]> {
    match message {
        NeighbourMsg::AppDocument {
            app_id,
            document_id,
            ..
        } => Some(neighbour_wire::app_document_group(app_id, document_id)),
        NeighbourMsg::MlsGovernance { workspace, .. }
        | NeighbourMsg::MlsAccessRequest { workspace, .. } => {
            match neighbour_wire::KeyScope::parse(workspace)? {
                neighbour_wire::KeyScope::Document { app, document, .. } => {
                    Some(neighbour_wire::app_document_group(&app, &document))
                }
                neighbour_wire::KeyScope::Workspace(_) => None,
            }
        }
        _ => None,
    }
}

/// The message an authored frame `bytes` holds, with the device that wrote it, read as
/// [`NeighbourMsg::decode`] reads a message.
fn decode_authored(bytes: &[u8]) -> Result<([u8; 32], NeighbourMsg), postcard::Error> {
    let (from, message) = bytes
        .split_first_chunk::<32>()
        .ok_or(postcard::Error::DeserializeUnexpectedEnd)?;
    Ok((*from, NeighbourMsg::decode(message)?))
}

/// A device's replica, shared by the device's engine and its direct links.
///
/// Clone it to hand it to an [`iroh::protocol::Router`] under [`DIRECT_ALPN`]
/// (it is the accepting [`ProtocolHandler`]) and to dial with
/// [`Self::connect`]; every clone is the same replica.
#[derive(Clone)]
pub struct DirectSync(Arc<Inner>);

/// A frame a link brought into this device's engine, as its engine sees it.
#[derive(Clone)]
pub struct Heard {
    /// The device that authored it: what its signed content names, else the
    /// device whose frame the host forwarded.
    pub author: [u8; 32],
    pub message: NeighbourMsg,
    /// Whether the workspace took it in.
    pub landed: bool,
    /// Whether it came from the other device's retained history rather than
    /// live traffic.
    pub retained: bool,
    /// What the workspace's admission decided: the frame is already in, and
    /// this says how. An [`crate::AdmissionError::Device`] means this device
    /// could not decide it, never that the frame was refused.
    pub report: Result<crate::AdmissionReport, crate::AdmissionError>,
    /// Whether this device had not taken the frame in before, on any carrier:
    /// a sealed frame its store already held is one it carried on already.
    pub fresh: bool,
}

/// A reader of what a device's links bring its engine ([`DirectSync::reader`]).
///
/// The links hand each frame to a channel that keeps the newest ones and drops
/// the oldest for a reader that falls behind. A dropped frame the workspace
/// retains is still in its store, so this reader takes those it missed from
/// there, after where it stands, before it reads on: each once, judged by the
/// workspace again, never one this device wrote itself, and a frame the
/// channel still held after the catch-up handed it is not read twice. What
/// the store never keeps — presence, asks for content and their answers,
/// channel governance — is asked again by whoever wants it.
pub struct Reader {
    link: DirectSync,
    heard: broadcast::Receiver<Heard>,
    /// Where this reader stands in the workspace's retained history.
    cursors: Vec<Cursor>,
    /// The frames the last catch-up handed, whose copy still in the channel
    /// is skipped.
    caught: std::collections::BTreeSet<(crate::retained::Lineage, u64)>,
    pending: std::collections::VecDeque<Heard>,
    /// Whether this reader fell behind and has not caught up yet.
    behind: bool,
}

impl Reader {
    /// The next frame the link brought, or `None` once the link is gone.
    /// Cancel-safe: a catch-up cut short is taken again on the next call.
    pub async fn recv(&mut self) -> Option<Heard> {
        loop {
            if self.behind {
                self.catch_up().await;
            }
            if let Some(heard) = self.pending.pop_front() {
                return Some(heard);
            }
            match self.heard.recv().await {
                Ok(heard) => {
                    if let Ok(frame) = Frame::of(&heard.message) {
                        if self.caught.remove(&(frame.lineage, frame.seq)) {
                            continue;
                        }
                        advance(&mut self.cursors, std::slice::from_ref(&heard.message));
                    }
                    return Some(heard);
                }
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    tracing::info!(
                        missed,
                        "a reader fell behind its link; it takes what it missed from the workspace's history"
                    );
                    self.behind = true;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Take the retained frames after this reader's cursors in, judged by the
    /// workspace as the link judged them when they came.
    async fn catch_up(&mut self) {
        let missed = match self.link.missed(&self.cursors).await {
            Ok(missed) => missed,
            Err(error) => {
                tracing::warn!(%error, "a reader that fell behind its link could not catch up");
                self.behind = false;
                return;
            }
        };
        self.behind = false;
        self.caught.clear();
        for message in missed {
            let Some(author) = author_of(&message) else {
                continue;
            };
            let identity = Frame::of(&message).map(|frame| (frame.lineage, frame.seq));
            advance(&mut self.cursors, std::slice::from_ref(&message));
            if let Ok(identity) = identity {
                self.caught.insert(identity);
            }
            self.pending
                .push_back(self.link.judge(author, message, true, false).await);
        }
    }
}

/// A frame on its way to this device's links, with the device that authored
/// it: a workspace's host forwards a member's frame under the member's name,
/// so every member knows whose it is.
#[derive(Clone)]
struct Authored {
    from: [u8; 32],
    message: NeighbourMsg,
    /// Whether this device published the frame itself just now, rather than
    /// carrying it from its replica or forwarding another device's: a device
    /// hosting its own workspace shares its key with the member it serves, so
    /// whose name a frame bears does not say whether the other end has it.
    published: bool,
}

/// What one link carries besides what this device sends every link: its own
/// ask for the other device's state, and its answer to the other's. Several
/// waiting at once go out as one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OnlyHere {
    /// A frame here waits for history: ask the other device for its state.
    Ask,
    /// The other device asked for this device's state.
    Answer,
}

/// Two devices of one workspace in one process, paired from the moment this
/// was made ([`DirectSync::pairing`]): their link, ready to sync.
#[cfg(not(target_arch = "wasm32"))]
pub struct Pairing {
    member: DirectSync,
    holder: DirectSync,
    live_mine: broadcast::Receiver<Authored>,
    live_theirs: broadcast::Receiver<Authored>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Pairing {
    /// Sync the two devices until the process ends ([`DirectSync::pair`]).
    ///
    /// # Errors
    ///
    /// Either device is not in the workspace's roster.
    pub async fn sync(self) -> Result<(), String> {
        let Self {
            member,
            holder,
            live_mine,
            live_theirs,
        } = self;
        let (own, theirs) = (member.device(), holder.device());
        holder.admits(own, false)?;
        member.admits(theirs, false)?;
        // Each side's catch-up is history, however old what it carries: it is
        // judged as retained, never as a late live frame.
        member
            .admit_history(
                holder
                    .catch_up()
                    .await?
                    .into_iter()
                    .map(|authored| (authored.from, authored.message))
                    .collect(),
            )
            .await;
        for authored in member.catch_up().await? {
            holder.admit(authored.from, authored.message, true).await;
        }
        member.replay_from(&holder).await?;
        if member.0.store.is_none() {
            *member.0.other_store.lock_recover() = Some(OtherStore::Paired(holder.clone()));
        }
        member.0.caught_up.send_replace(true);
        holder.0.caught_up.send_replace(true);
        let to_holder = forward(live_mine, &member, &holder, false);
        let to_member = forward(live_theirs, &holder, &member, false);
        tokio::try_join!(to_holder, to_member)?;
        Ok(())
    }
}

/// The streams of a dialled link after the other device admitted this one:
/// what runs it, once its dialer has answered.
pub struct Dialed {
    transport: Arc<dyn Transport>,
    /// The device dialled: whose the frames are that the other end's history names no author
    /// for.
    device: [u8; 32],
    /// Whether this device takes the workspace's records only as it opens
    /// them ([`DirectSync::when_opened`]).
    opens: bool,
    control: Control,
    /// Subscribed when the dial was answered: what this device sends from
    /// then on is the link's to carry, never lost between the dial and the
    /// link running.
    live: broadcast::Receiver<Authored>,
}

impl Dialed {
    /// Sync with the device on the other end until the link ends.
    ///
    /// # Errors
    ///
    /// As [`DirectSync::connect`]; a link the other device refused ends with
    /// the reason it gave ([`DirectSync::end`]).
    pub async fn run(self, link: &DirectSync) -> Result<(), Refusal> {
        let ran = link
            .link(
                self.transport.as_ref(),
                Some(self.device),
                self.control,
                self.live,
                self.opens,
                LinkAdmit::dialling(),
            )
            .await
            .map_err(Refusal::Other);
        self.transport.close_reason().map_or(ran, Err)
    }
}

/// Where this device's answers to the other device's writes go, and what
/// writes them on the stream it opens for them: after its frames', so the
/// other device takes the two in that order. A device that keeps no copy of
/// the workspace (`answers` false) answers nothing and opens no stream.
async fn answering(
    transport: &dyn Transport,
    answers: bool,
) -> Result<
    (
        Option<tokio::sync::mpsc::UnboundedSender<Answered>>,
        std::pin::Pin<Box<impl std::future::Future<Output = Result<(), String>>>>,
    ),
    String,
> {
    let (answering, answered) = tokio::sync::mpsc::unbounded_channel::<Answered>();
    if !answers {
        return Ok((None, Box::pin(answer_all(None, answered))));
    }
    let stream = transport.open_uni().await?;
    Ok((
        Some(answering),
        Box::pin(answer_all(Some(stream), answered)),
    ))
}

/// Write each of `answers` on `stream` as it comes; on no stream, wait for the link to end.
async fn answer_all(
    stream: Option<StreamSend>,
    mut answers: tokio::sync::mpsc::UnboundedReceiver<Answered>,
) -> Result<(), String> {
    let Some(mut stream) = stream else {
        return std::future::pending().await;
    };
    while let Some(answered) = answers.recv().await {
        let bytes = postcard::to_allocvec(&answered).map_err(|error| error.to_string())?;
        crate::frames::write_frame(&mut stream, &bytes).await?;
    }
    Ok(())
}

/// The wall clock in nanoseconds since the Unix epoch: when a device's
/// certificate expires, and a fresh value for a state request.
fn now_ns() -> u64 {
    tilo_clock::wall()
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_nanos()).ok())
        .unwrap_or(u64::MAX)
}

#[derive(Clone)]
struct Admission {
    owner: crate::owner::NativeWorkspaceOwner,
    workspace: tilo_desktop_model::WorkspaceId,
}

struct Inner {
    replica: Replica,
    /// Where this device's engine owns the workspace: frames are admitted into
    /// its live replica and published as they land.
    admission: Option<Admission>,
    /// This device's ownership of its key, which signs what the link writes.
    signer: Arc<dyn DeviceSigner>,
    /// The sealed history this device keeps for the workspace, when it is a
    /// store device. A page keeps none: it takes the records it opens from the
    /// device it dialled ([`OtherStore::Dialled`]).
    #[cfg(not(target_arch = "wasm32"))]
    store: Option<Arc<Store>>,
    outgoing: broadcast::Sender<Authored>,
    heard: broadcast::Sender<Heard>,
    /// What the devices at this device's links' other ends answered of its writes.
    answered: broadcast::Sender<Answered>,
    changed: watch::Sender<u64>,
    /// Set once the other device's retained history is in hand.
    caught_up: watch::Sender<bool>,
    /// Set once this device has written what the other one lacks — its
    /// certificate, the revocations and the workspace's governance — to the
    /// link. The other one decides the retained history it asks for next
    /// against that governance, so the history waits for it.
    catch_up_written: watch::Sender<bool>,
    /// Why the workspace ended for good here, once it did ([`DirectSync::end`]).
    ended: watch::Sender<Option<Refusal>>,
    /// The devices whose links this device ended, and why: a device that lost
    /// its access is sent nothing and refused a new link until a host admits
    /// it again ([`DirectSync::end_link`]).
    ended_links: Arc<RwLock<BTreeMap<[u8; 32], Refusal>>>,
    /// Told each link whose device's links this device ends, while it runs
    /// ([`DirectSync::end_link`]).
    endings: broadcast::Sender<([u8; 32], Refusal)>,
    /// Asks the link this device dialled to read the other device's retained
    /// history again ([`DirectSync::ask_replay`]).
    replay: Arc<tokio::sync::Notify>,
    /// This device is the workspace's host on its links: what they bring in,
    /// it sends on to its other links, so a member's frame reaches every
    /// member through one path.
    fan_out: std::sync::atomic::AtomicBool,
    /// The geneses of the workspace's group this host carries.
    geneses: Geneses,
    /// The other device's store, for a device that keeps none: where a reader
    /// that fell behind takes the frames it missed ([`Reader`]).
    other_store: Arc<std::sync::Mutex<Option<OtherStore>>>,
    /// Whether this device takes the workspace's records only as it opens
    /// them, and says so to the device it dials ([`DirectSync::when_opened`]):
    /// a page reaches its workspace that way.
    opens: bool,
    /// The records this device asks the one it dialled to open
    /// ([`DirectSync::open_record`]), while the link runs.
    opening: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<RecordAsk>>>>,
    /// Answers a test loses, as a link that ended before they came
    /// ([`DirectSync::lose_answers`]).
    #[cfg(feature = "test-support")]
    lost_answers: Arc<std::sync::atomic::AtomicUsize>,
}

/// The geneses of a workspace's group its host carries, each with its
/// author: what it answers a member's access request with
/// ([`DirectSync::answer_request`]). Taken from the channel history as a link
/// starts, and from every frame the host sends its links.
type Geneses =
    Arc<std::sync::Mutex<std::collections::HashMap<collab_types::WorkspaceId, Vec<Authored>>>>;

/// The store on the other end of a device's link.
#[derive(Clone)]
enum OtherStore {
    /// The device it pairs with in this process ([`DirectSync::pair`]).
    #[cfg(not(target_arch = "wasm32"))]
    Paired(DirectSync),
    /// The device it dialled, asked over the link's control stream.
    Dialled(tokio::sync::mpsc::UnboundedSender<HistoryAsk>),
}

/// An ask for the frames a store holds after some cursors, and where the
/// answer goes.
type HistoryAsk = (
    Vec<Cursor>,
    tokio::sync::oneshot::Sender<Result<Vec<NeighbourMsg>, String>>,
);

/// A round of frames each with its author, and whether more are left: what a
/// device that opens a record is answered.
#[cfg(not(target_arch = "wasm32"))]
type Round = (Vec<([u8; 32], NeighbourMsg)>, bool);

/// An ask for one record's frames, by its channel, and where the answer goes.
type RecordAsk = ([u8; 32], tokio::sync::oneshot::Sender<Result<(), String>>);

impl std::fmt::Debug for DirectSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectSync")
            .field("device", &hex::encode(self.0.signer.device_public_key()))
            .finish_non_exhaustive()
    }
}

impl DirectSync {
    /// Share `replica`, the replica of the device `signer` is the ownership of
    /// (and whose key its iroh endpoint's secret key is).
    #[must_use]
    pub fn new(replica: StatePeer, signer: impl DeviceSigner + 'static) -> Self {
        Self::over(Replica::new(replica), signer)
    }

    /// Sync `replica`, which other parts of the engine read as well: the
    /// action service authorizes against the state this link admits into.
    #[must_use]
    pub fn over(replica: Replica, signer: impl DeviceSigner + 'static) -> Self {
        // The replica is this device's copy: told so, it keeps as this device
        // what a base it adopts lacks (`StatePeer::write_as_device`).
        replica.write(|peer| peer.write_as_device(signer.device_public_key()));
        Self(Arc::new(Inner {
            replica,
            admission: None,
            signer: Arc::new(signer),
            #[cfg(not(target_arch = "wasm32"))]
            store: None,
            outgoing: broadcast::channel(OUTGOING).0,
            heard: broadcast::channel(OUTGOING).0,
            answered: broadcast::channel(OUTGOING).0,
            changed: watch::channel(0).0,
            caught_up: watch::channel(false).0,
            catch_up_written: watch::channel(false).0,
            ended: watch::channel(None).0,
            ended_links: Arc::default(),
            endings: broadcast::channel(ENDINGS).0,
            replay: Arc::default(),
            fan_out: std::sync::atomic::AtomicBool::new(false),
            geneses: Geneses::default(),
            other_store: Arc::default(),
            opens: false,
            opening: Arc::default(),
            #[cfg(feature = "test-support")]
            lost_answers: Arc::default(),
        }))
    }

    /// This link's channels, device and state, for a link built on it.
    fn copied(&self) -> Inner {
        Inner {
            replica: self.0.replica.clone(),
            admission: self.0.admission.clone(),
            signer: Arc::clone(&self.0.signer),
            #[cfg(not(target_arch = "wasm32"))]
            store: self.0.store.clone(),
            outgoing: self.0.outgoing.clone(),
            heard: self.0.heard.clone(),
            answered: self.0.answered.clone(),
            changed: self.0.changed.clone(),
            caught_up: self.0.caught_up.clone(),
            catch_up_written: self.0.catch_up_written.clone(),
            ended: self.0.ended.clone(),
            ended_links: Arc::clone(&self.0.ended_links),
            endings: self.0.endings.clone(),
            replay: Arc::clone(&self.0.replay),
            fan_out: std::sync::atomic::AtomicBool::new(self.fans_out()),
            geneses: Arc::clone(&self.0.geneses),
            other_store: Arc::clone(&self.0.other_store),
            opens: self.0.opens,
            opening: Arc::clone(&self.0.opening),
            #[cfg(feature = "test-support")]
            lost_answers: Arc::clone(&self.0.lost_answers),
        }
    }

    /// Keep this workspace's sealed history and channel governance at `store`
    /// and hand them to the devices that ask: what a store device does with
    /// every link. Call it before cloning the link to its handlers.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn with_store(&self, store: Arc<Store>) -> Self {
        Self(Arc::new(Inner {
            store: Some(store),
            ..self.copied()
        }))
    }

    /// Take the workspace's records only as this device opens them, and say
    /// so to the device it dials: it is replayed the records every device is
    /// sent and each chain's newest part, and every other record's history
    /// only once this device opens it ([`Self::open_record`]). What a page
    /// keeps no store to hold the workspace's records with does.
    #[must_use]
    pub fn when_opened(&self) -> Self {
        Self(Arc::new(Inner {
            opens: true,
            ..self.copied()
        }))
    }

    /// Ask the device this one dialled for the record on `channel`: what its
    /// store keeps of the record reaches this device, and every write of it
    /// from then on. A chain's head (`neighbour_wire::chain_head`) names
    /// whichever part is the chain's newest.
    ///
    /// # Errors
    ///
    /// This device dialled no link, or the link ended before the record came.
    pub async fn open_record(&self, channel: [u8; 32]) -> Result<(), String> {
        let opening = self.0.opening.lock_recover().clone();
        let Some(opening) = opening else {
            return Err("this device dialled no link to open a record on".into());
        };
        let (answer, answered) = tokio::sync::oneshot::channel();
        opening
            .send((channel, answer))
            .map_err(|_| "the link ended".to_owned())?;
        answered.await.map_err(|_| "the link ended".to_owned())?
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// Whether the workspace names `device` among the devices keeping it: one
    /// that holds its records and may pass another device's frames on, rather
    /// than the device of a page or an environment that reaches one record.
    fn keeps_workspace(&self, device: [u8; 32]) -> bool {
        self.0
            .replica
            .lock()
            .governance()
            .keeping_devices()
            .iter()
            .any(|address| address.host.device_public_key == device)
    }

    /// The same link onto the same replica and store, with room for `capacity`
    /// frames heard and not read yet: what a test that makes a reader fall
    /// behind the link builds, where the link otherwise keeps
    /// [`OUTGOING`] frames.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_heard_capacity(&self, capacity: usize) -> Self {
        Self(Arc::new(Inner {
            heard: broadcast::channel(capacity).0,
            ..self.copied()
        }))
    }

    /// Admit every frame this link brings into `owner`'s live replica of
    /// `workspace` and publish it, instead of into this device's copy alone:
    /// what a link brings is durable as it lands, and the device's own engine
    /// decides it. The link then syncs that replica.
    pub fn owning(
        &self,
        owner: crate::owner::NativeWorkspaceOwner,
        workspace: tilo_desktop_model::WorkspaceId,
    ) -> Result<Self, String> {
        let replica = owner.replica(workspace)?;
        Ok(Self(Arc::new(Inner {
            replica,
            admission: Some(Admission { owner, workspace }),
            ..self.copied()
        })))
    }

    /// The replica this link syncs.
    #[must_use]
    pub fn replica(&self) -> Replica {
        self.0.replica.clone()
    }

    /// The store this device keeps the workspace's history at, when it is a
    /// store device.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn store(&self) -> Option<&Arc<Store>> {
        self.0.store.as_ref()
    }

    /// Read the replica.
    pub fn read<R>(&self, read: impl FnOnce(&StatePeer) -> R) -> R {
        self.0.replica.read(read)
    }

    /// Make a local change the caller signed, e.g.
    /// `publication_mut().publish_incremental`, keep the message it returns at
    /// this device's store, send it to every open link, and return it.
    ///
    /// A write that also commits the workspace's own document — a checkpoint's
    /// coverage, say — sends that change on the same links as the write did:
    /// the workspace's changes are eager, and a device holding a frame until
    /// its coverage comes would still wait for it if the coverage waited for
    /// an ask.
    ///
    /// # Errors
    /// `write` refused or its frame could not be kept.
    pub fn publish(
        &self,
        write: impl FnOnce(&mut StatePeer) -> Result<NeighbourMsg, String>,
    ) -> Result<NeighbourMsg, String> {
        let (message, workspace) = {
            let mut peer = self.0.replica.lock();
            let heads = peer.workspace_heads().unwrap_or_default();
            let message = write(&mut peer)?;
            (message, peer.workspace_changes_since(&heads))
        };
        #[cfg(not(target_arch = "wasm32"))]
        self.keep(&message, false)
            .map_err(|error| error.to_string())?;
        #[cfg(not(target_arch = "wasm32"))]
        self.note_own(&message);
        self.0.changed.send_modify(bump);
        self.outgoing(Authored {
            from: self.device(),
            message: message.clone(),
            published: true,
        });
        if !workspace.is_empty() {
            self.outgoing(Authored {
                from: self.device(),
                message: NeighbourMsg::SyncResp {
                    changes: workspace,
                    workspace: self.0.replica.workspace(),
                },
                published: true,
            });
        }
        Ok(message)
    }

    /// Make `change` to the replica in one signed turn of the device's signer:
    /// each run changes a copy of the replica as it stands, and only a run that
    /// asked for no signature the signer had not given yet replaces it. The
    /// signer signs between runs, with the replica unlocked.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the replica stays held until the change replaces the state it was made from"
    )]
    async fn signed_change<T>(
        &self,
        mut change: impl FnMut(&mut StatePeer, &dyn DeviceSigner, SystemTime) -> Result<T, String>,
    ) -> Result<T, String> {
        let now = tilo_clock::wall();
        crate::owner::write::signed_turn(&*self.0.signer, |signed| {
            let mut replica = self.0.replica.lock();
            let mut candidate = replica.clone();
            let changed = change(&mut candidate, signed, now)?;
            signed.settled()?;
            *replica = candidate;
            Ok(changed)
        })
        .await
    }

    /// Ticks whenever the replica changed, by a local change or a frame a
    /// link admitted.
    #[must_use]
    pub fn changes(&self) -> watch::Receiver<u64> {
        self.0.changed.subscribe()
    }

    /// What the devices at this device's links' other ends answer of its own
    /// writes, on links of [`DIRECT_ALPN`]. A receiver that falls behind loses
    /// answers, and the writes they name wait for the next link's catch-up.
    /// A store device keeps every answer besides ([`Self::record_reach`]).
    #[must_use]
    pub fn answers(&self) -> broadcast::Receiver<Answered> {
        self.0.answered.subscribe()
    }

    /// Every frame a link brings this device's engine, in the order the
    /// workspace took them in. A receiver that falls behind loses frames;
    /// [`Self::reader`] does not.
    #[must_use]
    pub fn heard(&self) -> broadcast::Receiver<Heard> {
        self.0.heard.subscribe()
    }

    /// A reader of every frame a link brings this device's engine, from now
    /// on, that misses none of the workspace's retained frames: one that fell
    /// behind takes those it missed from the workspace's store
    /// ([`Reader::recv`]).
    #[must_use]
    pub fn reader(&self) -> Reader {
        Reader {
            heard: self.heard(),
            cursors: self.cursors(),
            link: self.clone(),
            caught: std::collections::BTreeSet::new(),
            pending: std::collections::VecDeque::new(),
            behind: false,
        }
    }

    /// The retained frames after `cursors` a reader of this device's links
    /// missed, each with whether it is the first copy this device took in:
    /// from this device's store, the frames its links brought it; from the
    /// other device's, every frame not this device's own, as the other device
    /// sent them all here.
    async fn missed(&self, cursors: &[Cursor]) -> Result<Vec<NeighbourMsg>, String> {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(store) = &self.0.store {
            let workspace = self.workspace()?;
            let mut cursors = cursors.to_vec();
            let mut missed = Vec::new();
            loop {
                let replay = store
                    .replay(workspace, &cursors, Budget::default())
                    .map_err(|error| error.to_string())?;
                advance(&mut cursors, &replay.frames);
                missed.extend(replay.frames.into_iter().filter(|message| {
                    Frame::of(message).is_ok_and(|frame| store.taken_from_link(workspace, &frame))
                }));
                if !replay.more {
                    return Ok(missed);
                }
            }
        }
        let other = self.0.other_store.lock_recover().clone();
        let frames = match other {
            None => return Err("neither end of the link keeps the workspace's history".into()),
            #[cfg(not(target_arch = "wasm32"))]
            Some(OtherStore::Paired(host)) => {
                let mut cursors = cursors.to_vec();
                let mut frames = Vec::new();
                loop {
                    let (round, more) = host.replay(&cursors)?;
                    advance(&mut cursors, &round);
                    frames.extend(round);
                    if !more {
                        break frames;
                    }
                }
            }
            Some(OtherStore::Dialled(asks)) => {
                let (answer, answered) = tokio::sync::oneshot::channel();
                asks.send((cursors.to_vec(), answer))
                    .map_err(|_| "the link ended".to_owned())?;
                answered.await.map_err(|_| "the link ended".to_owned())??
            }
        };
        let own = self.device();
        Ok(frames
            .into_iter()
            .filter(|message| author_of(message).is_some_and(|author| author != own))
            .collect())
    }

    /// Read the other device's retained history again, from where this
    /// device's replica stands, on the link this device dialled: what a write
    /// let go here unapplied waited for has landed
    /// ([`crate::ReceiveOutcome::replay_wanted`]), and the other device
    /// still keeps the write. Asked while the link reads the history
    /// already, it reads it once more after.
    pub fn ask_replay(&self) {
        self.0.replay.notify_one();
    }

    /// Lose the next `count` answers this device's links bring, as a link that
    /// ended after the other device kept a write and before its answer came:
    /// what a write waits for the next link's catch-up to be asked again on.
    #[cfg(feature = "test-support")]
    pub fn lose_answers(&self, count: usize) {
        self.0
            .lost_answers
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the other device's retained history is in hand on the link this
    /// device dialled: true once this link has taken its store in, and false
    /// again while a link being made takes it in. The device that was dialled
    /// takes the other device's history in as it answers, and says so then.
    #[must_use]
    pub fn caught_up(&self) -> watch::Receiver<bool> {
        self.0.caught_up.subscribe()
    }

    /// Send `message` to every open link, as this device's live traffic: what
    /// the workspace keeps of it does not come from here.
    ///
    /// # Errors
    ///
    /// The frame could not be kept.
    pub fn send(&self, message: NeighbourMsg) -> Result<(), String> {
        #[cfg(not(target_arch = "wasm32"))]
        self.keep(&message, false)
            .map_err(|error| error.to_string())?;
        #[cfg(not(target_arch = "wasm32"))]
        self.note_own(&message);
        self.outgoing(Authored {
            from: self.device(),
            message,
            published: true,
        });
        Ok(())
    }

    /// Carry `message`, `from`'s signed frame this device took in apart from
    /// its links (its own service's write, or one it wrote for an agent of
    /// its own), to every device on this link, once: kept at this device's
    /// store, the history a device that joins later replays, and sent live to
    /// the devices linked now. A frame the store already holds went out when
    /// it first arrived, and is not sent again. Returns whether it went out.
    ///
    /// # Errors
    ///
    /// The store could not keep it; nothing went out.
    pub fn carry(&self, from: [u8; 32], message: &NeighbourMsg) -> Result<bool, String> {
        #[cfg(not(target_arch = "wasm32"))]
        if !self
            .keep(message, false)
            .map_err(|error| error.to_string())?
        {
            return Ok(false);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if from == self.device() {
            self.note_own(message);
        }
        self.outgoing(Authored {
            from,
            message: message.clone(),
            published: false,
        });
        Ok(true)
    }

    /// Note `message`, this device's own write, as pending at its store until
    /// another device keeping the workspace takes it in ([`crate::reach`]).
    #[cfg(not(target_arch = "wasm32"))]
    fn note_own(&self, message: &NeighbourMsg) {
        if let (Some(store), Ok(workspace)) = (&self.0.store, self.workspace()) {
            store.note_own_write(workspace, message);
        }
    }

    /// This device's writes of `app`'s `record` that no other device keeping
    /// the workspace has taken in yet, and those it refused, as this device's
    /// store keeps them; `None` for a device that keeps no store.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn record_reach(&self, app: &str, record: &str) -> Option<crate::reach::RecordReach> {
        let store = self.0.store.as_ref()?;
        let workspace = self.workspace().ok()?;
        Some(store.record_reach(workspace, app, record))
    }

    /// This device's own writes no other device keeping the workspace has
    /// answered yet, each to go out once more as a link starts: the device at
    /// its other end answers each, whether it took it in just now or before.
    #[cfg(not(target_arch = "wasm32"))]
    fn unreached(&self) -> Vec<Authored> {
        let (Some(store), Ok(workspace)) = (&self.0.store, self.workspace()) else {
            return Vec::new();
        };
        let device = self.device();
        store
            .unreached(workspace)
            .into_iter()
            .map(|message| Authored {
                from: device,
                message,
                published: true,
            })
            .collect()
    }

    /// Hand `authored` to every open link; no open link is no error, as the
    /// next one catches up from the replica.
    fn outgoing(&self, authored: Authored) {
        if self.fans_out() {
            self.note_genesis(&authored);
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.keep_channel(&authored);
        let author = authored.from;
        let links = self.0.outgoing.send(authored).unwrap_or(0);
        tracing::debug!(
            workspace = ?self.workspace().ok(),
            author = %short(&author),
            links,
            "a frame goes out on the links up now"
        );
    }

    /// Keep `authored` at this device's store when it is channel governance
    /// and this device is the workspace's host on its links: what a device
    /// that links later joins each channel's group from
    /// ([`Store::keep_channel`]). Only the host keeps and hands it, so a
    /// member's older copy never stands in for the newest.
    #[cfg(not(target_arch = "wasm32"))]
    fn keep_channel(&self, authored: &Authored) {
        if !self.fans_out() || !carried_governance(&authored.message) {
            return;
        }
        let (Some(store), Ok(workspace)) = (&self.0.store, self.workspace()) else {
            return;
        };
        if let Err(error) = store.keep_channel(workspace, authored.from, &authored.message) {
            tracing::warn!(%error, "a channel's governance was not kept at the store; a device that links later asks its members for it");
        }
    }

    /// Note `authored` when it is a group's genesis this host carries.
    fn note_genesis(&self, authored: &Authored) {
        let NeighbourMsg::MlsGovernance { frame, .. } = &authored.message else {
            return;
        };
        if !matches!(frame.action, collab_types::MlsGovernanceAction::Genesis) {
            return;
        }
        let mut geneses = self.0.geneses.lock_recover();
        let known = geneses.entry(frame.workspace).or_default();
        if !known.iter().any(|kept| kept.message == authored.message) {
            known.push(authored.clone());
        }
        drop(geneses);
    }

    /// Answer `message` when it is a member's access request: with the
    /// geneses of its group this host carries, so the device asks to join
    /// the group.
    fn answer_request(&self, message: &NeighbourMsg) {
        let NeighbourMsg::MlsAccessRequest { request, .. } = message else {
            return;
        };
        let known = self
            .0
            .geneses
            .lock_recover()
            .get(&request.workspace)
            .cloned();
        for answer in known.unwrap_or_default() {
            let _ = self.0.outgoing.send(answer);
        }
    }

    /// Make this device the workspace's host on its links: what they bring in
    /// it sends on to its other links.
    fn host(&self) {
        self.0
            .fan_out
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Whether this device is the workspace's host on its links
    /// ([`Serving::serve`]).
    fn fans_out(&self) -> bool {
        self.0.fan_out.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Dial `device` at `addr` from `endpoint` and answer its admission,
    /// leaving the link ready to run ([`Dialed::run`]): the other device
    /// listens before this returns, so what this device publishes from now on
    /// reaches it. `addr` is the device's own key, or the service address of a
    /// server its signed host hint binds to it: a server answers on a key of
    /// its own, and what its history names no author for is still its device's.
    ///
    /// # Errors
    ///
    /// The connection fails, the other device refuses this one, or this
    /// device is not in the workspace's roster. A holder that refuses this
    /// device while it dials gives the reason typed, so a workspace its owner
    /// deleted says so here and not only on a link that had come up.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn dial(
        &self,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
        device: [u8; 32],
    ) -> Result<Dialed, Refusal> {
        let dialed = self.reach(endpoint, addr, device, false).await?;
        // Who the dialled device is, this one learns from what it sends: its
        // certificate is in the frames it opens with, and a device that has
        // not synced yet holds no roster to refuse by.
        self.certificate().map_err(Refusal::Other)?;
        Ok(dialed)
    }

    /// Dial `device` over `transport`, a link this device already opened to it
    /// — a page's WebTransport session — and answer its admission, as
    /// [`Self::dial`] does.
    ///
    /// # Errors
    ///
    /// As [`Self::dial`].
    pub async fn dial_over(
        &self,
        transport: Arc<dyn Transport>,
        device: [u8; 32],
    ) -> Result<Dialed, Refusal> {
        let dialed = self.named_over(transport, device, false).await?;
        self.certificate().map_err(Refusal::Other)?;
        Ok(dialed)
    }

    /// Dial `device` at `addr` from `endpoint` and answer its admission, as
    /// [`Self::dial`] does, whether or not the workspace as this device has
    /// it names it yet. A device that `takes` only the other's store says so
    /// ([`LinkFrame::Take`]), and the other admits it by what the workspace
    /// there names.
    #[cfg(not(target_arch = "wasm32"))]
    async fn reach(
        &self,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
        device: [u8; 32],
        takes: bool,
    ) -> Result<Dialed, Refusal> {
        let addr: EndpointAddr = addr.into();
        let connection = endpoint
            .connect(addr, DIRECT_ALPN)
            .await
            .map_err(|error| Refusal::Other(error.to_string()))?;
        // The link's first stream proves this device to the one dialled, and that one back,
        // before the dial names its workspace: every host reads the proof first.
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| Refusal::Other(error.to_string()))?;
        let workspace = self.workspace().map_err(Refusal::Other)?;
        crate::link_proof::prove_dialled(
            &mut send,
            &mut recv,
            &self.certificate().map_err(Refusal::Other)?,
            self.0.signer.as_ref(),
            &self.revocations(),
            &workspace.to_string(),
            device,
        )
        .await
        .map_err(Refusal::Other)?;
        send.finish()
            .map_err(|error| Refusal::Other(error.to_string()))?;
        self.named_over(Arc::new(connection), device, takes).await
    }

    /// Prove the device at the other end of `connection`, a link this device accepted, on its
    /// first stream ([`crate::link_proof`]), as the device the connection's own key names.
    ///
    /// # Errors
    ///
    /// The device did not prove itself, or proved another device than the connection's.
    #[cfg(not(target_arch = "wasm32"))]
    async fn prove_accepted(&self, connection: &Connection) -> Result<(), String> {
        let (mut send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|error| error.to_string())?;
        let proved = crate::link_proof::proved_dialler(
            &mut send,
            &mut recv,
            &self.certificate()?,
            self.0.signer.as_ref(),
            &self.revocations(),
        )
        .await?;
        if proved != Transport::remote(connection) {
            return Err("the dialling device proved another key than its connection's".into());
        }
        Ok(())
    }

    /// The revocations this device's replica of the workspace holds: what a link's proof checks
    /// the device at the other end against.
    #[cfg(not(target_arch = "wasm32"))]
    fn revocations(&self) -> tilo_user_identity::RevocationList {
        let mut revocations = tilo_user_identity::RevocationList::new();
        let held = self.0.replica.lock().revocations();
        for entry in held {
            // An entry this workspace cannot verify is one it does not keep.
            let _ = revocations.insert(entry);
        }
        revocations
    }

    /// Name this device's workspace on `transport` ([`LinkFrame::Workspace`])
    /// and wait for the other device to say it listens.
    async fn named_over(
        &self,
        transport: Arc<dyn Transport>,
        device: [u8; 32],
        takes: bool,
    ) -> Result<Dialed, Refusal> {
        let (mut send, mut recv) = transport.open_bi().await.map_err(Refusal::Other)?;
        let workspace = self.workspace().map_err(Refusal::Other)?;
        let named = if takes {
            LinkFrame::Take(workspace)
        } else {
            LinkFrame::Workspace {
                id: workspace,
                opens: self.0.opens,
            }
        };
        write_link(&mut send, &named)
            .await
            .map_err(Refusal::Other)?;
        // The other device listens before this returns, so what the caller
        // publishes from now on reaches it: an unsealed write it missed is in
        // no store to catch up from.
        match read_link(&mut recv, MAX_FRAME_BYTES).await {
            Ok(Some(LinkFrame::Ready)) => {}
            Ok(other) => {
                return Err(Refusal::Other(format!(
                    "the device answered {other:?} instead of that it listens"
                )));
            }
            // A device that refuses the dial closes with its reason typed: a
            // workspace its owner deleted is one this device forgets rather
            // than dials again.
            Err(error) => {
                return Err(transport.close_reason().unwrap_or(Refusal::Other(error)));
            }
        }
        // Subscribed before the dial returns: the caller may send at once.
        let live = self.0.outgoing.subscribe();
        Ok(Dialed {
            transport,
            device,
            opens: self.0.opens,
            control: Control { send, recv },
            live,
        })
    }

    /// Sync with the device at `addr` until the link ends.
    ///
    /// # Errors
    ///
    /// As [`Self::dial`].
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn connect(
        &self,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
    ) -> Result<(), Refusal> {
        let addr = addr.into();
        let device = *addr.id.as_bytes();
        let dialed = self.dial(endpoint, addr, device).await?;
        dialed.run(self).await
    }

    /// Take in, from the store of `device` dialled at `addr`, every frame of
    /// the workspace it keeps that this device's store does not, and answer
    /// where this device's store stands once each of them is kept here: what
    /// a device that is handed the workspace does before the device handing
    /// it may let it go, and what a device that stays does before another
    /// lets it go. The frames are asked for from this device's store's own
    /// cursors, so whatever reached its copy by another way is taken all the
    /// same, and admitted as the history a link's catch-up brings is. The
    /// link ends once they are kept.
    ///
    /// # Errors
    ///
    /// This device keeps no store, the dial failed or was refused, the other
    /// device's history did not arrive whole, or a frame of it was not kept.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn take_store(
        &self,
        endpoint: &Endpoint,
        addr: impl Into<EndpointAddr>,
        device: [u8; 32],
    ) -> Result<Vec<Cursor>, Refusal> {
        let store = self
            .0
            .store
            .clone()
            .ok_or_else(|| Refusal::Other("this device keeps no store".into()))?;
        let workspace = self.workspace().map_err(Refusal::Other)?;
        let Dialed {
            transport,
            mut control,
            ..
        } = self.reach(endpoint, addr, device, true).await?;
        let fetched = fetch_store(&mut control, standing(&store, workspace))
            .await
            .map_err(|error| transport.close_reason().unwrap_or(Refusal::Other(error)))?;
        let frames: Vec<Frame> = fetched
            .iter()
            .filter_map(|(_, message)| Frame::of(message).ok())
            .collect();
        self.admit_history(fetched).await;
        // Each is in this device's store before it says where it stands: one
        // the admission kept is there already, and one it did not keep is
        // written now, or the store says why not.
        for frame in &frames {
            store
                .retain(workspace, frame)
                .map_err(|error| Refusal::Other(format!("a frame was not kept: {error}")))?;
        }
        transport.close(0, b"taken");
        tracing::debug!(
            workspace = ?workspace,
            from = %short(&device),
            frames = frames.len(),
            "the other device's store is taken in"
        );
        Ok(standing(&store, workspace))
    }

    /// How many frames of the workspace this device's store keeps that
    /// `device` may read and, its own store standing at `cursors`, does not
    /// keep: none once it keeps every one this device does, whatever more it
    /// keeps of its own.
    ///
    /// # Errors
    ///
    /// A frame this device keeps could not be read back.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn lacked_by(&self, device: [u8; 32], cursors: &[Cursor]) -> Result<usize, String> {
        let Some(store) = &self.0.store else {
            return Ok(0);
        };
        let workspace = self.workspace()?;
        let reaches = self.reach_of(device, LinkScope::default());
        let mut after = cursors.to_vec();
        let mut lacked = 0_usize;
        loop {
            let replay = store
                .replay(workspace, &after, Budget::default())
                .map_err(|error| error.to_string())?;
            lacked = lacked.saturating_add(
                replay
                    .frames
                    .iter()
                    .filter(|message| reaches(message))
                    .count(),
            );
            advance(&mut after, &replay.frames);
            if !replay.more || replay.frames.is_empty() {
                return Ok(lacked);
            }
        }
    }

    /// Sync `member` with `holder`, two devices of one workspace in one
    /// process: what a link carries, with no network in between — each side's
    /// certificate and checkpoint, then the retained history `member` lacks
    /// from `holder`'s store, then every frame either publishes while both
    /// live, until the process ends.
    ///
    /// # Errors
    ///
    /// Either device is not in the workspace's roster.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn pair(member: &Self, holder: &Self) -> Result<(), String> {
        Self::pairing(member, holder).sync().await
    }

    /// The pairing of `member` with `holder` ([`Self::pair`]), taking what
    /// either publishes from now on: a write `member` makes before the pairing
    /// syncs reaches `holder` all the same, as a dialled link takes what its
    /// device publishes from the dial on.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn pairing(member: &Self, holder: &Self) -> Pairing {
        Pairing {
            live_mine: member.0.outgoing.subscribe(),
            live_theirs: holder.0.outgoing.subscribe(),
            member: member.clone(),
            holder: holder.clone(),
        }
    }

    /// Take in every frame `host` retains that this device's cursors do not
    /// cover, round by round.
    #[cfg(not(target_arch = "wasm32"))]
    async fn replay_from(&self, host: &Self) -> Result<(), String> {
        let mut cursors = self.cursors();
        loop {
            let (frames, more) = host.replay(&cursors)?;
            let mut authored = Vec::with_capacity(frames.len());
            for message in &frames {
                let author = author_of(message)
                    .ok_or("the store replayed a message that is not a document frame")?;
                authored.push((author, message.clone()));
            }
            if self.admit_history(authored).await {
                let state = host.answer_state_request().await;
                self.admit_history(
                    state
                        .into_iter()
                        .map(|state| (state.from, state.message))
                        .collect(),
                )
                .await;
            }
            advance(&mut cursors, &frames);
            if !more {
                return Ok(());
            }
        }
    }

    /// The frames `cursors` do not cover at this device's store, and whether
    /// more are left.
    #[cfg(not(target_arch = "wasm32"))]
    fn replay(&self, cursors: &[Cursor]) -> Result<(Vec<NeighbourMsg>, bool), String> {
        let Some(store) = &self.0.store else {
            return Ok((Vec::new(), false));
        };
        let replay = store
            .replay(self.workspace()?, cursors, Budget::default())
            .map_err(|error| error.to_string())?;
        Ok((replay.frames, replay.more))
    }

    /// The device key this link signs for.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.0.signer.device_public_key()
    }

    /// Serve the link a device dialled this one: its first frame names the
    /// workspace.
    ///
    /// # Errors
    ///
    /// The device named no workspace, or one this replica is not.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn serve(&self, transport: &dyn Transport) -> Result<(), String> {
        let dialed = dialed_workspace(transport).await?;
        if dialed.workspace != self.workspace()? {
            return Err(refuse(
                transport,
                &Refusal::Other("this device holds no such workspace".into()),
            ));
        }
        self.serve_as(transport, dialed, LinkScope::admitting())
            .await
    }

    /// Serve the link `dialed` named, the host having admitted its device with
    /// `scope` ([`Serving::admitting`]).
    #[cfg(not(target_arch = "wasm32"))]
    async fn serve_as(
        &self,
        transport: &dyn Transport,
        dialed: DialedWorkspace,
        scope: LinkScope,
    ) -> Result<(), String> {
        let DialedWorkspace {
            takes,
            opens,
            control,
            ..
        } = dialed;
        if let Some(refusal) = self.ended_link(transport.remote()) {
            return Err(refuse(transport, &refusal));
        }
        if takes {
            return self.give_store(transport, control, scope).await;
        }
        let live = self.0.outgoing.subscribe();
        let admit = LinkAdmit::of(self, transport.remote(), scope, opens);
        self.link(transport, None, control, live, opens, admit)
            .await
    }

    /// Sync with the device on the other end of `transport` until the link ends, whichever side
    /// dialled: `dialled` names the device this side dialled, and the side that accepted reads it
    /// off the transport. What a link starts by sending: the dialling device's catch-up, and the
    /// channels' governance a host hands with it from its store ([`Store::channels`]). The device
    /// that accepted the dial hands its replica's catch-up with the history the other one asks for
    /// ([`Self::answer_store`]), where it is judged as history: on the live stream, what it carries
    /// that was written more than the live window ago was refused as a late frame.
    async fn link_catch_up(&self, dialing: bool) -> Result<Vec<Authored>, String> {
        #[cfg_attr(
            target_arch = "wasm32",
            allow(unused_mut, reason = "a page keeps no store to open the link with")
        )]
        let mut catch_up = if dialing {
            self.catch_up().await?
        } else {
            Vec::new()
        };
        #[cfg(not(target_arch = "wasm32"))]
        if let (true, Some(store), Ok(workspace)) =
            (self.fans_out(), &self.0.store, self.workspace())
        {
            let history: Vec<Authored> = store
                .channels(workspace)
                .into_iter()
                .map(|(from, message)| Authored {
                    from,
                    message,
                    published: false,
                })
                .collect();
            for authored in &history {
                self.note_genesis(authored);
            }
            catch_up.extend(history);
        }
        Ok(catch_up)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "a link's sending, receiving, answers and history run as one select over its streams"
    )]
    async fn link(
        &self,
        transport: &dyn Transport,
        dialled: Option<[u8; 32]>,
        control: Control,
        live: broadcast::Receiver<Authored>,
        opens: bool,
        admit: LinkAdmit,
    ) -> Result<(), String> {
        let dialing = dialled.is_some();
        let remote = dialled.unwrap_or_else(|| transport.remote());
        let mut control = control;
        // Subscribed before the ended devices are read: an ending sent
        // between the two is seen by the channel or by the read, whichever.
        let mut endings = self.0.endings.subscribe();
        if let Some(refusal) = self.ended_link(remote) {
            return Err(refuse(transport, &refusal));
        }
        // The records this link opened ([`LinkFrame::Open`]), for the frames
        // it is sent from then on.
        let opened = Arc::new(RwLock::new(BTreeSet::new()));
        // The device that accepted a dial refuses one of the workspace's
        // roster it does not know before a frame is read; the one that
        // dialled cannot judge, and takes only what its roster verifies.
        if !dialing && let Err(reason) = self.admits(remote, false) {
            return Err(refuse(transport, &self.refusal_to(remote, reason)));
        }
        if !dialing {
            // Subscribed by the caller already: the dialling device publishes
            // nothing before this reaches it.
            write_link(&mut control.send, &LinkFrame::Ready).await?;
        }
        // Taken after the catch-up, which the caller subscribed before: a
        // change published in between reaches the link after the history
        // rather than not at all.
        #[cfg_attr(
            target_arch = "wasm32",
            allow(
                unused_mut,
                reason = "a page keeps no store of its writes to send again"
            )
        )]
        let mut catch_up = self.link_catch_up(dialing).await?;
        // This device's writes no device keeping the workspace answered yet go
        // once more, after what the link opens with: the other device answers
        // each, so a write made while it was away reaches it and says so.
        #[cfg(not(target_arch = "wasm32"))]
        catch_up.extend(self.unreached());
        tracing::debug!(
            workspace = ?self.workspace().ok(),
            remote = %short(&remote),
            dialing,
            opens,
            catch_up = catch_up.len(),
            "a direct link starts"
        );
        let send = transport.open_uni().await?;
        // Both ends answer each other's writes, except a device that takes the
        // records only as it opens them: it keeps no copy of the workspace, so
        // its taking a write in is no write reaching the workspace.
        let (answering, answer_all) = answering(transport, !(dialing && self.0.opens)).await?;
        // What this link alone carries, besides what every link does: this
        // device's asks for the other's state, and its answers to the other's.
        let (own, only_here) = tokio::sync::mpsc::unbounded_channel();
        // The accepting device's live frames follow the first history it
        // answers, which carries the governance they are judged against.
        let (answered, first_answer) = watch::channel(dialing);
        // A page keeps no store and answers no ask for history.
        #[cfg(target_arch = "wasm32")]
        drop(answered);
        let sending = send_authored(
            self,
            send,
            Sending {
                remote,
                dialing,
                // A link takes the records its device opens only when that is
                // the device on the other end: this side's own flag is what
                // this side's catch-up already obeys.
                opens: opens && !dialing,
                catch_up,
                first_answer,
                opened: Arc::clone(&opened),
            },
            live,
            only_here,
        );
        // The dialling device takes the other device's retained history in
        // before the live writes the link brings meanwhile: each author's
        // writes to a record apply in the order the author wrote them.
        let (history_in_hand, in_hand) = watch::channel(!dialing);
        let receiving = Box::pin(self.receive_all(
            transport,
            remote,
            &own,
            in_hand,
            Answering {
                sender: answering.as_ref(),
            },
            admit,
        ));
        let history = async {
            #[cfg(not(target_arch = "wasm32"))]
            if !dialing {
                // The other device asks for the history as the link starts,
                // and again whenever it wants what it let go.
                self.answer_store(&mut control, remote, &answered, admit.scope, opens, &opened)
                    .await?;
                self.0.caught_up.send_replace(true);
                return std::future::pending().await;
            }
            // A link being made has the other device's history to take in again,
            // however much of it an earlier link brought: what waits for this
            // link to catch up knows it is not in hand until this one says so.
            self.0.caught_up.send_replace(false);
            self.ask_store(&mut control).await?;
            history_in_hand.send_replace(true);
            // The history is in hand: the frames from here on are the live
            // ones the link brings. A write this device let go unapplied
            // waits in the other device's history; once what it waited for
            // lands here, the history is read again from where the replica
            // stands. A reader of this link that fell behind it asks the same
            // history for the frames it missed.
            // Installed before the link says it caught up: a caller that
            // waits for that may open a record at once.
            let (asks, mut asked) = tokio::sync::mpsc::unbounded_channel();
            *self.0.other_store.lock_recover() = Some(OtherStore::Dialled(asks));
            let (opening, mut opens) = tokio::sync::mpsc::unbounded_channel();
            *self.0.opening.lock_recover() = Some(opening);
            self.0.caught_up.send_replace(true);
            loop {
                tokio::select! {
                    () = self.0.replay.notified() => self.ask_store(&mut control).await?,
                    Some((cursors, answer)) = asked.recv() => {
                        let fetched = Self::fetch_history(&mut control, cursors).await;
                        let failed = fetched.as_ref().err().cloned();
                        let _ = answer.send(fetched);
                        if let Some(error) = failed {
                            return Err(error);
                        }
                    }
                    Some((channel, answer)) = opens.recv() => {
                        let opened = self.open_record_here(&mut control, channel).await;
                        let failed = opened.as_ref().err().cloned();
                        let _ = answer.send(opened);
                        if let Some(error) = failed {
                            return Err(error);
                        }
                    }
                }
            }
        };
        let ended = async {
            let mut ended = self.0.ended.subscribe();
            ended.wait_for(Option::is_some).await.ok()?.clone()
        };
        // This device's links to its own peer, ended alone ([`DirectSync::end_link`]):
        // the workspace's other links keep running.
        let ending = async {
            loop {
                match endings.recv().await {
                    Ok((device, refusal)) if device == remote => return Some(refusal),
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // A missed ending is still in the state that keeps it.
                        if let Some(refusal) = self.ended_link(remote) {
                            return Some(refusal);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return std::future::pending().await;
                    }
                }
            }
        };
        let ended = tokio::select! {
            result = receiving => result,
            result = sending => result,
            result = answer_all => result,
            result = history => result,
            Some(reason) = ending => Err(refuse(transport, &reason)),
            Some(reason) = ended => Err(refuse(transport, &reason)),
        };
        tracing::debug!(
            workspace = ?self.workspace().ok(),
            remote = %short(&remote),
            dialing,
            ended = ?ended,
            "a direct link ended"
        );
        ended
    }

    /// Ask the device this one dialled for the record `channel`
    /// ([`LinkFrame::Open`]) from where this device stands, taking each round
    /// in as it comes.
    async fn open_record_here(
        &self,
        control: &mut Control,
        channel: [u8; 32],
    ) -> Result<(), String> {
        let mut cursors = self.cursors();
        write_link(
            &mut control.send,
            &LinkFrame::Open {
                channel,
                cursors: cursors.clone(),
            },
        )
        .await?;
        loop {
            let Some(LinkFrame::Frames { frames, more }) =
                read_link(&mut control.recv, MAX_FRAME_BYTES).await?
            else {
                return Err("the device answered something else than retained frames".into());
            };
            let messages: Vec<NeighbourMsg> =
                frames.iter().map(|(_, message)| message.clone()).collect();
            advance(&mut cursors, &messages);
            self.admit_history(frames).await;
            if !more {
                return Ok(());
            }
            write_link(
                &mut control.send,
                &LinkFrame::Open {
                    channel,
                    cursors: cursors.clone(),
                },
            )
            .await?;
        }
    }

    /// The workspace ended for good on this device, for `reason`: every link of
    /// it closes, refused with that reason, which the device on the other end
    /// reads ([`Dialed::run`]).
    pub fn end(&self, refusal: Refusal) {
        self.0.ended.send_replace(Some(refusal));
    }

    /// End this device's links to `remote` alone, telling it `reason`: what a
    /// host does to a device that lost its access, revoked or narrowed, while
    /// the workspace's other links stay up and are sent what it writes. A link
    /// `remote` starts afterwards is refused the same way, and its frames are
    /// not sent, until a host admits it again ([`Self::reopen_link`]).
    pub fn end_link(&self, remote: [u8; 32], refusal: Refusal) {
        self.0
            .ended_links
            .write_recover()
            .insert(remote, refusal.clone());
        let _ = self.0.endings.send((remote, refusal));
    }

    /// Admit `remote`'s links again, as a host does when a device it ended the
    /// links of is admitted once more ([`Self::end_link`]).
    pub fn reopen_link(&self, remote: [u8; 32]) {
        self.0.ended_links.write_recover().remove(&remote);
    }

    /// Why this device ended its links to `remote`, while it has them ended
    /// ([`Self::end_link`]).
    #[must_use]
    pub fn ended_link(&self, remote: [u8; 32]) -> Option<Refusal> {
        self.0.ended_links.read_recover().get(&remote).cloned()
    }

    /// Ask the other device for the history this device lacks, round by round
    /// while it says more are left, and return once it says no more are: the
    /// history is in hand. Each ask's first round opens with the other
    /// device's state, its catch-up ([`Self::catch_up`]), and the frames its
    /// store keeps follow; each frame is judged under the device the round
    /// names as its author. A frame still waiting for history after that waits
    /// for what the other device lacks too: asking again would bring nothing.
    async fn ask_store(&self, control: &mut Control) -> Result<(), String> {
        let mut cursors = self.cursors();
        let mut taken = 0_usize;
        write_link(&mut control.send, &LinkFrame::Cursors(cursors.clone())).await?;
        // Each round is taken in as it comes, while the other device reads
        // the next: what the device's apps open first is here before the
        // last of a long history is.
        while let Some(frame) = read_link(&mut control.recv, MAX_FRAME_BYTES).await? {
            let LinkFrame::Frames { frames, more } = frame else {
                return Err("the device answered something else than retained frames".into());
            };
            let messages = frames
                .iter()
                .map(|(_, message)| message.clone())
                .collect::<Vec<_>>();
            advance(&mut cursors, &messages);
            taken = taken.saturating_add(frames.len());
            if more {
                write_link(&mut control.send, &LinkFrame::Cursors(cursors.clone())).await?;
            }
            self.admit_history(frames).await;
            if !more {
                tracing::debug!(
                    workspace = ?self.workspace().ok(),
                    frames = taken,
                    "the other device's history is taken in"
                );
                return Ok(());
            }
        }
        Err("the device's store ended the history before the last frame".into())
    }

    /// The frames the other device's store holds after `cursors`, read over
    /// the link's control stream for a reader of this device's links that
    /// fell behind them.
    async fn fetch_history(
        control: &mut Control,
        cursors: Vec<Cursor>,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let fetched = fetch_store(control, cursors).await?;
        Ok(fetched.into_iter().map(|(_, message)| message).collect())
    }

    /// Answer the dialling device's cursors from this device's store, for as
    /// long as it asks: the history is in its hands once the first answer
    /// with no more left is sent, and it asks again for what it let go.
    ///
    /// Each ask's first round opens with this device's state, its catch-up
    /// ([`Self::catch_up`]): history, which the other device judges as such
    /// however old what it carries. `answered` is set once the first round is
    /// written.
    #[cfg(not(target_arch = "wasm32"))]
    async fn answer_store(
        &self,
        control: &mut Control,
        remote: [u8; 32],
        answered: &watch::Sender<bool>,
        scope: LinkScope,
        opens: bool,
        opened: &RwLock<BTreeSet<[u8; 32]>>,
    ) -> Result<(), String> {
        // The history this answers is decided by the other device against the
        // governance it has not been sent yet: it arrives with the catch-up,
        // and the frames asked for before it are the ones it cannot admit.
        // The answer waits for the catch-up to be written, so a device that
        // joins takes the workspace's governance in before its records.
        let mut written = self.0.catch_up_written.subscribe();
        while !*written.borrow_and_update() {
            if written.changed().await.is_err() {
                return Ok(());
            }
        }
        self.answer_asks(control, remote, Some(answered), scope, opens, opened)
            .await
    }

    /// Answer each ask of the device `remote` on `control` until it asks no
    /// more: the history after its cursors with the state this device holds
    /// first, or the history of one record it opens. Each answer of a live
    /// link is told to `answered`. A link that takes records only as it opens
    /// them (`opens`) is sent the records every device is replayed and each
    /// chain's newest part, and the history of a record it opens
    /// ([`LinkFrame::Open`]), which is added to `opened`.
    #[cfg(not(target_arch = "wasm32"))]
    async fn answer_asks(
        &self,
        control: &mut Control,
        remote: [u8; 32],
        answered: Option<&watch::Sender<bool>>,
        scope: LinkScope,
        opens: bool,
        opened: &RwLock<BTreeSet<[u8; 32]>>,
    ) -> Result<(), String> {
        let mut opens_an_ask = true;
        // The state an ask opens with that earlier answers had not allocated.
        let mut state = std::collections::VecDeque::new();
        let own = self.device();
        let reaches = self.reach_of(remote, scope);
        while let Some(frame) = read_link(&mut control.recv, MAX_FRAME_BYTES).await? {
            let cursors = match frame {
                LinkFrame::Cursors(cursors) => cursors,
                LinkFrame::Open { channel, cursors } => {
                    // A record outside what the link's scope reaches is
                    // answered as one this device keeps nothing of: the
                    // device that asked is told nothing of it, and nothing of
                    // it reaches the link from then on.
                    let answer = self.openable(scope, channel);
                    if let Some(answer) = answer {
                        let mut opened = opened.write_recover();
                        opened.insert(channel);
                        opened.insert(answer);
                    }
                    let (frames, more) = match answer {
                        Some(answer) => self.record_round(answer, &cursors, &reaches)?,
                        None => (Vec::new(), false),
                    };
                    tracing::debug!(
                        workspace = ?self.workspace().ok(),
                        record = %short(&channel),
                        frames = frames.len(),
                        more,
                        "a link's open of a record is answered"
                    );
                    write_link(&mut control.send, &LinkFrame::Frames { frames, more }).await?;
                    continue;
                }
                _ => return Err("the device asked for something else than retained frames".into()),
            };
            if let Some(store) = &self.0.store {
                // What the device asks from is what it holds: the store keeps
                // what it still lacks when it compacts.
                store
                    .holds(self.workspace()?, remote, &cursors)
                    .map_err(|error| error.to_string())?;
            }
            // The records a device that opens them is replayed, read once per
            // answer: a chain's newest part is what this device holds now, and
            // the link follows each chain it is replayed from then on.
            let eager = opens.then(|| EagerRecords::of(self));
            if let Some(eager) = &eager
                && opens_an_ask
            {
                opened.write_recover().extend(eager.followed());
            }
            if opens_an_ask {
                state.extend(
                    self.answer_state_request()
                        .await
                        .into_iter()
                        .filter(|state| reaches(&state.message))
                        .filter(|state| {
                            eager
                                .as_ref()
                                .is_none_or(|eager| eager.admits(&state.message))
                        })
                        .map(|state| (state.from, state.message)),
                );
            }
            // An answer carries a round's worth of bytes: the state holds a
            // checkpoint of every record, and in one answer a workspace's
            // records outgrew what a link takes in one frame, which ended the
            // link at every try. The state goes first, a round at a time, in
            // its order (its governance before the records it admits); the
            // store's frames follow once it is all sent.
            let with_state = !state.is_empty();
            let mut frames = Vec::new();
            let mut bytes = 0_u64;
            while let Some((_, message)) = state.front() {
                let size = postcard::to_allocvec(message).map_or(u64::MAX, |bytes| {
                    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
                });
                if !frames.is_empty() && bytes.saturating_add(size) > Budget::default().max_bytes {
                    break;
                }
                bytes = bytes.saturating_add(size);
                frames.extend(state.pop_front());
            }
            let more = if state.is_empty() {
                let (kept, more) = self.replay(&cursors)?;
                // A frame the store keeps names its own author; one that does
                // not is this device's.
                frames.extend(
                    kept.into_iter()
                        .filter(|message| reaches(message))
                        .filter(|message| eager.as_ref().is_none_or(|eager| eager.admits(message)))
                        .map(|message| (author_of(&message).unwrap_or(own), message)),
                );
                more
            } else {
                true
            };
            tracing::debug!(
                workspace = ?self.workspace().ok(),
                frames = frames.len(),
                more,
                with_state,
                "a link's ask for history is answered"
            );
            write_link(&mut control.send, &LinkFrame::Frames { frames, more }).await?;
            opens_an_ask = !more;
            if let Some(answered) = answered {
                answered.send_replace(true);
                if !more {
                    self.0.caught_up.send_replace(true);
                }
            }
        }
        Ok(())
    }

    /// One round of the frames of `channel` this device's store keeps after
    /// `cursors`, each with its author, and whether more are left: what a
    /// device that opens the record is sent.
    #[cfg(not(target_arch = "wasm32"))]
    fn record_round(
        &self,
        channel: [u8; 32],
        cursors: &[Cursor],
        reaches: &impl Fn(&NeighbourMsg) -> bool,
    ) -> Result<Round, String> {
        let Some(store) = &self.0.store else {
            return Ok((Vec::new(), false));
        };
        let replay = store
            .replay_record(self.workspace()?, channel, cursors, Budget::default())
            .map_err(|error| error.to_string())?;
        let own = self.device();
        let frames = replay
            .frames
            .into_iter()
            .filter(|message| reaches(message))
            .map(|message| (author_of(&message).unwrap_or(own), message))
            .collect();
        Ok((frames, replay.more))
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// The channel a record `channel` is opened as, for a link whose scope is
    /// `scope`: the record itself, or the newest part of the chain whose head
    /// it named. `None` for a record outside the scope, which the link is
    /// answered nothing of.
    fn openable(&self, scope: LinkScope, channel: [u8; 32]) -> Option<[u8; 32]> {
        if scope.reads.is_some_and(|only| only != channel) {
            return None;
        }
        Some(self.newest_part(channel).unwrap_or(channel))
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// The channel of the newest part of the chain whose head `head` names,
    /// when this device holds a part of it: a chain's head names whichever
    /// part is its newest, and holds nothing of its own.
    fn newest_part(&self, head: [u8; 32]) -> Option<[u8; 32]> {
        let mut newest: Option<(u64, [u8; 32])> = None;
        for (app, record) in self.listing() {
            let neighbour_wire::DocumentSync::Part { base, index } =
                neighbour_wire::document_sync(&record)
            else {
                continue;
            };
            if neighbour_wire::app_document_group(&app, &neighbour_wire::chain_head(base)) != head {
                continue;
            }
            if newest.is_none_or(|(newest, _)| index > newest) {
                newest = Some((index, neighbour_wire::app_document_group(&app, &record)));
            }
        }
        newest.map(|(_, channel)| channel)
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// Every record this device holds, by its app and its record id.
    fn listing(&self) -> Vec<(String, String)> {
        self.read(|peer| {
            peer.plaintext_routes()
                .into_iter()
                .chain(peer.sealed_routes())
                .collect()
        })
    }

    /// What this device's replica holds, as the cursors it asks a store to
    /// continue from.
    #[must_use]
    pub fn cursors(&self) -> Vec<Cursor> {
        self.read(cursors_of)
    }

    /// Every frame of the record on `channel` this device's store keeps, in the
    /// order a device that opens the record is sent them, and `None` when this
    /// device keeps no store. Read once, whole: a record that outgrows one
    /// round's budget is read on from where the last round ended.
    ///
    /// # Errors
    ///
    /// A kept frame could not be read back.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn kept_record(&self, channel: [u8; 32]) -> Result<Option<Vec<NeighbourMsg>>, String> {
        let Some(store) = &self.0.store else {
            return Ok(None);
        };
        let workspace = self.workspace()?;
        let mut frames = Vec::new();
        let mut cursors = Vec::new();
        loop {
            let replay = store
                .replay_record(workspace, channel, &cursors, Budget::default())
                .map_err(|error| error.to_string())?;
            advance(&mut cursors, &replay.frames);
            frames.extend(replay.frames);
            if !replay.more {
                return Ok(Some(frames));
            }
        }
    }

    /// Whether `device` may read `message`, judged as this device judges each
    /// frame before its link sends it another: the record's readers name it,
    /// and the workspace's policy lets it read the record.
    pub fn reads_of(&self, device: [u8; 32]) -> impl Fn(&NeighbourMsg) -> bool + use<> {
        self.reach_of(device, LinkScope::default())
    }

    /// Whether the workspace removed `device`'s person: it reads nothing the
    /// workspace says from then on, and what it sends is no longer taken.
    fn removed(&self, device: [u8; 32]) -> bool {
        self.0.replica.lock().governance().removed_device(device)
    }

    /// What the recipient may read when each frame is sent, `scope` naming the
    /// one record its device reaches when it reaches no more
    /// ([`LinkScope::reads`]).
    fn reach_of(
        &self,
        device: [u8; 32],
        scope: LinkScope,
    ) -> impl Fn(&NeighbourMsg) -> bool + use<> {
        let link = self.clone();
        move |message| {
            if let Some(only) = scope.reads
                && let Some(record) = record_of(message)
                && record != only
            {
                return false;
            }
            match message {
                // What answers a device's ask is addressed to it in the
                // handle the workspace knows it by: a piece of content goes
                // to its requester's link alone, not to every member's. The
                // ask itself reaches every other device, which may answer it.
                NeighbourMsg::DocumentContentPiece { requester, .. }
                | NeighbourMsg::DocumentContentLacks { requester, .. }
                | NeighbourMsg::DocumentContentRefused { requester, .. } => {
                    *requester == peer_handle(&device)
                }
                NeighbourMsg::AppDocument {
                    app_id,
                    document_id,
                    readers,
                    ..
                } => {
                    let replica = link.0.replica.lock();
                    let named_matches = readers
                        .as_deref()
                        .is_none_or(|readers| reads_named(&replica, readers, device));
                    // A device the workspace names among those keeping it is
                    // sent what it keeps: the workspace's policy answers a
                    // member's access, and such a device is no member, so what
                    // a member wrote never reached the device it links to.
                    let keeping = replica
                        .governance()
                        .keeping_devices()
                        .iter()
                        .any(|hint| hint.host.device_public_key == device);
                    named_matches
                        && (keeping
                            || !replica.has_governance()
                            || replica
                                .check_document_read(
                                    device,
                                    app_id,
                                    document_id,
                                    tilo_clock::wall(),
                                )
                                .is_ok())
                }
                NeighbourMsg::MlsGovernance { workspace, .. }
                | NeighbourMsg::MlsAccessRequest { workspace, .. } => {
                    match neighbour_wire::KeyScope::parse(workspace) {
                        Some(neighbour_wire::KeyScope::Document {
                            app,
                            document,
                            readers,
                            ..
                        }) => {
                            let replica = link.0.replica.lock();
                            let now = tilo_clock::wall();
                            readers
                                .as_deref()
                                .is_none_or(|readers| reads_named(&replica, readers, device))
                                && replica
                                    .check_document_read(device, &app, &document, now)
                                    .is_ok()
                        }
                        _ => true,
                    }
                }
                _ => true,
            }
        }
    }

    /// Answer a device that takes this device's store over `transport`
    /// ([`Self::take_store`]) with what the store keeps that it may read,
    /// and nothing live. A device the workspace names among those keeping
    /// it is let take it ([`certified`]), dialling on the service key its
    /// signed address binds to it.
    #[cfg(not(target_arch = "wasm32"))]
    async fn give_store(
        &self,
        transport: &dyn Transport,
        mut control: Control,
        scope: LinkScope,
    ) -> Result<(), String> {
        let remote = self.device_at(transport.remote());
        if let Err(reason) = self.admits(remote, true) {
            return Err(refuse(transport, &self.refusal_to(remote, reason)));
        }
        write_link(&mut control.send, &LinkFrame::Ready).await?;
        let opened = RwLock::new(BTreeSet::new());
        self.answer_asks(&mut control, remote, None, scope, false, &opened)
            .await
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// The device whose link dials this one from the endpoint `endpoint`: a
    /// device the workspace names among those keeping it may answer on a
    /// service key of its own, which its signed address binds to it; any
    /// other dials on its own key.
    fn device_at(&self, endpoint: [u8; 32]) -> [u8; 32] {
        self.0
            .replica
            .lock()
            .governance()
            .keeping_devices()
            .into_iter()
            .find(|address| address.service_public_key == endpoint)
            .map_or(endpoint, |address| address.host.device_public_key)
    }

    /// Whether `device` may sync with this replica: a certified device of
    /// its roster, or, when it only takes this device's store (`keeping`), of
    /// those the workspace names as keeping it ([`certified`]); unrevoked and
    /// unexpired, of a person the workspace has not removed.
    fn admits(&self, device: [u8; 32], keeping: bool) -> Result<(), String> {
        if self.removed(device) {
            return Err(REMOVED.into());
        }
        let replica = self.0.replica.lock();
        let certificate = if keeping {
            certified(&replica, device)
        } else {
            replica
                .certificates()
                .into_iter()
                .find(|certificate| certificate.device_public_key == device)
        };
        // A revocation reaches the keys its own root certified: another
        // person's entry naming this key refuses nothing of its device's.
        let revoked = certificate.as_ref().is_some_and(|certificate| {
            replica.revocations().iter().any(|entry| {
                (entry.user_public_key, entry.revoked_device_public_key)
                    == (certificate.user_public_key, device)
            })
        });
        drop(replica);
        let certificate = certificate.ok_or("the device is not in the workspace roster")?;
        if revoked {
            return Err("the device is revoked".into());
        }
        let now = now_ns();
        if certificate
            .expires_at_ns
            .is_some_and(|expiry| expiry <= now)
        {
            return Err("the device's certificate expired".into());
        }
        Ok(())
    }

    /// Why this device turned `device` away, `reason` being what
    /// [`Self::admits`] said: a device that was retired is told so with the
    /// root-signed entry that says it, which it keeps as an announced
    /// revocation, so a server that was away learns to let go rather than only
    /// that it is not admitted. A device lost, stolen or compromised is told
    /// only that it was removed: the entry says why, and the reason is the
    /// person's.
    fn refusal_to(&self, device: [u8; 32], reason: String) -> Refusal {
        self.revocation_of(device).map_or_else(
            || Refusal::NotAMember(reason),
            |entry| Refusal::Revoked(Box::new(entry)),
        )
    }

    /// The root-signed entry that retires `device`, as the replica carries it:
    /// the entry `device`'s roster certificate's root signed with
    /// [`RevocationReason::Retired`] ([`Self::admits`]). `None` when the replica
    /// carries none for it, or carries one for another reason
    /// ([`Self::refusal_to`]).
    fn revocation_of(&self, device: [u8; 32]) -> Option<tilo_user_identity::RevocationEntry> {
        let replica = self.0.replica.lock();
        let certificate = replica
            .certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == device)?;
        replica.revocations().into_iter().find(|entry| {
            (entry.user_public_key, entry.revoked_device_public_key)
                == (certificate.user_public_key, device)
                && entry.reason == tilo_user_identity::RevocationReason::Retired
        })
    }

    /// Whether `device` may send `workspace`'s frames to this device: a
    /// certified device of its roster, in its member list, unrevoked and
    /// unexpired ([`Self::admits`]). What a session's author is checked
    /// against, at the one point a link's other end is.
    ///
    /// # Errors
    ///
    /// Why the device may not: it is not in the roster, is revoked, expired,
    /// or of a person the workspace removed.
    pub fn admits_author(&self, device: [u8; 32]) -> Result<(), String> {
        self.admits(device, false)
    }

    /// What the other device may lack: this device's certificate and the
    /// revocations its person signed, then, from a holder of the workspace of
    /// its owning person, the governance and a checkpoint of every document,
    /// signed under the holder's own key. A member device signs no
    /// checkpoints, and a holder of another person cannot sign checkpoints
    /// others' writes depend on; their own writes reach the other device
    /// live.
    async fn catch_up(&self) -> Result<Vec<Authored>, String> {
        let device = self.device();
        let (hello, bootstraps) = {
            let replica = self.0.replica.lock();
            let cert = replica
                .certificates()
                .into_iter()
                .find(|certificate| certificate.device_public_key == device)
                .ok_or("this device is not in the workspace roster")?;
            let revocations = replica
                .revocations()
                .into_iter()
                .filter(|entry| entry.user_public_key == cert.user_public_key)
                .collect();
            let hello = NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(cert),
                revocations,
            };
            // Only a holder of the workspace signs checkpoints of it: a member device's link
            // is to a holder, which has every write the member made, and a member signing
            // snapshots would take numbers of its own sequence its engine never committed.
            (
                hello,
                self.0.admission.is_none()
                    && replica.has_governance()
                    && replica.signed_by_governor(&device),
            )
        };
        let mut messages = vec![hello];
        if bootstraps {
            messages.extend(
                self.signed_change(|peer, signer, now| {
                    peer.publication_mut().bootstrap_messages(signer, now)
                })
                .await?,
            );
        }
        Ok(messages
            .into_iter()
            .map(|message| Authored {
                from: device,
                message,
                published: false,
            })
            .collect())
    }

    /// Admit every frame the other device sends until its stream ends. A
    /// frame the replica refuses is that message's verdict, not the link's: the link
    /// stays up for the frames after it, and the device files what it refused
    /// as its own verdict in the workspace document, so the write's author
    /// reads why it is not here.
    ///
    /// A frame that leaves something waiting for history asks `remote` for
    /// its state, and `remote`'s ask for this device's is answered, each on
    /// `link`, this link alone: no other device sent the frame, or asked.
    ///
    /// An app write waits until `history` says the link's retained history is
    /// in hand: written after that history, it applies after it. The stream
    /// is read meanwhile, so the other device's writes never stall.
    async fn receive_all(
        &self,
        transport: &dyn Transport,
        remote: [u8; 32],
        link: &tokio::sync::mpsc::UnboundedSender<OnlyHere>,
        history: watch::Receiver<bool>,
        answering: Answering<'_>,
        admit: LinkAdmit,
    ) -> Result<(), String> {
        let mut recv = transport.accept_uni().await?;
        // Frames are taken off the stream as they are admitted; only the
        // app writes held for the history pile up while it is read.
        let (read, frames) = tokio::sync::mpsc::channel(1);
        let reading = async move {
            while let Some(authored) = read_authored(&mut recv).await? {
                if read.send(authored).await.is_err() {
                    break;
                }
            }
            Ok::<(), String>(())
        };
        let admitting = self.admit_in_order(remote, link, answering.sender, frames, history, admit);
        let received = async {
            tokio::try_join!(reading, async {
                admitting.await;
                Ok(())
            })
            .map(|_| ())
        };
        tokio::select! {
            result = received => result,
            result = Box::pin(self.take_answers(transport)) => result,
        }
    }

    /// Take what the device at the link's other end answers of this device's
    /// writes, on the stream it opens after its frames', and keep each at this
    /// device's store ([`crate::reach`]). A device that answers nothing opens
    /// no such stream, and this waits for the link to end.
    async fn take_answers(&self, transport: &dyn Transport) -> Result<(), String> {
        let mut recv = transport.accept_uni().await?;
        while let Some(bytes) = crate::frames::read_frame(&mut recv, MAX_FRAME_BYTES).await? {
            let answered: Answered =
                postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
            #[cfg(feature = "test-support")]
            if self
                .0
                .lost_answers
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |lost| lost.checked_sub(1),
                )
                .is_ok()
            {
                tracing::debug!("a test loses an answer this link brought");
                continue;
            }
            #[cfg(not(target_arch = "wasm32"))]
            if let (Some(store), Ok(workspace)) = (&self.0.store, self.workspace()) {
                store.answered_own_write(workspace, answered.write, &answered.kept);
            }
            let _ = self.0.answered.send(answered);
        }
        std::future::pending().await
    }

    /// Take in `frames`, the other device's live frames in the order it sent
    /// them, holding its app writes back until `history` is in hand.
    async fn admit_in_order(
        &self,
        remote: [u8; 32],
        link: &tokio::sync::mpsc::UnboundedSender<OnlyHere>,
        answering: Option<&tokio::sync::mpsc::UnboundedSender<Answered>>,
        mut frames: tokio::sync::mpsc::Receiver<Authored>,
        mut history: watch::Receiver<bool>,
        admit: LinkAdmit,
    ) {
        let mut waiting = Vec::new();
        loop {
            let next = tokio::select! {
                biased;
                Ok(_) = history.wait_for(|in_hand| *in_hand), if !waiting.is_empty() => None,
                next = frames.recv() => match next {
                    Some(authored) => Some(authored),
                    None => return,
                },
            };
            let in_hand = *history.borrow();
            if in_hand {
                for authored in std::mem::take(&mut waiting) {
                    self.receive_live(remote, link, answering, authored, admit)
                        .await;
                }
            }
            let Some(authored) = next else {
                continue;
            };
            if !in_hand && matches!(authored.message, NeighbourMsg::AppDocument { .. }) {
                waiting.push(authored);
                continue;
            }
            self.receive_live(remote, link, answering, authored, admit)
                .await;
        }
    }

    /// Whether `authored`, a frame `remote` sent live on its link, is decided as history: a
    /// member's own record write to the workspace this device hosts. A member keeps its writes
    /// held while its host is away, or while ownership's head is full, and sends them when it
    /// can: how long one waited says nothing of it. A server host decides a member's writes the
    /// same way, whatever carried them ([`TransportOrigin::RetainedServerDelta`]).
    ///
    /// What stands in for the live window is what a server host relies on too: the write's own
    /// signature by its author, its author's sequence on the record (a write replayed or
    /// numbered below one already taken is never taken again), and its author's access in the
    /// workspace's current governance (the role it holds now, not the one it held when it
    /// wrote, and no revoked device), decided as of the write's own time for what is timed. The
    /// link's other end is besides the device the link authenticated, and is still a device
    /// of the roster, unrevoked, unexpired and of a person not removed ([`Self::admits`]), as
    /// a server's session proves of the member it carries. A write that founds a record this
    /// device does not hold yet is history only from a device that may edit the record now, as
    /// a server host founds a record only for one. Any other frame, and every frame a device
    /// that does not host the workspace takes in, is judged live.
    fn member_history(&self, remote: [u8; 32], authored: &Authored) -> bool {
        let NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            ..
        } = &authored.message
        else {
            return false;
        };
        if authored.from != remote
            || envelope.author.as_bytes() != &remote
            || !self.fans_out()
            || self.admits(remote, false).is_err()
        {
            return false;
        }
        let now = tilo_clock::wall();
        let replica = self.0.replica.lock();
        replica.holds_document(app_id, document_id)
            || replica.permissions(remote, now).is_ok_and(|member| {
                let node = collab_types::validator::document_node(
                    &member.policy.workspace,
                    app_id,
                    document_id,
                );
                let cx = tilo_action_policy::Context::at_time(now);
                member.answer(&node, tilo_action_policy::ActionKind::Undoable, &cx)
                    == tilo_action_policy::Answer::Allow
            })
    }

    /// Take in one frame the other device sent live on the link.
    /// Admit `message`, `author`'s signed frame, exactly as a frame a link
    /// brings is admitted: judged as a member's own write is, by its signature,
    /// its author's sequence on its record and the author's access in the
    /// workspace now ([`Self::member_history`]), kept at this device's store and
    /// taken into the workspace's replica. Whether the write's author is a
    /// device that may send here at all is [`Self::admits_author`], checked
    /// before this is called, as a link's other end is.
    ///
    /// Returns whether the frame waits for history this device lacks, and
    /// whether the workspace kept it.
    pub async fn admit_one(&self, author: [u8; 32], message: NeighbourMsg) -> (bool, Option<Kept>) {
        let history = self.member_history(
            author,
            &Authored {
                from: author,
                message: message.clone(),
                published: false,
            },
        );
        self.admit_kept(author, message, false, history).await
    }

    async fn receive_live(
        &self,
        remote: [u8; 32],
        link: &tokio::sync::mpsc::UnboundedSender<OnlyHere>,
        answering: Option<&tokio::sync::mpsc::UnboundedSender<Answered>>,
        authored: Authored,
        admit: LinkAdmit,
    ) {
        // What a link from a device that does not keep the workspace carries:
        // its own frames, and none but those a device that opens the records
        // it reads takes. A frame it may not send is that frame's refusal,
        // never the link's, as a frame the workspace refuses is.
        if !admit.permits(self.device(), remote, &authored) {
            tracing::warn!(
                workspace = ?self.workspace().ok(),
                remote = %short(&remote),
                from = %short(&authored.from),
                kind = authored.message.kind(),
                "a link sent a frame its device may not send; it is refused"
            );
            return;
        }
        // An ask another device made reached this one only because a
        // host forwarded it, and is not this device's to answer.
        if matches!(authored.message, NeighbourMsg::SyncReq { .. }) {
            if authored.from == remote {
                let _ = link.send(OnlyHere::Answer);
            }
            return;
        }
        // The other device's own write is answered, named as its ownership names it.
        let write = match &authored.message {
            NeighbourMsg::AppDocument { envelope, .. } if authored.from == remote => {
                postcard::to_allocvec(&authored.message)
                    .ok()
                    .map(|bytes| (*blake3::hash(&bytes).as_bytes(), envelope.seq))
            }
            _ => None,
        };
        let (lacks_history, kept) = self.admit_one(authored.from, authored.message).await;
        if lacks_history {
            let _ = link.send(OnlyHere::Ask);
        }
        if let (Some(answering), Some((write, seq)), Some(kept)) = (answering, write, kept) {
            let _ = answering.send(Answered { write, seq, kept });
        }
    }

    /// Take `message` in as `claimed`'s frame, admitted into this device's
    /// workspace, kept at its store and handed to its engine. What the frame
    /// itself signs names its author over what the link says.
    ///
    /// Returns whether the frame left something waiting for history this
    /// device does not hold ([`crate::ReceiveOutcome::lacks_history`]): the
    /// device that sent it is asked for its state. What the workspace
    /// refuses of a frame is that frame's verdict, never the link's.
    async fn admit(&self, claimed: [u8; 32], message: NeighbourMsg, retained: bool) -> bool {
        self.admit_kept(claimed, message, retained, false).await.0
    }

    /// [`Self::admit`], answering besides whether the workspace keeps the frame, where that
    /// is settled: it took it in, or refused it for good. A frame that waits for history, or was
    /// refused for now, is answered nothing yet. A live frame that is `history` is judged as
    /// history all the same ([`Self::member_history`]).
    async fn admit_kept(
        &self,
        claimed: [u8; 32],
        message: NeighbourMsg,
        retained: bool,
        history: bool,
    ) -> (bool, Option<Kept>) {
        let author = author_of(&message).unwrap_or(claimed);
        if carried_governance(&message) {
            if self.fans_out() && !retained {
                self.answer_request(&message);
            }
            self.carry_governance(author, message, retained);
            return (false, None);
        }
        let heard = self.judge(author, message, retained, history).await;
        self.settle(heard)
    }

    /// Admit `frames`, history the other device holds, each with its author,
    /// in order: [`Self::admit`] for each of them, except that this device's
    /// owner takes in each batch of them between two carried governance frames
    /// together and commits the workspace once for the batch, not once for each
    /// frame. A device's first sync is its workspace's whole history, and a
    /// commit per frame rewrote what the device held for every one of them.
    ///
    /// Returns whether any frame left something waiting for history this
    /// device does not hold.
    async fn admit_history(&self, frames: Vec<([u8; 32], NeighbourMsg)>) -> bool {
        let Some(admission) = &self.0.admission else {
            let mut lacks_history = false;
            for (author, message) in frames {
                lacks_history |= self.admit(author, message, true).await;
            }
            return lacks_history;
        };
        let mut lacks_history = false;
        let mut batch = Vec::with_capacity(frames.len());
        for (claimed, message) in frames {
            let author = author_of(&message).unwrap_or(claimed);
            if carried_governance(&message) {
                lacks_history |= self
                    .admit_batch(admission, std::mem::take(&mut batch))
                    .await;
                self.carry_governance(author, message, true);
                continue;
            }
            batch.push((author, message));
        }
        lacks_history | self.admit_batch(admission, batch).await
    }

    /// Take `batch`, frames of retained history with their authors, into this
    /// device's owner in one commit, keep its verdicts on them, and settle each
    /// as [`Self::admit`] does. Whether any of them waits for history.
    async fn admit_batch(
        &self,
        admission: &Admission,
        batch: Vec<([u8; 32], NeighbourMsg)>,
    ) -> bool {
        if batch.is_empty() {
            return false;
        }
        let now = tilo_clock::wall();
        let inputs = batch
            .iter()
            .map(|(author, message)| {
                (
                    TransportOrigin::RetainedServerDelta(*author),
                    message.clone(),
                )
            })
            .collect();
        let reports = match admission
            .owner
            .receive_batch(admission.workspace, inputs, now)
        {
            Ok(reports) => reports,
            // The commit failed: so did every frame of it, as one frame's
            // failed commit fails that frame. This device failed to keep them,
            // which is no verdict on what their writers wrote.
            Err(error) => batch
                .iter()
                .map(|_| Err(crate::AdmissionError::Device(error.clone())))
                .collect(),
        };
        let verdicts: Vec<_> = batch
            .iter()
            .map(|(_, message)| message)
            .zip(&reports)
            .filter(|(_, report)| !report.as_ref().is_err_and(arrived_late))
            .collect();
        if !verdicts.is_empty()
            && let Err(error) = admission
                .owner
                .keep_verdicts(admission.workspace, &verdicts, &*self.0.signer, now)
                .await
        {
            tracing::warn!(%error, "a document write's verdict was not kept");
        }
        drop(verdicts);
        let mut lacks_history = false;
        for ((author, message), report) in batch.into_iter().zip(reports) {
            lacks_history |= self.settle(heard_of(author, message, report, true)).0;
        }
        lacks_history
    }

    /// What `heard`, a frame this device's workspace decided, settles: it is
    /// kept at this device's store, carried on to the workspace's other
    /// devices where this one is their host, and handed to its engine.
    /// Whether it waits for history, and whether the workspace keeps it.
    fn settle(&self, mut heard: Heard) -> (bool, Option<Kept>) {
        let retained = heard.retained;
        let lacks_history = heard
            .report
            .as_ref()
            .is_ok_and(|report| report.lacks_history);
        #[cfg(not(target_arch = "wasm32"))]
        {
            heard.fresh = match self.keep(&heard.message, true) {
                Ok(fresh) => fresh,
                Err(error) => {
                    tracing::warn!(%error, "a link's frame was not kept at the store");
                    return (true, None);
                }
            };
        }
        #[cfg(target_arch = "wasm32")]
        {
            heard.fresh = true;
        }
        let kept = match &heard.report {
            Ok(report) if !report.lacks_history => Some(Kept::Durable),
            Ok(_) => None,
            // A frame this device could not decide is no verdict on it: it is
            // not kept as refused, and the writer's next frame proceeds.
            Err(error) if error.is_device_failure() || arrived_late(error) => None,
            Err(error) => error.reason().permanent().then(|| Kept::Refused {
                reason: error.to_string(),
            }),
        };
        if let Err(error) = &heard.report
            && error.is_device_failure()
        {
            tracing::warn!(
                workspace = ?self.workspace().ok(),
                author = %short(&heard.author),
                %error,
                "this device could not decide a link's frame; nothing was refused"
            );
        }
        tracing::debug!(
            workspace = ?self.workspace().ok(),
            author = %short(&heard.author),
            retained,
            landed = heard.landed,
            fresh = heard.fresh,
            lacks_history,
            refused = ?heard.report.as_ref().err(),
            "a link's frame is judged"
        );
        if heard.landed {
            self.0.changed.send_modify(bump);
        }
        // What a write let go here waited for came with this frame: the
        // write is still in the other device's history, read again for it.
        if heard
            .report
            .as_ref()
            .is_ok_and(|report| report.replay_wanted)
        {
            self.ask_replay();
        }
        // The workspace's host is its members' one path: what one member's
        // link brings reaches every other, under the member's own name, once:
        // a frame its store already held went out when it first arrived.
        if self.fans_out() && heard.report.is_ok() && heard.fresh {
            self.outgoing(Authored {
                from: heard.author,
                message: heard.message.clone(),
                published: false,
            });
        }
        self.tell(heard);
        (lacks_history, kept)
    }

    /// What this device sends the other end of a link to ask for the state
    /// it holds: a frame here waits for history this device lacks.
    fn state_request(&self) -> Authored {
        Authored {
            from: self.device(),
            message: NeighbourMsg::SyncReq {
                since: Vec::new(),
                workspace: self.0.replica.workspace(),
                invite_id: None,
                nonce: now_ns(),
            },
            published: true,
        }
    }

    /// Answer the other end of a link's ask for the state this device holds:
    /// what the link started with, again ([`Self::catch_up`]).
    async fn answer_state_request(&self) -> Vec<Authored> {
        self.catch_up().await.unwrap_or_else(|error| {
            tracing::warn!(%error, "a device's ask for this device's state went unanswered");
            Vec::new()
        })
    }

    /// Admit `message`, `author`'s frame, into this device's workspace: what
    /// the workspace decided of it, as its engine hears it. A retained frame, or
    /// one that is `history` ([`Self::member_history`]), is decided as history: as of
    /// when its author signed it, never refused for how long it took to come.
    async fn judge(
        &self,
        author: [u8; 32],
        message: NeighbourMsg,
        retained: bool,
        history: bool,
    ) -> Heard {
        let origin = if retained || history {
            TransportOrigin::RetainedServerDelta(author)
        } else {
            TransportOrigin::ServerDelta(author)
        };
        match &self.0.admission {
            Some(admission) => {
                self.admit_owned(admission, (author, origin), message, retained)
                    .await
            }
            None => self.admit_local((author, origin), message, retained).await,
        }
    }

    /// Hand `heard` to this device's engine.
    fn tell(&self, heard: Heard) {
        let _ = self.0.heard.send(heard);
    }

    /// Carry `message`, `author`'s channel governance, without admitting it:
    /// a channel's group is settled between its members, never in the
    /// workspace's state, so no admission decides it. The workspace's host
    /// sends it on to its other links, as it does every member's frame, and
    /// this device's own channel service takes it from [`Self::heard`]. A host
    /// that dropped it left each member founding the channel's group alone,
    /// sealing what it wrote under keys no other member held.
    fn carry_governance(&self, author: [u8; 32], message: NeighbourMsg, retained: bool) {
        if self.fans_out() {
            self.outgoing(Authored {
                from: author,
                message: message.clone(),
                published: false,
            });
        }
        self.tell(Heard {
            author,
            message,
            landed: false,
            retained,
            report: Err(crate::AdmissionError::malformed(NOT_WORKSPACE_STATE)),
            fresh: true,
        });
    }

    /// Take `message` into the workspace's own live replica, which the
    /// device's engine decides on: durable as it lands.
    async fn admit_owned(
        &self,
        admission: &Admission,
        (author, origin): ([u8; 32], TransportOrigin),
        message: NeighbourMsg,
        retained: bool,
    ) -> Heard {
        let now = tilo_clock::wall();
        let report = admission
            .owner
            .receive(admission.workspace, origin, message.clone(), now);
        // A refused write reaches its author: this device keeps its verdict on it in its own
        // slice of the workspace, which the next sync carries back. A write that only came late
        // is no verdict on the write ([`arrived_late`]).
        let late = report.as_ref().is_err_and(arrived_late);
        if !late
            && let Err(error) = admission
                .owner
                .keep_verdicts(
                    admission.workspace,
                    &[(&message, &report)],
                    &*self.0.signer,
                    now,
                )
                .await
        {
            tracing::warn!(%error, "a document write's verdict was not kept");
        }
        heard_of(author, message, report, retained)
    }

    /// Take `message` into this device's own replica: whether it is the
    /// workspace's governor decides there too, and what it refuses is filed
    /// where the write's author reads why.
    async fn admit_local(
        &self,
        (author, origin): ([u8; 32], TransportOrigin),
        message: NeighbourMsg,
        retained: bool,
    ) -> Heard {
        // The certificate the verdicts below are filed under is this device's
        // own, read before the replica is locked: a replica locked already is
        // not to be read again. A device not in the roster files none.
        let own = matches!(message, NeighbourMsg::AppDocument { .. })
            .then(|| self.certificate().ok())
            .flatten();
        let received =
            self.0
                .replica
                .lock()
                .receive_from_report(origin, message.clone(), tilo_clock::wall());
        let mut landed = received
            .as_ref()
            .is_ok_and(|report| report.changed || report.metadata_changed);
        if received.is_ok() && matches!(&message, NeighbourMsg::SyncResp { .. }) {
            // The workspace's state came with this frame: its governance, members and grants moved
            // with it, and everything this device decides for others reads them. A device holding
            // the workspace as a member copy has no publication of its own to wake them, as the
            // device keeping it has, so they are woken here.
            crate::owner::policy_changed();
        }
        // The verdict is this device's own, filed under its own certificate:
        // a document write the workspace refuses is filed where its author
        // reads why it is not here, and a landed one clears the verdicts it
        // lands on. The carrier's certificate says who sent the frame, not
        // whose verdicts these are. Any other frame is the link's alone to
        // drop.
        // A frame this device could not decide is no refusal to file: the
        // writer's write is not marked as missing, and its next one proceeds.
        let refused = received
            .as_ref()
            .err()
            .filter(|error| !error.is_device_failure() && !arrived_late(error));
        if let Some(own) = own.filter(|_| received.is_ok() || refused.is_some()) {
            let filed = self
                .signed_change(|peer, signer, now| match (&received, refused) {
                    (Ok(_), _) => peer
                        .workspace_mut()
                        .clear_landed_write(&own, &message, signer, now),
                    (Err(_), Some(reason)) => peer
                        .workspace_mut()
                        .refuse_write(&own, &message, reason, signer, now),
                    (Err(_), None) => Ok(false),
                })
                .await;
            match filed {
                Ok(filed) => landed |= filed,
                Err(error) => tracing::warn!(%error, "a write's verdict was not kept"),
            }
        }
        Heard {
            author,
            message,
            landed,
            retained,
            report: received,
            fresh: true,
        }
    }

    /// Keep `message` at this device's store when it is a sealed document
    /// frame, the only history a store holds, as one a link brought when
    /// `from_link`. Returns `Ok(false)` for a frame the store already held.
    #[cfg(not(target_arch = "wasm32"))]
    fn keep(&self, message: &NeighbourMsg, from_link: bool) -> Result<bool, StoreError> {
        let Some(store) = &self.0.store else {
            return Ok(true);
        };
        let Ok(workspace) = self.0.replica.id() else {
            return Ok(true);
        };
        // A sealed compaction's writes go once every device with its record's keys checked it:
        // the checks reach this device in the workspace's own document, any message later.
        let unchecked = store.unchecked(workspace);
        if !unchecked.is_empty() {
            let checked: Vec<_> = {
                let replica = self.0.replica.lock();
                unchecked
                    .into_iter()
                    .filter(|checkpoint| {
                        replica.publication().checkpoint_reached(
                            (checkpoint.app.as_str(), checkpoint.record.as_str()),
                            (checkpoint.author, checkpoint.seq),
                        )
                    })
                    .collect()
            };
            for checkpoint in &checked {
                store.retire_checked(workspace, checkpoint);
            }
        }
        let Ok(frame) = Frame::of(message) else {
            return Ok(true);
        };
        let retained = if from_link {
            store.retain_from_link(workspace, &frame)
        } else {
            store.retain(workspace, &frame)
        };
        retained.map(|retained| {
            matches!(
                retained,
                crate::store::Retained::New | crate::store::Retained::Beside
            )
        })
    }

    fn workspace(&self) -> Result<WorkspaceId, String> {
        self.0.replica.id()
    }

    /// This device's certificate in the replica's roster: the one its key signs
    /// for.
    fn certificate(&self) -> Result<tilo_user_identity::DeviceCert, String> {
        let device = self.device();
        self.0
            .replica
            .lock()
            .certificates()
            .into_iter()
            .find(|certificate| certificate.device_public_key == device)
            .ok_or_else(|| "this device is not in the workspace roster".into())
    }
}

/// Where this device's answers to the other device's writes go on one link: through `sender`,
/// where this device answers them.
#[derive(Clone, Copy)]
struct Answering<'a> {
    sender: Option<&'a tokio::sync::mpsc::UnboundedSender<Answered>>,
}

/// The control stream of one link: what a device asks the other for, and what
/// it answers with.
struct Control {
    send: StreamSend,
    recv: StreamRecv,
}

#[cfg(not(target_arch = "wasm32"))]
/// What a dialling device named first on the link it opened: the workspace,
/// whether it takes the workspace's records only as it opens them, whether it
/// only takes this device's store ([`LinkFrame::Take`])
/// ([`DirectSync::take_store`]), and the control stream it asked on.
struct DialedWorkspace {
    workspace: WorkspaceId,
    takes: bool,
    opens: bool,
    control: Control,
}

#[cfg(not(target_arch = "wasm32"))]
/// Read the workspace a dialling device names first, off its control stream.
async fn dialed_workspace(transport: &dyn Transport) -> Result<DialedWorkspace, String> {
    let (send, mut recv) = transport.accept_bi().await?;
    let (workspace, takes, opens) = match read_link(&mut recv, MAX_FRAME_BYTES).await? {
        Some(LinkFrame::Workspace { id, opens }) => (id, false, opens),
        Some(LinkFrame::Take(id)) => (id, true, false),
        _ => {
            return Err(refuse(
                transport,
                &Refusal::Invalid("the device did not name its workspace".into()),
            ));
        }
    };
    Ok(DialedWorkspace {
        workspace,
        takes,
        opens,
        control: Control { send, recv },
    })
}

/// Close `transport`, refused for `refusal`, which the device on the other
/// end reads typed ([`Transport::close_reason`]); its text, for this side's
/// log.
fn refuse(transport: &dyn Transport, refusal: &Refusal) -> String {
    let said = postcard::to_allocvec(refusal).unwrap_or_default();
    transport.close(REFUSED, &said);
    refusal.to_string()
}

#[cfg(not(target_arch = "wasm32"))]
impl ProtocolHandler for DirectSync {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        // A connection this device accepts is a member's, so this device is
        // the workspace's host on it.
        self.host();
        async {
            self.prove_accepted(&connection).await?;
            self.serve(&connection).await
        }
        .await
        .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}

/// A host's direct links, one per workspace it serves, answering
/// [`DIRECT_ALPN`] on one router.
///
/// The dialling device names its workspace in the link's first frame; a
/// workspace this host holds and does not serve yet is refused, one it does
/// not hold at all is refused the same way, and one its owner deleted is
/// refused as deleted. The device at the other end is admitted for the
/// workspace before the link listens, by the hook a host installs
/// ([`Serving::admitting`]).
#[derive(Clone, Default)]
pub struct Serving {
    links: Arc<RwLock<BTreeMap<WorkspaceId, DirectSync>>>,
    /// The workspaces deleted here, refused as deleted to whoever names one
    /// ([`Self::delete`]).
    deleted: Arc<RwLock<std::collections::BTreeSet<WorkspaceId>>>,
    /// What admits the device at the other end of a link to the workspace it
    /// names, before the link listens ([`Self::admitting`]).
    #[cfg(not(target_arch = "wasm32"))]
    admitting: Option<Arc<Admitting>>,
}

/// What [`Serving::admitting`] installs: the scope a device at `remote`
/// reaches in `link`'s workspace, or why it is refused.
pub type Admitting = dyn Fn([u8; 32], &DirectSync) -> Result<LinkScope, Refusal> + Send + Sync;

impl std::fmt::Debug for Serving {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Serving")
            .field("workspaces", &self.links.read_recover().len())
            .finish_non_exhaustive()
    }
}

impl Serving {
    /// A host serving no workspace yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit the device at the other end of every link by `hook`, before the
    /// link listens: the host's one admission, which answers the scope the
    /// device reaches ([`LinkScope`]) or the refusal the dialling device is
    #[cfg(not(target_arch = "wasm32"))]
    /// told. A host without one admits every certified device of the workspace
    /// to everything it serves.
    #[must_use]
    pub fn admitting(self, hook: Arc<Admitting>) -> Self {
        Self {
            admitting: Some(hook),
            ..self
        }
    }

    /// Serve `link`'s workspace, replacing a link this host served before.
    ///
    /// # Errors
    ///
    /// The link's replica names no workspace id.
    pub fn serve(&self, link: DirectSync) -> Result<(), String> {
        let workspace = link.workspace()?;
        link.host();
        self.links.write_recover().insert(workspace, link);
        Ok(())
    }

    /// Stop serving `workspace`: a workspace this host released.
    pub fn release(&self, workspace: WorkspaceId) {
        self.links.write_recover().remove(&workspace);
    }

    /// `workspace` was deleted here, for good: every link of it ends and a
    /// device that names it from now on is refused, each told
    /// [`Refusal::WorkspaceDeleted`].
    /// A device that knows the workspace's random id learns nothing more than
    /// that it is gone.
    pub fn delete(&self, workspace: WorkspaceId) {
        self.deleted.write_recover().insert(workspace);
        let link = self.links.write_recover().remove(&workspace);
        if let Some(link) = link {
            link.end(Refusal::WorkspaceDeleted);
        }
    }

    /// Whether `workspace` was deleted here ([`Self::delete`]).
    #[must_use]
    pub fn deleted(&self, workspace: WorkspaceId) -> bool {
        self.deleted.read_recover().contains(&workspace)
    }

    /// The link serving `workspace`, when this host serves it.
    #[must_use]
    pub fn link(&self, workspace: WorkspaceId) -> Option<DirectSync> {
        self.links.read_recover().get(&workspace).cloned()
    }

    /// Serve one link a device dialled: the workspace it names, admitted by
    /// [`Self::admitting`] before the link listens.
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn serve_link(&self, transport: &dyn Transport) -> Result<(), String> {
        let dialed = dialed_workspace(transport).await?;
        if self.deleted(dialed.workspace) {
            return Err(refuse(transport, &Refusal::WorkspaceDeleted));
        }
        let link = self.link(dialed.workspace).ok_or_else(|| {
            refuse(
                transport,
                &Refusal::Other("this host holds no such workspace".into()),
            )
        })?;
        let scope = match &self.admitting {
            Some(hook) => hook(transport.remote(), &link).map_err(|refusal| {
                tracing::info!(
                    %refusal,
                    remote = %short(&transport.remote()),
                    "a host refused a link's device before it listened"
                );
                refuse(transport, &refusal)
            })?,
            None => LinkScope::admitting(),
        };
        link.serve_as(transport, dialed, scope).await
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ProtocolHandler for Serving {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        async {
            // Every link this host serves is this device's: any of them proves it, and the dial
            // proves its device before it names the workspace it wants.
            let any = self.links.read_recover().values().next().cloned();
            let Some(link) = any else {
                return Err(refuse(
                    &connection,
                    &Refusal::Other("this host holds no such workspace".into()),
                ));
            };
            link.prove_accepted(&connection).await?;
            self.serve_link(&connection).await
        }
        .await
        .map_err(|error| AcceptError::from_err(std::io::Error::other(error)))
    }
}

#[cfg(not(target_arch = "wasm32"))]
/// Take every frame `live`, the links of `sender`, publishes into
/// `device`'s workspace, in its author's name: what `device` wrote itself
/// does not come back to it. A frame that leaves `device` waiting for
/// history takes in what `sender` holds, as a link asks for it.
async fn forward(
    mut live: broadcast::Receiver<Authored>,
    sender: &DirectSync,
    device: &DirectSync,
    retained: bool,
) -> Result<(), String> {
    loop {
        match live.recv().await {
            Ok(authored) if already_theirs(&authored, device.device()) => {}
            Ok(authored) => {
                if device
                    .admit(authored.from, authored.message, retained)
                    .await
                {
                    for state in sender.answer_state_request().await {
                        // What still waits after the answer waits for history
                        // the sender lacks too: asking again brings nothing.
                        device.admit(state.from, state.message, true).await;
                    }
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return Err("the link fell behind the device's changes".into());
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

/// Whether `error` refused a frame only for arriving after the live window: a write judged live
/// (one not its author's own on a link to its host, [`DirectSync::member_history`]) that waited
/// somewhere on its way. That says nothing of the write, so it is
/// neither answered as refused nor filed as a verdict: the writer keeps it, in its replica and
/// in its ownership, rather than losing an edit the workspace never judged.
fn arrived_late(error: &crate::AdmissionError) -> bool {
    error
        .to_string()
        .contains(&collab_types::validator::ValidationError::OutsideValidityWindow.to_string())
}

/// Why a link to a device ends, or is refused: the workspace removed its
/// person.
const REMOVED: &str = "the workspace removed the device's person";

/// Why a channel's governance is never admitted into the workspace: what a
/// [`Heard`] carrying it reports.
const NOT_WORKSPACE_STATE: &str =
    "channel governance is settled by the channel's members, not admitted into the workspace";

/// Whether `message` is a channel's governance: a frame of its group, or a
/// device asking to join it.
fn carried_governance(message: &NeighbourMsg) -> bool {
    message.governance_workspace().is_some()
}

/// Whether `authored` is the other end's already: the device `remote` sent it
/// itself.
fn already_theirs(authored: &Authored, remote: [u8; 32]) -> bool {
    !authored.published && from_device(&authored.message, remote)
}

/// What one link's live stream sends besides the frames published live.
struct Sending {
    /// The device on the other end.
    remote: [u8; 32],
    /// Whether this device dialled: the device that did asks the other one
    /// for its state over the link's control stream, with its history.
    dialing: bool,
    /// Whether the device on the other end takes the workspace's records only
    /// as it opens them: it is sent a write of a record it did not open only
    /// when the record is one every device is sent.
    opens: bool,
    /// What this stream opens with.
    catch_up: Vec<Authored>,
    /// Set once the first history this device answers is written: its live
    /// frames follow it.
    first_answer: watch::Receiver<bool>,
    /// The records the device on the other end opened ([`LinkFrame::Open`]):
    /// their writes reach it from then on.
    opened: Arc<RwLock<BTreeSet<[u8; 32]>>>,
}

/// Whether `message`, a write this link would send, is one the device at its
/// other end takes: a record it opened, a chain it follows, or one every
/// device is sent. A link that is sent every record takes this one.
fn opened_admits(message: &NeighbourMsg, opens: bool, opened: &RwLock<BTreeSet<[u8; 32]>>) -> bool {
    if !opens {
        return true;
    }
    let NeighbourMsg::AppDocument {
        app_id,
        document_id,
        ..
    } = message
    else {
        return true;
    };
    match neighbour_wire::document_sync(document_id) {
        neighbour_wire::DocumentSync::Eager => true,
        neighbour_wire::DocumentSync::Part { base, .. } => {
            let opened = opened.read_recover();
            opened.contains(&neighbour_wire::app_document_group(app_id, document_id))
                || opened.contains(&neighbour_wire::app_document_group(
                    app_id,
                    &neighbour_wire::chain_head(base),
                ))
        }
        // No document lives under a chain's head: it is a name a device opens
        // the chain by, never a record with writes of its own.
        neighbour_wire::DocumentSync::ChainHead { .. } => false,
        neighbour_wire::DocumentSync::OnDemand | neighbour_wire::DocumentSync::Chunk { .. } => {
            opened
                .read_recover()
                .contains(&neighbour_wire::app_document_group(app_id, document_id))
        }
    }
}

/// Write `sending`'s catch-up, then every frame published live, to `send`,
/// under the name of the device that authored each, until the workspace
/// removes the device on the other end: the governance that removes it is
/// the last frame it is sent, and the link ends there.
async fn send_authored(
    link: &DirectSync,
    mut send: StreamSend,
    sending: Sending,
    mut live: broadcast::Receiver<Authored>,
    mut only_here: tokio::sync::mpsc::UnboundedReceiver<OnlyHere>,
) -> Result<(), String> {
    let Sending {
        remote,
        dialing,
        opens,
        catch_up,
        mut first_answer,
        opened,
    } = sending;
    let reaches = link.reach_of(remote, LinkScope::default());
    let takes = |message: &NeighbourMsg| reaches(message) && opened_admits(message, opens, &opened);
    for authored in catch_up.iter().filter(|authored| takes(&authored.message)) {
        write_authored(&mut send, authored).await?;
    }
    link.0.catch_up_written.send_replace(true);
    while !*first_answer.borrow_and_update() {
        if first_answer.changed().await.is_err() {
            return Ok(());
        }
    }
    loop {
        let next = tokio::select! {
            Some(first) = only_here.recv() => {
                if link.removed(remote) {
                    return Err(REMOVED.into());
                }
                let mut waiting = vec![first];
                while let Ok(more) = only_here.try_recv() {
                    waiting.push(more);
                }
                // The device that dialled reads the other one's state with its
                // history, judged as history; the one that accepted asks on
                // this stream.
                if waiting.contains(&OnlyHere::Ask) {
                    if dialing {
                        link.ask_replay();
                    } else {
                        write_authored(&mut send, &link.state_request()).await?;
                    }
                }
                if waiting.contains(&OnlyHere::Answer) {
                    for state in link.answer_state_request().await {
                        if takes(&state.message) {
                            write_authored(&mut send, &state).await?;
                        }
                    }
                }
                continue;
            }
            next = live.recv() => next,
        };
        match next {
            Ok(authored) if already_theirs(&authored, remote) || !takes(&authored.message) => {}
            Ok(authored) => {
                // The governance that removes the device is the last frame it
                // is sent, whichever path told this device of the removal.
                let governance = matches!(authored.message, NeighbourMsg::SyncResp { .. });
                if !governance && link.removed(remote) {
                    return Err(REMOVED.into());
                }
                write_authored(&mut send, &authored).await?;
                if governance && link.removed(remote) {
                    return Err(REMOVED.into());
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                return Err("the link fell behind the device's changes".into());
            }
            Err(broadcast::error::RecvError::Closed) => return Ok(()),
        }
    }
}

/// Write `authored` as this link's next record: the device that authored it,
/// then its frame.
async fn write_authored(send: &mut StreamSend, authored: &Authored) -> Result<(), String> {
    let bytes = postcard::to_allocvec(&(authored.from, &authored.message))
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("frame too large".into());
    }
    crate::frames::write_frame(send, &bytes).await
}

/// The next frame this link brought, with its author, or `None` once the
/// other side finished its stream.
async fn read_authored(recv: &mut StreamRecv) -> Result<Option<Authored>, String> {
    match crate::frames::read_frame(recv, MAX_FRAME_BYTES).await? {
        Some(bytes) => decode_authored(&bytes)
            .map(|(from, message)| {
                Some(Authored {
                    from,
                    message,
                    published: false,
                })
            })
            .map_err(|error| error.to_string()),
        None => Ok(None),
    }
}

/// A device key, short, as the logs name it.
fn short(key: &[u8; 32]) -> String {
    hex::encode(&key[..4])
}

#[cfg(test)]
#[path = "direct_history_tests.rs"]
mod history_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, RevocationEntry, RevocationList, RevocationReason,
        sign_device_cert, sign_revocation_entry,
    };

    const WORKSPACE: &str = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";

    /// A member device `n` certified by `root`.
    fn device(root: &SigningKey, n: u8) -> (SigningKey, DeviceCert) {
        let key = SigningKey::from_bytes(&[n; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: format!("device-{n}"),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, root).unwrap();
        (key, cert)
    }

    fn revocation(root: &SigningKey, device: [u8; 32]) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: 1,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, root).unwrap();
        entry
    }

    /// A link over a replica of `roster`'s workspace, its first certificate
    /// the authority's, holding `revocations` and `documents`.
    fn link(
        roster: &[(SigningKey, DeviceCert)],
        revocations: RevocationList,
        documents: Vec<crate::SeedDocument>,
    ) -> DirectSync {
        let (key, authority) = roster.first().unwrap();
        let now = tilo_clock::wall();
        let mut peer = StatePeer::new(
            WORKSPACE,
            authority.clone(),
            roster
                .iter()
                .map(|(_, certificate)| certificate.clone())
                .collect(),
            revocations,
            now,
        )
        .unwrap();
        peer.select_exact_admissions().unwrap();
        let roster = roster
            .iter()
            .map(|(_, certificate)| certificate.clone())
            .collect::<Vec<_>>();
        peer.install_seed(
            crate::SeedState {
                workspace_changes: crate::seed_workspace(WORKSPACE, authority, &roster).unwrap(),
                documents,
            },
            key,
            now,
        )
        .unwrap();
        DirectSync::new(peer, key.clone())
    }

    /// A revocation refuses the device of the root that signed it, and no
    /// other: another root's entry naming the same key leaves the device a
    /// device of the workspace's.
    #[test]
    fn a_revocation_refuses_only_the_device_its_own_root_certified() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let roster = [device(&root, 2), device(&root, 3)];
        let member = roster[1].1.clone();
        let stranger = SigningKey::from_bytes(&[7; 32]);

        let mut theirs = RevocationList::new();
        theirs
            .insert(revocation(&stranger, member.device_public_key))
            .unwrap();
        let sync = link(&roster, theirs, Vec::new());
        assert_eq!(
            sync.admits(member.device_public_key, false),
            Ok(()),
            "a stranger's entry refuses nothing of the member's device"
        );
        assert_eq!(
            sync.admits(
                SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes(),
                false
            ),
            Err("the device is not in the workspace roster".to_owned())
        );

        // The member's own root's entry is what refuses it.
        assert!(
            sync.0
                .replica
                .write(|peer| {
                    peer.governance_mut().publish_governed_revocations(
                        &[revocation(&root, member.device_public_key)],
                        &roster[0].0,
                        tilo_clock::wall(),
                    )
                })
                .unwrap()
        );
        assert_eq!(
            sync.admits(member.device_public_key, false),
            Err("the device is revoked".to_owned())
        );
    }

    /// A device whose key its person's root retired is told so with the entry
    /// the root signed, which it keeps as an announced revocation and so lets a
    /// server that was away retire itself. A key lost, stolen or compromised is
    /// told only that it is not a member: the entry names why, and the reason
    /// is the person's.
    #[test]
    fn a_retired_device_is_told_the_entry_that_retired_it() {
        let root = SigningKey::from_bytes(&[1; 32]);
        let roster = [device(&root, 2), device(&root, 3)];
        let member = roster[1].1.clone();
        for (reason, told) in [
            (RevocationReason::Retired, true),
            (RevocationReason::Lost, false),
            (RevocationReason::Stolen, false),
            (RevocationReason::Compromised, false),
        ] {
            let mut entry = revocation(&root, member.device_public_key);
            entry.reason = reason;
            entry.signature = [0; 64];
            sign_revocation_entry(&mut entry, &root).unwrap();
            // The replica held the device before its root revoked it, and
            // carries the entry its governance was sent: a replica is never
            // made over a roster that holds a revoked device.
            let sync = link(&roster, RevocationList::new(), Vec::new());
            assert!(
                sync.0
                    .replica
                    .lock()
                    .governance_mut()
                    .publish_governed_revocations(
                        std::slice::from_ref(&entry),
                        &roster[0].0,
                        tilo_clock::wall(),
                    )
                    .unwrap()
            );
            let Err(said) = sync.admits(member.device_public_key, false) else {
                panic!("a revoked device is refused");
            };
            assert_eq!(said, "the device is revoked");
            match (told, sync.refusal_to(member.device_public_key, said)) {
                (true, Refusal::Revoked(entry)) => {
                    assert_eq!(entry.reason, RevocationReason::Retired);
                }
                (false, Refusal::NotAMember(said)) => assert_eq!(said, "the device is revoked"),
                (_, other) => panic!("{reason:?} is told {other:?}"),
            }
        }
    }

    /// A write the other device sent live while this one still reads the
    /// history written before it waits for that history: the author's writes
    /// to a record apply in the order the author wrote them, even where the
    /// later one does not build on the earlier.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_live_write_waits_for_the_history_written_before_it() {
        use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable as _};
        use collab_types::CommitNamed as _;
        const APP: &str = "test.app";
        const RECORD: &str = "tab:1";
        let root = SigningKey::from_bytes(&[1; 32]);
        let roster = [device(&root, 2), device(&root, 3)];
        let mut record = AutoCommit::new();
        record.put(ROOT, "start", "here").unwrap();
        record.commit_edit("fixture", "start", &root.verifying_key().to_bytes());
        let host = link(
            &roster,
            RevocationList::new(),
            vec![crate::SeedDocument {
                app_id: APP.into(),
                document_id: RECORD.into(),
                kind: "test".into(),
                bytes: record.save(),
            }],
        );
        let member = DirectSync::new(
            StatePeer::from_join(
                WORKSPACE,
                roster[0].1.clone(),
                roster.iter().map(|(_, cert)| cert.clone()).collect(),
                &[],
                tilo_clock::wall(),
            )
            .unwrap(),
            roster[1].0.clone(),
        );
        let state = host.catch_up().await.unwrap();
        // Two writes on the record as the member's history has it, neither
        // built on the other.
        let mut edit = |field: &str, actor: u8| {
            let mut edit = record
                .fork()
                .with_actor(automerge::ActorId::from(vec![actor; 16]));
            let heads = edit.get_heads();
            edit.put(ROOT, field, "written").unwrap();
            edit.commit_edit("fixture", field, &root.verifying_key().to_bytes());
            edit.save_after(&heads)
        };
        let (first, second) = (edit("first", 4), edit("second", 5));
        let publish = |bytes: Vec<u8>| {
            host.publish(|host| {
                host.publication_mut().publish_incremental(
                    APP,
                    RECORD,
                    bytes,
                    &roster[0].0,
                    tilo_clock::wall(),
                )
            })
            .unwrap()
        };
        let (first, second) = (publish(first), publish(second));

        // The later write comes live before the member has read the history
        // that holds the earlier one.
        let (live, frames) = tokio::sync::mpsc::channel(1);
        live.try_send(Authored {
            from: host.device(),
            message: second,
            published: false,
        })
        .unwrap();
        let (in_hand, history) = watch::channel(false);
        let (asks, _asked) = tokio::sync::mpsc::unbounded_channel();
        let reading_history = async {
            for authored in state {
                member.admit(authored.from, authored.message, true).await;
            }
            member.admit(host.device(), first, true).await;
            in_hand.send_replace(true);
            drop(live);
        };
        tokio::join!(
            member.admit_in_order(
                host.device(),
                &asks,
                None,
                frames,
                history,
                LinkAdmit::dialling(),
            ),
            reading_history
        );

        let held =
            AutoCommit::load(&member.read(|member| member.document(APP, RECORD)).unwrap()).unwrap();
        for field in ["start", "first", "second"] {
            assert!(
                held.get(ROOT, field).unwrap().is_some(),
                "the member holds the {field} write"
            );
        }
    }
}
