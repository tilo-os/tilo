//! Handoff: what the shell and one application instance say to each other
//! while that instance's window moves to another workspace.
//!
//! The shell addresses one instance by its primary route and asks it for
//! one step at a time; the instance answers on the same route. Both ride
//! session-local control frames that are never relayed to peers
//! (the toolkit's `CollabEvent::Handoff` and `CollabRequest::ReportHandoff`).
//!
//! - [`HandoffRequest::Checkpoint`]: make everything the window shows
//!   durable now, so the shell reads the document as the person left it
//!   rather than as its writer last got round to saving it.
//! - [`HandoffRequest::Retire`]: the window has reopened in its new
//!   workspace. Stamp the document it showed retired, as one change of its
//!   own that travels to everyone holding it over its normal route, make
//!   that durable, and say which heads it had before the stamp — everything
//!   up to there is what still has to come along.
//!
//! An instance also reports on its own when a document it shows arrives
//! retired from somewhere else ([`HandoffReport::Retired`]), so its desktop
//! can tell the person who moved it where.
//!
//! A native owner view ([`HandoffRequest::OwnerView`]) is session-local
//! owner-view control. Documents retain their normal CRDT routes; these
//! messages carry presentation only and grant no focus or input.

use automerge::ChangeHash;

/// Upper bound on one encoded message; anything larger is not a handoff.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// Where a moved record went and who moved it, as its retirement records.
/// Anyone who can read the old record learns only these ids; whether they
/// can see the destination is for their own desktop to say.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Successor {
    /// The room the record now lives in.
    pub room: String,
    /// Its id there.
    pub record: String,
    /// The person who moved it.
    pub by: String,
}

/// What a native owner shows of one instance, for the same person's other
/// window to present read-only.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerView {
    pub room: String,
    pub app: String,
    pub instance: u64,
    pub generation: u64,
    pub counter: u64,
    pub source: String,
    pub projection: Option<OwnerProjection>,
    /// A local takeover is acknowledged only after the host records its
    /// newer Lamport revision. Older queued remote views then remain stale.
    pub local_claim: bool,
}

/// The owner's presentation state, opaque here, under its schema version.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerProjection {
    pub schema_version: u16,
    pub bytes: Vec<u8>,
}

impl OwnerView {
    pub fn valid(&self) -> bool {
        self.generation > 0
            && self
                .room
                .parse::<uuid::Uuid>()
                .is_ok_and(|room| !room.is_nil())
            && !self.app.is_empty()
            && self.app.len() <= 256
            && self.source.len() <= 256
            && !self.source.is_empty()
            && self
                .source
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            && self.projection.as_ref().is_none_or(|metadata| {
                metadata.schema_version > 0
                    && !metadata.bytes.is_empty()
                    && metadata.bytes.len() <= 32 * 1024
            })
    }
}

/// The shell asking one instance for the next step of a handoff.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandoffRequest {
    /// Make everything this window shows durable, then answer.
    Checkpoint { token: u64 },
    /// Read-only presentation on this exact authenticated native route.
    OwnerView { view: OwnerView },
    /// The window moved to `successor`: stamp what it showed retired, make
    /// that durable, then answer with the heads from before the stamp.
    Retire { token: u64, successor: Successor },
}

/// One instance answering a handoff request, or reporting a retirement.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "report", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandoffReport {
    /// The step is done. `heads` are the shown document's heads before the
    /// step changed it, hex-encoded; empty when it changed nothing.
    Done { token: u64, heads: Vec<String> },
    /// Replay the latest view after this native route is acknowledged.
    OwnerSubscribe { version: u16 },
    /// Explicit local interaction takes over this instance's owner view.
    OwnerTakeover {
        token: u64,
        schema: u16,
        metadata: Vec<u8>,
    },
    /// The step could not be done; nothing was retired.
    Failed { token: u64, reason: String },
    /// A document this instance shows arrived retired: moved elsewhere.
    Retired { successor: Successor },
}

impl HandoffRequest {
    pub fn token(&self) -> u64 {
        match self {
            Self::Checkpoint { token } | Self::Retire { token, .. } => *token,
            Self::OwnerView { view } => view.counter,
        }
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// `None` for anything that is not a bounded, well-formed request.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 192 * 1024 {
            return None;
        }
        let request: Self = serde_json::from_slice(bytes).ok()?;
        match &request {
            Self::OwnerView { view } if view.valid() => Some(request),
            Self::OwnerView { .. } => None,
            _ if bytes.len() <= MAX_MESSAGE_BYTES => Some(request),
            _ => None,
        }
    }
}

impl HandoffReport {
    /// Done, with `heads` as they were before the step.
    pub fn done(token: u64, heads: &[ChangeHash]) -> Self {
        Self::Done {
            token,
            heads: heads.iter().map(ToString::to_string).collect(),
        }
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// `None` for anything that is not a bounded, well-formed report.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 192 * 1024 {
            return None;
        }
        let report: Self = serde_json::from_slice(bytes).ok()?;
        match &report {
            Self::OwnerTakeover {
                schema, metadata, ..
            } if *schema > 0 && !metadata.is_empty() && metadata.len() <= 32 * 1024 => Some(report),
            Self::OwnerTakeover { .. } => None,
            Self::OwnerSubscribe { version } if *version != 1 => None,
            _ if bytes.len() <= MAX_MESSAGE_BYTES => Some(report),
            _ => None,
        }
    }

    /// The heads a [`Self::Done`] carries; `None` when any is malformed.
    pub fn heads(&self) -> Option<Vec<ChangeHash>> {
        match self {
            Self::Done { heads, .. } => heads.iter().map(|head| head.parse().ok()).collect(),
            Self::Failed { .. }
            | Self::Retired { .. }
            | Self::OwnerTakeover { .. }
            | Self::OwnerSubscribe { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_messages_round_trip_and_refuse_anything_else() {
        let successor = Successor {
            room: "3f2b1c00-0000-4000-8000-0000000000e1".into(),
            record: "uuid:5b0e".into(),
            by: "person:ada".into(),
        };
        for request in [
            HandoffRequest::Checkpoint { token: 7 },
            HandoffRequest::Retire {
                token: 8,
                successor: successor.clone(),
            },
        ] {
            assert_eq!(HandoffRequest::decode(&request.encode()), Some(request));
        }
        let mut doc = automerge::AutoCommit::new();
        let _ = automerge::transaction::Transactable::put(&mut doc, automerge::ROOT, "a", 1);
        doc.commit();
        let heads = doc.get_heads();
        let done = HandoffReport::done(8, &heads);
        let decoded = HandoffReport::decode(&done.encode()).unwrap();
        assert_eq!(decoded.heads(), Some(heads));
        let retired = HandoffReport::Retired { successor };
        assert_eq!(HandoffReport::decode(&retired.encode()), Some(retired));
        assert_eq!(HandoffRequest::decode(b"{\"step\":\"format_disk\"}"), None);
        assert_eq!(
            HandoffRequest::decode(&vec![b' '; MAX_MESSAGE_BYTES + 1]),
            None
        );
    }
    #[test]
    fn owner_messages_are_bounded_and_do_not_expand_legacy_handoffs() {
        let mut view = OwnerView {
            room: "3f2b1c00-0000-4000-8000-0000000000e1".into(),
            app: "so.tilo.code-editor".into(),
            instance: 7,
            generation: 1,
            counter: 8,
            source: "certified-device".into(),
            local_claim: false,
            projection: Some(OwnerProjection {
                schema_version: 1,
                bytes: vec![255; 32 * 1024],
            }),
        };
        let request = HandoffRequest::OwnerView { view: view.clone() };
        assert!(request.encode().len() > MAX_MESSAGE_BYTES);
        assert_eq!(HandoffRequest::decode(&request.encode()), Some(request));
        let report = HandoffReport::OwnerTakeover {
            token: 8,
            schema: 1,
            metadata: vec![255; 32 * 1024],
        };
        assert_eq!(HandoffReport::decode(&report.encode()), Some(report));
        view.projection.as_mut().unwrap().bytes.push(0);
        assert!(
            HandoffRequest::decode(&HandoffRequest::OwnerView { view: view.clone() }.encode())
                .is_none()
        );
        view.projection = None;
        view.source = "file:/secret".into();
        assert!(HandoffRequest::decode(&HandoffRequest::OwnerView { view }.encode()).is_none());
        let oversized = HandoffReport::Failed {
            token: 1,
            reason: "x".repeat(MAX_MESSAGE_BYTES),
        };
        assert!(HandoffReport::decode(&oversized.encode()).is_none());
        assert!(
            HandoffReport::decode(
                &HandoffReport::OwnerTakeover {
                    token: 1,
                    schema: 1,
                    metadata: vec![0; 32 * 1024 + 1]
                }
                .encode()
            )
            .is_none()
        );
        assert!(
            HandoffReport::decode(br#"{"report":"owner_subscribe","version":1,"grant":true}"#)
                .is_none()
        );
    }
}
