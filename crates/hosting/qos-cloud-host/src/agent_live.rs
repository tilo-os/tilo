//! Agent live streaming (lane H), host side: a process-global, bounded,
//! non-blocking bus from the Store-owning agent paths to the serve-https live
//! publisher (`https_owner::agent_live`), plus a small registry of each agent's
//! latest live state for owner-only reads.
//!
//! * [`emit`] never blocks: with no installed bus, or a full queue, the event is
//!   dropped. Live frames are display state only and carry no authority; the
//!   durable Store path stays the only source of truth.
//! * Agent progress messages that start with [`LIVE_PREFIX`] are parsed
//!   strictly and bounded ([`parse_update`]): the outer message is at most
//!   [`MAX_LIVE_MESSAGE_BYTES`], every field is length and character bounded,
//!   `state` is the closed [`AGENT_ACTIVITY_STATES`] set, and a preview for
//!   anything but a Writer record is dropped (the rest of the update kept). Unknown fields are IGNORED (the agent is a separately
//!   deployed binary; a newer agent must not lose its whole update), but a
//!   known field of the wrong type or out of bounds refuses the update.
//! * The chat reply text stays private: it only enters the registry, read by
//!   `agent_runtime_reads` for the owning caller, and never reaches the bus.
//!
//! Hooks (one line each, in lane G files): `capture_worker_commit` in
//! `Store::commit_agent_document_checked`, `on_worker_commit` in
//! `apply_worker_document`, `on_progress` / `on_update` in `RuntimeOwner`.
// Until those hook lines land, the producer half is only reached by tests.
#![cfg_attr(not(test), allow(dead_code))]
use qos_relay::delivery_v3::live_wire::{
    AGENT_ACTIVITY_STATES, MAX_AGENT_LABEL_BYTES, MAX_AGENT_NAME_BYTES, MAX_AGENT_TOOL_BYTES,
    MAX_PREVIEW_TEXT_BYTES, bounded_display_text,
};
use serde::Deserialize;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use uuid::Uuid;

/// Progress message prefix of an agent live update.
pub(crate) const LIVE_PREFIX: &str = "tilo-live-v1:";
/// Whole progress message bound (prefix included): the enrolled progress
/// carrier's own 32000-byte bound (the agent encodes at most 31000).
pub(crate) const MAX_LIVE_MESSAGE_BYTES: usize = 32_000;
/// Reply-so-far bound (bytes).
pub(crate) const MAX_REPLY_BYTES: usize = 4 * 1024;
/// Pending preview text bound (bytes); the agent sends at most the last 512.
pub(crate) const MAX_PENDING_BYTES: usize = MAX_PREVIEW_TEXT_BYTES;
/// Events queued between the Store owner and the publisher.
pub(crate) const BUS_CAPACITY: usize = 256;
/// Agents retained in the registry at once (oldest dropped first).
const MAX_REGISTRY: usize = 64;
/// A finished agent's registry entry is kept this long.
const ENDED_RETAIN: Duration = Duration::from_secs(10);
/// The only application whose documents get an agent caret and previews.
pub(crate) const WRITER_APP: &str = "so.tilo.word-processor";

/// Writer caret: block id and UTF-8 byte offset inside the block text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Caret {
    pub(crate) block: String,
    pub(crate) offset: usize,
}

/// One bus event (design 2c). `name` is the owner's spawn-request name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AgentLiveEvent {
    Activity {
        agent_id: String,
        name: String,
        state: String,
        label: String,
        tool: Option<String>,
    },
    Preview {
        agent_id: String,
        name: String,
        app: String,
        record: String,
        pending_text: String,
        caret: Option<Caret>,
    },
    Committed {
        agent_id: String,
        name: String,
        app: String,
        record: String,
        /// Exact canonical `NeighbourMsg::AppDocument` bytes of the commit.
        canonical: Vec<u8>,
        caret: Option<Caret>,
    },
    Ended {
        agent_id: String,
        /// Final activity state (`done` or `failed`).
        state: String,
    },
    /// The agent's managed process exited (no further messages possible).
    Exited { agent_id: String, failed: bool },
    /// The agent's real caret/selection from its worker participant view
    /// (parity lanes B/C). Once seen it supersedes the commit-derived caret.
    /// `caret: None` clears the agent's caret (participant left).
    Presence {
        agent_id: String,
        name: String,
        app: String,
        record: String,
        caret: Option<Caret>,
        /// Selection head (byte offset in `caret.block`); `None` for a caret.
        head: Option<usize>,
    },
}

// ---- Bus -------------------------------------------------------------------

struct Bus {
    token: u64,
    sender: tokio::sync::mpsc::Sender<AgentLiveEvent>,
}
#[cfg(not(test))]
static BUS: Mutex<Option<Bus>> = Mutex::new(None);
static TOKENS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The process-global bus slot.
#[cfg(not(test))]
fn with_bus<R>(f: impl FnOnce(&mut Option<Bus>) -> R) -> Option<R> {
    BUS.lock().ok().map(|mut bus| f(&mut bus))
}
// Tests: one slot per thread, so concurrently running tests (a serve test
// installs its own publisher) never steal each other's events.
#[cfg(test)]
thread_local! {
    static BUS: RefCell<Option<Bus>> = const { RefCell::new(None) };
}
#[cfg(test)]
fn with_bus<R>(f: impl FnOnce(&mut Option<Bus>) -> R) -> Option<R> {
    Some(BUS.with(|bus| f(&mut bus.borrow_mut())))
}

/// Install a fresh bus (replacing any earlier one) and return its receiver and
/// the token that uninstalls exactly this bus.
pub(crate) fn install() -> (tokio::sync::mpsc::Receiver<AgentLiveEvent>, u64) {
    let (sender, receiver) = tokio::sync::mpsc::channel(BUS_CAPACITY);
    let token = TOKENS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    with_bus(|bus| *bus = Some(Bus { token, sender }));
    (receiver, token)
}

/// Remove the bus installed with `token` (a later install is kept).
pub(crate) fn uninstall(token: u64) {
    with_bus(|bus| {
        if bus.as_ref().is_some_and(|bus| bus.token == token) {
            *bus = None;
        }
    });
}

/// Whether a publisher currently listens.
pub(crate) fn installed() -> bool {
    with_bus(|bus| bus.as_ref().is_some_and(|bus| !bus.sender.is_closed())).unwrap_or(false)
}

/// Non-blocking: drops the event when no bus is installed or it is full.
pub(crate) fn emit(event: AgentLiveEvent) -> bool {
    with_bus(|bus| {
        bus.as_ref()
            .is_some_and(|bus| bus.sender.try_send(event).is_ok())
    })
    .unwrap_or(false)
}

// ---- Agent progress update ---------------------------------------------------

/// Parsed, validated `tilo-live-v1:` progress update (H-agent contract).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct AgentLiveUpdate {
    pub(crate) v: u8,
    pub(crate) seq: u64,
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) tool: Option<String>,
    #[serde(default)]
    pub(crate) reply: Option<LiveReply>,
    #[serde(default)]
    pub(crate) preview: Option<LivePreview>,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct LiveReply {
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) done: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct LivePreview {
    pub(crate) app: String,
    #[serde(default)]
    pub(crate) record: String,
    pub(crate) text: String,
}

fn no_control(text: &str) -> bool {
    !text.chars().any(char::is_control)
}

/// `Some` only for a well-formed, in-bounds live update; `None` for any other
/// progress message (including ordinary human progress text).
pub(crate) fn parse_update(message: &str) -> Option<AgentLiveUpdate> {
    if message.len() > MAX_LIVE_MESSAGE_BYTES {
        return None;
    }
    let json = message.strip_prefix(LIVE_PREFIX)?;
    let mut update: AgentLiveUpdate = serde_json::from_str(json).ok()?;
    let tool_ok = update.tool.as_deref().is_none_or(|tool| {
        !tool.is_empty() && tool.len() <= MAX_AGENT_TOOL_BYTES && no_control(tool)
    });
    let reply_ok = update
        .reply
        .as_ref()
        .is_none_or(|reply| reply.text.len() <= MAX_REPLY_BYTES && !reply.text.contains('\0'));
    // A preview for anything but a live-addressable Writer record is dropped
    // (the rest of the update still counts); an oversized one refuses it.
    if update.preview.as_ref().is_some_and(|preview| {
        preview.app != WRITER_APP || preview.record.len() > 128 || !no_control(&preview.record)
    }) {
        update.preview = None;
    }
    let preview_ok = update.preview.as_ref().is_none_or(|preview| {
        preview.text.len() <= MAX_PENDING_BYTES && !preview.text.contains('\0')
    });
    (update.v == 1
        && AGENT_ACTIVITY_STATES.contains(&update.state.as_str())
        && update.label.len() <= MAX_AGENT_LABEL_BYTES
        && no_control(&update.label)
        && tool_ok
        && reply_ok
        && preview_ok)
        .then_some(update)
}

// ---- Registry ----------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct AgentLiveState {
    name: String,
    invocation: Option<Uuid>,
    activity: Option<(String, String, Option<String>)>,
    reply_partial: Option<String>,
    updated_at_ms: u64,
    updated: Option<Instant>,
    ended: Option<Instant>,
    last_seq: u64,
    /// Latest caret per (app, record).
    caret: Option<(String, String, Caret)>,
    /// The caret comes from the worker participant view (not commit diffs).
    real_caret: bool,
}

static REGISTRY: Mutex<BTreeMap<String, AgentLiveState>> = Mutex::new(BTreeMap::new());

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

fn with_registry<R>(f: impl FnOnce(&mut BTreeMap<String, AgentLiveState>) -> R) -> Option<R> {
    let mut registry = REGISTRY.lock().ok()?;
    let now = Instant::now();
    registry.retain(|_, state| {
        state
            .ended
            .is_none_or(|at| now.duration_since(at) < ENDED_RETAIN)
    });
    Some(f(&mut registry))
}

fn entry<'a>(
    registry: &'a mut BTreeMap<String, AgentLiveState>,
    agent_id: &str,
) -> &'a mut AgentLiveState {
    if !registry.contains_key(agent_id) && registry.len() >= MAX_REGISTRY {
        let oldest = registry
            .iter()
            .min_by_key(|(_, state)| state.updated)
            .map(|(id, _)| id.clone());
        if let Some(oldest) = oldest {
            registry.remove(&oldest);
        }
    }
    registry.entry(agent_id.to_owned()).or_default()
}

/// Display name cached for an agent (set by the first resolved update).
pub(crate) fn cached_name(agent_id: &str) -> Option<String> {
    with_registry(|registry| registry.get(agent_id).map(|state| state.name.clone()))
        .flatten()
        .filter(|name| !name.is_empty())
}

/// Latest caret known for an agent in `(app, record)`.
pub(crate) fn caret_for(agent_id: &str, app: &str, record: &str) -> Option<Caret> {
    with_registry(|registry| {
        registry.get(agent_id).and_then(|state| {
            state
                .caret
                .as_ref()
                .filter(|(a, r, _)| a == app && r == record)
                .map(|(_, _, caret)| caret.clone())
        })
    })
    .flatten()
}

/// A display name within the activity bounds (never empty).
pub(crate) fn display_name(name: &str) -> String {
    let name = bounded_display_text(name.trim(), MAX_AGENT_NAME_BYTES);
    let name = name.trim();
    if name.is_empty() {
        "Agent".into()
    } else {
        name.to_owned()
    }
}

/// Ingest one parsed update for `agent_id` running `invocation`. Stale
/// sequences (at or below the last ingested for this invocation) are ignored.
/// Emits `Activity` on a change and `Preview` when the update carries one.
/// `initial_caret` is asked only when a preview arrives and no caret is known
/// for its target yet (the end of the committed document).
pub(crate) fn ingest(
    agent_id: &str,
    name: &str,
    invocation: Uuid,
    update: AgentLiveUpdate,
    initial_caret: impl FnOnce(&str, &str) -> Option<Caret>,
) {
    let name = display_name(name);
    let Some((changed, known_caret)) = with_registry(|registry| {
        let state = entry(registry, agent_id);
        if state.invocation != Some(invocation) {
            *state = AgentLiveState {
                caret: state.caret.take(),
                ..AgentLiveState::default()
            };
            state.invocation = Some(invocation);
        } else if update.seq <= state.last_seq {
            return None;
        }
        state.last_seq = update.seq;
        state.name = name.clone();
        state.updated = Some(Instant::now());
        state.updated_at_ms = wall_ms();
        state.ended = None;
        if let Some(reply) = &update.reply {
            state.reply_partial = (!reply.text.is_empty()).then(|| reply.text.clone());
        }
        let activity = (
            update.state.clone(),
            update.label.clone(),
            update.tool.clone(),
        );
        let changed = state.activity.as_ref() != Some(&activity);
        state.activity = Some(activity);
        let known = update.preview.as_ref().and_then(|preview| {
            state
                .caret
                .as_ref()
                .filter(|(a, r, _)| *a == preview.app && *r == preview.record)
                .map(|(_, _, caret)| caret.clone())
        });
        Some((changed, known))
    })
    .flatten() else {
        return;
    };
    if changed {
        emit(AgentLiveEvent::Activity {
            agent_id: agent_id.to_owned(),
            name: name.clone(),
            state: update.state.clone(),
            label: update.label.clone(),
            tool: update.tool.clone(),
        });
    }
    if let Some(preview) = update.preview {
        let caret = known_caret.or_else(|| {
            let caret = initial_caret(&preview.app, &preview.record)?;
            remember_caret(agent_id, &preview.app, &preview.record, &caret);
            Some(caret)
        });
        emit(AgentLiveEvent::Preview {
            agent_id: agent_id.to_owned(),
            name,
            app: preview.app,
            record: preview.record,
            pending_text: preview.text,
            caret,
        });
    }
}

fn remember_caret(agent_id: &str, app: &str, record: &str, caret: &Caret) {
    with_registry(|registry| {
        entry(registry, agent_id).caret = Some((app.to_owned(), record.to_owned(), caret.clone()));
    });
}

/// The owner-read chat message of an in-progress reply: id
/// `live-reply-<agent>` (only when that id is a valid opaque id), role agent,
/// status sent. `None` when there is no live reply for exactly `invocation`.
pub(crate) fn live_reply_message(
    agent_id: &str,
    invocation: &str,
) -> Option<(String, String, u64)> {
    let id = format!("live-reply-{agent_id}");
    if id.len() > qos_agent_control_protocol::MAX_OPAQUE_ID_BYTES || !id.is_ascii() {
        return None;
    }
    let (text, at_ms) = live_reply(agent_id, invocation)?;
    let text: String = text
        .chars()
        .take(qos_agent_control_protocol::MAX_TEXT_SCALARS)
        .collect();
    (!text.is_empty()).then_some((id, text, at_ms))
}

/// A committed agent document change (after its Store barrier).
pub(crate) fn committed(
    agent_id: &str,
    name: &str,
    app: &str,
    record: &str,
    canonical: Vec<u8>,
    caret: Option<Caret>,
) {
    let name = display_name(name);
    with_registry(|registry| {
        let state = entry(registry, agent_id);
        state.name = name.clone();
        state.updated = Some(Instant::now());
        state.updated_at_ms = wall_ms();
        if let Some(caret) = &caret {
            state.caret = Some((app.to_owned(), record.to_owned(), caret.clone()));
        }
    });
    emit(AgentLiveEvent::Committed {
        agent_id: agent_id.to_owned(),
        name,
        app: app.to_owned(),
        record: record.to_owned(),
        canonical,
        caret,
    });
}

/// Worker participant presence changes for one `(app, record)` worker
/// (drained with `HostWorker::take_participant_presence_changes` after each
/// pump). Only host-derived agent-task participants are published; the agent
/// identity comes from `agent_for_task(task_id)` (the host's own dispatch
/// ledger), never from anything the agent sent. Writer carets/selections
/// (`wp_block` key, byte offsets) become the agent's LIVE_V1 presence and
/// supersede the commit-derived caret; other keys are ignored.
pub(crate) fn on_participant_presence(
    app: &str,
    record: &str,
    updates: Vec<ui::host_worker::ParticipantPresenceUpdate>,
    agent_for_task: &dyn Fn(&str) -> Option<String>,
) {
    use ui::host_worker::ParticipantPresenceUpdate;
    for update in updates {
        let (participant, selection, label) = match update {
            ParticipantPresenceUpdate::Changed(presence) => {
                let selection = writer_selection(&presence.key, &presence.payload);
                if selection.is_none() {
                    continue;
                }
                (presence.participant, selection, presence.label)
            }
            ParticipantPresenceUpdate::Removed(participant) => (participant, None, None),
        };
        let Some(task) = participant.agent_task() else {
            continue;
        };
        let Some(agent_id) = agent_for_task(task) else {
            continue;
        };
        participant_presence(app, record, &agent_id, label.as_deref(), selection);
    }
}

/// `(block, start, end)` of a Writer caret/selection presence.
pub(crate) fn writer_selection(
    key: &ui::presence::PresenceKey,
    payload: &ui::collaboration::PresencePayload,
) -> Option<(String, usize, usize)> {
    use ui::{collaboration::PresencePayload, presence::PathSegment};
    let ui::presence::PresenceKey::Widget { path } = key else {
        return None;
    };
    let [PathSegment::Field(kind), PathSegment::Field(block)] = path.as_slice() else {
        return None;
    };
    if kind != "wp_block" || block.is_empty() {
        return None;
    }
    match payload {
        PresencePayload::Caret { pos } => Some((block.clone(), *pos, *pos)),
        PresencePayload::Selection { start, end } => Some((block.clone(), *start, *end)),
        _ => None,
    }
}

/// Record and publish one agent's real caret (`None` clears it).
pub(crate) fn participant_presence(
    app: &str,
    record: &str,
    agent_id: &str,
    label: Option<&str>,
    selection: Option<(String, usize, usize)>,
) {
    let caret = selection.as_ref().map(|(block, start, _)| Caret {
        block: block.clone(),
        offset: *start,
    });
    let head = selection
        .as_ref()
        .and_then(|(_, start, end)| (end != start).then_some(*end));
    let name = with_registry(|registry| {
        let state = entry(registry, agent_id);
        if state.name.is_empty()
            && let Some(label) = label
        {
            state.name = display_name(label);
        }
        match &caret {
            Some(caret) => {
                state.caret = Some((app.to_owned(), record.to_owned(), caret.clone()));
                state.real_caret = true;
            }
            None => state.caret = None,
        }
        state.name.clone()
    })
    .unwrap_or_default();
    emit(AgentLiveEvent::Presence {
        agent_id: agent_id.to_owned(),
        name,
        app: app.to_owned(),
        record: record.to_owned(),
        caret,
        head,
    });
}

/// The agent's task ended (terminal update committed).
pub(crate) fn ended(agent_id: &str, invocation: Uuid, succeeded: bool) {
    with_registry(|registry| {
        if let Some(state) = registry.get_mut(agent_id)
            && state.invocation.is_none_or(|current| current == invocation)
        {
            state.ended = Some(Instant::now());
            state.reply_partial = None;
            state.caret = None;
        }
    });
    emit(AgentLiveEvent::Ended {
        agent_id: agent_id.to_owned(),
        state: if succeeded { "done" } else { "failed" }.into(),
    });
}

/// The agent's managed process exited: its member-visible status becomes
/// `completed` (or `failed`), matching the owner's agent reads. The runtime
/// calls this when it observes the exit (lane G hook).
pub(crate) fn process_exited(agent_id: &str, failed: bool) {
    emit(AgentLiveEvent::Exited {
        agent_id: agent_id.to_owned(),
        failed,
    });
}

/// In-progress reply text of `agent_id` for exactly `invocation`, with its
/// update time (ms). Owner reads only; never on the live relay.
pub(crate) fn live_reply(agent_id: &str, invocation: &str) -> Option<(String, u64)> {
    with_registry(|registry| {
        let state = registry.get(agent_id)?;
        if state.ended.is_some() || Some(state.invocation?) != invocation.parse::<Uuid>().ok() {
            return None;
        }
        state
            .reply_partial
            .clone()
            .map(|text| (text, state.updated_at_ms))
    })
    .flatten()
}

// ---- Commit capture (Store thread) ---------------------------------------------

/// What an agent document commit produced, captured inside the Store commit.
pub(crate) struct CommitCapture {
    pub(crate) app: String,
    pub(crate) record: String,
    pub(crate) canonical: Vec<u8>,
    pub(crate) before: Option<Vec<u8>>,
    pub(crate) after: Option<Vec<u8>>,
}

thread_local! {
    static CAPTURE: RefCell<Option<Option<CommitCapture>>> = const { RefCell::new(None) };
}

/// Run `operation` with commit capture armed on this thread; returns its
/// result and whatever [`capture`] recorded.
pub(crate) fn capturing<T>(operation: impl FnOnce() -> T) -> (T, Option<CommitCapture>) {
    struct Restore(Option<Option<CommitCapture>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CAPTURE.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let restore = Restore(CAPTURE.with(|slot| slot.replace(Some(None))));
    let result = operation();
    let captured = CAPTURE.with(|slot| slot.borrow_mut().as_mut().and_then(Option::take));
    drop(restore);
    (result, captured)
}

/// Record a commit only when [`capturing`] is active on this thread (the
/// closure is not called otherwise, so ordinary commits pay nothing).
pub(crate) fn capture(make: impl FnOnce() -> Option<CommitCapture>) {
    let armed = CAPTURE.with(|slot| slot.borrow().is_some());
    if !armed {
        return;
    }
    let value = make();
    CAPTURE.with(|slot| {
        if let Some(slot) = slot.borrow_mut().as_mut() {
            *slot = value;
        }
    });
}

// ---- Writer caret -------------------------------------------------------------

/// Block texts of a Writer document, depth first in document order.
pub(crate) fn writer_blocks(bytes: &[u8]) -> Vec<(String, String)> {
    fn walk(blocks: &[qdoc::BlockView], out: &mut Vec<(String, String)>) {
        for block in blocks {
            out.push((block.id.clone(), block.text.clone()));
            walk(&block.children, out);
        }
    }
    let mut out = Vec::new();
    let Ok(doc) = automerge::AutoCommit::load(bytes) else {
        return out;
    };
    let Ok(containers) = qdoc::model::open(&doc, &automerge::ROOT) else {
        return out;
    };
    walk(
        &qdoc::model::materialize(&doc, &containers).blocks,
        &mut out,
    );
    out
}

/// The character range `after` added relative to `before` as `[start, end)`
/// (the `apps/agent/src/apply.rs` `added_range` rule).
fn added_range(before: &str, after: &str) -> Option<(usize, usize)> {
    let b: Vec<char> = before.chars().collect();
    let a: Vec<char> = after.chars().collect();
    if a.len() <= b.len() {
        return None;
    }
    let mut head = 0;
    while head < b.len() && b.get(head) == a.get(head) {
        head += 1;
    }
    let after_head = b.get(head..).unwrap_or(&[]);
    let mut tail = 0;
    for (&x, &y) in after_head.iter().rev().zip(a.iter().rev()) {
        if x != y {
            break;
        }
        tail += 1;
    }
    let end = a.len() - tail;
    (end > head).then_some((head, end))
}

/// The agent caret after a Writer commit: the end of the grown range of the
/// last block (in document order) whose text grew, as a UTF-8 byte offset.
pub(crate) fn commit_caret(before: Option<&[u8]>, after: &[u8]) -> Option<Caret> {
    let before: BTreeMap<String, String> = before
        .map(writer_blocks)
        .unwrap_or_default()
        .into_iter()
        .collect();
    writer_blocks(after)
        .into_iter()
        .filter_map(|(block, text)| {
            let prior = before.get(&block).map(String::as_str).unwrap_or("");
            let (_, end) = added_range(prior, &text)?;
            let offset = text
                .char_indices()
                .nth(end)
                .map_or(text.len(), |(at, _)| at);
            Some(Caret { block, offset })
        })
        .last()
}

/// End of the last block of a Writer document (the caret before any commit).
pub(crate) fn document_end_caret(bytes: &[u8]) -> Option<Caret> {
    writer_blocks(bytes)
        .into_iter()
        .last()
        .map(|(block, text)| Caret {
            block,
            offset: text.len(),
        })
}

// ---- Hooks -----------------------------------------------------------------------

/// Display name of `agent_id` from the owner's spawn request: the attempt's own
/// request when it is the Spawn, else the retained Spawn in the private ledger.
/// `ledger_name` looks the Spawn up (for example [`spawn_name`] on a Store
/// the caller already holds) and is asked only when nothing cheaper answers.
pub(crate) fn resolve_name(
    attempt: &crate::agent_runtime_adapter::LiveAttempt,
    ledger_name: impl FnOnce(&str) -> Option<String>,
) -> String {
    let agent_id = &attempt.ids.agent_id;
    if let Some(name) = cached_name(agent_id) {
        return name;
    }
    if let Ok(qos_agent_control_protocol::AgentRequest::Spawn { name, .. }) =
        qos_agent_control_protocol::decode_request(attempt.intent.request.as_bytes())
    {
        return display_name(&name);
    }
    ledger_name(agent_id).map_or_else(|| "Agent".into(), |name| display_name(&name))
}

/// The retained Spawn name of `agent_id` in the private ledger.
pub(crate) fn spawn_name(store: &crate::Store, agent_id: &str) -> Option<String> {
    use qos_peer_state::owner_commit::{AgentRuntimeFact, PrivateAgentState};
    let PrivateAgentState::V2(ledger) = store.state.private_agent_ledger.as_ref()? else {
        return None;
    };
    ledger
        .facts
        .values()
        .find_map(|receipt| match &receipt.change.fact {
            AgentRuntimeFact::Admitted {
                ids,
                intent,
                parent_invocation: None,
                ..
            } if ids.agent_id == agent_id => {
                match qos_agent_control_protocol::decode_request(intent.request.as_bytes()).ok()? {
                    qos_agent_control_protocol::AgentRequest::Spawn { name, .. } => Some(name),
                    _ => None,
                }
            }
            _ => None,
        })
}

/// `RuntimeOwner::progress` hook, called after the update was validated and
/// retained (no owner lock held). A `tilo-live-v1:` message is ingested;
/// anything else is ignored. The agent id is the attempt's
/// `ids.agent_id`; the name is the owner's Spawn request name.
pub(crate) fn on_progress(
    owner: &crate::agent_runtime_adapter::RuntimeOwner,
    attempt: &crate::agent_runtime_adapter::LiveAttempt,
    invocation: Uuid,
    update: &qos_action_broker::enrolled_dispatch::TaskRunUpdate,
) {
    let qos_action_broker::enrolled_dispatch::TaskRunUpdate::Progress { message } = update else {
        return;
    };
    let Some(parsed) = parse_update(message) else {
        return;
    };
    let name = resolve_name(attempt, |agent_id| {
        owner
            .store()
            .ok()
            .and_then(|store| spawn_name(&store, agent_id))
    });
    ingest(
        &attempt.ids.agent_id,
        &name,
        invocation,
        parsed,
        |app, record| {
            let store = owner.store().ok()?;
            let now = store.policy_now_ns().ok()?;
            let bytes = store.peer_at(now).ok()?.document(app, record)?;
            document_end_caret(&bytes)
        },
    );
}

/// The enrolled agent task invocation behind a worker publication: the first
/// contributor whose original custody carries an enrolled dispatch. Pure: the
/// tokens are only inspected, never validated or used as authority here.
pub(crate) fn enrolled_invocation(
    authorization: &ui::invocation_scope::DocumentAuthorization,
) -> Option<Uuid> {
    let mut found = None;
    let _ = authorization.validate_owner_contributors(|_, _, token| {
        if found.is_none()
            && let Some(dispatch) = token
                .downcast_ref::<qos_action_broker::collab_broker_capnp::OriginalInvocation>()
                .and_then(|original| original.enrolled_dispatch())
        {
            found = Some(dispatch.invocation());
        }
        Ok(())
    });
    found
}

/// `Store::commit_agent_document_checked` hook (one line, after the signed
/// `AppDocument` message was built and exactly admitted into `candidate`).
/// Records the exact canonical bytes and, for Writer, the before/after
/// documents — only while [`on_worker_commit`] armed capture on this thread,
/// so every other commit pays nothing. Never affects the commit.
pub(crate) fn capture_worker_commit(
    route: &ui::collaboration::RouteSpec,
    record: &str,
    message: &qos_peer_state::NeighbourMsg,
    before: &qos_peer_state::StatePeer,
    candidate: &qos_peer_state::StatePeer,
) {
    capture(|| {
        let writer = route.app_id == WRITER_APP;
        Some(CommitCapture {
            app: route.app_id.clone(),
            record: record.to_owned(),
            canonical: postcard::to_stdvec(message).ok()?,
            before: writer
                .then(|| before.document(&route.app_id, record))
                .flatten(),
            after: writer
                .then(|| candidate.document(&route.app_id, record))
                .flatten(),
        })
    });
}

/// `apply_worker_document` hook: runs `commit` exactly as before and returns
/// its result unchanged. When a live publisher listens and the publication's
/// contributor is an enrolled agent task, capture is armed around `commit`
/// and a successful commit emits `Committed` (exact canonical item, Writer
/// caret). Nothing here authorizes, refuses or delays the commit.
pub(crate) fn on_worker_commit<T, E>(
    owner: &crate::agent_runtime_adapter::RuntimeOwner,
    authorization: Option<&ui::invocation_scope::DocumentAuthorization>,
    store: &mut crate::Store,
    commit: impl FnOnce(&mut crate::Store) -> Result<T, E>,
) -> Result<T, E> {
    let agent = authorization
        .filter(|_| installed())
        .and_then(enrolled_invocation);
    let Some(invocation) = agent else {
        return commit(store);
    };
    let (result, captured) = capturing(|| commit(store));
    if result.is_ok()
        && let Some(captured) = captured
    {
        on_committed(owner, store, invocation, captured);
    }
    result
}

/// After an enrolled agent step's document committed: emit `Committed` with
/// the exact canonical item and, for Writer, the caret at the end of the grown
/// range.
fn on_committed(
    owner: &crate::agent_runtime_adapter::RuntimeOwner,
    store: &crate::Store,
    invocation: Uuid,
    captured: CommitCapture,
) {
    let Ok(attempt) = owner.attempt(invocation) else {
        return;
    };
    let name = resolve_name(&attempt, |agent_id| spawn_name(store, agent_id));
    let caret = (captured.app == WRITER_APP)
        .then_some(captured.after.as_deref())
        .flatten()
        .and_then(|after| commit_caret(captured.before.as_deref(), after));
    committed(
        &attempt.ids.agent_id,
        &name,
        &captured.app,
        &captured.record,
        captured.canonical,
        caret,
    );
}

/// `RuntimeOwner::update` hook, after a terminal update committed.
pub(crate) fn on_update(
    attempt: &crate::agent_runtime_adapter::LiveAttempt,
    invocation: Uuid,
    update: &qos_action_broker::enrolled_dispatch::TaskRunUpdate,
) {
    use qos_action_broker::enrolled_dispatch::TaskRunUpdate;
    let succeeded = match update {
        TaskRunUpdate::Succeeded { .. } => true,
        TaskRunUpdate::Failed { .. } | TaskRunUpdate::Cancelled { .. } => false,
        TaskRunUpdate::Accepted | TaskRunUpdate::Progress { .. } => return,
    };
    ended(&attempt.ids.agent_id, invocation, succeeded);
}

#[cfg(test)]
pub(crate) mod tests;
