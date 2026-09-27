//! The content of a `session` record.
//!
//! A recording is a silent follower (`docs/architecture/feedback.md`): a
//! read-only agent participant that joins a room and keeps what it is
//! delivered. This crate is the shape of what it keeps:
//!
//! - one **starting snapshot** per recorded route, taken from the broker's
//!   late-joiner snapshot ([`RouteSnapshot`]);
//! - the ordered **timeline** of what arrived after it ([`TimedEvent`]):
//!   signed document ops exactly as the broker fanned them out, presence
//!   (View subset, caret and selection, the `Interaction` lifecycle), and
//!   inspect keyframes;
//! - the **environment** it ran in ([`Environment`]), including a marker
//!   for each capture-protected app that was present ([`ProtectedApp`]):
//!   such an app contributes nothing else — no snapshot, op, presence,
//!   keyframe, geometry or frame.
//!
//! Timestamps are monotonic offsets from the start of the recording
//! ([`Offset`]), never wall clock, so reordering by clock skew is
//! impossible. Every collection is bounded ([`Limits`]).
//!
//! A trace is not a side file. It lives inside a record document whose
//! identity (`tilo:record`, kind [`SESSION_RECORD_KIND`], home room) is
//! stamped by the record machinery; [`write`] adds the `session` map beside
//! it and [`read`] / [`read_summary`] read it back. The frame blob (pixels,
//! a separate layer) is a record blob reference with relation
//! [`FRAMES_BLOB_RELATION`], and a redacted duplicate names its origin with
//! a record reference of relation [`REDACTED_FROM_RELATION`].
//!
//! The pixel layer is [`frames`]: masked ([`mask`]) lossless frames of the
//! recorded surfaces, and the [`divergence`] check that compares them with
//! a replay's.
//!
//! [`RecentBuffer`] is the in-memory ring behind "record what just
//! happened": presence and inspect keyframes by value, document ops by
//! reference only, because the ops already live in the record's history.

mod builder;
pub mod divergence;
pub mod frames;
pub mod mask;
mod recent;
mod record;
pub mod redact;
pub mod replay;

pub use builder::{Limits, Pushed, TraceBuilder};
pub use recent::{OpReference, RecentBuffer, RecentEntry, RecentItem};
pub use record::{TraceError, is_session, read, read_details, read_summary, write};

use std::time::Duration;

use collab_types::SignedOp;
use serde::{Deserialize, Serialize};
use tilo_actions::SemanticInteraction;

pub use tilo_reference::{Redactions, SESSION_RECORD_KIND, SessionLayer, SessionSummary};
/// Blob relation naming the compositor frame blob of a session.
pub const FRAMES_BLOB_RELATION: &str = "frames";
/// Record relation from a redacted duplicate to the session it was built from.
pub const REDACTED_FROM_RELATION: &str = "redacted-from";
/// Record relation from a session to a record it recorded.
pub const RECORDED_RELATION: &str = "recorded";
/// Layout version of the `session` map. Readers refuse any other.
pub const FORMAT_VERSION: u64 = 3;

/// Monotonic time since the recording started, in microseconds.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct Offset(pub u64);

impl Offset {
    /// Saturates at `u64::MAX` microseconds (about 584 000 years).
    pub fn from_duration(duration: Duration) -> Self {
        Self(u64::try_from(duration.as_micros()).unwrap_or(u64::MAX))
    }

    pub const fn as_duration(self) -> Duration {
        Duration::from_micros(self.0)
    }

    pub const fn as_millis(self) -> u64 {
        self.0 / 1_000
    }
}

/// The collaboration route an event was delivered on: the broker's
/// `(app, room, record)` key. `record_id` is empty for an app whose state is
/// still one document per `(app, room)`.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Route {
    pub app_id: String,
    pub room: String,
    pub record_id: String,
}

impl Route {
    pub fn new(
        app_id: impl Into<String>,
        room: impl Into<String>,
        record_id: impl Into<String>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            room: room.into(),
            record_id: record_id.into(),
        }
    }
}

/// How the recording came about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// The user started and stopped it.
    Explicit,
    /// Built from the in-memory buffer of what just happened.
    Retroactive,
}

impl Origin {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Retroactive => "retroactive",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "explicit" => Some(Self::Explicit),
            "retroactive" => Some(Self::Retroactive),
            _ => None,
        }
    }
}

/// The image a recording ran on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageIdentity {
    pub name: String,
    pub version: String,
    /// Tilo image generation, matched against "fixed in generation N".
    pub generation: Option<u64>,
    pub digest: ImageDigest,
}

/// Which immutable image the recording ran on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageDigest {
    /// The OCI manifest digest bootc reports as booted.
    Booted(String),
    /// The host is not a bootc system (a development or nested host), so
    /// there is no image to name.
    NotBootc,
    /// A bootc system that could not say; the reason is kept.
    Unavailable(String),
    /// Not asked yet.
    #[default]
    Unknown,
}

/// Size, scale and output of one recorded app surface, in logical pixels.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SurfaceGeometry {
    pub app_id: String,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    /// Name of the output the surface is shown on, empty when unknown.
    pub output: String,
}

/// A capture-protected app that was present in a recorded room. This
/// marker is all a session keeps of it; review and replay show the capture
/// placeholder in its place.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProtectedApp {
    pub app_id: String,
    pub room: String,
}

/// Where a recording ran.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Environment {
    /// Build of the recording shell.
    pub build: String,
    pub image: Option<ImageIdentity>,
    pub surfaces: Vec<SurfaceGeometry>,
    pub theme: String,
    pub locale: String,
    /// Capture-protected apps that were present, sorted.
    pub protected: Vec<ProtectedApp>,
}

/// Which end of the recorded window a snapshot describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotPoint {
    /// Requested when recording started: replay loads it, then the ops.
    Start,
    /// Requested when a retroactive recording was captured. The window's
    /// ops are already in its history; replay forks it before the first
    /// recorded change.
    End,
}

/// The late-joiner snapshot of one route, as the signed frames the broker
/// carried: one frame, or every segment of a segmented transfer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteSnapshot {
    pub route: Route,
    /// Participant that answered the snapshot request.
    pub from: String,
    pub at: Offset,
    pub point: SnapshotPoint,
    pub frames: Vec<SignedOp>,
}

/// One signed document op as the broker fanned it out. The recorder keeps an
/// op of an encrypted channel opened with the channel's keys, so its
/// signature covers the sealed form it arrived in; one it could not open
/// stays ciphertext.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentOp {
    pub route: Route,
    /// Participant the broker attributed the op to.
    pub from: String,
    pub envelope: SignedOp,
}

/// A presence change, in the terms of the SDK's `PresencePayload`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PresenceChange {
    Pointer {
        x: f32,
        y: f32,
    },
    Caret {
        pos: u64,
    },
    Selection {
        start: u64,
        end: u64,
    },
    Typing {
        start: u64,
        end: u64,
    },
    Arrived,
    Left,
    /// The participant's View subset changed; the subset rides in the
    /// update's `scope`, as the SDK's `PresenceKey::View`.
    View,
    Following {
        target: Option<String>,
    },
    /// The semantic action lifecycle, including attempts that failed or
    /// changed nothing.
    Interaction(#[serde(with = "interaction_json")] Box<SemanticInteraction>),
    Status {
        text: String,
        automatic: bool,
    },
}

/// One presence update as delivered to the recorder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresenceUpdate {
    /// Participant id the broker stamped on the frame.
    pub participant: String,
    pub room: String,
    /// The SDK `PresenceKey`, in its postcard wire encoding. A View subset
    /// (and the overlays it carries) rides here as `PresenceKey::View`.
    pub scope: Vec<u8>,
    pub change: PresenceChange,
}

/// The semantic tree of an app at one moment, as the broker's Inspect
/// request answered it: roles, labels, values, sensitivity and surface
/// bounds of each node, by stable id. Kept as served to the local session;
/// the redacted duplicate re-redacts it for leaving the device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InspectKeyframe {
    pub route: Route,
    #[serde(with = "reply_json")]
    pub reply: tilo_reference::inspect::InspectReply,
}

impl InspectKeyframe {
    /// Approximate encoded size of the answer.
    fn answer_weight(&self) -> usize {
        self.reply.nodes.iter().fold(0_usize, |sum, node| {
            sum.saturating_add(128)
                .saturating_add(node.id.len())
                .saturating_add(node.role.len())
                .saturating_add(node.label.as_ref().map_or(0, String::len))
                .saturating_add(node.value.as_ref().map_or(0, String::len))
                .saturating_add(node.children.iter().map(String::len).sum::<usize>())
        })
    }
}

/// What happened at one point of the timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[allow(
    clippy::large_enum_variant,
    reason = "ops dominate a timeline; boxing them would add an allocation per event"
)]
pub enum Event {
    Op(DocumentOp),
    Presence(PresenceUpdate),
    Inspect(InspectKeyframe),
}

impl Event {
    /// Approximate encoded size, used to enforce byte budgets without
    /// encoding every event twice.
    fn weight(&self) -> usize {
        const FIXED: usize = 192;
        let variable = match self {
            Self::Op(op) => op
                .envelope
                .payload
                .len()
                .saturating_add(route_weight(&op.route))
                .saturating_add(op.from.len()),
            Self::Presence(update) => update
                .scope
                .len()
                .saturating_add(update.participant.len())
                .saturating_add(update.room.len())
                .saturating_add(match &update.change {
                    PresenceChange::Interaction(_) => 2_048,
                    PresenceChange::Status { text, .. } => text.len(),
                    PresenceChange::Following { target } => target.as_ref().map_or(0, String::len),
                    _ => 0,
                }),
            Self::Inspect(frame) => frame
                .answer_weight()
                .saturating_add(route_weight(&frame.route)),
        };
        FIXED.saturating_add(variable)
    }
}

fn route_weight(route: &Route) -> usize {
    route
        .app_id
        .len()
        .saturating_add(route.room.len())
        .saturating_add(route.record_id.len())
}

/// One timeline entry. Offsets never decrease along the timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimedEvent {
    pub at: Offset,
    pub event: Event,
}

/// What the card does not carry: the rest of a session's cheap metadata,
/// readable from the record document without decoding the timeline. The
/// card itself is [`SessionSummary`], the one type chat renders.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDetails {
    pub origin: Option<Origin>,
    /// Wall-clock start, milliseconds since the Unix epoch. Display only.
    pub started_unix_ms: u64,
    /// Distinct rooms recorded, sorted.
    pub rooms: Vec<String>,
    /// Of the steps, the ones whose last phase was a failure.
    pub failed_steps: u32,
    pub ops: u32,
    pub tags: Vec<String>,
    /// A bound was hit and later events were not kept.
    pub truncated: bool,
}

/// Most apps a card lists, the card's own bound.
const MAX_SUMMARY_APPS: usize = 16;

/// A complete session trace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionTrace {
    pub origin: Origin,
    pub started_unix_ms: u64,
    pub duration: Offset,
    pub environment: Environment,
    pub snapshots: Vec<RouteSnapshot>,
    pub timeline: Vec<TimedEvent>,
    pub tags: Vec<String>,
    pub layer: SessionLayer,
    pub redactions: Redactions,
    pub truncated: bool,
}

impl SessionTrace {
    /// Timeline in order.
    pub fn events(&self) -> impl Iterator<Item = &TimedEvent> {
        self.timeline.iter()
    }

    /// Every route the trace touched: snapshots and timeline ops, sorted.
    pub fn routes(&self) -> Vec<Route> {
        let mut routes: Vec<Route> = self
            .snapshots
            .iter()
            .map(|snapshot| snapshot.route.clone())
            .chain(self.timeline.iter().filter_map(|entry| match &entry.event {
                Event::Op(op) => Some(op.route.clone()),
                Event::Inspect(frame) => Some(frame.route.clone()),
                Event::Presence(_) => None,
            }))
            .collect();
        routes.sort();
        routes.dedup();
        routes
    }

    /// The starting snapshot of `route`, if one was received.
    pub fn snapshot_of(&self, route: &Route) -> Option<&RouteSnapshot> {
        self.snapshots
            .iter()
            .find(|snapshot| &snapshot.route == route)
    }

    /// Apps, sorted, and rooms the trace touched.
    fn apps_and_rooms(&self) -> (Vec<String>, Vec<String>) {
        let routes = self.routes();
        let mut apps: Vec<String> = routes
            .iter()
            .map(|route| route.app_id.clone())
            .filter(|app| !app.is_empty())
            .collect();
        apps.sort();
        apps.dedup();
        let mut rooms: Vec<String> = routes.iter().map(|route| route.room.clone()).collect();
        rooms.extend(self.timeline.iter().filter_map(|entry| match &entry.event {
            Event::Presence(update) => Some(update.room.clone()),
            _ => None,
        }));
        rooms.retain(|room| !room.is_empty());
        rooms.sort();
        rooms.dedup();
        (apps, rooms)
    }

    /// Each distinct semantic interaction (a step) and whether its last
    /// phase failed, plus the number of document ops.
    fn steps_and_ops(&self) -> (Vec<(tilo_actions::InvocationId, bool)>, u32) {
        let mut last_phase: Vec<(tilo_actions::InvocationId, bool)> = Vec::new();
        let mut ops = 0_u32;
        for entry in &self.timeline {
            match &entry.event {
                Event::Op(_) => ops = ops.saturating_add(1),
                Event::Presence(PresenceUpdate {
                    change: PresenceChange::Interaction(interaction),
                    ..
                }) => {
                    let failed = matches!(
                        interaction.phase,
                        tilo_actions::InteractionPhase::Failed { .. }
                    );
                    match last_phase
                        .iter_mut()
                        .find(|(id, _)| *id == interaction.invocation)
                    {
                        Some(slot) => slot.1 = failed,
                        None => last_phase.push((interaction.invocation, failed)),
                    }
                }
                Event::Presence(_) | Event::Inspect(_) => {}
            }
        }
        (last_phase, ops)
    }

    /// The card a remark carries for this session.
    pub fn summary(&self) -> SessionSummary {
        let (mut apps, _) = self.apps_and_rooms();
        apps.truncate(MAX_SUMMARY_APPS);
        let (steps, _) = self.steps_and_ops();
        SessionSummary {
            apps,
            duration_ms: self.duration.as_millis(),
            steps: u32::try_from(steps.len()).unwrap_or(u32::MAX),
            layer: self.layer,
            redactions: self.redactions,
            generation: self
                .environment
                .image
                .as_ref()
                .and_then(|image| image.generation),
        }
    }

    /// The rest of the cheap metadata.
    pub fn details(&self) -> SessionDetails {
        let (_, rooms) = self.apps_and_rooms();
        let (steps, ops) = self.steps_and_ops();
        SessionDetails {
            origin: Some(self.origin),
            started_unix_ms: self.started_unix_ms,
            rooms,
            failed_steps: u32::try_from(steps.iter().filter(|(_, failed)| *failed).count())
                .unwrap_or(u32::MAX),
            ops,
            tags: self.tags.clone(),
            truncated: self.truncated,
        }
    }
}

/// An Inspect answer travels as its own JSON form, the one the broker
/// carries (`InspectReply::encode`), which a non-self-describing format
/// could not read back.
mod reply_json {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use tilo_reference::inspect::InspectReply;

    pub fn serialize<S: Serializer>(
        value: &InspectReply,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serde_json::to_string(value)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<InspectReply, D::Error> {
        let json = String::deserialize(deserializer)?;
        InspectReply::decode(json.as_bytes()).map_err(serde::de::Error::custom)
    }
}

/// `SemanticInteraction` skips absent optional fields when serialising,
/// which a non-self-describing format cannot read back. It travels as its
/// JSON form (the same form the broker's presence wire uses).
mod interaction_json {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use tilo_actions::SemanticInteraction;

    #[allow(
        clippy::borrowed_box,
        reason = "serde's `with` passes the field by reference"
    )]
    pub fn serialize<S: Serializer>(
        value: &Box<SemanticInteraction>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serde_json::to_string(value.as_ref())
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Box<SemanticInteraction>, D::Error> {
        let json = String::deserialize(deserializer)?;
        serde_json::from_str(&json)
            .map(Box::new)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
pub(crate) mod fixtures;
