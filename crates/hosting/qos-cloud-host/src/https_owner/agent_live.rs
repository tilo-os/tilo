//! Agent live publisher (lane H, design 2d): a serve-https task that consumes
//! the host agent live bus (`crate::agent_live`) and speaks for each agent in
//! its own host-fed live namespace
//! `agent_publisher(room, host, agent_id)`:
//!
//! | What | Carrier | Cadence |
//! |---|---|---|
//! | Presence (`focus` = Writer record, `selection` = caret) | `presence` datagram via [`LiveRelay::inject_presence`] | on caret/focus change, heartbeat every [`PRESENCE_HEARTBEAT`] while active, a final `focus/selection = None` frame at `Ended` |
//! | Text preview | `OpsFrame::Preview` on the Writer record | appended characters as updates arrive; the pending tail is replaced every [`PREVIEW_RESEND`] until committed |
//! | Real text | `OpsFrame::SignedChange` of the committed canonical item | right after each agent commit |
//! | Activity | `OpsFrame::Preview` on the activity record | on change, heartbeat every [`ACTIVITY_HEARTBEAT`] while active |
//!
//! Every relay call is non-blocking; a frame nobody reads is dropped. Frames
//! carry no authority: receivers re-verify signed changes through admission and
//! accept agent attribution only for the recomputed agent namespace. Only the
//! host injects into agent namespaces (no grant can name a digest publisher).
//! The private chat reply never reaches this task.
use super::live::{live_record, signed_change};
use crate::agent_live::{AgentLiveEvent, Caret, WRITER_APP};
use qos_moq::live::LiveRelay;
use qos_relay::delivery_v3::live_wire::{
    AgentActivity, LiveAnchor, LiveRecord, MAX_AGENT_LABEL_BYTES, MAX_AGENT_TOOL_BYTES,
    MAX_OBJECT_ID_BYTES, OpsFrame, PresenceFrame, PreviewOp, agent_activity_record,
    agent_publisher, bounded_display_text, room_digest,
};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
use ui::presence::{PathSegment, PresenceKey};

/// Presence heartbeat while an agent is active.
pub(crate) const PRESENCE_HEARTBEAT: Duration = Duration::from_secs(2);
/// Activity heartbeat while an agent is active (a late subscriber starts at the
/// latest group).
pub(crate) const ACTIVITY_HEARTBEAT: Duration = Duration::from_secs(1);
/// Pending preview re-send while uncommitted (browser expiry is 600 ms).
pub(crate) const PREVIEW_RESEND: Duration = Duration::from_millis(400);
/// Publisher tick.
const TICK: Duration = Duration::from_millis(100);
/// Typing cadence of the pending-text preview: the agent's not-yet-committed
/// text is revealed after its caret at this rate (characters per second), so
/// members see the words being typed rather than the whole remainder at once.
/// The real text still arrives only as forwarded signed changes.
pub(crate) const REVEAL_CHARS_PER_SECOND: u64 = 30;
/// Characters of pending text a receiver displays (`TYPING_MAX_CHARS`).
const PREVIEW_TAIL_CHARS: usize = 64;
/// A finished agent is forgotten after this long.
const FORGET_ENDED: Duration = Duration::from_secs(5);
/// A finished task's agent stays announced (status `waiting`: it can take
/// another message) with a slow activity heartbeat until its process exits,
/// at most this long.
const ENDED_ANNOUNCE: Duration = Duration::from_secs(30 * 60);
/// Activity heartbeat of an announced, finished agent (browser expiry 15 s).
const ENDED_HEARTBEAT: Duration = Duration::from_secs(5);
/// An agent with no event for this long stops heartbeating.
const IDLE_AFTER: Duration = Duration::from_secs(60);
/// Agents tracked at once.
const MAX_AGENTS: usize = 64;
const TAG_SCOPED: u8 = 1;
const TAG_SAME_SCOPE: u8 = 2;
const TAG_DIGEST: u8 = 3;

/// Where frames go; the live relay in production, a recorder in tests.
pub(crate) trait LiveSink: Send + 'static {
    fn presence(&self, room: &str, publisher: &str, frame: &[u8]) -> bool;
    fn ops(&self, room: &str, publisher: &str, record: &str, frame: &[u8]) -> bool;
}
impl LiveSink for LiveRelay {
    fn presence(&self, room: &str, publisher: &str, frame: &[u8]) -> bool {
        self.inject_presence(room, publisher, frame)
    }
    fn ops(&self, room: &str, publisher: &str, record: &str, frame: &[u8]) -> bool {
        self.inject_ops(room, publisher, record, frame)
    }
}

fn push_uvarint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// `(anchor, head)` of a Writer caret, byte-identical to the browser's
/// `live_view::encode_selection` for `PresenceKey::Widget{[Field("wp_block"),
/// Field(block)]}` and `Caret{pos: offset}`; `None` past the 64-byte bound.
pub(crate) fn caret_selection(caret: &Caret) -> Option<(LiveAnchor, LiveAnchor)> {
    range_selection(caret, caret.offset)
}

/// `(anchor, head)` of a Writer selection from `caret.offset` to `head` in
/// `caret.block` (browser `encode_selection` with `Selection{start, end}`).
pub(crate) fn range_selection(
    caret: &Caret,
    head_offset: usize,
) -> Option<(LiveAnchor, LiveAnchor)> {
    let key = PresenceKey::Widget {
        path: vec![
            PathSegment::Field("wp_block".into()),
            PathSegment::Field(caret.block.clone()),
        ],
    };
    let scope = postcard::to_allocvec(&key).ok()?;
    let offset = u64::try_from(caret.offset).ok()?;
    let mut anchor = vec![TAG_SCOPED];
    push_uvarint(&mut anchor, offset);
    anchor.extend_from_slice(&scope);
    let mut head = vec![TAG_SAME_SCOPE];
    push_uvarint(&mut head, u64::try_from(head_offset).ok()?);
    if anchor.len() > MAX_OBJECT_ID_BYTES {
        // Long (worker-minted) block ids: the digest form the browser's
        // `live_view` decodes and Writer resolves (`PresenceKey::digest_form`).
        anchor = vec![TAG_DIGEST];
        push_uvarint(&mut anchor, offset);
        anchor.extend_from_slice(&key.scope_digest()?);
    }
    (anchor.len() <= MAX_OBJECT_ID_BYTES)
        .then_some((LiveAnchor { cursor: anchor }, LiveAnchor { cursor: head }))
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Strictly increasing and restart-robust (wall clock based).
fn next_seq(last: &mut u64) -> u64 {
    *last = (*last + 1).max(wall_ms());
    *last
}

fn tail_chars(text: &str, chars: usize) -> &str {
    let count = text.chars().count();
    let skip = count.saturating_sub(chars);
    text.char_indices()
        .nth(skip)
        .map_or("", |(at, _)| &text[at..])
}

struct Pending {
    app: String,
    record: String,
    /// Latest full pending text from the agent.
    text: String,
    /// Leading characters of `text` revealed to receivers so far.
    revealed: usize,
    /// Last time `revealed` advanced (typing cadence clock).
    reveal_at: Instant,
    /// Characters this publisher believes the receivers hold for it.
    sent_chars: usize,
    sent_at: Instant,
}

fn prefix_chars(text: &str, chars: usize) -> &str {
    text.char_indices()
        .nth(chars)
        .map_or(text, |(at, _)| &text[..at])
}

struct Agent {
    name: String,
    publisher: String,
    presence_seq: u64,
    ops_seq: u64,
    /// `(state, label, tool)`.
    activity: Option<(String, String, Option<String>)>,
    active: bool,
    focus: Option<(String, String)>,
    caret: Option<Caret>,
    /// Selection head of a real (participant-view) selection.
    head: Option<usize>,
    /// The caret comes from the worker participant view; commit-derived
    /// carets no longer move it.
    real_caret: bool,
    pending: Option<Pending>,
    presence_at: Option<Instant>,
    activity_at: Option<Instant>,
    ended_at: Option<Instant>,
    /// Member-visible status (`live_wire::AGENT_STATUSES`).
    status: &'static str,
    /// The process exited: forget after `FORGET_ENDED`.
    exited_at: Option<Instant>,
    touched: Instant,
}

/// Synchronous publisher state; the task drives it with events and ticks.
pub(crate) struct Publisher<S: LiveSink> {
    sink: S,
    room: String,
    room_hex: String,
    host: [u8; 32],
    activity_record: LiveRecord,
    agents: BTreeMap<String, Agent>,
}

impl<S: LiveSink> Publisher<S> {
    /// `room` is the native room identifier, `host` the Store authority key.
    pub(crate) fn new(sink: S, room: &str, host: [u8; 32]) -> Self {
        Self {
            sink,
            room: room.to_owned(),
            room_hex: hex::encode(room_digest(room)),
            host,
            activity_record: agent_activity_record(),
            agents: BTreeMap::new(),
        }
    }

    /// Hex publisher of `agent_id` in this room.
    pub(crate) fn publisher_of(&self, agent_id: &str) -> String {
        hex::encode(agent_publisher(&self.room, self.host, agent_id))
    }

    fn agent(&mut self, agent_id: &str, name: &str, now: Instant) -> Option<&mut Agent> {
        if !self.agents.contains_key(agent_id) {
            if agent_id.is_empty() || agent_id.len() > 128 {
                return None;
            }
            if self.agents.len() >= MAX_AGENTS {
                let oldest = self
                    .agents
                    .iter()
                    .min_by_key(|(_, agent)| agent.touched)
                    .map(|(id, _)| id.clone())?;
                self.agents.remove(&oldest);
            }
            let publisher = self.publisher_of(agent_id);
            self.agents.insert(
                agent_id.to_owned(),
                Agent {
                    name: String::new(),
                    publisher,
                    presence_seq: 0,
                    ops_seq: 0,
                    activity: None,
                    active: false,
                    focus: None,
                    caret: None,
                    head: None,
                    real_caret: false,
                    pending: None,
                    presence_at: None,
                    activity_at: None,
                    ended_at: None,
                    status: "running",
                    exited_at: None,
                    touched: now,
                },
            );
        }
        let agent = self.agents.get_mut(agent_id)?;
        if !name.is_empty() {
            agent.name = name.to_owned();
        }
        agent.touched = now;
        Some(agent)
    }

    pub(crate) fn handle(&mut self, event: AgentLiveEvent, now: Instant) {
        // Content-free operator trace of caret-bearing events (no text).
        match &event {
            AgentLiveEvent::Presence {
                agent_id,
                caret,
                head,
                ..
            } => tracing::info!(
                target: "agent_live",
                agent = %agent_id,
                source = "participant",
                block_bytes = caret.as_ref().map_or(0, |c| c.block.len()),
                offset = caret.as_ref().map(|c| c.offset),
                head = *head,
                "agent live caret"
            ),
            AgentLiveEvent::Committed {
                agent_id,
                app,
                record,
                caret,
                canonical,
                ..
            } => tracing::info!(
                target: "agent_live",
                agent = %agent_id,
                source = "commit",
                // Which document the agent's commit landed in (Chat: the whole
                // channel set is one record; the item names no channel).
                app = %app,
                record = %record,
                block_bytes = caret.as_ref().map_or(0, |c| c.block.len()),
                offset = caret.as_ref().map(|c| c.offset),
                canonical_bytes = canonical.len(),
                "agent live commit forwarded"
            ),
            _ => {}
        }
        match event {
            AgentLiveEvent::Activity {
                agent_id,
                name,
                state,
                label,
                tool,
            } => {
                let Some(agent) = self.agent(&agent_id, &name, now) else {
                    return;
                };
                agent.active = !matches!(state.as_str(), "done" | "failed" | "idle");
                agent.ended_at = None;
                if agent.active {
                    agent.status = "running";
                }
                agent.activity = Some((state, label, tool));
                // Presence first: it announces the host-fed namespace, so
                // browsers subscribe before the activity frame is written.
                if agent.presence_at.is_none() {
                    self.send_presence(&agent_id, now);
                }
                self.send_activity(&agent_id, now);
            }
            AgentLiveEvent::Preview {
                agent_id,
                name,
                app,
                record,
                pending_text,
                caret,
            } => {
                let Some(agent) = self.agent(&agent_id, &name, now) else {
                    return;
                };
                agent.active = true;
                agent.ended_at = None;
                let target = (app.clone(), record.clone());
                let mut moved = agent.focus.as_ref() != Some(&target);
                if moved {
                    agent.focus = Some(target);
                    agent.caret = caret;
                } else if agent.caret.is_none() && caret.is_some() {
                    agent.caret = caret;
                    moved = true;
                }
                let needs_activity = agent.activity.is_none();
                if needs_activity {
                    agent.activity = Some(("writing".into(), "Writing in Writer".into(), None));
                }
                if moved || agent.presence_at.is_none() {
                    self.send_presence(&agent_id, now);
                }
                if needs_activity {
                    self.send_activity(&agent_id, now);
                }
                self.preview(&agent_id, app, record, pending_text, now);
            }
            AgentLiveEvent::Committed {
                agent_id,
                name,
                app,
                record,
                canonical,
                caret,
            } => {
                let Some(agent) = self.agent(&agent_id, &name, now) else {
                    return;
                };
                agent.active = true;
                agent.ended_at = None;
                if agent
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.app == app && pending.record == record)
                {
                    agent.pending = None;
                }
                // Latest wins: the participant view and commit carets both move
                // it (a worker that publishes its view only on join must not
                // pin the caret while the agent keeps typing elsewhere).
                let target = (app.clone(), record.clone());
                let refocused = agent.focus.as_ref() != Some(&target);
                let moved = (caret.is_some() && agent.caret != caret) || refocused;
                if caret.is_some() {
                    agent.focus = Some(target);
                    agent.caret = caret;
                    agent.head = None;
                } else if refocused {
                    // A commit in a record without a caret (Chat post, any
                    // non-Writer app): the agent's focus is that record.
                    // Receivers subscribe to a member's ops only for records
                    // they watch, focus themselves, or the member focuses; this
                    // announces the record so the forwarded signed changes
                    // reach every member, not only one focused on it.
                    agent.focus = Some(target);
                    agent.caret = None;
                    agent.head = None;
                }
                let publisher = agent.publisher.clone();
                let needs_activity = agent.activity.is_none();
                if needs_activity {
                    let label = if app == WRITER_APP {
                        "Writing in Writer"
                    } else {
                        "Working"
                    };
                    agent.activity = Some(("applying".into(), label.into(), None));
                }
                // Caret presence with (and a new namespace announced before)
                // the forwarded change.
                if moved || agent.presence_at.is_none() {
                    self.send_presence(&agent_id, now);
                }
                if let Some((record, frame)) = signed_change(&canonical) {
                    self.sink.ops(
                        &self.room_hex,
                        &publisher,
                        &hex::encode(record.digest()),
                        &frame,
                    );
                }
                if needs_activity {
                    self.send_activity(&agent_id, now);
                }
            }
            AgentLiveEvent::Presence {
                agent_id,
                name,
                app,
                record,
                caret,
                head,
            } => {
                let Some(agent) = self.agent(&agent_id, &name, now) else {
                    return;
                };
                let changed = agent.caret != caret
                    || agent.head != head
                    || agent.focus.as_ref() != Some(&(app.clone(), record.clone()));
                match caret {
                    Some(caret) => {
                        agent.focus = Some((app, record));
                        agent.caret = Some(caret);
                        agent.head = head;
                        agent.real_caret = true;
                    }
                    None => {
                        agent.caret = None;
                        agent.head = None;
                    }
                }
                if changed || agent.presence_at.is_none() {
                    self.send_presence(&agent_id, now);
                }
            }
            AgentLiveEvent::Ended { agent_id, state } => {
                let Some(agent) = self.agents.get_mut(&agent_id) else {
                    return;
                };
                agent.touched = now;
                agent.active = false;
                agent.ended_at = Some(now);
                agent.pending = None;
                let label = if state == "done" { "Done" } else { "Failed" };
                agent.activity = Some((state, label.into(), None));
                // Same rule as the owner's agent reads: a live process that
                // took the terminal report is waiting for another message.
                agent.status = "waiting";
                agent.focus = None;
                agent.caret = None;
                self.send_activity(&agent_id, now);
                self.send_presence(&agent_id, now);
            }
            AgentLiveEvent::Exited { agent_id, failed } => {
                let Some(agent) = self.agents.get_mut(&agent_id) else {
                    return;
                };
                agent.touched = now;
                agent.active = false;
                agent.ended_at.get_or_insert(now);
                agent.exited_at = Some(now);
                agent.pending = None;
                agent.status = if failed { "failed" } else { "completed" };
                if agent.activity.is_none() {
                    agent.activity = Some(("idle".into(), String::new(), None));
                }
                self.send_activity(&agent_id, now);
            }
        }
    }

    /// Heartbeats, pending re-sends and forgetting finished agents.
    pub(crate) fn tick(&mut self, now: Instant) {
        self.agents.retain(|_, agent| {
            agent
                .exited_at
                .is_none_or(|at| now.duration_since(at) < FORGET_ENDED)
                && agent
                    .ended_at
                    .is_none_or(|at| now.duration_since(at) < ENDED_ANNOUNCE)
        });
        let ids: Vec<String> = self.agents.keys().cloned().collect();
        for id in ids {
            let agent = &self.agents[&id];
            let due = |at: Option<Instant>, every: Duration| {
                at.is_none_or(|at| now.duration_since(at) >= every)
            };
            if !agent.active {
                // Finished (not exited): stay announced with its status.
                if agent.ended_at.is_some()
                    && agent.exited_at.is_none()
                    && due(agent.activity_at, ENDED_HEARTBEAT)
                {
                    self.send_activity(&id, now);
                }
                continue;
            }
            if now.duration_since(agent.touched) >= IDLE_AFTER {
                continue;
            }
            let due = |at: Option<Instant>, every: Duration| {
                at.is_none_or(|at| now.duration_since(at) >= every)
            };
            let presence = due(agent.presence_at, PRESENCE_HEARTBEAT);
            let activity = agent.activity.is_some() && due(agent.activity_at, ACTIVITY_HEARTBEAT);
            let reveal = agent.pending.is_some();
            let resend = agent
                .pending
                .as_ref()
                .is_some_and(|pending| now.duration_since(pending.sent_at) >= PREVIEW_RESEND);
            if presence {
                self.send_presence(&id, now);
            }
            if activity {
                self.send_activity(&id, now);
            }
            if reveal {
                self.reveal_pending(&id, now);
            }
            // A re-send is due only when the reveal did not just refresh it.
            if resend
                && self
                    .agents
                    .get(&id)
                    .and_then(|agent| agent.pending.as_ref())
                    .is_some_and(|pending| now.duration_since(pending.sent_at) >= PREVIEW_RESEND)
            {
                self.resend_pending(&id, now);
            }
        }
    }

    fn send_presence(&mut self, agent_id: &str, now: Instant) {
        let Some(agent) = self.agents.get_mut(agent_id) else {
            return;
        };
        let focus = agent
            .focus
            .clone()
            .and_then(|(app, record)| live_record(app, record));
        let selection = focus
            .as_ref()
            .filter(|record| record.app == WRITER_APP)
            .and(agent.caret.as_ref())
            .and_then(|caret| range_selection(caret, agent.head.unwrap_or(caret.offset)));
        if selection.is_none() && agent.caret.is_some() && focus.is_some() {
            tracing::info!(
                target: "agent_live",
                block_bytes = agent.caret.as_ref().map_or(0, |c| c.block.len()),
                "agent caret not publishable (not a Writer caret)"
            );
        }
        let frame = PresenceFrame {
            seq: next_seq(&mut agent.presence_seq),
            sent_at_ms: wall_ms(),
            focus,
            pointer: None,
            selection,
            drag: None,
        };
        agent.presence_at = Some(now);
        if let Ok(bytes) = frame.encode() {
            self.sink.presence(&self.room_hex, &agent.publisher, &bytes);
        }
    }

    fn send_activity(&mut self, agent_id: &str, now: Instant) {
        let Some(agent) = self.agents.get_mut(agent_id) else {
            return;
        };
        let Some((state, label, tool)) = agent.activity.clone() else {
            return;
        };
        let value = AgentActivity {
            v: 1,
            agent: agent_id.to_owned(),
            name: crate::agent_live::display_name(&agent.name),
            state,
            label: bounded_display_text(&label, MAX_AGENT_LABEL_BYTES),
            tool: tool
                .map(|tool| bounded_display_text(&tool, MAX_AGENT_TOOL_BYTES))
                .filter(|tool| !tool.is_empty()),
            at_ms: wall_ms(),
            status: Some(agent.status.to_owned()),
        };
        agent.activity_at = Some(now);
        let seq = next_seq(&mut agent.ops_seq);
        if let Ok(bytes) = value.to_frame(seq).and_then(|frame| frame.encode()) {
            self.sink.ops(
                &self.room_hex,
                &agent.publisher,
                &hex::encode(self.activity_record.digest()),
                &bytes,
            );
        }
    }

    /// Preview frame with `ops` on the agent's pending target.
    fn send_preview(&mut self, agent_id: &str, app: &str, record: &str, ops: Vec<PreviewOp>) {
        let Some(agent) = self.agents.get_mut(agent_id) else {
            return;
        };
        let Some(live) = live_record(app.to_owned(), record.to_owned()) else {
            return;
        };
        let seq = next_seq(&mut agent.ops_seq);
        let frame = OpsFrame::Preview {
            seq,
            record: live.clone(),
            ops,
        };
        if let Ok(bytes) = frame.encode() {
            self.sink.ops(
                &self.room_hex,
                &agent.publisher,
                &hex::encode(live.digest()),
                &bytes,
            );
        }
    }

    fn anchor(&self, agent_id: &str) -> LiveAnchor {
        self.agents
            .get(agent_id)
            .and_then(|agent| agent.caret.as_ref())
            .and_then(caret_selection)
            .map_or(
                LiveAnchor {
                    cursor: vec![TAG_SAME_SCOPE, 0],
                },
                |(anchor, _)| anchor,
            )
    }

    /// Replace everything receivers hold for this agent's pending text with
    /// the displayed tail (delete what was sent, insert the tail).
    fn replace_ops(
        &self,
        agent_id: &str,
        sent_chars: usize,
        text: &str,
    ) -> (Vec<PreviewOp>, usize) {
        let mut ops = Vec::new();
        if sent_chars > 0 {
            ops.push(PreviewOp::Delete {
                at: self.anchor(agent_id),
                len: u32::try_from(sent_chars).unwrap_or(u32::MAX),
            });
        }
        let tail = tail_chars(text, PREVIEW_TAIL_CHARS);
        if !tail.is_empty() {
            ops.push(PreviewOp::Insert {
                at: self.anchor(agent_id),
                text: tail.to_owned(),
            });
        }
        (ops, tail.chars().count())
    }

    fn preview(&mut self, agent_id: &str, app: String, record: String, text: String, now: Instant) {
        let previous = self
            .agents
            .get_mut(agent_id)
            .and_then(|agent| agent.pending.take());
        // Receivers show only the revealed prefix; the tick advances it at the
        // typing cadence (`REVEAL_CHARS_PER_SECOND`).
        let (ops, sent_chars, revealed, reveal_at) = match previous {
            Some(previous) if previous.app == app && previous.record == record => {
                if text == previous.text || text.starts_with(previous.text.as_str()) {
                    // Unchanged or extended (the model is still streaming it):
                    // keep what is revealed and its clocks.
                    if let Some(agent) = self.agents.get_mut(agent_id) {
                        agent.pending = Some(Pending { text, ..previous });
                    }
                    return;
                }
                // A revision that no longer extends: withdraw what was shown
                // and start revealing the new text.
                let (ops, _) = self.replace_ops(agent_id, previous.sent_chars, "");
                (ops, 0, 0, now)
            }
            Some(previous) => {
                // Another target: clear the old one, start the new one.
                let (clear, _) = self.replace_ops(agent_id, previous.sent_chars, "");
                if !clear.is_empty() {
                    self.send_preview(agent_id, &previous.app, &previous.record, clear);
                }
                (Vec::new(), 0, 0, now)
            }
            None => (Vec::new(), 0, 0, now),
        };
        if !ops.is_empty() {
            self.send_preview(agent_id, &app, &record, ops);
        }
        if !text.is_empty()
            && let Some(agent) = self.agents.get_mut(agent_id)
        {
            agent.pending = Some(Pending {
                app,
                record,
                text,
                revealed,
                reveal_at,
                sent_chars,
                sent_at: now,
            });
        }
    }

    /// Advance the revealed prefix of the pending text at the typing cadence
    /// and send only the newly revealed characters.
    fn reveal_pending(&mut self, agent_id: &str, now: Instant) {
        let Some(mut pending) = self
            .agents
            .get_mut(agent_id)
            .and_then(|agent| agent.pending.take())
        else {
            return;
        };
        let total = pending.text.chars().count();
        let elapsed = now.saturating_duration_since(pending.reveal_at).as_millis() as u64;
        let step = usize::try_from(elapsed * REVEAL_CHARS_PER_SECOND / 1000).unwrap_or(usize::MAX);
        if step > 0 && pending.revealed < total {
            let next = pending.revealed.saturating_add(step).min(total);
            let shown = prefix_chars(&pending.text, next);
            let appended = shown
                .char_indices()
                .nth(pending.revealed)
                .map_or("", |(at, _)| &shown[at..])
                .to_owned();
            if !appended.is_empty() {
                let ops = vec![PreviewOp::Insert {
                    at: self.anchor(agent_id),
                    text: appended.clone(),
                }];
                self.send_preview(agent_id, &pending.app, &pending.record, ops);
                pending.sent_chars += appended.chars().count();
                pending.sent_at = now;
            }
            pending.revealed = next;
            pending.reveal_at = now;
        } else if pending.revealed >= total {
            pending.reveal_at = now;
        }
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.pending = Some(pending);
        }
    }

    fn resend_pending(&mut self, agent_id: &str, now: Instant) {
        let Some(pending) = self
            .agents
            .get_mut(agent_id)
            .and_then(|agent| agent.pending.take())
        else {
            return;
        };
        let shown = prefix_chars(&pending.text, pending.revealed).to_owned();
        let (ops, sent_chars) = self.replace_ops(agent_id, pending.sent_chars, &shown);
        if !ops.is_empty() {
            self.send_preview(agent_id, &pending.app, &pending.record, ops);
        }
        if let Some(agent) = self.agents.get_mut(agent_id) {
            agent.pending = Some(Pending {
                sent_chars,
                sent_at: now,
                ..pending
            });
        }
    }
}

/// The running publisher task and its installed bus; dropping it stops both.
pub(crate) struct AgentLivePublisher {
    task: tokio::task::JoinHandle<()>,
    token: u64,
}

impl AgentLivePublisher {
    /// Install the bus and spawn the task. Must run inside Tokio.
    pub(crate) fn start(relay: LiveRelay, room: String, host: [u8; 32]) -> Self {
        let (receiver, token) = crate::agent_live::install();
        Self {
            task: Self::spawn(relay, room, host, receiver),
            token,
        }
    }

    /// The publisher loop over `receiver` (the installed bus in production).
    fn spawn<S: LiveSink>(
        sink: S,
        room: String,
        host: [u8; 32],
        mut receiver: tokio::sync::mpsc::Receiver<AgentLiveEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut publisher = Publisher::new(sink, &room, host);
            let mut tick = tokio::time::interval(TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    event = receiver.recv() => match event {
                        Some(event) => publisher.handle(event, Instant::now()),
                        None => break,
                    },
                    _ = tick.tick() => publisher.tick(Instant::now()),
                }
            }
        })
    }
}

impl Drop for AgentLivePublisher {
    fn drop(&mut self) {
        crate::agent_live::uninstall(self.token);
        self.task.abort();
    }
}

#[cfg(test)]
mod tests;
