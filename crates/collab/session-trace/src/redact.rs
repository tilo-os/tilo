//! The redacted duplicate: what of a session may leave the device.
//!
//! Automerge history cannot be served in part (`docs/architecture/rooms.md`),
//! so a recording never sends the user's document. It sends a duplicate: a
//! new trace built by replaying the recorded window into fresh documents.
//! Each route's starting state is materialised and written into a new
//! document with every personal value masked to its shape; each recorded op
//! is applied to the original and the new document is brought to the same,
//! masked, state, and that step becomes the duplicate's op. Presence text is
//! masked the same way; cut steps' interactions are dropped; inspect
//! keyframes are rewritten by the caller, which owns their encoding.
//!
//! The duplicate's envelopes are unsigned and cleartext. Replay never
//! forwards a recorded envelope — it re-frames each mutation onto its own
//! route under its own key — so a signature would vouch for nothing, and
//! a ciphertext op cannot be masked and is left out.

use std::collections::{BTreeSet, HashSet};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ChangeHash, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use collab_types::document_transfer::{DocumentFrame, DocumentReassembler};
use collab_types::{DocumentMutation, SignedOp};
use tilo_actions::{InteractionPhase, InvocationId, SemanticInteraction};
use tilo_automerge::ApplyIncoming as _;

use crate::{
    DocumentOp, Event, PresenceChange, PresenceUpdate, Route, RouteSnapshot, SessionTrace,
    SnapshotPoint, TimedEvent,
};

/// The glyph a masked character becomes, shared with review so the
/// walk-through shows exactly what is sent.
pub const MASK: char = '\u{25AE}';

/// Values shorter than this are not hunted for inside other text: they would
/// hide ordinary words.
const MIN_VALUE_CHARS: usize = 3;
/// A word of a value is hunted for on its own from this length, and so is a
/// prefix of one this long (what the text held while it was being typed).
/// Three, as review hides the parts of a declared name.
const MIN_WORD_CHARS: usize = 3;

/// Masks every occurrence of a set of values, wherever it appears.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Masker {
    values: Vec<String>,
    words: Vec<String>,
}

fn words_of(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
}

impl Masker {
    /// A masker for `values`: whole values, and their longer words, are
    /// masked case-insensitively.
    pub fn new(values: impl IntoIterator<Item = String>) -> Self {
        let mut all: BTreeSet<String> = BTreeSet::new();
        let mut words: BTreeSet<String> = BTreeSet::new();
        for value in values {
            let value = value.trim().to_lowercase();
            if value.chars().count() < MIN_VALUE_CHARS {
                continue;
            }
            for word in words_of(&value) {
                if word.chars().count() >= MIN_WORD_CHARS {
                    words.insert(word.to_string());
                }
            }
            all.insert(value);
        }
        // Longest first, so a value is masked whole before its parts.
        let mut values: Vec<String> = all.into_iter().collect();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        Self {
            values,
            words: words.into_iter().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// `text` with every hidden run masked, one mask per character.
    pub fn mask(&self, text: &str) -> String {
        if self.values.is_empty() || text.is_empty() {
            return text.to_string();
        }
        let lowered = text.to_lowercase();
        // Lowercasing can change byte lengths; then only the token pass,
        // which walks the original text, is safe.
        let aligned = lowered.len() == text.len();
        let mut hidden = vec![false; text.len()];
        let mut hide = |start: usize, end: usize| {
            for flag in hidden.iter_mut().take(end).skip(start) {
                *flag = true;
            }
        };
        if aligned {
            for value in &self.values {
                let mut from = 0_usize;
                while let Some(found) = lowered
                    .get(from..)
                    .and_then(|rest| rest.find(value.as_str()))
                {
                    let start = from.saturating_add(found);
                    let end = start.saturating_add(value.len());
                    hide(start, end);
                    from = end;
                }
            }
        }
        let mut token_start: Option<usize> = None;
        let check = |start: usize, end: usize, hide: &mut dyn FnMut(usize, usize)| {
            let Some(token) = text.get(start..end) else {
                return;
            };
            let token = token.to_lowercase();
            if token.chars().count() < MIN_VALUE_CHARS {
                return;
            }
            let secret = self.words.iter().any(|word| {
                *word == token
                    || (token.chars().count() >= MIN_VALUE_CHARS && word.starts_with(&token))
                    || (word.chars().count() >= MIN_WORD_CHARS && token.contains(word.as_str()))
            });
            if secret {
                hide(start, end);
            }
        };
        for (offset, c) in text.char_indices() {
            if c.is_alphanumeric() {
                token_start.get_or_insert(offset);
            } else if let Some(start) = token_start.take() {
                check(start, offset, &mut hide);
            }
        }
        if let Some(start) = token_start {
            check(start, text.len(), &mut hide);
        }
        text.char_indices()
            .map(|(offset, c)| {
                if !c.is_whitespace() && hidden.get(offset).copied().unwrap_or(false) {
                    MASK
                } else {
                    c
                }
            })
            .collect()
    }
}

/// Rewrites one inspect keyframe's answer for leaving the device, or drops
/// it with `None`.
pub type KeyframeRedactor<'a> = &'a dyn Fn(&Route, &[u8], &Masker) -> Option<Vec<u8>>;

/// What to take out of a session.
pub struct Redaction<'a> {
    /// Values hidden wherever they occur: in documents, presence and labels.
    pub masker: Masker,
    /// Interactions the person cut from the walk-through.
    pub cut: HashSet<InvocationId>,
    /// Rewrite one inspect keyframe's answer for leaving the device, or drop
    /// it with `None`. The answer's encoding belongs to the Inspect request.
    pub keyframe: KeyframeRedactor<'a>,
}

/// Why a duplicate could not be built.
#[derive(Debug, thiserror::Error)]
pub enum RedactError {
    #[error("route {0:?} has no readable starting snapshot")]
    NoSnapshot(Route),
    #[error("a recorded document could not be read: {0}")]
    Document(String),
}

/// What building the duplicate had to leave out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuplicateReport {
    /// Encrypted ops that could not be read, so could not be masked.
    pub unreadable_ops: u32,
    /// Routes with no readable snapshot, whose ops were left out.
    pub unreadable_routes: u32,
    pub dropped_keyframes: u32,
}

fn cleartext(op: &SignedOp) -> bool {
    op.encryption.is_none()
}

/// Reassemble a snapshot's frames into its Automerge bytes.
fn snapshot_bytes(snapshot: &RouteSnapshot) -> Option<Vec<u8>> {
    let mut reassembler = DocumentReassembler::default();
    for frame in &snapshot.frames {
        if !cleartext(frame) {
            return None;
        }
        match DocumentFrame::decode(&frame.payload).ok()? {
            DocumentFrame::Whole(mutation) => return Some(mutation.data().to_vec()),
            DocumentFrame::Segment(segment) => {
                if let Ok(Some(done)) =
                    reassembler.accept(frame, segment, std::time::Instant::now())
                {
                    return Some(done.payload);
                }
            }
        }
    }
    None
}

fn incremental_bytes(op: &SignedOp) -> Option<Vec<u8>> {
    if !cleartext(op) {
        return None;
    }
    match DocumentMutation::decode(&op.payload).ok()? {
        DocumentMutation::Incremental(data) | DocumentMutation::Snapshot(data) => Some(data),
    }
}

fn envelope(like: &SignedOp, mutation: &DocumentMutation, seq: u64) -> Option<Vec<SignedOp>> {
    let frames = mutation.encode_frames().ok()?;
    Some(
        frames
            .into_iter()
            .enumerate()
            .map(|(index, payload)| {
                SignedOp::new_unsigned(
                    payload,
                    like.artifact,
                    like.author,
                    like.attestation_id,
                    like.op_class,
                    like.timestamp,
                    seq.saturating_add(u64::try_from(index).unwrap_or(0)),
                )
            })
            .collect(),
    )
}

/// Bring `dst_obj` to `src_obj`'s masked state.
fn sync(
    src: &AutoCommit,
    src_obj: &ObjId,
    dst: &mut AutoCommit,
    dst_obj: &ObjId,
    masker: &Masker,
) -> Result<(), automerge::AutomergeError> {
    match src.object_type(src_obj)? {
        ObjType::Map | ObjType::Table => sync_map(src, src_obj, dst, dst_obj, masker),
        ObjType::List => sync_list(src, src_obj, dst, dst_obj, masker),
        ObjType::Text => {
            let text = masker.mask(&src.text(src_obj)?);
            if dst.text(dst_obj)? != text {
                dst.update_text(dst_obj, text)?;
            }
            Ok(())
        }
    }
}

fn masked_scalar(value: &ScalarValue, masker: &Masker) -> ScalarValue {
    match value {
        ScalarValue::Str(text) => ScalarValue::Str(masker.mask(text).into()),
        other => other.clone(),
    }
}

fn same_scalar(dst: Option<(Value<'_>, ObjId)>, want: &ScalarValue) -> bool {
    matches!(dst, Some((Value::Scalar(have), _)) if have.as_ref() == want)
}

fn sync_map(
    src: &AutoCommit,
    src_obj: &ObjId,
    dst: &mut AutoCommit,
    dst_obj: &ObjId,
    masker: &Masker,
) -> Result<(), automerge::AutomergeError> {
    let keys: Vec<String> = src.keys(src_obj).collect();
    for key in &keys {
        let Some((value, child)) = src.get(src_obj, key.as_str())? else {
            continue;
        };
        match value {
            Value::Object(kind) => {
                let existing = dst.get(dst_obj, key.as_str())?.and_then(|(value, id)| {
                    matches!(value, Value::Object(have) if have == kind).then_some(id)
                });
                let target = match existing {
                    Some(id) => id,
                    None => dst.put_object(dst_obj, key.as_str(), kind)?,
                };
                sync(src, &child, dst, &target, masker)?;
            }
            Value::Scalar(scalar) => {
                let want = masked_scalar(&scalar, masker);
                if !same_scalar(dst.get(dst_obj, key.as_str())?, &want) {
                    dst.put(dst_obj, key.as_str(), want)?;
                }
            }
        }
    }
    let stale: Vec<String> = dst
        .keys(dst_obj)
        .filter(|key| !keys.contains(key))
        .collect();
    for key in stale {
        dst.delete(dst_obj, key.as_str())?;
    }
    Ok(())
}

fn sync_list(
    src: &AutoCommit,
    src_obj: &ObjId,
    dst: &mut AutoCommit,
    dst_obj: &ObjId,
    masker: &Masker,
) -> Result<(), automerge::AutomergeError> {
    let len = src.length(src_obj);
    while dst.length(dst_obj) > len {
        let last = dst.length(dst_obj).saturating_sub(1);
        dst.delete(dst_obj, last)?;
    }
    for index in 0..len {
        let Some((value, child)) = src.get(src_obj, index)? else {
            continue;
        };
        let have = (index < dst.length(dst_obj))
            .then(|| dst.get(dst_obj, index))
            .transpose()?
            .flatten();
        match value {
            Value::Object(kind) => {
                let existing = have.and_then(|(value, id)| {
                    matches!(value, Value::Object(k) if k == kind).then_some(id)
                });
                let target = match existing {
                    Some(id) => id,
                    None if index < dst.length(dst_obj) => dst.put_object(dst_obj, index, kind)?,
                    None => dst.insert_object(dst_obj, index, kind)?,
                };
                sync(src, &child, dst, &target, masker)?;
            }
            Value::Scalar(scalar) => {
                let want = masked_scalar(&scalar, masker);
                if index >= dst.length(dst_obj) {
                    dst.insert(dst_obj, index, want)?;
                } else if !same_scalar(have, &want) {
                    dst.put(dst_obj, index, want)?;
                }
            }
        }
    }
    Ok(())
}

/// The starting state of a route: its start snapshot as is, or an end
/// snapshot with the window's own changes taken back out.
fn starting_doc(
    snapshot: &RouteSnapshot,
    window: &[Vec<u8>],
) -> Result<Option<AutoCommit>, RedactError> {
    let Some(bytes) = snapshot_bytes(snapshot) else {
        return Ok(None);
    };
    let mut doc =
        AutoCommit::load(&bytes).map_err(|error| RedactError::Document(error.to_string()))?;
    if snapshot.point == SnapshotPoint::Start {
        return Ok(Some(doc));
    }
    let mut scratch = AutoCommit::new();
    let mut window_hashes: BTreeSet<ChangeHash> = BTreeSet::new();
    for change in window {
        let before: Vec<ChangeHash> = scratch.get_heads();
        let _ = scratch.load_incoming(change);
        for change in scratch.get_changes(&before) {
            window_hashes.insert(change.hash());
        }
    }
    let kept: Vec<automerge::Change> = doc
        .get_changes(&[])
        .into_iter()
        .filter(|change| !window_hashes.contains(&change.hash()))
        .collect();
    let mut start = AutoCommit::new();
    start
        .apply_incoming(kept)
        .map_err(|error| RedactError::Document(error.to_string()))?;
    Ok(Some(start))
}

fn masked_interaction(interaction: &SemanticInteraction, masker: &Masker) -> SemanticInteraction {
    let mut out = interaction.clone();
    out.resource.label = masker.mask(&out.resource.label);
    out.phase = match &interaction.phase {
        InteractionPhase::Failed { code, message } => InteractionPhase::Failed {
            code: code.clone(),
            message: masker.mask(message),
        },
        InteractionPhase::Cancelled { message } => InteractionPhase::Cancelled {
            message: message.as_ref().map(|m| masker.mask(m)),
        },
        InteractionPhase::Progress {
            label,
            completed,
            total,
        } => InteractionPhase::Progress {
            label: masker.mask(label),
            completed: *completed,
            total: *total,
        },
        InteractionPhase::Waiting { reason } => InteractionPhase::Waiting {
            reason: reason.as_ref().map(|r| masker.mask(r)),
        },
        other => other.clone(),
    };
    out
}

/// One route being replayed: the original document, its masked duplicate,
/// and the next sequence number of the duplicate's envelopes.
struct Replayed {
    route: Route,
    src: AutoCommit,
    dst: AutoCommit,
    seq: u64,
}

fn document_error(error: impl std::fmt::Display) -> RedactError {
    RedactError::Document(error.to_string())
}

/// Materialise `route`'s starting state into a masked duplicate, and the
/// duplicate's starting snapshot.
fn start_route(
    trace: &SessionTrace,
    route: &Route,
    masker: &Masker,
) -> Result<Option<(RouteSnapshot, Replayed)>, RedactError> {
    let Some(snapshot) = trace.snapshot_of(route) else {
        return Ok(None);
    };
    let window: Vec<Vec<u8>> = trace
        .events()
        .filter_map(|entry| match &entry.event {
            Event::Op(op) if &op.route == route => incremental_bytes(&op.envelope),
            _ => None,
        })
        .collect();
    let Some(src) = starting_doc(snapshot, &window)? else {
        return Ok(None);
    };
    let mut dst = AutoCommit::new();
    sync(&src, &automerge::ROOT, &mut dst, &automerge::ROOT, masker).map_err(document_error)?;
    let Some(like) = snapshot.frames.first() else {
        return Ok(None);
    };
    let Some(frames) = envelope(like, &DocumentMutation::Snapshot(dst.save()), 0) else {
        return Ok(None);
    };
    let seq = u64::try_from(frames.len()).unwrap_or(u64::MAX);
    Ok(Some((
        RouteSnapshot {
            route: route.clone(),
            from: snapshot.from.clone(),
            at: snapshot.at,
            point: SnapshotPoint::Start,
            frames,
        },
        Replayed {
            route: route.clone(),
            src,
            dst,
            seq,
        },
    )))
}

/// Apply one recorded op to the original and re-express its effect as the
/// duplicate's own change. Nothing when the masked state did not change.
fn replay_op(
    replayed: &mut Replayed,
    op: &DocumentOp,
    bytes: &[u8],
    at: crate::Offset,
    masker: &Masker,
) -> Result<Vec<TimedEvent>, RedactError> {
    replayed.src.load_incoming(bytes).map_err(document_error)?;
    sync(
        &replayed.src,
        &automerge::ROOT,
        &mut replayed.dst,
        &automerge::ROOT,
        masker,
    )
    .map_err(document_error)?;
    let change = replayed.dst.save_incremental();
    if change.is_empty() {
        return Ok(Vec::new());
    }
    let Some(frames) = envelope(
        &op.envelope,
        &DocumentMutation::Incremental(change),
        replayed.seq,
    ) else {
        return Ok(Vec::new());
    };
    replayed.seq = replayed
        .seq
        .saturating_add(u64::try_from(frames.len()).unwrap_or(1));
    Ok(frames
        .into_iter()
        .map(|envelope| TimedEvent {
            at,
            event: Event::Op(DocumentOp {
                route: replayed.route.clone(),
                from: op.from.clone(),
                envelope,
            }),
        })
        .collect())
}

/// A presence update as it may leave, or `None` for a cut step's.
fn redacted_presence(update: &PresenceUpdate, redaction: &Redaction<'_>) -> Option<PresenceUpdate> {
    let change = match &update.change {
        PresenceChange::Interaction(interaction) => {
            if redaction.cut.contains(&interaction.invocation) {
                return None;
            }
            PresenceChange::Interaction(Box::new(masked_interaction(
                interaction,
                &redaction.masker,
            )))
        }
        PresenceChange::Status { text, automatic } => PresenceChange::Status {
            text: redaction.masker.mask(text),
            automatic: *automatic,
        },
        other => other.clone(),
    };
    Some(PresenceUpdate {
        change,
        ..update.clone()
    })
}

/// Build the redacted duplicate of `trace`.
///
/// # Errors
///
/// [`RedactError`] when a readable snapshot's document cannot be loaded.
pub fn duplicate(
    trace: &SessionTrace,
    redaction: &Redaction<'_>,
) -> Result<(SessionTrace, DuplicateReport), RedactError> {
    let mut report = DuplicateReport::default();
    let mut replaying: Vec<Replayed> = Vec::new();
    let mut snapshots: Vec<RouteSnapshot> = Vec::new();
    for route in trace.routes() {
        if trace.snapshot_of(&route).is_none() {
            continue;
        }
        match start_route(trace, &route, &redaction.masker)? {
            Some((snapshot, replayed)) => {
                snapshots.push(snapshot);
                replaying.push(replayed);
            }
            None => report.unreadable_routes = report.unreadable_routes.saturating_add(1),
        }
    }

    let mut timeline: Vec<TimedEvent> = Vec::with_capacity(trace.timeline.len());
    for entry in trace.events() {
        let event = match &entry.event {
            Event::Op(op) => {
                let Some(bytes) = incremental_bytes(&op.envelope) else {
                    report.unreadable_ops = report.unreadable_ops.saturating_add(1);
                    continue;
                };
                if let Some(replayed) = replaying.iter_mut().find(|r| r.route == op.route) {
                    timeline.extend(replay_op(
                        replayed,
                        op,
                        &bytes,
                        entry.at,
                        &redaction.masker,
                    )?);
                }
                continue;
            }
            Event::Presence(update) => match redacted_presence(update, redaction) {
                Some(update) => Event::Presence(update),
                None => continue,
            },
            Event::Inspect(keyframe) => {
                let Some(answer) =
                    (redaction.keyframe)(&keyframe.route, &keyframe.answer, &redaction.masker)
                else {
                    report.dropped_keyframes = report.dropped_keyframes.saturating_add(1);
                    continue;
                };
                Event::Inspect(crate::InspectKeyframe {
                    route: keyframe.route.clone(),
                    answer,
                })
            }
        };
        timeline.push(TimedEvent {
            at: entry.at,
            event,
        });
    }

    Ok((
        SessionTrace {
            origin: trace.origin,
            started_unix_ms: trace.started_unix_ms,
            duration: trace.duration,
            environment: trace.environment.clone(),
            snapshots,
            timeline,
            tags: trace.tags.clone(),
            layer: trace.layer,
            redactions: trace.redactions,
            truncated: trace.truncated || report.unreadable_ops > 0,
        },
        report,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Environment, Offset, Origin, RedactionSummary, SessionLayer};
    use automerge::transaction::Transactable;

    #[test]
    fn a_value_is_masked_whole_in_parts_and_while_it_was_typed() {
        let masker = Masker::new(["Ada Lovelace".to_string()]);
        assert_eq!(masker.mask("To: Ada Lovelace,"), "To: ▮▮▮ ▮▮▮▮▮▮▮▮,");
        assert_eq!(masker.mask("dear lovelace"), "dear ▮▮▮▮▮▮▮▮");
        assert_eq!(
            masker.mask("Lov"),
            "▮▮▮",
            "a prefix of a hidden word is what the field held while it was typed"
        );
        assert_eq!(
            masker.mask("the love of numbers"),
            "the ▮▮▮▮ of numbers",
            "an ordinary word that begins a hidden one is hidden too: the typed \
             prefixes of a value are indistinguishable from it"
        );
        assert_eq!(Masker::default().mask("Ada"), "Ada");
    }

    fn op(route: &Route, payload: Vec<u8>, seq: u64) -> SignedOp {
        SignedOp::new_unsigned(
            payload,
            collab_types::document_artifact_id(&route.app_id, &route.room, &route.record_id),
            collab_types::Ed25519PublicKey::from_bytes([7; 32]),
            collab_types::document_attestation_id(&route.app_id, &route.room, &route.record_id),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            seq,
        )
    }

    fn no_keyframes(_: &Route, _: &[u8], _: &Masker) -> Option<Vec<u8>> {
        None
    }

    /// A draft is written to Ada in two edits; the duplicate replays to the
    /// same shape with her name nowhere in any document it carries.
    #[test]
    fn the_duplicate_replays_the_window_with_the_value_masked_everywhere() {
        let route = Route::new("so.tilo.mail", "room-1", "uuid:draft");
        let mut doc = AutoCommit::new();
        let text = doc
            .put_object(automerge::ROOT, "body", ObjType::Text)
            .unwrap();
        doc.put(automerge::ROOT, "to", "").unwrap();
        let start = doc.save();
        doc.put(automerge::ROOT, "to", "Ada Lovelace").unwrap();
        let first = doc.save_incremental();
        doc.splice_text(&text, 0, 0, "Dear Ada, the numbers")
            .unwrap();
        let second = doc.save_incremental();

        let trace = SessionTrace {
            origin: Origin::Explicit,
            started_unix_ms: 0,
            duration: Offset(3),
            environment: Environment::default(),
            snapshots: vec![RouteSnapshot {
                route: route.clone(),
                from: "me".into(),
                at: Offset(0),
                point: SnapshotPoint::Start,
                frames: vec![op(
                    &route,
                    DocumentMutation::Snapshot(start).encode().unwrap(),
                    0,
                )],
            }],
            timeline: [first, second]
                .into_iter()
                .enumerate()
                .map(|(index, change)| TimedEvent {
                    at: Offset(u64::try_from(index).unwrap() + 1),
                    event: Event::Op(DocumentOp {
                        route: route.clone(),
                        from: "me".into(),
                        envelope: op(
                            &route,
                            DocumentMutation::Incremental(change).encode().unwrap(),
                            1,
                        ),
                    }),
                })
                .collect(),
            tags: Vec::new(),
            layer: SessionLayer::Logic,
            redactions: RedactionSummary::default(),
            truncated: false,
        };
        let redaction = Redaction {
            masker: Masker::new(["Ada Lovelace".to_string()]),
            cut: HashSet::new(),
            keyframe: &no_keyframes,
        };
        let (dup, report) = duplicate(&trace, &redaction).unwrap();
        assert_eq!(report, DuplicateReport::default());
        assert_eq!(dup.timeline.len(), 2);

        let mut replay = AutoCommit::load(&snapshot_bytes(&dup.snapshots[0]).unwrap()).unwrap();
        for entry in &dup.timeline {
            let Event::Op(op) = &entry.event else {
                panic!("ops only")
            };
            replay
                .load_incoming(&incremental_bytes(&op.envelope).unwrap())
                .unwrap();
        }
        let (_, body) = replay.get(automerge::ROOT, "body").unwrap().unwrap();
        assert_eq!(replay.text(&body).unwrap(), "Dear ▮▮▮, the numbers");
        let to = replay.get(automerge::ROOT, "to").unwrap().unwrap().0;
        assert_eq!(to.as_str(), Some("▮▮▮ ▮▮▮▮▮▮▮▮"));
        let everything = serde_json::to_string(&dup).unwrap();
        for entry in &dup.timeline {
            let Event::Op(op) = &entry.event else {
                continue;
            };
            assert!(
                !String::from_utf8_lossy(&op.envelope.payload).contains("Ada"),
                "no op carries the value"
            );
        }
        assert!(!everything.contains("Lovelace"));
    }
}
