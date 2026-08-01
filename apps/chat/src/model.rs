//! Pure data model for the chat workspace.
//!
//! These types are the in-memory projection of the Automerge document
//! (see [`crate::doc`]). The convergent state — channels, messages,
//! reactions, edit history, threads, attachments — lives in the CRDT;
//! this module is just the strongly-typed view the renderer consumes,
//! plus the pure helper logic (message grouping, reply counts, reaction
//! toggling) that has no business reaching into Automerge.

use serde::{Deserialize, Serialize};

/// String identifier (UUID or stable seed key). Used as Automerge map
/// keys so it must round-trip as a plain string.
pub type Id = String;

/// The local user. Authored messages + the user's own reactions are keyed
/// to this; the call host tile represents this user's camera.
pub const ME: &str = "u_me";

/// Two messages by the same author land in one visual group when they're
/// within this many seconds of each other (Slack/Discord convention).
pub const GROUP_WINDOW_SECS: i64 = 5 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelKind {
    Text,
    Voice,
}

impl ChannelKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "voice" => ChannelKind::Voice,
            _ => ChannelKind::Text,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Voice => "voice",
            ChannelKind::Text => "text",
        }
    }
    /// Channel-list glyph: `#` for text, a speaker for voice.
    pub fn glyph(self) -> &'static str {
        match self {
            ChannelKind::Text => "#",
            ChannelKind::Voice => "\u{1F50A}",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Online,
    Idle,
    Dnd,
    Offline,
}

impl Presence {
    pub fn from_str(s: &str) -> Self {
        match s {
            "idle" => Presence::Idle,
            "dnd" => Presence::Dnd,
            "offline" => Presence::Offline,
            _ => Presence::Online,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Presence::Idle => "idle",
            Presence::Dnd => "dnd",
            Presence::Offline => "offline",
            Presence::Online => "online",
        }
    }
}

/// A top-level workspace. Channels belong to exactly one workspace; the
/// left rail switches between them.
#[derive(Clone, Debug)]
pub struct Workspace {
    pub id: Id,
    pub name: String,
    /// Two-letter monogram shown in the rail.
    pub glyph: String,
    pub color_idx: usize,
    pub order: u32,
}

/// A collapsible group of channels within a workspace (Discord-style
/// "category"). Channels carry a `category` id; the special empty id `""`
/// means "uncategorised" and renders above the first named category.
#[derive(Clone, Debug)]
pub struct Category {
    pub id: Id,
    pub workspace: Id,
    pub name: String,
    pub order: u32,
    /// Collapsed categories hide their channels in the sidebar (the
    /// active channel still shows so you never lose your place).
    pub collapsed: bool,
}

#[derive(Clone, Debug)]
pub struct Channel {
    pub id: Id,
    pub workspace: Id,
    /// Owning category id, or `""` for uncategorised.
    pub category: Id,
    pub name: String,
    pub topic: String,
    pub kind: ChannelKind,
    pub order: u32,
    /// Unread top-level messages for the local peer. Derived (not stored) —
    /// filled in [`crate::app::ChatApp::reload`] from the read watermark; the
    /// active channel is always shown as 0.
    pub unread: u32,
    /// Archived channels are hidden from the sidebar to cut clutter but stay in
    /// the document (and reachable via the Ctrl+K command palette to unarchive).
    /// Convergent state — a plain scalar flag toggled by an action.
    pub archived: bool,
}

/// One person currently connected to a voice channel, with their live
/// audio state. Rendered both under the channel in the sidebar and as a
/// tile/chip in the call overlay.
#[derive(Clone, Debug)]
pub struct VoiceMember {
    pub user: Id,
    /// True while this person's mic is transmitting above the speaking
    /// threshold — drives the speaking ring in both surfaces.
    pub talking: bool,
    pub muted: bool,
    pub deafened: bool,
    /// Camera off / audio-only participant.
    pub audio_only: bool,
    /// True while this person is sharing their screen into the call (portal
    /// ScreenCast → the shared `TrackKind::Screen`). Shared truth in the CRDT so
    /// every peer reserves the featured screen tile and shows "N is sharing"
    /// the instant the share begins — before the first decoded frame arrives.
    pub sharing: bool,
    /// The peer's dialable media ticket (iroh node ticket in the live engine;
    /// a `sim://` placeholder otherwise). Published into the shared voice
    /// membership on join so every other peer can dial it directly — this map
    /// **is** the call's signalling channel, riding the same collab transport
    /// as the messages. `None` until the peer has announced one.
    pub ticket: Option<String>,
}

/// How a call's media is routed. Peer-to-peer below the mesh threshold (each
/// peer sends directly to every other); host-relay above it (peers publish to
/// one designated host that re-fans the streams), which keeps a growing call's
/// upload bandwidth flat. Chosen from the live participant count, so a call
/// transparently promotes from P2P to host-based as people join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallSession {
    Peer,
    Host,
}

impl CallSession {
    /// Mesh P2P stays practical up to a small fan-out; beyond it the call
    /// switches to a host relay.
    pub const PEER_MAX: usize = 3;

    pub fn select(participants: usize) -> Self {
        if participants <= Self::PEER_MAX {
            CallSession::Peer
        } else {
            CallSession::Host
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CallSession::Peer => "peer-to-peer",
            CallSession::Host => "host relay",
        }
    }
}

/// The designated relay host for a host-based call: the lexicographically
/// smallest member id, so every peer independently agrees who relays with **no
/// extra signalling round-trip**. `None` for an empty call.
pub fn call_host(members: &[VoiceMember]) -> Option<Id> {
    members.iter().map(|m| m.user.clone()).min()
}

#[derive(Clone, Debug)]
pub struct User {
    pub id: Id,
    pub name: String,
    pub initials: String,
    pub color_idx: usize,
    pub status: Presence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Attachment {
    /// An image projected from the P2P FUSE filesystem. `w`/`h` are the real
    /// pixel dimensions read from the file header; the bytes are decoded and
    /// painted through the shared image cache (content is fetched on first
    /// read — see `fs`), so there is no placeholder stand-in for the pixels.
    Image {
        name: String,
        path: String,
        w: u32,
        h: u32,
    },
    /// A video projected from the P2P FUSE filesystem. Rendered inline as a
    /// poster/preview card with a play affordance and the real resolution; the
    /// bytes hydrate from the FUSE mount **by reference** on demand (a full
    /// frame decode needs the media engine's decoder — see [`crate::media`] —
    /// available only in the `live-media` build), never copied into the CRDT.
    /// `w`/`h` are the pixel dimensions when known (0 when the container header
    /// wasn't probed).
    Video {
        name: String,
        path: String,
        w: u32,
        h: u32,
        size: u64,
    },
    /// A non-image file share (chip with name + size).
    File {
        name: String,
        path: String,
        size: u64,
    },
}

impl Attachment {
    /// The FUSE / filesystem path whose bytes back this attachment. Every
    /// variant references a real projected file — the gate the permissions
    /// layer checks before hydrating, and the key the image cache loads from.
    pub fn path(&self) -> &str {
        match self {
            Attachment::Image { path, .. }
            | Attachment::Video { path, .. }
            | Attachment::File { path, .. } => path,
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Attachment::Image { name, .. }
            | Attachment::Video { name, .. }
            | Attachment::File { name, .. } => name,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Reaction {
    pub emoji: String,
    pub users: Vec<Id>,
}

impl Reaction {
    pub fn count(&self) -> usize {
        self.users.len()
    }
    pub fn reacted_by(&self, user: &str) -> bool {
        self.users.iter().any(|u| u == user)
    }
}

/// One prior version of a message body, captured when the author edits.
#[derive(Clone, Debug)]
pub struct EditRecord {
    pub ts: i64,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub id: Id,
    pub channel: Id,
    pub author: Id,
    pub ts: i64,
    pub text: String,
    pub edited: bool,
    pub history: Vec<EditRecord>,
    /// `Some(parent_id)` when this message is a threaded reply.
    pub parent: Option<Id>,
    pub reactions: Vec<Reaction>,
    pub attachment: Option<Attachment>,
    /// Pinned messages are flagged in the shared document so every peer sees the
    /// same pins. A plain scalar flag toggled by an action.
    pub pinned: bool,
}

impl Message {
    pub fn is_top_level(&self) -> bool {
        self.parent.is_none()
    }
}

/// A run of consecutive top-level messages by one author shown as a single
/// avatar + name header followed by stacked bodies.
#[derive(Clone, Debug)]
pub struct MessageGroup {
    pub author: Id,
    /// Indices into the source `&[Message]` slice, in order.
    pub message_indices: Vec<usize>,
}

/// Group consecutive **top-level** messages (parent == None) by the same
/// author within [`GROUP_WINDOW_SECS`]. Threaded replies are excluded —
/// they render inside the thread panel, not the main timeline.
pub fn group_messages(messages: &[Message]) -> Vec<MessageGroup> {
    let mut groups: Vec<MessageGroup> = Vec::new();
    for (i, m) in messages.iter().enumerate() {
        if !m.is_top_level() {
            continue;
        }
        let extend = groups.last().is_some_and(|g| {
            g.author == m.author
                && messages
                    .get(*g.message_indices.last().unwrap())
                    .is_some_and(|prev| (m.ts - prev.ts).abs() <= GROUP_WINDOW_SECS)
        });
        if extend {
            groups.last_mut().unwrap().message_indices.push(i);
        } else {
            groups.push(MessageGroup {
                author: m.author.clone(),
                message_indices: vec![i],
            });
        }
    }
    groups
}

/// Number of replies whose `parent` is `parent_id`.
pub fn reply_count(messages: &[Message], parent_id: &str) -> usize {
    messages
        .iter()
        .filter(|m| m.parent.as_deref() == Some(parent_id))
        .count()
}

/// Distinct authors who replied in a thread, in first-seen order. Used to
/// render the stacked avatar cluster on the "N replies" affordance.
pub fn thread_participants(messages: &[Message], parent_id: &str) -> Vec<Id> {
    let mut seen: Vec<Id> = Vec::new();
    for m in messages {
        if m.parent.as_deref() == Some(parent_id) && !seen.contains(&m.author) {
            seen.push(m.author.clone());
        }
    }
    seen
}

/// Toggle `user`'s reaction with `emoji` on a reaction set. Adds the emoji
/// (and user) when absent, removes the user when present, and drops the
/// emoji entirely once nobody is left reacting. Returns the resulting
/// "is the user now reacting" flag. Pure — the CRDT writer mirrors this.
pub fn toggle_reaction(reactions: &mut Vec<Reaction>, emoji: &str, user: &str) -> bool {
    if let Some(pos) = reactions.iter().position(|r| r.emoji == emoji) {
        let r = &mut reactions[pos];
        if let Some(uidx) = r.users.iter().position(|u| u == user) {
            r.users.remove(uidx);
            if r.users.is_empty() {
                reactions.remove(pos);
            }
            false
        } else {
            r.users.push(user.to_string());
            true
        }
    } else {
        reactions.push(Reaction {
            emoji: emoji.to_string(),
            users: vec![user.to_string()],
        });
        true
    }
}

/// Format an epoch-seconds timestamp as a 24-hour `HH:MM` clock.
pub fn clock(ts: i64) -> String {
    let secs_in_day = ts.rem_euclid(86_400);
    let h = secs_in_day / 3600;
    let m = (secs_in_day % 3600) / 60;
    format!("{h:02}:{m:02}")
}

/// Human relative label for a message timestamp relative to `now` —
/// "Today" / "Yesterday" / "{n}d ago". Used for the date divider.
pub fn day_label(ts: i64, now: i64) -> String {
    let day = ts.div_euclid(86_400);
    let today = now.div_euclid(86_400);
    match today - day {
        d if d <= 0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        d => format!("{d} days ago"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, author: &str, ts: i64, parent: Option<&str>) -> Message {
        Message {
            id: id.into(),
            channel: "c".into(),
            author: author.into(),
            ts,
            text: "x".into(),
            edited: false,
            history: vec![],
            parent: parent.map(|s| s.to_string()),
            reactions: vec![],
            attachment: None,
            pinned: false,
        }
    }

    #[test]
    fn grouping_merges_same_author_within_window() {
        let msgs = vec![
            msg("a", "u1", 0, None),
            msg("b", "u1", 60, None),
            msg("c", "u1", 120, None),
        ];
        let groups = group_messages(&msgs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].message_indices, vec![0, 1, 2]);
    }

    #[test]
    fn grouping_splits_on_author_change_and_time_gap() {
        let msgs = vec![
            msg("a", "u1", 0, None),
            msg("b", "u2", 30, None),       // author change
            msg("c", "u2", 30 + 999, None), // gap > window
        ];
        let groups = group_messages(&msgs);
        assert_eq!(groups.len(), 3);
    }

    #[test]
    fn grouping_excludes_thread_replies() {
        let msgs = vec![
            msg("a", "u1", 0, None),
            msg("r", "u1", 10, Some("a")), // reply — not in the timeline
        ];
        let groups = group_messages(&msgs);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].message_indices, vec![0]);
    }

    #[test]
    fn reply_count_and_participants() {
        let msgs = vec![
            msg("a", "u1", 0, None),
            msg("r1", "u2", 10, Some("a")),
            msg("r2", "u3", 20, Some("a")),
            msg("r3", "u2", 30, Some("a")),
        ];
        assert_eq!(reply_count(&msgs, "a"), 3);
        assert_eq!(thread_participants(&msgs, "a"), vec!["u2", "u3"]);
    }

    #[test]
    fn toggle_reaction_adds_removes_and_prunes() {
        let mut rx: Vec<Reaction> = vec![];
        assert!(toggle_reaction(&mut rx, "👍", "u1"));
        assert_eq!(rx.len(), 1);
        assert!(toggle_reaction(&mut rx, "👍", "u2"));
        assert_eq!(rx[0].count(), 2);
        // u1 removes — emoji survives with u2.
        assert!(!toggle_reaction(&mut rx, "👍", "u1"));
        assert_eq!(rx[0].count(), 1);
        // u2 removes — emoji pruned.
        assert!(!toggle_reaction(&mut rx, "👍", "u2"));
        assert!(rx.is_empty());
    }

    #[test]
    fn clock_formats_24h() {
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(14 * 3600 + 32 * 60), "14:32");
        assert_eq!(clock(23 * 3600 + 59 * 60), "23:59");
    }

    #[test]
    fn day_label_relative() {
        let now = 100 * 86_400 + 3600;
        assert_eq!(day_label(now, now), "Today");
        assert_eq!(day_label(now - 86_400, now), "Yesterday");
        assert_eq!(day_label(now - 3 * 86_400, now), "3 days ago");
    }

    fn vm(user: &str) -> VoiceMember {
        VoiceMember {
            user: user.into(),
            talking: false,
            muted: false,
            deafened: false,
            audio_only: false,
            sharing: false,
            ticket: None,
        }
    }

    #[test]
    fn call_session_promotes_from_p2p_to_host_relay() {
        // Small calls mesh peer-to-peer; past the threshold they switch to a
        // host relay so a growing call's upload bandwidth stays flat.
        assert_eq!(CallSession::select(1), CallSession::Peer);
        assert_eq!(CallSession::select(CallSession::PEER_MAX), CallSession::Peer);
        assert_eq!(CallSession::select(CallSession::PEER_MAX + 1), CallSession::Host);
        assert_eq!(CallSession::Peer.label(), "peer-to-peer");
        assert_eq!(CallSession::Host.label(), "host relay");
    }

    #[test]
    fn call_host_is_the_deterministic_smallest_id() {
        // Every peer independently agrees on the host (smallest id) with no
        // extra signalling round-trip.
        let members = vec![vm("u_charlie"), vm("u_alice"), vm("u_bob")];
        assert_eq!(call_host(&members), Some("u_alice".to_string()));
        assert_eq!(call_host(&[]), None);
    }
}
