//! The `session` map inside a record document.
//!
//! ```text
//! tilo:record { id, kind: "session", home, refs, blobs }   (record machinery)
//! session {
//!   format:      FORMAT_VERSION
//!   duration_us: monotonic span
//!   summary:     { apps[], duration_ms, steps, layer, redacted_text,
//!                  redacted_regions, generation? }        (the card)
//!   details:     { origin, started_unix_ms, rooms[], failed_steps, ops,
//!                  tags[], truncated }
//!   environment: bytes (postcard Environment)
//!   snapshots:   [bytes (postcard RouteSnapshot)]
//!   timeline:    [bytes (postcard Vec<TimedEvent>)]   chunks, in order
//! }
//! ```
//!
//! Both maps are plain Automerge scalars so a card reads them without
//! decoding a single timeline chunk.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::de::DeserializeOwned;

use crate::{
    Environment, FORMAT_VERSION, Origin, Redactions, RouteSnapshot, SessionDetails, SessionLayer,
    SessionSummary, SessionTrace, TimedEvent,
};

const SESSION: &str = "session";
/// Soft ceiling on one encoded timeline chunk.
const CHUNK_BYTES: usize = 1024 * 1024;
const CHUNK_EVENTS: usize = 1_024;
/// Readers refuse to allocate beyond these, whatever a document claims.
const MAX_CHUNKS: usize = 4_096;
const MAX_SNAPSHOTS: usize = 256;
const MAX_LISTED: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("the document already holds a session")]
    AlreadyWritten,
    #[error("the document holds no session")]
    Missing,
    #[error("session format {0} is not supported")]
    UnsupportedFormat(u64),
    #[error("session field {0} is missing or malformed")]
    Malformed(&'static str),
    #[error("session encoding failed: {0}")]
    Encode(#[from] postcard::Error),
    #[error("session document write failed: {0}")]
    Document(#[from] automerge::AutomergeError),
}

/// Whether `doc` holds a session trace.
pub fn is_session(doc: &impl ReadDoc) -> bool {
    session_obj(doc).is_some()
}

/// Write `trace` beside the record identity in `doc`. A session is written
/// once; a redacted duplicate is a new record, never a rewrite.
///
/// # Errors
///
/// [`TraceError::AlreadyWritten`] when `doc` already holds a session, or an
/// encoding or document error.
pub fn write(doc: &mut AutoCommit, trace: &SessionTrace) -> Result<(), TraceError> {
    if is_session(doc) {
        return Err(TraceError::AlreadyWritten);
    }
    let session = doc.put_object(ROOT, SESSION, ObjType::Map)?;
    doc.put(&session, "format", FORMAT_VERSION)?;
    doc.put(&session, "duration_us", trace.duration.0)?;
    write_summary(doc, &session, &trace.summary())?;
    write_details(doc, &session, &trace.details())?;
    doc.put(
        &session,
        "environment",
        ScalarValue::Bytes(postcard::to_allocvec(&trace.environment)?),
    )?;
    let snapshots = doc.put_object(&session, "snapshots", ObjType::List)?;
    for (index, snapshot) in trace.snapshots.iter().enumerate() {
        doc.insert(
            &snapshots,
            index,
            ScalarValue::Bytes(postcard::to_allocvec(snapshot)?),
        )?;
    }
    let timeline = doc.put_object(&session, "timeline", ObjType::List)?;
    let mut chunk: Vec<&TimedEvent> = Vec::new();
    let mut chunk_bytes = 0_usize;
    let mut chunks = 0_usize;
    for entry in &trace.timeline {
        chunk.push(entry);
        chunk_bytes = chunk_bytes.saturating_add(entry.event.weight());
        if chunk.len() >= CHUNK_EVENTS || chunk_bytes >= CHUNK_BYTES {
            doc.insert(
                &timeline,
                chunks,
                ScalarValue::Bytes(postcard::to_allocvec(&chunk)?),
            )?;
            chunks = chunks.saturating_add(1);
            chunk.clear();
            chunk_bytes = 0;
        }
    }
    if !chunk.is_empty() {
        doc.insert(
            &timeline,
            chunks,
            ScalarValue::Bytes(postcard::to_allocvec(&chunk)?),
        )?;
    }
    Ok(())
}

const fn layer_name(layer: SessionLayer) -> &'static str {
    match layer {
        SessionLayer::Logic => "logic",
        SessionLayer::BelowToolkit => "below_toolkit",
    }
}

fn parse_layer(value: &str) -> Option<SessionLayer> {
    match value {
        "logic" => Some(SessionLayer::Logic),
        "below_toolkit" => Some(SessionLayer::BelowToolkit),
        _ => None,
    }
}

fn write_summary(
    doc: &mut AutoCommit,
    session: &ObjId,
    summary: &SessionSummary,
) -> Result<(), TraceError> {
    let map = doc.put_object(session, "summary", ObjType::Map)?;
    put_strings(doc, &map, "apps", &summary.apps)?;
    doc.put(&map, "duration_ms", summary.duration_ms)?;
    doc.put(&map, "steps", u64::from(summary.steps))?;
    doc.put(&map, "layer", layer_name(summary.layer))?;
    doc.put(&map, "redacted_text", u64::from(summary.redactions.text))?;
    doc.put(
        &map,
        "redacted_regions",
        u64::from(summary.redactions.regions),
    )?;
    if let Some(generation) = summary.generation {
        doc.put(&map, "generation", generation)?;
    }
    Ok(())
}

fn write_details(
    doc: &mut AutoCommit,
    session: &ObjId,
    details: &SessionDetails,
) -> Result<(), TraceError> {
    let map = doc.put_object(session, "details", ObjType::Map)?;
    if let Some(origin) = details.origin {
        doc.put(&map, "origin", origin.as_str())?;
    }
    doc.put(&map, "started_unix_ms", details.started_unix_ms)?;
    put_strings(doc, &map, "rooms", &details.rooms)?;
    doc.put(&map, "failed_steps", u64::from(details.failed_steps))?;
    doc.put(&map, "ops", u64::from(details.ops))?;
    put_strings(doc, &map, "tags", &details.tags)?;
    doc.put(&map, "truncated", details.truncated)?;
    Ok(())
}

fn put_strings(
    doc: &mut AutoCommit,
    map: &ObjId,
    key: &str,
    values: &[String],
) -> Result<(), TraceError> {
    let list = doc.put_object(map, key, ObjType::List)?;
    for (index, value) in values.iter().take(MAX_LISTED).enumerate() {
        doc.insert(&list, index, value.as_str())?;
    }
    Ok(())
}

fn session_obj(doc: &impl ReadDoc) -> Option<ObjId> {
    match doc.get(ROOT, SESSION) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

fn object(doc: &impl ReadDoc, parent: &ObjId, key: &str, kind: ObjType) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(found), id))) if found == kind => Some(id),
        _ => None,
    }
}

fn scalar(doc: &impl ReadDoc, parent: &ObjId, key: &str) -> Option<ScalarValue> {
    match doc.get(parent, key) {
        Ok(Some((Value::Scalar(value), _))) => Some(value.into_owned()),
        _ => None,
    }
}

fn uint(doc: &impl ReadDoc, parent: &ObjId, key: &str) -> Option<u64> {
    match scalar(doc, parent, key)? {
        ScalarValue::Uint(n) => Some(n),
        ScalarValue::Int(n) => u64::try_from(n).ok(),
        _ => None,
    }
}

fn count(doc: &impl ReadDoc, parent: &ObjId, key: &str) -> u32 {
    uint(doc, parent, key).map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX))
}

fn string(doc: &impl ReadDoc, parent: &ObjId, key: &str) -> Option<String> {
    match scalar(doc, parent, key)? {
        ScalarValue::Str(text) => Some(text.to_string()),
        _ => None,
    }
}

fn strings(doc: &impl ReadDoc, parent: &ObjId, key: &str) -> Vec<String> {
    let Some(list) = object(doc, parent, key, ObjType::List) else {
        return Vec::new();
    };
    (0..doc.length(&list).min(MAX_LISTED))
        .filter_map(|index| match doc.get(&list, index) {
            Ok(Some((Value::Scalar(value), _))) => match value.as_ref() {
                ScalarValue::Str(text) => Some(text.to_string()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn check_format(doc: &impl ReadDoc, session: &ObjId) -> Result<(), TraceError> {
    match uint(doc, session, "format") {
        Some(FORMAT_VERSION) => Ok(()),
        Some(other) => Err(TraceError::UnsupportedFormat(other)),
        None => Err(TraceError::Malformed("format")),
    }
}

/// Read the card a remark carries for this session, without decoding the
/// timeline. `None` when `doc` holds no readable session.
pub fn read_summary(doc: &impl ReadDoc) -> Option<SessionSummary> {
    let session = session_obj(doc)?;
    check_format(doc, &session).ok()?;
    let map = object(doc, &session, "summary", ObjType::Map)?;
    Some(SessionSummary {
        apps: strings(doc, &map, "apps"),
        duration_ms: uint(doc, &map, "duration_ms").unwrap_or(0),
        steps: count(doc, &map, "steps"),
        layer: string(doc, &map, "layer")
            .and_then(|value| parse_layer(&value))
            .unwrap_or(SessionLayer::Logic),
        redactions: Redactions {
            text: count(doc, &map, "redacted_text"),
            regions: count(doc, &map, "redacted_regions"),
        },
        generation: uint(doc, &map, "generation"),
    })
}

/// Read the rest of the cheap metadata, without decoding the timeline.
pub fn read_details(doc: &impl ReadDoc) -> Option<SessionDetails> {
    let session = session_obj(doc)?;
    check_format(doc, &session).ok()?;
    let map = object(doc, &session, "details", ObjType::Map)?;
    Some(SessionDetails {
        origin: string(doc, &map, "origin").and_then(|value| Origin::parse(&value)),
        started_unix_ms: uint(doc, &map, "started_unix_ms").unwrap_or(0),
        rooms: strings(doc, &map, "rooms"),
        failed_steps: count(doc, &map, "failed_steps"),
        ops: count(doc, &map, "ops"),
        tags: strings(doc, &map, "tags"),
        truncated: matches!(
            scalar(doc, &map, "truncated"),
            Some(ScalarValue::Boolean(true))
        ),
    })
}

fn decode_bytes<T: DeserializeOwned>(
    value: Option<ScalarValue>,
    field: &'static str,
) -> Result<T, TraceError> {
    match value {
        Some(ScalarValue::Bytes(bytes)) => {
            postcard::from_bytes(&bytes).map_err(|_| TraceError::Malformed(field))
        }
        _ => Err(TraceError::Malformed(field)),
    }
}

fn decode_list<T: DeserializeOwned>(
    doc: &impl ReadDoc,
    session: &ObjId,
    key: &'static str,
    max: usize,
) -> Result<Vec<T>, TraceError> {
    let list = object(doc, session, key, ObjType::List).ok_or(TraceError::Malformed(key))?;
    let len = doc.length(&list);
    if len > max {
        return Err(TraceError::Malformed(key));
    }
    (0..len)
        .map(|index| {
            let value = match doc.get(&list, index) {
                Ok(Some((Value::Scalar(value), _))) => Some(value.into_owned()),
                _ => None,
            };
            decode_bytes(value, key)
        })
        .collect()
}

/// Read the whole trace back.
///
/// # Errors
///
/// [`TraceError::Missing`] when `doc` holds no session, or a format or
/// decoding error.
pub fn read(doc: &impl ReadDoc) -> Result<SessionTrace, TraceError> {
    let session = session_obj(doc).ok_or(TraceError::Missing)?;
    check_format(doc, &session)?;
    let summary = read_summary(doc).ok_or(TraceError::Malformed("summary"))?;
    let details = read_details(doc).ok_or(TraceError::Malformed("details"))?;
    let environment: Environment =
        decode_bytes(scalar(doc, &session, "environment"), "environment")?;
    let snapshots: Vec<RouteSnapshot> = decode_list(doc, &session, "snapshots", MAX_SNAPSHOTS)?;
    let chunks: Vec<Vec<TimedEvent>> = decode_list(doc, &session, "timeline", MAX_CHUNKS)?;
    let timeline: Vec<TimedEvent> = chunks.into_iter().flatten().collect();
    if timeline
        .windows(2)
        .any(|pair| matches!(pair, [a, b] if a.at > b.at))
    {
        return Err(TraceError::Malformed("timeline order"));
    }
    Ok(SessionTrace {
        origin: details.origin.ok_or(TraceError::Malformed("origin"))?,
        started_unix_ms: details.started_unix_ms,
        duration: crate::Offset(
            uint(doc, &session, "duration_us").ok_or(TraceError::Malformed("duration_us"))?,
        ),
        environment,
        snapshots,
        timeline,
        tags: details.tags,
        layer: summary.layer,
        redactions: summary.redactions,
        truncated: details.truncated,
    })
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "test timelines are small constant offsets"
)]
mod tests {
    use std::time::{Duration, Instant, SystemTime};

    use super::*;
    use crate::fixtures::{incremental, interaction, presence, route, snapshot};
    use crate::{
        Event, ImageIdentity, InspectKeyframe, Limits, PresenceChange, SurfaceGeometry,
        TraceBuilder,
    };

    fn sample(events: u64) -> SessionTrace {
        let epoch = Instant::now();
        let mut trace = TraceBuilder::new(
            Origin::Explicit,
            epoch,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            Limits::default(),
        );
        trace.set_environment(Environment {
            build: "shell 0.1.0".into(),
            image: Some(ImageIdentity {
                name: "tilo".into(),
                version: "44.20260925".into(),
                generation: Some(12),
                digest: crate::ImageDigest::Booted("sha256:abc".into()),
            }),
            surfaces: vec![SurfaceGeometry {
                app_id: "so.tilo.notes".into(),
                width: 800,
                height: 600,
                scale: 1.5,
                output: "eDP-1".into(),
            }],
            theme: "dark".into(),
            locale: "de-DE".into(),
        });
        let notes = route("so.tilo.notes");
        trace.push_op(epoch, notes.clone(), "alice".into(), snapshot(1));
        let (started, failed) = interaction();
        trace.push_presence(
            epoch,
            presence("alice", PresenceChange::Interaction(Box::new(started))),
        );
        for n in 0..events {
            let at = epoch + Duration::from_millis(n);
            trace.push_op(
                at,
                notes.clone(),
                "alice".into(),
                incremental(n.saturating_add(2)),
            );
            trace.push_presence(
                at,
                presence(
                    "alice",
                    PresenceChange::Selection {
                        start: n,
                        end: n + 1,
                    },
                ),
            );
        }
        trace.push_presence(
            epoch,
            presence("alice", PresenceChange::Interaction(Box::new(failed))),
        );
        trace.push_inspect(
            epoch,
            InspectKeyframe {
                route: notes,
                reply: crate::fixtures::inspect_reply(),
            },
        );
        trace.add_tag("send-fails");
        trace.finish(epoch + Duration::from_secs(4))
    }

    #[test]
    fn a_session_round_trips_through_its_record_document() {
        let trace = sample(3_000);
        let mut doc = AutoCommit::new();
        write(&mut doc, &trace).expect("write");
        let saved = doc.save();
        let loaded = AutoCommit::load(&saved).expect("load");
        let read_back = read(&loaded).expect("read");
        assert_eq!(read_back, trace);
        let chunks = match loaded.get(ROOT, SESSION) {
            Ok(Some((_, session))) => object(&loaded, &session, "timeline", ObjType::List)
                .map(|list| loaded.length(&list)),
            _ => None,
        };
        assert!(
            chunks.is_some_and(|n| n > 1),
            "long timelines are chunked: {chunks:?}"
        );
        assert!(matches!(
            &read_back.timeline.last().expect("last").event,
            Event::Inspect(_)
        ));
    }

    #[test]
    fn the_summary_is_read_without_the_timeline() {
        let trace = sample(2);
        let mut doc = AutoCommit::new();
        write(&mut doc, &trace).expect("write");
        let summary = read_summary(&doc).expect("summary");
        assert_eq!(summary, trace.summary());
        assert_eq!(summary.apps, vec!["so.tilo.notes"]);
        assert_eq!(summary.steps, 1);
        assert_eq!(summary.duration_ms, 4_000);
        assert_eq!(summary.generation, Some(12));
        assert_eq!(summary.layer, SessionLayer::Logic);
        let details = read_details(&doc).expect("details");
        assert_eq!(details, trace.details());
        assert_eq!(details.failed_steps, 1);
        assert_eq!(details.ops, 2);
        assert_eq!(details.started_unix_ms, 1_700_000_000_000);
        assert_eq!(details.origin, Some(Origin::Explicit));
        // It is the card chat attaches to a remark.
        assert!(
            tilo_reference::RecordAttachment::session(
                "uuid:1",
                "Send fails",
                "personal",
                summary.clone()
            )
            .is_valid()
        );

        // The summary survives a corrupt timeline chunk; the full read does not.
        let session = session_obj(&doc).expect("session");
        let timeline = object(&doc, &session, "timeline", ObjType::List).expect("timeline");
        doc.put(&timeline, 0, ScalarValue::Bytes(vec![0xff; 4]))
            .expect("corrupt");
        assert_eq!(read_summary(&doc), Some(summary));
        assert!(matches!(read(&doc), Err(TraceError::Malformed("timeline"))));
    }

    #[test]
    fn a_session_is_written_once_and_foreign_formats_are_refused() {
        let trace = sample(1);
        let mut doc = AutoCommit::new();
        assert!(!is_session(&doc));
        assert!(matches!(read(&doc), Err(TraceError::Missing)));
        write(&mut doc, &trace).expect("write");
        assert!(is_session(&doc));
        assert!(matches!(
            write(&mut doc, &trace),
            Err(TraceError::AlreadyWritten)
        ));
        let session = session_obj(&doc).expect("session");
        let foreign = crate::FORMAT_VERSION + 1;
        doc.put(&session, "format", foreign).expect("bump");
        assert!(matches!(read(&doc), Err(TraceError::UnsupportedFormat(v)) if v == foreign));
        assert_eq!(read_summary(&doc), None);
    }
}
