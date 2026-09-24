//! LIVE_V1 browser custody: live grant requests and verification, and inert
//! JSON projections of the frozen live wire values for the JavaScript client.
//!
//! Live frames never carry authority. The grant is host-signed and verified
//! here against the pinned host key before its bytes are handed to the relay
//! connection as the MoQ Authorization Token. Forwarded signed changes are
//! admitted by the Peer's checked replay path (see `peer_live/wasm.rs`), never
//! from these projections, and stay speculative (in memory, outside durable
//! cuts and the persisted journal) until durable state covers them
//! (`peer_live/speculation.rs`).
// Native builds compile this module for its unit tests only.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use tilo_relay::delivery_v3::live_wire::{
    self, DragPhase, LiveAnchor, LiveDirections, LiveGrantRequest, LiveObject, LiveRecord,
    OpsFrame, PresenceFrame, PreviewOp, SignedLiveGrant,
};

/// Tolerated browser clock lead/lag against the host's issue time.
const CLOCK_SKEW_NS: u64 = 5_000_000_000;
/// Renewal starts no later than this before expiry.
const RENEW_MARGIN_NS: u64 = 10_000_000_000;
/// Outstanding grant requests per Peer.
const MAX_PENDING: usize = 2;
/// Recently admitted forwarded-change digests remembered for de-duplication.
const ADMITTED_WINDOW: usize = 1024;

fn fail(message: impl ToString) -> String {
    message.to_string()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(text: &str, max: usize) -> Result<Vec<u8>, String> {
    if text.is_empty() || text.len() % 2 != 0 || text.len() / 2 > max {
        return Err("invalid live hex bytes".into());
    }
    (0..text.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| "invalid live hex bytes".into())
        })
        .collect()
}

/// Exact expected grant binding captured when the request was prepared.
#[derive(Clone, Debug)]
pub(crate) struct GrantBinding {
    /// Delivery origin (`https://host:port`) of the verified descriptor.
    pub origin: String,
    /// Participant root key (the enrolled device certificate's user key).
    pub root: [u8; 32],
    /// Pinned host signer device key.
    pub host: [u8; 32],
    /// Exact request sent.
    pub request: LiveGrantRequest,
}

/// A verified grant and its operational projection.
#[derive(Clone, Debug)]
pub(crate) struct VerifiedGrant {
    pub bytes: Vec<u8>,
    pub grant: SignedLiveGrant,
    pub endpoint: String,
    pub expires_in_ns: u64,
    pub renew_in_ns: u64,
}

/// Structurally valid request for the session device and room.
pub(crate) fn grant_request(
    device: [u8; 32],
    room: &str,
    participation_id: [u8; 32],
    directions: LiveDirections,
) -> Result<LiveGrantRequest, String> {
    let request = LiveGrantRequest {
        version: 1,
        device,
        room: room.into(),
        participation_id,
        directions,
    };
    request.validate().map_err(fail)?;
    Ok(request)
}

/// Decode, bind and verify one host response. Nothing in the response can
/// widen the request: directions may only narrow, and device, room,
/// participation, root, host and audience must match exactly.
pub(crate) fn verify_grant(
    binding: &GrantBinding,
    bytes: &[u8],
    now_ns: u64,
) -> Result<VerifiedGrant, String> {
    let grant = SignedLiveGrant::decode(bytes).map_err(fail)?;
    let m = &grant.metadata;
    let asked = &binding.request;
    let endpoint = format!("{}{}", binding.origin, live_wire::LIVE_WEBTRANSPORT_PATH);
    if m.request.device != asked.device
        || m.request.room != asked.room
        || m.request.participation_id != asked.participation_id
        || (m.request.directions.publish && !asked.directions.publish)
        || (m.request.directions.subscribe && !asked.directions.subscribe)
        || m.root != binding.root
        || m.host != binding.host
        || m.audience != endpoint
    {
        return Err("live grant binding mismatch".into());
    }
    if m.expires_at_ns <= now_ns || m.issued_at_ns > now_ns.saturating_add(CLOCK_SKEW_NS) {
        return Err("live grant expired or not yet valid".into());
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&binding.host).map_err(fail)?;
    // `verify` checks the signature and that the instant lies in the lifetime;
    // a browser clock slightly behind the host is evaluated at issue time.
    grant
        .verify(&key, now_ns.max(m.issued_at_ns))
        .map_err(fail)?;
    let remaining = m.expires_at_ns - now_ns;
    let margin = (remaining / 3).max(RENEW_MARGIN_NS.min(remaining));
    Ok(VerifiedGrant {
        bytes: bytes.to_vec(),
        endpoint,
        expires_in_ns: remaining,
        renew_in_ns: remaining.saturating_sub(margin),
        grant,
    })
}

/// Bounded digest window for forwarded signed changes.
#[derive(Default)]
pub(crate) struct DigestWindow {
    order: VecDeque<[u8; 32]>,
    seen: BTreeSet<[u8; 32]>,
}
impl DigestWindow {
    pub fn contains(&self, digest: &[u8; 32]) -> bool {
        self.seen.contains(digest)
    }
    /// Returns false if the digest was already present.
    pub fn insert(&mut self, digest: [u8; 32]) -> bool {
        if !self.seen.insert(digest) {
            return false;
        }
        self.order.push_back(digest);
        while self.order.len() > ADMITTED_WINDOW {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }
}

/// Per-Peer live grant custody. A connection holds one participation id;
/// renewals reuse it so the relay session continues without reconnecting.
#[derive(Default)]
pub(crate) struct LiveCustody {
    participation: Option<[u8; 32]>,
    pending: BTreeMap<u32, GrantBinding>,
    next: u32,
    current: Option<SignedLiveGrant>,
    pub admitted: DigestWindow,
    /// Forwarded changes shown but not yet confirmed by durable state.
    pub speculation: speculation::Speculation,
}
impl LiveCustody {
    /// Reserve a request. `fresh` starts a new relay participation.
    pub fn begin(
        &mut self,
        binding: impl FnOnce([u8; 32]) -> Result<GrantBinding, String>,
        fresh: bool,
        nonce: impl FnOnce() -> Result<[u8; 32], String>,
    ) -> Result<(u32, GrantBinding), String> {
        if !fresh && self.pending.len() >= MAX_PENDING {
            return Err("live grant capacity".into());
        }
        let participation = match (fresh, self.participation) {
            (false, Some(current)) => current,
            (false, None) => return Err("live renewal without participation".into()),
            (true, _) => nonce()?,
        };
        let binding = binding(participation)?;
        let id = self
            .next
            .checked_add(1)
            .ok_or("live request counter exhausted")?;
        self.next = id;
        if fresh {
            // A new participation supersedes any renewal of the previous one.
            self.pending.clear();
            self.current = None;
            self.participation = Some(participation);
        }
        self.pending.insert(id, binding.clone());
        Ok((id, binding))
    }
    pub fn complete(
        &mut self,
        id: u32,
        bytes: &[u8],
        now_ns: u64,
    ) -> Result<VerifiedGrant, String> {
        let binding = self
            .pending
            .remove(&id)
            .ok_or("unknown or consumed live request")?;
        if self.participation != Some(binding.request.participation_id) {
            return Err("live participation superseded".into());
        }
        let verified = verify_grant(&binding, bytes, now_ns)?;
        if let Some(current) = &self.current {
            // A new host process (boot) no longer knows this relay session:
            // the client must reconnect with a fresh participation.
            if verified.grant.metadata.host_boot != current.metadata.host_boot {
                self.revoke();
                return Err("live host restarted".into());
            }
            // A renewal may not regress or change the namespace.
            if verified.grant.metadata.namespace != current.metadata.namespace
                || verified.grant.metadata.expires_at_ns < current.metadata.expires_at_ns
            {
                return Err("live grant renewal mismatch".into());
            }
        }
        self.current = Some(verified.grant.clone());
        Ok(verified)
    }
    pub fn forget(&mut self, id: u32) {
        self.pending.remove(&id);
    }
    pub fn current(&self, now_ns: u64) -> Option<&SignedLiveGrant> {
        self.current
            .as_ref()
            .filter(|g| g.metadata.expires_at_ns > now_ns)
    }
    /// Drop live authority (revocation, expiry, session change).
    pub fn revoke(&mut self) {
        self.pending.clear();
        self.current = None;
        self.participation = None;
    }
}

// ---- JSON projections for JavaScript -------------------------------------

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordJson {
    pub app: String,
    pub record: String,
}
impl RecordJson {
    fn wire(self) -> LiveRecord {
        LiveRecord {
            app: self.app,
            record: self.record,
        }
    }
    fn of(record: &LiveRecord) -> Self {
        Self {
            app: record.app.clone(),
            record: record.record.clone(),
        }
    }
}
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct SelectionJson {
    anchor: String,
    head: String,
}
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct ObjectJson {
    record: Option<RecordJson>,
    id: String,
}
#[derive(Serialize, Deserialize, Debug, PartialEq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum PhaseJson {
    Start,
    Move,
    End,
    Cancel,
}
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
struct DragJson {
    object: ObjectJson,
    phase: PhaseJson,
    position: [f32; 2],
    size: Option<[f32; 2]>,
}
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PresenceJson {
    seq: u64,
    sent_at_ms: u64,
    #[serde(default)]
    focus: Option<RecordJson>,
    #[serde(default)]
    pointer: Option<[f32; 2]>,
    #[serde(default)]
    selection: Option<SelectionJson>,
    #[serde(default)]
    drag: Option<DragJson>,
}
fn anchor(text: &str) -> Result<LiveAnchor, String> {
    Ok(LiveAnchor {
        cursor: unhex(text, live_wire::MAX_OBJECT_ID_BYTES)?,
    })
}

/// JSON presence (`{seq, sentAtMs, focus, pointer, selection, drag}`) to canonical bytes.
pub(crate) fn encode_presence(json: &str) -> Result<Vec<u8>, String> {
    if json.len() > 8192 {
        return Err("live presence JSON exceeds bounds".into());
    }
    let value: PresenceJson = serde_json::from_str(json).map_err(fail)?;
    let frame = PresenceFrame {
        seq: value.seq,
        sent_at_ms: value.sent_at_ms,
        focus: value.focus.map(RecordJson::wire),
        pointer: value.pointer,
        selection: value
            .selection
            .map(|s| Ok::<_, String>((anchor(&s.anchor)?, anchor(&s.head)?)))
            .transpose()?,
        drag: value
            .drag
            .map(|d| {
                Ok::<_, String>((
                    LiveObject {
                        record: d.object.record.map(RecordJson::wire),
                        id: unhex(&d.object.id, live_wire::MAX_OBJECT_ID_BYTES)?,
                    },
                    match d.phase {
                        PhaseJson::Start => DragPhase::Start,
                        PhaseJson::Move => DragPhase::Move,
                        PhaseJson::End => DragPhase::End,
                        PhaseJson::Cancel => DragPhase::Cancel,
                    },
                    d.position,
                    d.size,
                ))
            })
            .transpose()?,
    };
    frame.encode().map_err(fail)
}

/// Canonical presence bytes to the JSON projection.
pub(crate) fn decode_presence(bytes: &[u8]) -> Result<String, String> {
    let frame = PresenceFrame::decode(bytes).map_err(fail)?;
    let value = PresenceJson {
        seq: frame.seq,
        sent_at_ms: frame.sent_at_ms,
        focus: frame.focus.as_ref().map(RecordJson::of),
        pointer: frame.pointer,
        selection: frame.selection.as_ref().map(|(a, h)| SelectionJson {
            anchor: hex(&a.cursor),
            head: hex(&h.cursor),
        }),
        drag: frame
            .drag
            .as_ref()
            .map(|(object, phase, position, size)| DragJson {
                object: ObjectJson {
                    record: object.record.as_ref().map(RecordJson::of),
                    id: hex(&object.id),
                },
                phase: match phase {
                    DragPhase::Start => PhaseJson::Start,
                    DragPhase::Move => PhaseJson::Move,
                    DragPhase::End => PhaseJson::End,
                    DragPhase::Cancel => PhaseJson::Cancel,
                },
                position: *position,
                size: *size,
            }),
    };
    serde_json::to_string(&value).map_err(fail)
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum PreviewOpJson {
    Insert { at: String, text: String },
    Delete { at: String, len: u32 },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewJson {
    seq: u64,
    record: RecordJson,
    ops: Vec<PreviewOpJson>,
}

/// JSON preview (`{seq, record:{app,record}, ops:[{kind:'insert',at,text}|{kind:'delete',at,len}]}`).
pub(crate) fn encode_preview(json: &str) -> Result<Vec<u8>, String> {
    if json.len() > live_wire::MAX_STREAM_FRAME_BYTES {
        return Err("live preview JSON exceeds bounds".into());
    }
    let value: PreviewJson = serde_json::from_str(json).map_err(fail)?;
    if value.ops.len() > live_wire::MAX_PREVIEW_OPS {
        return Err("live preview has too many operations".into());
    }
    let ops = value
        .ops
        .into_iter()
        .map(|op| {
            Ok(match op {
                PreviewOpJson::Insert { at, text } => PreviewOp::Insert {
                    at: anchor(&at)?,
                    text,
                },
                PreviewOpJson::Delete { at, len } => PreviewOp::Delete {
                    at: anchor(&at)?,
                    len,
                },
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    OpsFrame::Preview {
        seq: value.seq,
        record: value.record.wire(),
        ops,
    }
    .encode()
    .map_err(fail)
}

/// Ops frame projection. Signed changes expose only record and digest; the
/// canonical bytes stay in the frame and are admitted from it by Rust.
pub(crate) fn decode_ops(bytes: &[u8]) -> Result<String, String> {
    let value = match OpsFrame::decode(bytes).map_err(fail)? {
        OpsFrame::Preview { seq, record, ops } => serde_json::json!({
            "kind": "preview",
            "seq": seq,
            "record": RecordJson::of(&record),
            "track": live_wire::ops_track(&record),
            "ops": ops.iter().map(|op| match op {
                PreviewOp::Insert { at, text } => PreviewOpJson::Insert { at: hex(&at.cursor), text: text.clone() },
                PreviewOp::Delete { at, len } => PreviewOpJson::Delete { at: hex(&at.cursor), len: *len },
            }).collect::<Vec<_>>(),
        }),
        OpsFrame::SignedChange { record, digest, .. } => serde_json::json!({
            "kind": "signed-change",
            "record": RecordJson::of(&record),
            "track": live_wire::ops_track(&record),
            "digest": hex(&digest),
        }),
    };
    Ok(value.to_string())
}

/// Ops track name (`ops/<hex record digest>`) for an application record.
pub(crate) fn ops_track(app: &str, record: &str) -> Result<String, String> {
    if app.is_empty() || app.len() > 128 || record.is_empty() || record.len() > 128 {
        return Err("invalid live record".into());
    }
    Ok(live_wire::ops_track(&LiveRecord {
        app: app.into(),
        record: record.into(),
    }))
}

/// Live record of a durable `(app_id, record_id)`, exactly as the host maps it
/// (`qos-session-peer` `live_service::live_record`): an empty record id is the
/// package-primary sentinel; a durable id spelling the sentinel, or ids out of
/// bounds, have no live record.
pub(crate) fn durable_live_record(app: &str, record: &str) -> Option<LiveRecord> {
    let record = match record {
        "" => crate::live_view::PACKAGE_PRIMARY_RECORD,
        crate::live_view::PACKAGE_PRIMARY_RECORD => return None,
        other => other,
    };
    (!app.is_empty() && app.len() <= 128 && record.len() <= 128).then(|| LiveRecord {
        app: app.into(),
        record: record.into(),
    })
}

/// Author-side forward of this Peer's own signed `AppDocument` publication:
/// `(digest, record, OpsFrame::SignedChange bytes)` over the exact canonical
/// delivery item bytes the outbox uploads. The author publishes it on its own
/// ops track at once, in order after the previews of the keys it contains, so
/// members see the real change one relay hop after the edit instead of after
/// the upload and host admission. Receivers admit it exactly like a host
/// forward (checked native-cut path, speculative until durable state covers
/// it, withdrawn otherwise) and de-duplicate the host's later forward by
/// digest. Other messages, unmapped records and frames over 64 KiB return
/// `None` (members then receive the change through the host / durable path).
pub(crate) fn author_signed_change(canonical: &[u8]) -> Option<([u8; 32], LiveRecord, Vec<u8>)> {
    if canonical.is_empty() || canonical.len() >= live_wire::MAX_STREAM_FRAME_BYTES {
        return None;
    }
    let qos_peer_state::NeighbourMsg::AppDocument {
        app_id, record_id, ..
    } = postcard::from_bytes(canonical).ok()?
    else {
        return None;
    };
    let record = durable_live_record(&app_id, &record_id)?;
    let digest = tilo_relay::delivery_v3::state::canonical_item_digest(canonical);
    let frame = OpsFrame::SignedChange {
        record: record.clone(),
        digest,
        canonical: canonical.to_vec(),
    }
    .encode()
    .ok()?;
    Some((digest, record, frame))
}

/// Canonical bytes of a forwarded signed change, if the frame is one.
pub(crate) fn signed_change(bytes: &[u8]) -> Result<([u8; 32], LiveRecord, Vec<u8>), String> {
    match OpsFrame::decode(bytes).map_err(fail)? {
        OpsFrame::SignedChange {
            record,
            digest,
            canonical,
        } => Ok((digest, record, canonical)),
        OpsFrame::Preview { .. } => Err("live frame is not a signed change".into()),
    }
}

#[path = "peer_live/speculation.rs"]
pub(crate) mod speculation;

#[cfg(test)]
#[path = "peer_live/tests.rs"]
mod tests;
