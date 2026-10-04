//! Automerge document model for the chat workspace.
//!
//! Every piece of convergent state lives here as a live CRDT structure so
//! two peers editing the same channel — sending, reacting, editing,
//! threading — converge without a server:
//!
//! ```text
//! root
//! ├── "seeded"       (bool)                      # one-time seed guard
//! ├── "workspaces"   (Map<wsId,  {name,glyph,color_idx,order}>)
//! ├── "categories"   (Map<catId, {workspace,name,order,collapsed}>)
//! ├── "users"        (Map<userId,{name,initials,color_idx,status}>)
//! ├── "channels"     (Map<chId,  {workspace,category,name,topic,kind,order}>)
//! ├── "messages"     (Map<chId,  List<Message>>)
//! │      Message = { id, author, ts, text(Text), rich(rich text)?, edited(bool),
//! │                  parent(Str ""), reactions(List<{emoji,user}>),
//! │                  history(List<{ts,text}>), attachment(Map? blob hash),
//! │                  work(Map{state, since, reasoning, trace}) }
//! ├── "voice"        (Map<chId,  Map<userId,{talking,muted,deafened,audio_only}>>)
//! ├── "read"         (Map<userId, Map<chId, i64>>) # each peer's read watermark
//! └── "rich_drafts"  (Map<chId, rich text>)      # per-channel composer body
//! ```
//!
//! A message's `text` is its plain text; `rich`, when present, is the body as
//! written (qdoc rich text, mentions marked). A message written before bodies
//! were rich has only `text`, and reads as that text with its mentions
//! marked. Drafts are rich text.
//!
//! A channel's **unread** count is not stored — it is derived per-peer as the
//! number of top-level messages beyond that peer's `read` watermark (advanced
//! whenever the channel is the one being viewed). "Have I read this" is per-peer
//! state, not convergent truth, so the watermark is keyed by `userId`: a shared
//! per-channel scalar would have two peers clobbering each other's badge.
//!
//! Structure (workspaces / categories / channels) is full CRUD: created,
//! renamed, deleted, reordered, and moved between categories — all as plain
//! map/scalar writes so two peers' edits converge.
//!
//! Persistence: the whole document is saved to disk (see
//! [`crate::storage`]) so messages, structure, reactions, edits, and
//! voice membership survive across runs. The save bytes are the raw
//! Automerge document — no lossy projection — so reloading is a faithful
//! restore and a future peer can merge against it.
//!
//! "Which channel I'm viewing", the open call overlay's position, and the
//! composer caret are deliberately NOT in this document — they're per-peer
//! UI state owned by [`crate::app::ChatApp`], exactly so two peers can read
//! different channels while sharing one workspace. Voice *membership* IS in
//! the document (it's shared truth: everyone sees who is in a call), while a
//! given peer's overlay layout is local.

use std::collections::HashMap;
use ui::doc::{read_bool, read_i64_coerced, read_str, read_text};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use tilo_content::ContentStore;
use uuid::Uuid;

use crate::model::{
    Attachment, Category, Channel, ChannelKind, EditRecord, Message, Presence, Reaction, User,
    VoiceMember, Workspace,
};

/// Attachments in the chat CRDT are portable content-addressed descriptors;
/// bytes are kept in the app's content store so a message never names a
/// sender-local filesystem path.
///
/// A deliberately small import ceiling. Media bytes live in the content store,
/// but accepting an arbitrary local file on the UI event path would
/// still let one click exhaust memory and room bandwidth.
pub const MAX_ATTACHMENT_BYTES: u64 = 32 * 1024 * 1024;

/// A collaborative message is a small piece of conversation, not an
/// unbounded document upload.  Keeping the durable body bounded prevents one
/// malformed/replayed action from making every peer allocate arbitrary memory
/// while still leaving room for long code snippets and detailed hand-offs.
pub const MAX_MESSAGE_TEXT_BYTES: usize = 64 * 1024;
/// Attachment captions are display metadata.  The payload is addressed by its
/// hash, so accepting an unbounded filename buys no fidelity and can make a
/// timeline or search row needlessly expensive to shape.
const MAX_ATTACHMENT_NAME_BYTES: usize = 255;
/// Image dimensions are metadata supplied by an untrusted container header;
/// keep absurd values out of layout and thumbnail paths.  This is not a claim
/// to validate pixels or decode arbitrary media safely.
const MAX_ATTACHMENT_DIMENSION: u32 = 16_384;
const MAX_MESSAGE_ID_BYTES: usize = 128;

/// Deterministic "now" for the seeded fixture so the offline screenshot is
/// stable: 2026-06-30 10:40 UTC, just before the capture clock's 10:42.
/// Runtime sends use wall-clock.
pub const NOW: i64 = 1_782_470_400;

/// A message to append, assembled by the seed or the live composer.
#[derive(Clone, Debug, Default)]
pub struct NewMessage {
    pub author: String,
    pub ts: i64,
    pub text: String,
    /// The body as written; empty stores only `text`.
    pub body: Vec<tilo_document::ClipBlock>,
    pub parent: Option<String>,
    pub reactions: Vec<(String, Vec<String>)>,
    pub attachment: Option<Attachment>,
    pub history: Vec<EditRecord>,
    pub edited: bool,
    /// Who the body refers to, already resolved to participant ids. Minted at
    /// authoring time by [`crate::mention::resolve`] and carried into the
    /// document, so a reader never has to re-parse the text.
    pub mentions: Vec<crate::mention::Mention>,
    /// The address of the thing this message is about, when it was written
    /// by selecting something. Minted by the publishing app at authoring
    /// time and carried verbatim, so it survives into the document instead
    /// of being lost at the composer.
    pub reference: Option<tilo_reference::ResourceRef>,
    /// An agent's record of what it is doing with the message this one
    /// replies to. Written by the agent at the moment it takes a summons, so
    /// the reply exists — and says so — before there is an outcome.
    pub work: Option<crate::model::AgentWork>,
}

/// Mutable call-participant fields persisted together in the voice map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "typed snapshot of the four independent call-state fields persisted atomically for one participant"
)]
pub struct VoiceState {
    pub talking: bool,
    pub muted: bool,
    pub deafened: bool,
    pub audio_only: bool,
}

pub struct ChatDoc;

/// One agent lifecycle transition is an indivisible authored claim: pairing a
/// state from one transition with the timestamp of another would invent an
/// event nobody observed. This scalar deliberately remains atomic while trace
/// rows retain their independently mergeable CRDT structure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredAgentTransition {
    state: crate::model::AgentWorkState,
    since: i64,
}

/// One row of the demo channel roster: (id, workspace, category, name, topic,
/// kind, sort order within its category).
type DemoChannelSeed = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    ChannelKind,
    u32,
);

// ---------------------------------------------------------------------------
// Low-level scalar/object accessors
// ---------------------------------------------------------------------------

fn map_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Map), o))) => Some(o),
        _ => None,
    }
}

fn list_at(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::List), o))) => Some(o),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Conflicting containers
//
// A container created on first use is a per-replica object: two peers that
// each `put_object` the same key before syncing leave a map-key conflict, and
// `doc.get` shows only the winner — the loser's whole subtree (a channel's
// first messages, a peer's read marks, a draft) disappears from view. New
// documents create these containers up front (seed / channel creation, both
// replayed deterministically), but documents written before that still carry
// lazy containers, so every read below joins *all* candidates and writes go
// to the winner. Automerge orders the candidates identically on every replica.
// ---------------------------------------------------------------------------

/// Every object of type `ty` stored under `obj[key]`, the `doc.get` winner
/// first, then the concurrent losers in Automerge's deterministic order.
fn object_candidates(doc: &AutoCommit, obj: &ObjId, key: &str, ty: ObjType) -> Vec<ObjId> {
    let mut found: Vec<ObjId> = doc
        .get_all(obj, key)
        .map(|values| {
            values
                .into_iter()
                .filter_map(|(value, id)| {
                    matches!(value, Value::Object(t) if t == ty).then_some(id)
                })
                .collect()
        })
        .unwrap_or_default();
    if let Ok(Some((Value::Object(t), winner))) = doc.get(obj, key)
        && t == ty
        && let Some(at) = found.iter().position(|id| *id == winner)
    {
        let winner = found.remove(at);
        found.insert(0, winner);
    }
    found
}

fn map_candidates(doc: &AutoCommit, obj: &ObjId, key: &str) -> Vec<ObjId> {
    object_candidates(doc, obj, key, ObjType::Map)
}

/// Every message list of `channel`, across every `messages` map candidate.
fn message_lists(doc: &AutoCommit, channel: &str) -> Vec<ObjId> {
    map_candidates(doc, &ROOT, "messages")
        .iter()
        .flat_map(|root_map| object_candidates(doc, root_map, channel, ObjType::List))
        .collect()
}

/// Where each composer's rich-text draft is kept.
const RICH_DRAFTS: &str = "rich_drafts";

/// # Panics
/// Never in practice: every caller passes `ROOT` or an object it just
/// fetched-or-created moments earlier in the same synchronous call, so
/// `put_object` on it only fails for an already-corrupted document.
#[allow(
    clippy::expect_used,
    reason = "callers only ever pass ROOT or an object fetched-or-created earlier in the same call, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
)]
fn get_or_map(doc: &mut AutoCommit, obj: &ObjId, key: &str) -> ObjId {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Map), o))) => o,
        _ => doc.put_object(obj, key, ObjType::Map).expect("put map"),
    }
}

/// # Panics
/// Never in practice: every caller passes `ROOT` or an object it just
/// fetched-or-created moments earlier in the same synchronous call, so
/// `put_object` on it only fails for an already-corrupted document.
#[allow(
    clippy::expect_used,
    reason = "callers only ever pass ROOT or an object fetched-or-created earlier in the same call, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
)]
fn get_or_list(doc: &mut AutoCommit, obj: &ObjId, key: &str) -> ObjId {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::List), o))) => o,
        _ => doc.put_object(obj, key, ObjType::List).expect("put list"),
    }
}

/// # Panics
/// Never in practice: every caller passes `ROOT` or an object it just
/// fetched-or-created moments earlier in the same synchronous call, so
/// `put_object` on it only fails for an already-corrupted document.
#[allow(
    clippy::expect_used,
    reason = "callers only ever pass ROOT or an object fetched-or-created earlier in the same call, so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
)]
fn get_or_text(doc: &mut AutoCommit, obj: &ObjId, key: &str) -> ObjId {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Text), o))) => o,
        _ => doc.put_object(obj, key, ObjType::Text).expect("put text"),
    }
}

/// A up-to-two-letter uppercase monogram for a workspace glyph, taken from the
/// initials of the first words of `name` (e.g. "Weekend Jam" → "WJ", "design" →
/// "D"). Falls back to "WS" for an empty name.
fn monogram(name: &str) -> String {
    let letters: String = name
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    if letters.is_empty() {
        "WS".to_string()
    } else {
        letters
    }
}

// ---------------------------------------------------------------------------
// Seed
// ---------------------------------------------------------------------------

impl ChatDoc {
    /// Store and verify bytes in `content`. This is deliberately
    /// document-free: callers do it on a bounded worker before `sync` authors
    /// the small hash-only `SendMessage` action.
    pub fn store_attachment_blob(content: &ContentStore, bytes: &[u8]) -> Option<String> {
        if u64::try_from(bytes.len()).ok()? > MAX_ATTACHMENT_BYTES {
            return None;
        }
        let hash = content
            .put(bytes)
            .inspect_err(|error| eprintln!("chat: couldn't store attachment bytes: {error}"))
            .ok()?;
        (content.read(&hash).as_deref() == Some(bytes)).then_some(hash)
    }

    pub fn is_seeded(doc: &AutoCommit) -> bool {
        read_bool(doc, &ROOT, "seeded").unwrap_or(false)
    }

    /// First-run scaffolding for a **real** install: just the local person and
    /// an empty workspace they own — a `#general` text channel and a `General`
    /// voice channel to start in. No fabricated people or conversation; the
    /// user creates channels, invites peers, and types. Idempotent via the
    /// `seeded` guard so adopting a peer's shared document never re-seeds.
    ///
    /// The rich demo workspace ([`Self::seed_if_empty`]) is reserved for the
    /// ephemeral `--shots`/test path — it never lands on a real user's disk.
    pub fn seed_starter(doc: &mut AutoCommit, me_id: &str, me_name: &str, me_initials: &str) {
        if Self::is_seeded(doc) {
            return;
        }
        let _ = doc.put(&ROOT, "seeded", true);
        Self::put_shared_roots(doc);

        Self::put_user(doc, me_id, me_name, me_initials, 0, Presence::Online);
        Self::put_workspace(doc, "ws_home", "My Workspace", "MW", 0, 0);
        Self::put_category(doc, "cat_home_text", "ws_home", "Channels", 0, false);
        Self::put_category(doc, "cat_home_voice", "ws_home", "Voice", 1, false);
        Self::put_channel(
            doc,
            "c_general",
            "ws_home",
            "cat_home_text",
            "general",
            "",
            ChannelKind::Text,
            0,
        );
        Self::put_channel(
            doc,
            "c_lounge",
            "ws_home",
            "cat_home_voice",
            "Lounge",
            "",
            ChannelKind::Voice,
            0,
        );
    }

    /// Populate the document with a believable demo workspace if it is
    /// empty. **Demo/screenshot only** — the live app seeds via
    /// [`Self::seed_starter`] instead. Idempotent via the `seeded` guard.
    pub fn seed_if_empty(doc: &mut AutoCommit) {
        if Self::is_seeded(doc) {
            return;
        }
        let _ = doc.put(&ROOT, "seeded", true);
        Self::put_shared_roots(doc);

        Self::seed_demo_users(doc);
        Self::seed_demo_workspaces(doc);
        Self::seed_demo_categories(doc);
        Self::seed_demo_channels(doc);
        Self::seed_demo_voice_membership(doc);
        Self::seed_demo_design_crit(doc);
        Self::seed_demo_design_crit_thread(doc, "m_concept");
        Self::seed_demo_general_welcome(doc);
    }

    fn seed_demo_users(doc: &mut AutoCommit) {
        for (id, name, initials, color, status) in [
            ("u_me", "You", "QD", 0usize, Presence::Online),
            ("u_aria", "Aria Chen", "AC", 1, Presence::Online),
            ("u_milo", "Milo Park", "MP", 2, Presence::Idle),
            ("u_sora", "Sora Vega", "SV", 3, Presence::Online),
            ("u_devon", "Devon Reyes", "DR", 5, Presence::Dnd),
            ("u_iris", "Iris Wolfe", "IW", 6, Presence::Offline),
        ] {
            Self::put_user(doc, id, name, initials, color, status);
        }
    }

    fn seed_demo_workspaces(doc: &mut AutoCommit) {
        for (id, name, glyph, color, order) in [
            ("ws_acme", "Acme Design", "AD", 0usize, 0u32),
            ("ws_tilo", "Tilo Core", "QC", 1, 1),
            ("ws_jam", "Weekend Jam", "WJ", 3, 2),
        ] {
            Self::put_workspace(doc, id, name, glyph, color, order);
        }
    }

    /// Channels live under a named category; "" is the implicit
    /// uncategorised group rendered first.
    fn seed_demo_categories(doc: &mut AutoCommit) {
        for (id, ws, name, order) in [
            ("cat_acme_main", "ws_acme", "Channels", 0u32),
            ("cat_acme_voice", "ws_acme", "Voice", 1),
            ("cat_tilo_eng", "ws_tilo", "Engineering", 0),
            ("cat_tilo_voice", "ws_tilo", "Voice", 1),
            ("cat_jam_main", "ws_jam", "Hangout", 0),
        ] {
            Self::put_category(doc, id, ws, name, order, false);
        }
    }

    /// Unread badges are not seeded — they derive from genuinely unseen
    /// messages (the `read` watermark). #general carries a welcome message
    /// the demo peer hasn't opened, so it shows a real "1" out of the box.
    fn seed_demo_channels(doc: &mut AutoCommit) {
        for (id, ws, cat, name, topic, kind, order) in Self::demo_channel_table_acme() {
            Self::put_channel(doc, id, ws, cat, name, topic, *kind, *order);
        }
        for (id, ws, cat, name, topic, kind, order) in Self::demo_channel_table_tilo_and_jam() {
            Self::put_channel(doc, id, ws, cat, name, topic, *kind, *order);
        }
    }

    /// The demo `ws_acme` workspace's channel roster: (id, workspace,
    /// category, name, topic, kind, sort order within its category). Part
    /// of [`Self::seed_demo_channels`].
    fn demo_channel_table_acme() -> &'static [DemoChannelSeed] {
        &[
            (
                "c_general",
                "ws_acme",
                "cat_acme_main",
                "general",
                "Company-wide announcements",
                ChannelKind::Text,
                0,
            ),
            (
                "c_crit",
                "ws_acme",
                "cat_acme_main",
                "design-crit",
                "Share work, get sharp feedback",
                ChannelKind::Text,
                1,
            ),
            (
                "c_frontend",
                "ws_acme",
                "cat_acme_main",
                "frontend",
                "ui toolkit + app shells",
                ChannelKind::Text,
                2,
            ),
            (
                "c_random",
                "ws_acme",
                "cat_acme_main",
                "random",
                "Off-topic & watercooler",
                ChannelKind::Text,
                3,
            ),
            (
                "c_standup",
                "ws_acme",
                "cat_acme_voice",
                "Daily Standup",
                "",
                ChannelKind::Voice,
                0,
            ),
            (
                "c_huddle",
                "ws_acme",
                "cat_acme_voice",
                "Design Huddle",
                "",
                ChannelKind::Voice,
                1,
            ),
        ]
    }

    /// The demo `ws_tilo` and `ws_jam` workspaces' channel rosters: (id,
    /// workspace, category, name, topic, kind, sort order within its
    /// category). Part of [`Self::seed_demo_channels`].
    fn demo_channel_table_tilo_and_jam() -> &'static [DemoChannelSeed] {
        &[
            (
                "c_kernel",
                "ws_tilo",
                "cat_tilo_eng",
                "kernel",
                "bootc + immutable base",
                ChannelKind::Text,
                0,
            ),
            (
                "c_compositor",
                "ws_tilo",
                "cat_tilo_eng",
                "compositor",
                "2D clusters & sandboxing",
                ChannelKind::Text,
                1,
            ),
            (
                "c_fssync",
                "ws_tilo",
                "cat_tilo_eng",
                "fs-sync",
                "iroh projection & hydration",
                ChannelKind::Text,
                2,
            ),
            (
                "c_releases",
                "ws_tilo",
                "cat_tilo_eng",
                "releases",
                "ship logs",
                ChannelKind::Text,
                3,
            ),
            (
                "c_warroom",
                "ws_tilo",
                "cat_tilo_voice",
                "War Room",
                "",
                ChannelKind::Voice,
                0,
            ),
            (
                "c_ideas",
                "ws_jam",
                "cat_jam_main",
                "ideas",
                "weekend hacks",
                ChannelKind::Text,
                0,
            ),
            (
                "c_music",
                "ws_jam",
                "cat_jam_main",
                "music",
                "now playing",
                ChannelKind::Text,
                1,
            ),
        ]
    }

    /// A lively Design Huddle: mirrors the call overlay's demo roster so the
    /// sidebar presence and the overlay agree out of the box. Aria is the
    /// active speaker.
    fn seed_demo_voice_membership(doc: &mut AutoCommit) {
        for (user, talking, muted, audio_only) in [
            ("u_aria", true, false, false),
            ("u_milo", false, true, false),
            ("u_sora", false, false, true),
        ] {
            Self::voice_join(doc, "c_huddle", user);
            Self::voice_set_state(
                doc,
                "c_huddle",
                user,
                VoiceState {
                    talking,
                    muted,
                    deafened: false,
                    audio_only,
                },
            );
        }
    }

    /// The first half of a real conversation in #design-crit.
    fn seed_demo_design_crit_early() -> Vec<(&'static str, NewMessage)> {
        let m = |off_min: i64| NOW - off_min * 60;
        vec![
            (
                "m_open",
                NewMessage {
                    author: "u_aria".into(),
                    ts: m(96),
                    text: "Morning! Pushed the new call overlay mock — pulling tiles into a 2x2 grid reads way calmer than the old filmstrip.".into(),
                    reactions: vec![("🎉".into(), vec!["u_me".into(), "u_milo".into()])],
                    ..Default::default()
                },
            ),
            (
                "m_concept",
                NewMessage {
                    author: "u_aria".into(),
                    ts: m(95),
                    text: "Here's the frame — the host tile keeps the live camera, everyone else collapses to chips when they're audio-only.".into(),
                    attachment: Some(Attachment::Image {
                        name: "call-overlay-v3.png".into(),
                        blob: crate::images::DEMO_CALL_GRID.into(),
                        w: 640,
                        h: 360,
                    }),
                    reactions: vec![
                        ("❤️".into(), vec!["u_me".into(), "u_sora".into(), "u_milo".into()]),
                        ("🔥".into(), vec!["u_devon".into()]),
                    ],
                    ..Default::default()
                },
            ),
            (
                "m_me1",
                NewMessage {
                    author: "u_me".into(),
                    ts: m(91),
                    text: "Love it. One ask: can the 'follow the host' affordance live on the host tile itself instead of the controls bar?".into(),
                    ..Default::default()
                },
            ),
            (
                "m_milo",
                NewMessage {
                    author: "u_milo".into(),
                    ts: m(84),
                    text: "+1 — and we should read every colour from the OS theme so it matches whatever the cluster is themed to.".into(),
                    reactions: vec![("👍".into(), vec!["u_me".into(), "u_aria".into()])],
                    ..Default::default()
                },
            ),
        ]
    }

    /// The second half of the #design-crit conversation.
    fn seed_demo_design_crit_late() -> Vec<(&'static str, NewMessage)> {
        let m = |off_min: i64| NOW - off_min * 60;
        vec![
            (
                "m_devon",
                NewMessage {
                    author: "u_devon".into(),
                    ts: m(70),
                    text: "Dropping the transport notes here so they don't get lost.".into(),
                    attachment: Some(Attachment::File {
                        name: "moq-over-iroh.md".into(),
                        blob: crate::images::DEMO_NOTES.into(),
                        size: 18_944,
                    }),
                    ..Default::default()
                },
            ),
            (
                "m_clip",
                NewMessage {
                    author: "u_sora".into(),
                    ts: m(64),
                    text: "Screen recording of the tile-promotion motion — this is the feel I'm after:".into(),
                    attachment: Some(Attachment::Video {
                        name: "tile-promotion.webm".into(),
                        blob: crate::images::DEMO_CLIP.into(),
                        w: 1280,
                        h: 720,
                        size: 6_815_744,
                    }),
                    reactions: vec![("🔥".into(), vec!["u_me".into(), "u_aria".into()])],
                    ..Default::default()
                },
            ),
            (
                "m_sora",
                NewMessage {
                    author: "u_sora".into(),
                    ts: m(41),
                    text: "Wired the screen-share stage — whoever presents gets a big featured tile and everyone else drops to the filmstrip beneath it. Reads really cleanly.".into(),
                    edited: true,
                    history: vec![
                        EditRecord {
                            ts: m(43),
                            text: "Wired up screen-share — the presenter gets a featured tile, everyone else goes to a filmstrip.".into(),
                        },
                    ],
                    reactions: vec![
                        ("🚀".into(), vec!["u_me".into(), "u_aria".into(), "u_milo".into(), "u_devon".into()]),
                        ("👀".into(), vec!["u_iris".into()]),
                    ],
                    ..Default::default()
                },
            ),
            (
                "m_me2",
                NewMessage {
                    author: "u_me".into(),
                    ts: m(38),
                    text: "That's the one. Starting a huddle to walk through the grid — jump in.".into(),
                    ..Default::default()
                },
            ),
        ]
    }

    fn seed_demo_design_crit(doc: &mut AutoCommit) {
        let mut convo = Self::seed_demo_design_crit_early();
        convo.extend(Self::seed_demo_design_crit_late());
        for (id, nm) in convo {
            Self::push_message_with_id(doc, "c_crit", id, nm);
        }
    }

    /// A thread hanging off the concept image (`parent_id`).
    fn seed_demo_design_crit_thread(doc: &mut AutoCommit, parent_id: &str) {
        let m = |off_min: i64| NOW - off_min * 60;
        let thread: Vec<NewMessage> = vec![
            NewMessage {
                author: "u_milo".into(),
                ts: m(93),
                text: "Should the chips show a mic-level ring when someone's talking?".into(),
                parent: Some(parent_id.into()),
                ..Default::default()
            },
            NewMessage {
                author: "u_aria".into(),
                ts: m(92),
                text: "Yeah — a thin accent ring that pulses. I'll spec the motion token.".into(),
                parent: Some(parent_id.into()),
                reactions: vec![("👍".into(), vec!["u_me".into()])],
                ..Default::default()
            },
            NewMessage {
                author: "u_me".into(),
                ts: m(90),
                text: "Perfect, that keeps the audio-only state legible.".into(),
                parent: Some(parent_id.into()),
                ..Default::default()
            },
        ];
        for nm in thread {
            Self::push_message(doc, "c_crit", nm);
        }
    }

    /// A little life in #general too.
    fn seed_demo_general_welcome(doc: &mut AutoCommit) {
        let m = |off_min: i64| NOW - off_min * 60;
        Self::push_message(
            doc,
            "c_general",
            NewMessage {
                author: "u_iris".into(),
                ts: m(220),
                text: "Welcome to Acme Design 👋 Say hello and tell us what you're working on this week.".into(),
                reactions: vec![("👋".into(), vec!["u_me".into(), "u_aria".into(), "u_milo".into()])],
                ..Default::default()
            },
        );
    }

    // -----------------------------------------------------------------------
    // Writers
    // -----------------------------------------------------------------------

    /// Create the root containers other peers write into on first use, in the
    /// seed change every peer adopts. Created lazily instead, two peers that
    /// each send the first message, mark a channel read, or start a draft
    /// before syncing would each mint their own root map and a merge would
    /// show only one of them (see "Conflicting containers" above).
    fn put_shared_roots(doc: &mut AutoCommit) {
        for key in ["messages", "read", RICH_DRAFTS] {
            let _ = get_or_map(doc, &ROOT, key);
        }
    }

    fn put_user(
        doc: &mut AutoCommit,
        id: &str,
        name: &str,
        initials: &str,
        color: usize,
        status: Presence,
    ) {
        let users = get_or_map(doc, &ROOT, "users");
        let u = get_or_map(doc, &users, id);
        let _ = doc.put(&u, "name", name);
        let _ = doc.put(&u, "initials", initials);
        let _ = doc.put(&u, "color_idx", color as i64);
        let _ = doc.put(&u, "status", status.as_str());
    }

    fn put_workspace(
        doc: &mut AutoCommit,
        id: &str,
        name: &str,
        glyph: &str,
        color: usize,
        order: u32,
    ) {
        let wss = get_or_map(doc, &ROOT, "workspaces");
        let w = get_or_map(doc, &wss, id);
        let _ = doc.put(&w, "name", name);
        let _ = doc.put(&w, "glyph", glyph);
        let _ = doc.put(&w, "color_idx", color as i64);
        let _ = doc.put(&w, "order", i64::from(order));
    }

    fn put_category(
        doc: &mut AutoCommit,
        id: &str,
        ws: &str,
        name: &str,
        order: u32,
        collapsed: bool,
    ) {
        let cats = get_or_map(doc, &ROOT, "categories");
        let cat = get_or_map(doc, &cats, id);
        let _ = doc.put(&cat, "workspace", ws);
        let _ = doc.put(&cat, "name", name);
        let _ = doc.put(&cat, "order", i64::from(order));
        let _ = doc.put(&cat, "collapsed", collapsed);
    }

    fn put_channel(
        doc: &mut AutoCommit,
        id: &str,
        ws: &str,
        category: &str,
        name: &str,
        topic: &str,
        kind: ChannelKind,
        order: u32,
    ) {
        let chans = get_or_map(doc, &ROOT, "channels");
        let c = get_or_map(doc, &chans, id);
        let _ = doc.put(&c, "workspace", ws);
        let _ = doc.put(&c, "category", category);
        let _ = doc.put(&c, "name", name);
        let _ = doc.put(&c, "topic", topic);
        let _ = doc.put(&c, "kind", kind.as_str());
        let _ = doc.put(&c, "order", i64::from(order));
        // Pre-create the shared, empty voice-membership map for a voice channel
        // at creation time. Without this, two peers who join the same call
        // before syncing each independently create the `voice→channel` map, and
        // an Automerge merge keeps only one — silently dropping a participant.
        // Seeding one shared map object means concurrent joins add distinct
        // user keys to the *same* map and converge to the full roster.
        if kind == ChannelKind::Voice {
            let voice = get_or_map(doc, &ROOT, "voice");
            let _ = get_or_map(doc, &voice, id);
        }
        // The same for the channel's message list and composer draft: created
        // here, inside the one (replayed, content-addressed) channel-creation
        // change, so two peers' first messages or first keystrokes land in one
        // shared list / Text instead of two conflicting ones.
        let messages = get_or_map(doc, &ROOT, "messages");
        let _ = get_or_list(doc, &messages, id);
        if Self::rich_draft_candidates(doc, id).is_empty() {
            let _ = Self::replace_rich_draft(doc, id, &[]);
        }
    }

    fn messages_list_mut(doc: &mut AutoCommit, channel: &str) -> ObjId {
        let root_map = get_or_map(doc, &ROOT, "messages");
        get_or_list(doc, &root_map, channel)
    }

    /// The message's identity scalars: id, author, timestamp, thread parent
    /// and edited flag.
    fn write_message_identity(
        doc: &mut AutoCommit,
        block: &ObjId,
        id: &str,
        author: &str,
        ts: i64,
        parent: Option<String>,
        edited: bool,
    ) {
        let _ = doc.put(block, "id", id);
        let _ = doc.put(block, "author", author);
        let _ = doc.put(block, "ts", ts);
        let _ = doc.put(block, "parent", parent.unwrap_or_default());
        let _ = doc.put(block, "edited", edited);
    }

    /// The body as written, as rich text under the message's `rich` map;
    /// nothing for an empty body (the plain text is the whole message).
    fn write_message_body(doc: &mut AutoCommit, block: &ObjId, body: &[tilo_document::ClipBlock]) {
        if body.is_empty() {
            let _ = doc.delete(block, "rich");
            return;
        }
        let Ok(root) = doc.put_object(block, "rich", ObjType::Map) else {
            return;
        };
        let written = tilo_document::RichText::initialize_under(doc, &root).is_ok()
            && tilo_rich_text::fill_clips_under(doc, &root, body).is_ok();
        if !written {
            // The plain text still stands for the message.
            let _ = doc.delete(block, "rich");
        }
    }

    /// The body as written, when the message has one.
    fn read_message_body(doc: &AutoCommit, block: &ObjId) -> Vec<ui::widget::BlockContent> {
        map_at(doc, block, "rich")
            .and_then(|root| tilo_rich_text::blocks_under(doc, &root).ok())
            .unwrap_or_default()
    }

    /// # Panics
    /// Never in practice: `block` is the freshly inserted message map this
    /// same call created a moment earlier, so `put_object` on it only fails
    /// for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "block is the freshly inserted message map this same call created a moment earlier; see push_message_with_id's doc comment"
    )]
    fn write_message_text(doc: &mut AutoCommit, block: &ObjId, text: &str) {
        let text_obj = doc.put_object(block, "text", ObjType::Text).expect("text");
        if !text.is_empty() {
            let _ = doc.splice_text(&text_obj, 0, 0, text);
        }
    }

    /// Reactions — an append-only List of {emoji,user} entries. Modelling
    /// each reaction as its own list element (rather than a Map keyed by
    /// emoji holding a per-emoji list) is what makes two peers reacting with
    /// the *same new emoji* concurrently converge: independent appends to
    /// one List both survive a merge, whereas two competing objects under
    /// the same map key would not.
    ///
    /// # Panics
    /// Never in practice: `block` is the freshly inserted message map, and
    /// `rx` is the List this call itself just created — see
    /// `push_message_with_id`'s doc comment.
    #[allow(
        clippy::expect_used,
        reason = "block is the freshly inserted message map and rx is a List this same call just created; see push_message_with_id's doc comment"
    )]
    fn write_reactions(doc: &mut AutoCommit, block: &ObjId, reactions: &[(String, Vec<String>)]) {
        let rx = doc
            .put_object(block, "reactions", ObjType::List)
            .expect("rx");
        let mut ri = 0usize;
        for (emoji, users) in reactions {
            for u in users {
                let e = doc.insert_object(&rx, ri, ObjType::Map).expect("rxentry");
                let _ = doc.put(&e, "emoji", emoji.as_str());
                let _ = doc.put(&e, "user", u.as_str());
                ri += 1;
            }
        }
    }

    /// Mentions — an ordered List of resolved participant references. A list
    /// (not a set) because two mentions of one person are two references,
    /// and the order is the order they read in the body.
    ///
    /// # Panics
    /// Never in practice: `block` is the freshly inserted message map, and
    /// `mentions_obj` is the List this call itself just created — see
    /// `push_message_with_id`'s doc comment.
    #[allow(
        clippy::expect_used,
        reason = "block is the freshly inserted message map and mentions_obj is a List this same call just created; see push_message_with_id's doc comment"
    )]
    fn write_mentions(doc: &mut AutoCommit, block: &ObjId, mentions: &[crate::mention::Mention]) {
        let mentions_obj = doc
            .put_object(block, "mentions", ObjType::List)
            .expect("mentions");
        for (i, m) in mentions.iter().enumerate() {
            let e = doc
                .insert_object(&mentions_obj, i, ObjType::Map)
                .expect("mention");
            let _ = doc.put(&e, "user", m.user.as_str());
            let _ = doc.put(&e, "start", m.start as i64);
            let _ = doc.put(&e, "len", m.len as i64);
            let _ = doc.put(&e, "agent", m.is_agent);
        }
    }

    /// Reference — the address of what this message is about.
    ///
    /// This remains one JSON scalar by design: it is an immutable, opaque
    /// address minted once at authoring time. Its constituent fields are not
    /// independently editable facts, so representing them as mergeable fields
    /// would permit a hybrid address that no sender ever selected.
    fn write_reference(
        doc: &mut AutoCommit,
        block: &ObjId,
        reference: Option<&tilo_reference::ResourceRef>,
    ) {
        if let Some(r) = reference
            && let Ok(json) = serde_json::to_string(r)
        {
            let _ = doc.put(block, "reference", json.as_str());
        }
    }

    /// Write an agent record as a structured CRDT map. Authored compound values
    /// (the state transition and reasoning) are atomic scalars, while each trace
    /// row has a stable key so concurrent appends merge without one whole
    /// record erasing the other.
    /// Reasoning is deliberately one atomic scalar: `route` and `why` are one
    /// assertion, and merging them independently could manufacture a pairing
    /// no agent authored. Trace rows are keyed by an authored step id, never a
    /// positional index, and trace updates are additive.
    fn write_work(doc: &mut AutoCommit, block: &ObjId, work: &crate::model::AgentWork) {
        // A state update is often accompanied by an unchanged trace or
        // reasoning record. Do not rewrite those independent fields: doing so
        // would turn an unrelated concurrent change into a competing LWW write.
        let prior =
            map_at(doc, block, "work").and_then(|record| Self::read_native_work(doc, &record));
        let record = get_or_map(doc, block, "work");
        if prior
            .as_ref()
            .is_none_or(|old| old.since != work.since || old.state != work.state)
            && let Ok(transition) = serde_json::to_string(&StoredAgentTransition {
                state: work.state.clone(),
                since: work.since,
            })
        {
            let _ = doc.put(&record, "transition", transition.as_str());
        }

        if prior
            .as_ref()
            .is_none_or(|old| old.reasoning != work.reasoning)
            && let Ok(reasoning) = serde_json::to_string(&work.reasoning)
        {
            let _ = doc.put(&record, "reasoning", reasoning.as_str());
        }

        let trace = get_or_map(doc, &record, "trace");
        let prior_steps: HashMap<_, _> = prior
            .as_ref()
            .map(|old| {
                old.trace
                    .iter()
                    .map(|step| (step.id.as_str(), step))
                    .collect()
            })
            .unwrap_or_default();
        for step in work.trace.iter().filter(|step| !step.id.is_empty()) {
            let prior_step = prior_steps.get(step.id.as_str()).copied();
            if prior_step == Some(step) {
                continue;
            }
            let row_exists = map_at(doc, &trace, &step.id).is_some();
            let row = get_or_map(doc, &trace, &step.id);
            let _ = doc.put(&row, "present", true);
            // Identity and authoring order are immutable once a row exists.
            // Later updates to the same step may change only its lifecycle
            // outcome, avoiding a hybrid dispatch description.
            if !row_exists {
                let _ = doc.put(&row, "sequence", step.sequence as i64);
                let _ = doc.put(&row, "resource", step.resource.as_str());
                let _ = doc.put(&row, "label", step.label.as_str());
            }
            if prior_step.is_none_or(|old| old.outcome != step.outcome)
                && let Ok(outcome) = serde_json::to_string(&step.outcome)
            {
                let _ = doc.put(&row, "outcome", outcome.as_str());
            }
        }
        // Traces are dispatch evidence: an update whose payload predates a
        // collaborator's append cannot know that row is obsolete. Therefore
        // `SetAgentWork` is additive; only an explicit future tombstone action
        // may clear a stable step id.
    }

    /// # Panics
    /// Never in practice: `block` is the freshly inserted message map, and
    /// `hist` is the List this call itself just created — see
    /// `push_message_with_id`'s doc comment.
    #[allow(
        clippy::expect_used,
        reason = "block is the freshly inserted message map and hist is a List this same call just created; see push_message_with_id's doc comment"
    )]
    fn write_history(doc: &mut AutoCommit, block: &ObjId, history: &[EditRecord]) {
        let hist = doc
            .put_object(block, "history", ObjType::List)
            .expect("hist");
        for (i, rec) in history.iter().enumerate() {
            let h = doc.insert_object(&hist, i, ObjType::Map).expect("hrec");
            let _ = doc.put(&h, "ts", rec.ts);
            let _ = doc.put(&h, "text", rec.text.as_str());
        }
    }

    /// # Panics
    /// Never in practice: `block` is the freshly inserted message map, so
    /// `put_object` on it only fails for an already-corrupted document — see
    /// `push_message_with_id`'s doc comment.
    #[allow(
        clippy::expect_used,
        reason = "block is the freshly inserted message map this same call created a moment earlier; see push_message_with_id's doc comment"
    )]
    fn write_attachment(doc: &mut AutoCommit, block: &ObjId, attachment: Option<&Attachment>) {
        let Some(att) = attachment else {
            return;
        };
        let a = doc
            .put_object(block, "attachment", ObjType::Map)
            .expect("att");
        match att {
            Attachment::Image { name, blob, w, h } => {
                let _ = doc.put(&a, "kind", "image");
                let _ = doc.put(&a, "name", name.as_str());
                let _ = doc.put(&a, "blob", blob.as_str());
                let _ = doc.put(&a, "w", i64::from(*w));
                let _ = doc.put(&a, "h", i64::from(*h));
            }
            Attachment::Video {
                name,
                blob,
                w,
                h,
                size,
            } => {
                let _ = doc.put(&a, "kind", "video");
                let _ = doc.put(&a, "name", name.as_str());
                let _ = doc.put(&a, "blob", blob.as_str());
                let _ = doc.put(&a, "w", i64::from(*w));
                let _ = doc.put(&a, "h", i64::from(*h));
                let _ = doc.put(&a, "size", *size as i64);
            }
            Attachment::File { name, blob, size } => {
                let _ = doc.put(&a, "kind", "file");
                let _ = doc.put(&a, "name", name.as_str());
                let _ = doc.put(&a, "blob", blob.as_str());
                let _ = doc.put(&a, "size", *size as i64);
            }
        }
    }

    /// Append a message with a caller-chosen id (used by the seed so the
    /// thread parent / reaction targets are stable string keys).
    ///
    /// # Panics
    /// Never in practice: every `put_object`/`insert_object` in the writer
    /// helpers this calls targets either the freshly inserted message map or
    /// a child object created a moment earlier, so it only fails for an
    /// already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "insert_object targets the message list this call just fetched-or-created, so it cannot fail short of a corrupted document — there is no meaningful fallback"
    )]
    pub fn push_message_with_id(
        doc: &mut AutoCommit,
        channel: &str,
        id: &str,
        nm: NewMessage,
    ) -> String {
        let NewMessage {
            author,
            ts,
            text,
            body,
            parent,
            reactions,
            attachment,
            history,
            edited,
            mentions,
            reference,
            work,
        } = nm;

        // A repeated stable id is harmless. New network/palette actions go
        // through `accepts_message_action` below, which also validates their
        // channel, hierarchy, text, and attachment metadata before it reaches
        // this low-level writer.
        if Self::message_id_exists(doc, id) {
            return id.to_string();
        }

        let list = Self::messages_list_mut(doc, channel);
        let idx = doc.length(&list);
        let block = doc
            .insert_object(&list, idx, ObjType::Map)
            .expect("insert msg");

        Self::write_message_identity(doc, &block, id, &author, ts, parent, edited);
        Self::write_message_text(doc, &block, &text);
        Self::write_message_body(doc, &block, &body);
        Self::write_reactions(doc, &block, &reactions);
        Self::write_mentions(doc, &block, &mentions);
        Self::write_reference(doc, &block, reference.as_ref());
        if let Some(work) = work.as_ref() {
            Self::write_work(doc, &block, work);
        }
        Self::write_history(doc, &block, &history);
        Self::write_attachment(doc, &block, attachment.as_ref());

        id.to_string()
    }

    /// Whether a new wire action is safe to add to the durable conversation.
    /// This is intentionally separate from `push_message_with_id`: an incoming
    /// action must not create an ambiguous resource,
    /// hidden channel, dangling thread edge, or unbounded display metadata.
    pub fn accepts_message_action(
        doc: &AutoCommit,
        channel: &str,
        id: &str,
        text: &str,
        parent: Option<&str>,
        attachment: Option<&Attachment>,
    ) -> bool {
        Self::channel_exists(doc, channel)
            && Self::channel_has_timeline(doc, channel)
            && Self::valid_message_id(id)
            && !Self::message_id_exists(doc, id)
            && text.len() <= MAX_MESSAGE_TEXT_BYTES
            && Self::valid_attachment(attachment)
            && Self::valid_parent(doc, channel, parent)
    }

    /// A message id belongs to one durable row across the whole chat
    /// namespace.  This makes direct resolution deterministic and makes an
    /// action replay idempotent before it reaches the list writer.
    fn message_id_exists(doc: &AutoCommit, id: &str) -> bool {
        map_candidates(doc, &ROOT, "messages").iter().any(|channels| {
            doc.keys(channels).any(|channel| {
                message_lists(doc, &channel).into_iter().any(|list| {
                    (0..doc.length(&list)).any(|index| {
                        matches!(doc.get(&list, index), Ok(Some((Value::Object(ObjType::Map), row))) if read_str(doc, &row, "id").unwrap_or_default() == id)
                    })
                })
            })
        })
    }

    /// Record `name` as the agent display name of `author`, beside the
    /// message `id` it just wrote (`agent_authors/<author> = name`). Only an
    /// agent task participant author is labelled, only when that message
    /// exists with that author, and only when the stored name differs.
    pub fn label_agent_author(doc: &mut AutoCommit, id: &str, author: &str, name: &str) {
        if !author.starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX)
            || name.trim().is_empty()
            || name.len() > crate::actions::MAX_AGENT_AUTHOR_NAME_BYTES
        {
            return;
        }
        let authored = map_candidates(doc, &ROOT, "messages").iter().any(|channels| {
            doc.keys(channels).any(|channel| {
                message_lists(doc, &channel).into_iter().any(|list| {
                    (0..doc.length(&list)).any(|index| {
                        matches!(doc.get(&list, index), Ok(Some((Value::Object(ObjType::Map), row)))
                            if read_str(doc, &row, "id").unwrap_or_default() == id && read_str(doc, &row, "author").unwrap_or_default() == author)
                    })
                })
            })
        });
        if !authored {
            return;
        }
        let agents = get_or_map(doc, &ROOT, "agent_authors");
        if read_str(doc, &agents, author).unwrap_or_default() != name {
            let _ = doc.put(&agents, author, name);
        }
    }

    /// Agent display names of agent-authored message authors.
    pub fn agent_authors(doc: &AutoCommit) -> std::collections::HashMap<String, String> {
        let Some(agents) = map_at(doc, &ROOT, "agent_authors") else {
            return std::collections::HashMap::new();
        };
        doc.keys(&agents)
            .filter(|author| author.starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX))
            .filter_map(|author| {
                let name = read_str(doc, &agents, author.as_str()).unwrap_or_default();
                (!name.trim().is_empty()).then_some((author, name))
            })
            .collect()
    }

    /// Only a text channel shows a message timeline: opening a voice channel
    /// joins the call and no view ever renders its message list. A new message
    /// there would be durable but seen by no one (an agent's post reported as
    /// sent while every member looked at `#general`), so it is refused.
    /// Rows that already exist stay readable; this gates new actions only.
    fn channel_has_timeline(doc: &AutoCommit, channel: &str) -> bool {
        map_at(doc, &ROOT, "channels")
            .and_then(|channels| map_at(doc, &channels, channel))
            .is_some_and(|row| {
                read_str(doc, &row, "kind").unwrap_or_default() != ChannelKind::Voice.as_str()
            })
    }

    fn channel_exists(doc: &AutoCommit, channel: &str) -> bool {
        map_at(doc, &ROOT, "channels")
            .and_then(|channels| map_at(doc, &channels, channel))
            .is_some()
    }

    fn valid_message_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= MAX_MESSAGE_ID_BYTES
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    }

    /// A reply can only point at a known top-level row in the same
    /// conversation. An unknown parent is accepted provisionally: independent
    /// peers may receive a causally related reply before its parent, and
    /// rejecting it would turn network ordering into permanent data loss. Once
    /// the parent arrives it resolves to the same one-hop thread everywhere.
    fn valid_parent(doc: &AutoCommit, channel: &str, parent: Option<&str>) -> bool {
        let Some(parent) = parent.filter(|parent| !parent.is_empty()) else {
            return true;
        };
        Self::find_message_obj(doc, channel, parent)
            .is_none_or(|row| read_str(doc, &row, "parent").unwrap_or_default().is_empty())
    }

    fn valid_attachment(attachment: Option<&Attachment>) -> bool {
        let Some(attachment) = attachment else {
            return true;
        };
        let valid_blob =
            |blob: &str| blob.len() == 64 && blob.bytes().all(|byte| byte.is_ascii_hexdigit());
        let valid_name = |name: &str| {
            !name.is_empty()
                && name.len() <= MAX_ATTACHMENT_NAME_BYTES
                && !name.chars().any(char::is_control)
        };
        match attachment {
            Attachment::Image { name, blob, w, h } => {
                valid_name(name)
                    && valid_blob(blob)
                    && *w > 0
                    && *h > 0
                    && *w <= MAX_ATTACHMENT_DIMENSION
                    && *h <= MAX_ATTACHMENT_DIMENSION
            }
            Attachment::Video {
                name,
                blob,
                w,
                h,
                size,
            } => {
                valid_name(name)
                    && valid_blob(blob)
                    && *size > 0
                    && *size <= MAX_ATTACHMENT_BYTES
                    && *w <= MAX_ATTACHMENT_DIMENSION
                    && *h <= MAX_ATTACHMENT_DIMENSION
            }
            Attachment::File { name, blob, size } => {
                valid_name(name) && valid_blob(blob) && *size > 0 && *size <= MAX_ATTACHMENT_BYTES
            }
        }
    }

    /// Append a message with a fresh UUID id.
    pub fn push_message(doc: &mut AutoCommit, channel: &str, nm: NewMessage) -> String {
        let id = format!("m_{}", Uuid::new_v4().simple());
        Self::push_message_with_id(doc, channel, &id, nm)
    }

    fn find_message_obj(doc: &AutoCommit, channel: &str, msg_id: &str) -> Option<ObjId> {
        for list in message_lists(doc, channel) {
            for i in 0..doc.length(&list) {
                if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
                    && read_str(doc, &m, "id").unwrap_or_default() == msg_id
                {
                    return Some(m);
                }
            }
        }
        None
    }

    /// Toggle `user`'s `emoji` reaction on a message. Creates the reactions
    /// map / emoji list as needed and prunes an emoji once its last reactor
    /// leaves — converges with the pure [`crate::model::toggle_reaction`].
    ///
    /// # Panics
    /// Never in practice: `list` was just fetched-or-created above, so
    /// `insert_object` on it only fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "list is a live List this call itself just fetched-or-created via get_or_list, so insert_object on it cannot fail short of a corrupted document — there is no meaningful fallback"
    )]
    pub fn toggle_reaction(
        doc: &mut AutoCommit,
        channel: &str,
        msg_id: &str,
        emoji: &str,
        user: &str,
    ) {
        let Some(block) = Self::find_message_obj(doc, channel, msg_id) else {
            return;
        };
        let list = get_or_list(doc, &block, "reactions");
        // Find an existing (emoji,user) entry.
        let mut found: Option<usize> = None;
        for i in 0..doc.length(&list) {
            if let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&list, i)
                && read_str(doc, &e, "emoji").unwrap_or_default() == emoji
                && read_str(doc, &e, "user").unwrap_or_default() == user
            {
                found = Some(i);
                break;
            }
        }
        if let Some(i) = found {
            let _ = doc.delete(&list, i);
        } else {
            let n = doc.length(&list);
            let e = doc.insert_object(&list, n, ObjType::Map).expect("rxentry");
            let _ = doc.put(&e, "emoji", emoji);
            let _ = doc.put(&e, "user", user);
        }
    }

    /// The index of an existing `(emoji, user)` reaction entry on `list`, or
    /// `None`. Shared by the toggle / add / remove writers so they agree on what
    /// "already reacted" means.
    fn reaction_index(doc: &AutoCommit, list: &ObjId, emoji: &str, user: &str) -> Option<usize> {
        for i in 0..doc.length(list) {
            if let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(list, i)
                && read_str(doc, &e, "emoji").unwrap_or_default() == emoji
                && read_str(doc, &e, "user").unwrap_or_default() == user
            {
                return Some(i);
            }
        }
        None
    }

    /// Add `user`'s `emoji` reaction to a message. Idempotent: a second add of
    /// the same `(emoji, user)` is a no-op, so replaying an add never
    /// double-counts. The counterpart of [`Self::remove_reaction`]; the on-screen
    /// toggle picks whichever the current state calls for at authoring time.
    ///
    /// # Panics
    /// Never in practice: `list` was just fetched-or-created above, so
    /// `insert_object` on it only fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "list is a live List this call itself just fetched-or-created via get_or_list, so insert_object on it cannot fail short of a corrupted document — there is no meaningful fallback"
    )]
    pub fn add_reaction(
        doc: &mut AutoCommit,
        channel: &str,
        msg_id: &str,
        emoji: &str,
        user: &str,
    ) {
        let Some(block) = Self::find_message_obj(doc, channel, msg_id) else {
            return;
        };
        let list = get_or_list(doc, &block, "reactions");
        if Self::reaction_index(doc, &list, emoji, user).is_some() {
            return;
        }
        let n = doc.length(&list);
        let e = doc.insert_object(&list, n, ObjType::Map).expect("rxentry");
        let _ = doc.put(&e, "emoji", emoji);
        let _ = doc.put(&e, "user", user);
    }

    /// Remove `user`'s `emoji` reaction from a message. Idempotent: removing an
    /// absent reaction is a no-op, so a bad replay can't wedge.
    pub fn remove_reaction(
        doc: &mut AutoCommit,
        channel: &str,
        msg_id: &str,
        emoji: &str,
        user: &str,
    ) {
        let Some(block) = Self::find_message_obj(doc, channel, msg_id) else {
            return;
        };
        let Some(list) = list_at(doc, &block, "reactions") else {
            return;
        };
        if let Some(i) = Self::reaction_index(doc, &list, emoji, user) {
            let _ = doc.delete(&list, i);
        }
    }

    /// Delete a message row from a channel. Idempotent — an unknown id is a
    /// no-op. Threaded replies (if any) are left in place; they simply lose their
    /// parent card, which is recoverable.
    pub fn delete_message(doc: &mut AutoCommit, channel: &str, msg_id: &str) {
        for list in message_lists(doc, channel) {
            for i in 0..doc.length(&list) {
                if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i)
                    && read_str(doc, &m, "id").unwrap_or_default() == msg_id
                {
                    let _ = doc.delete(&list, i);
                    return;
                }
            }
        }
    }

    /// Set (or clear) a message's pinned flag. Shared truth: every peer sees the
    /// same pins. Guarded so a redundant write doesn't churn the document.
    pub fn set_message_pinned(doc: &mut AutoCommit, channel: &str, msg_id: &str, pinned: bool) {
        let Some(block) = Self::find_message_obj(doc, channel, msg_id) else {
            return;
        };
        if read_bool(doc, &block, "pinned").unwrap_or(false) != pinned {
            let _ = doc.put(&block, "pinned", pinned);
        }
    }

    /// Replace a message's body, archiving the previous text into its edit
    /// history and flagging it edited.
    ///
    /// # Panics
    /// Never in practice: `hist` was just fetched-or-created above, so
    /// `insert_object` on it only fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "hist is a live List this call itself just fetched-or-created via get_or_list, so insert_object on it cannot fail short of a corrupted document — there is no meaningful fallback"
    )]
    pub fn edit_message(
        doc: &mut AutoCommit,
        channel: &str,
        msg_id: &str,
        new_text: &str,
        body: &[tilo_document::ClipBlock],
        ts: i64,
    ) {
        let Some(block) = Self::find_message_obj(doc, channel, msg_id) else {
            return;
        };
        let old = read_text(doc, &block, "text").unwrap_or_default();
        let old_body: Vec<tilo_document::ClipBlock> = map_at(doc, &block, "rich")
            .and_then(|root| tilo_doc::model::open(doc, &root).ok())
            .map(|containers| {
                tilo_doc::model::materialize_region(doc, &containers, tilo_doc::model::Region::Body)
                    .iter()
                    .map(tilo_rich_text::clip_block)
                    .collect()
            })
            .unwrap_or_default();
        if old == new_text && old_body == body {
            return;
        }
        Self::write_message_body(doc, &block, body);
        if old == new_text {
            // Reformatted, same words: no revision to archive.
            let _ = doc.put(&block, "edited", true);
            return;
        }
        let hist = get_or_list(doc, &block, "history");
        let n = doc.length(&hist);
        let h = doc.insert_object(&hist, n, ObjType::Map).expect("hrec");
        let _ = doc.put(&h, "ts", ts);
        let _ = doc.put(&h, "text", old.as_str());

        let text = get_or_text(doc, &block, "text");
        ui::doc::splice_text_diffed(doc, &text, new_text);
        let _ = doc.put(&block, "edited", true);
    }

    /// Every rich-text draft stored for `key`: the winner first, then drafts
    /// left by two peers creating the same one at once.
    fn rich_draft_candidates(doc: &AutoCommit, key: &str) -> Vec<ObjId> {
        map_candidates(doc, &ROOT, RICH_DRAFTS)
            .iter()
            .flat_map(|drafts| object_candidates(doc, drafts, key, ObjType::Map))
            .collect()
    }

    /// The rich text the composer for `key` edits, when it exists.
    pub fn rich_draft(doc: &AutoCommit, key: &str) -> Option<ObjId> {
        Self::rich_draft_candidates(doc, key).into_iter().next()
    }

    /// The rich text the composer for `key` edits, created when missing.
    /// Drafts two peers created at once are joined into one, winner first,
    /// so no one's keystrokes are hidden.
    pub fn rich_draft_is_settled(doc: &AutoCommit, key: &str) -> bool {
        Self::rich_draft_candidates(doc, key).len() == 1
    }

    pub fn ensure_rich_draft(doc: &mut AutoCommit, key: &str) -> Option<ObjId> {
        let candidates = Self::rich_draft_candidates(doc, key);
        if let [only] = candidates.as_slice() {
            return Some(only.clone());
        }
        let blocks: Vec<ui::widget::BlockContent> = Self::distinct_drafts(doc, key)
            .into_iter()
            .flat_map(|(_, blocks)| blocks)
            .filter(|block| !block.text.is_empty())
            .collect();
        Self::replace_rich_draft(doc, key, &blocks)
    }

    /// Replace the draft for `key` with `blocks` (none: an empty draft).
    fn replace_rich_draft(
        doc: &mut AutoCommit,
        key: &str,
        blocks: &[ui::widget::BlockContent],
    ) -> Option<ObjId> {
        for drafts in map_candidates(doc, &ROOT, RICH_DRAFTS) {
            let _ = doc.delete(&drafts, key);
        }
        let drafts = get_or_map(doc, &ROOT, RICH_DRAFTS);
        let root = doc.put_object(&drafts, key, ObjType::Map).ok()?;
        tilo_document::RichText::initialize_under(doc, &root).ok()?;
        let blocks: Vec<ui::widget::BlockContent> = blocks
            .iter()
            .map(|block| ui::widget::BlockContent {
                id: String::new(),
                ..block.clone()
            })
            .collect();
        tilo_rich_text::fill_under(doc, &root, &blocks).ok()?;
        Some(root)
    }

    /// The draft for `key` as written: every draft stored for it, the same
    /// text twice (two peers joining the same drafts at once) once.
    pub fn draft_blocks(doc: &AutoCommit, key: &str) -> Vec<ui::widget::BlockContent> {
        Self::distinct_drafts(doc, key)
            .into_iter()
            .flat_map(|(_, blocks)| blocks)
            .collect()
    }

    fn distinct_drafts(doc: &AutoCommit, key: &str) -> Vec<(ObjId, Vec<ui::widget::BlockContent>)> {
        let mut seen: Vec<String> = Vec::new();
        Self::rich_draft_candidates(doc, key)
            .into_iter()
            .filter_map(|root| {
                let blocks = tilo_rich_text::blocks_under(doc, &root).unwrap_or_default();
                let text = crate::body::plain_text(&blocks);
                if seen.contains(&text) {
                    return None;
                }
                seen.push(text);
                Some((root, blocks))
            })
            .collect()
    }

    /// The draft for `key` as an action carries it.
    pub fn draft_clips(doc: &AutoCommit, key: &str) -> Vec<tilo_document::ClipBlock> {
        Self::distinct_drafts(doc, key)
            .iter()
            .filter_map(|(root, _)| tilo_doc::model::open(doc, root).ok())
            .flat_map(|containers| {
                tilo_doc::model::materialize_region(doc, &containers, tilo_doc::model::Region::Body)
            })
            .map(|view| tilo_rich_text::clip_block(&view))
            .collect()
    }

    /// The draft's plain text, a line per paragraph.
    pub fn draft_text(doc: &AutoCommit, key: &str) -> String {
        crate::body::plain_text(&Self::draft_blocks(doc, key))
    }

    /// Replace the draft with `text` as one paragraph (line breaks inside
    /// it), as if typed: a staged draft, a test.
    pub fn set_draft(doc: &mut AutoCommit, key: &str, text: &str) {
        let blocks = if text.is_empty() {
            Vec::new()
        } else {
            vec![ui::widget::BlockContent::paragraph("", text)]
        };
        let _ = Self::replace_rich_draft(doc, key, &blocks);
    }

    /// Empty the draft once it is sent.
    pub fn clear_draft(doc: &mut AutoCommit, key: &str) {
        let blocks = Self::draft_blocks(doc, key);
        let empty = blocks.len() <= 1 && blocks.iter().all(|block| block.text.is_empty());
        if empty && Self::rich_draft_candidates(doc, key).len() == 1 {
            return;
        }
        let _ = Self::replace_rich_draft(doc, key, &[]);
    }

    // -----------------------------------------------------------------------
    // Read state / unread badges
    //
    // "Have I read this channel" is per-peer, so it is NOT a shared scalar.
    // Each peer keeps a `read` watermark per channel — the count of top-level
    // messages it had seen the last time it viewed the channel — nested under
    // the peer's own user id so two peers never clobber each other. Unread is
    // then derived: (top-level count − watermark), floored at zero.
    // -----------------------------------------------------------------------

    /// Number of top-level (non-threaded) messages in `channel`.
    pub fn top_level_count(doc: &AutoCommit, channel: &str) -> usize {
        Self::list_messages(doc, channel)
            .iter()
            .filter(|m| m.is_top_level())
            .count()
    }

    /// Guarded upsert of a user record: writes each field only when it differs
    /// from what's stored, so registering the local peer every frame doesn't
    /// churn the document (which would spew empty collab deltas). Returns true
    /// iff it wrote. This is how a peer that just adopted someone else's shared
    /// document announces itself into the roster so others see its name/colour.
    pub fn ensure_user(
        doc: &mut AutoCommit,
        id: &str,
        name: &str,
        initials: &str,
        color: usize,
        status: Presence,
    ) -> bool {
        let users = get_or_map(doc, &ROOT, "users");
        let u = get_or_map(doc, &users, id);
        let mut wrote = false;
        if read_str(doc, &u, "name").unwrap_or_default() != name {
            let _ = doc.put(&u, "name", name);
            wrote = true;
        }
        if read_str(doc, &u, "initials").unwrap_or_default() != initials {
            let _ = doc.put(&u, "initials", initials);
            wrote = true;
        }
        if read_i64_coerced(doc, &u, "color_idx").unwrap_or(0) != color as i64 {
            let _ = doc.put(&u, "color_idx", color as i64);
            wrote = true;
        }
        if read_str(doc, &u, "status").unwrap_or_default() != status.as_str() {
            let _ = doc.put(&u, "status", status.as_str());
            wrote = true;
        }
        wrote
    }

    /// Whether the roster row already matches this peer. Does not create maps.
    pub fn user_is_current(
        doc: &AutoCommit,
        id: &str,
        name: &str,
        initials: &str,
        color: usize,
        status: Presence,
    ) -> bool {
        let Some(users) = map_at(doc, &ROOT, "users") else {
            return false;
        };
        let Some(user) = map_at(doc, &users, id) else {
            return false;
        };
        read_str(doc, &user, "name").unwrap_or_default() == name
            && read_str(doc, &user, "initials").unwrap_or_default() == initials
            && read_i64_coerced(doc, &user, "color_idx").unwrap_or(0) == color as i64
            && read_str(doc, &user, "status").unwrap_or_default() == status.as_str()
    }

    /// The peer's `read` watermark map (channel → count), created on demand
    /// under its own user id. Keyed by `user` so two peers never clobber each
    /// other's unread badges.
    fn read_map(doc: &mut AutoCommit, user: &str) -> ObjId {
        let read = get_or_map(doc, &ROOT, "read");
        get_or_map(doc, &read, user)
    }

    /// A peer's read watermark for `channel` (0 if never opened).
    pub fn read_watermark(doc: &AutoCommit, channel: &str, user: &str) -> i64 {
        // The winner first (where `read_map` writes), then any concurrently
        // created `read` map that still holds this peer's marks.
        map_candidates(doc, &ROOT, "read")
            .iter()
            .filter_map(|r| map_at(doc, r, user))
            .find_map(|m| read_i64_coerced(doc, &m, channel))
            .unwrap_or(0)
    }

    /// Unread top-level messages in `channel` for `user`.
    pub fn unread_count(doc: &AutoCommit, channel: &str, user: &str) -> u32 {
        let total = Self::top_level_count(doc, channel) as i64;
        (total - Self::read_watermark(doc, channel, user)).max(0) as u32
    }

    /// Mark `channel` read up to the present: advance `user`'s watermark to the
    /// current top-level count. Idempotent — only writes when the watermark
    /// actually moves, so viewing a channel doesn't churn the document every
    /// frame.
    pub fn mark_read(doc: &mut AutoCommit, channel: &str, user: &str) {
        let total = Self::top_level_count(doc, channel) as i64;
        Self::set_read_watermark(doc, channel, user, total);
    }

    /// Set `user`'s read watermark for `channel` to an explicit `count`. The
    /// action-model entry point for "Mark as read": the count is minted at
    /// authoring time and carried in the payload, so replay never re-reads live
    /// state. Guarded so a redundant write doesn't churn the document.
    pub fn set_read_watermark(doc: &mut AutoCommit, channel: &str, user: &str, count: i64) {
        let m = Self::read_map(doc, user);
        if read_i64_coerced(doc, &m, channel).unwrap_or(0) != count {
            let _ = doc.put(&m, channel, count);
        }
    }

    // -----------------------------------------------------------------------
    // Structure CRUD — categories & channels
    //
    // Every structural edit is an idempotent set of scalar writes against a
    // map keyed by a stable id, so two peers reordering or renaming converge
    // without a server. Ordering uses a dense `order` integer; reorders
    // renumber the affected siblings.
    // -----------------------------------------------------------------------

    /// Next free `order` for a category in `ws` (max + 1).
    fn next_category_order(doc: &AutoCommit, ws: &str) -> u32 {
        Self::list_categories(doc)
            .into_iter()
            .filter(|c| c.workspace == ws)
            .map(|c| c.order)
            .max()
            .map_or(0, |m| m + 1)
    }

    /// Next free `order` for a channel within `(ws, category)`.
    fn next_channel_order(doc: &AutoCommit, ws: &str, category: &str) -> u32 {
        Self::list_channels(doc)
            .into_iter()
            .filter(|c| c.workspace == ws && c.category == category)
            .map(|c| c.order)
            .max()
            .map_or(0, |m| m + 1)
    }

    /// Create a category at the end of `ws`. Returns its fresh id.
    pub fn create_category(doc: &mut AutoCommit, ws: &str, name: &str) -> String {
        let id = format!("cat_{}", Uuid::new_v4().simple());
        Self::insert_category(doc, &id, ws, name);
        id
    }

    /// Create a category with a caller-chosen stable id.  Authoring owns id
    /// generation; deriving the end-of-workspace order from the replay base
    /// keeps the replicated action deterministic.
    pub fn insert_category(doc: &mut AutoCommit, id: &str, ws: &str, name: &str) {
        let order = Self::next_category_order(doc, ws);
        Self::put_category(doc, id, ws, name, order, false);
    }

    /// Create a workspace at the end of the rail, scaffolded with a minimal
    /// usable structure — a `Channels` category holding a `general` text
    /// channel and a `Voice` category holding a `Lounge` voice channel — so
    /// switching to it lands on a real channel and the header "Call" button
    /// has somewhere to go. Returns the new workspace id.
    pub fn create_workspace(doc: &mut AutoCommit, name: &str) -> String {
        let id = format!("ws_{}", Uuid::new_v4().simple());
        let order = Self::list_workspaces(doc)
            .into_iter()
            .map(|w| w.order)
            .max()
            .map_or(0, |m| m + 1);
        Self::put_workspace(doc, &id, name, &monogram(name), order as usize, order);
        let text_cat = Self::create_category(doc, &id, "Channels");
        Self::create_channel(doc, &id, &text_cat, "general", ChannelKind::Text);
        let voice_cat = Self::create_category(doc, &id, "Voice");
        Self::create_channel(doc, &id, &voice_cat, "Lounge", ChannelKind::Voice);
        id
    }

    /// Create a channel at the end of its category. Returns its fresh id.
    pub fn create_channel(
        doc: &mut AutoCommit,
        ws: &str,
        category: &str,
        name: &str,
        kind: ChannelKind,
    ) -> String {
        let id = format!("c_{}", Uuid::new_v4().simple());
        Self::insert_channel(doc, &id, ws, category, name, kind);
        id
    }

    /// Create a channel with a **caller-chosen** id at the end of its category.
    /// The order is derived from the document's current state, so the mint of the
    /// id happens once at authoring time (carried in the action payload) while
    /// replay stays deterministic. A no-op if the id already exists is not
    /// enforced — an action mints a fresh id, and `put_channel` is an idempotent
    /// upsert regardless.
    pub fn insert_channel(
        doc: &mut AutoCommit,
        id: &str,
        ws: &str,
        category: &str,
        name: &str,
        kind: ChannelKind,
    ) {
        let order = Self::next_channel_order(doc, ws, category);
        Self::put_channel(doc, id, ws, category, name, "", kind, order);
    }

    /// Set (or clear) a channel's archived flag. Archived channels drop out of
    /// the sidebar but stay in the document. Guarded so a redundant write doesn't
    /// churn the document.
    pub fn set_channel_archived(doc: &mut AutoCommit, id: &str, archived: bool) {
        if let Some(c) = map_at(doc, &map_at(doc, &ROOT, "channels").unwrap_or(ROOT), id)
            && read_bool(doc, &c, "archived").unwrap_or(false) != archived
        {
            let _ = doc.put(&c, "archived", archived);
        }
    }

    pub fn rename_channel(doc: &mut AutoCommit, id: &str, name: &str) {
        if let Some(c) = map_at(doc, &map_at(doc, &ROOT, "channels").unwrap_or(ROOT), id) {
            let _ = doc.put(&c, "name", name);
        }
    }

    pub fn set_channel_topic(doc: &mut AutoCommit, id: &str, topic: &str) {
        if let Some(c) = map_at(doc, &map_at(doc, &ROOT, "channels").unwrap_or(ROOT), id) {
            let _ = doc.put(&c, "topic", topic);
        }
    }

    pub fn rename_category(doc: &mut AutoCommit, id: &str, name: &str) {
        if let Some(c) = map_at(doc, &map_at(doc, &ROOT, "categories").unwrap_or(ROOT), id) {
            let _ = doc.put(&c, "name", name);
        }
    }

    pub fn set_category_collapsed(doc: &mut AutoCommit, id: &str, collapsed: bool) {
        if let Some(c) = map_at(doc, &map_at(doc, &ROOT, "categories").unwrap_or(ROOT), id) {
            let _ = doc.put(&c, "collapsed", collapsed);
        }
    }

    pub fn toggle_category_collapsed(doc: &mut AutoCommit, id: &str) {
        let collapsed = Self::list_categories(doc)
            .into_iter()
            .find(|c| c.id == id)
            .is_some_and(|c| c.collapsed);
        Self::set_category_collapsed(doc, id, !collapsed);
    }

    /// Delete a channel and its messages, voice membership, and draft.
    pub fn delete_channel(doc: &mut AutoCommit, id: &str) {
        if let Some(chans) = map_at(doc, &ROOT, "channels") {
            let _ = doc.delete(&chans, id);
        }
        for msgs in map_candidates(doc, &ROOT, "messages") {
            let _ = doc.delete(&msgs, id);
        }
        if let Some(voice) = map_at(doc, &ROOT, "voice") {
            let _ = doc.delete(&voice, id);
        }
        for drafts in map_candidates(doc, &ROOT, RICH_DRAFTS) {
            let _ = doc.delete(&drafts, id);
        }
    }

    /// Delete a category. Its channels are moved to the workspace's
    /// uncategorised group (`""`) rather than destroyed, so a misclick is
    /// recoverable.
    pub fn delete_category(doc: &mut AutoCommit, id: &str) {
        let channels: Vec<Channel> = Self::list_channels(doc)
            .into_iter()
            .filter(|c| c.category == id)
            .collect();
        for ch in channels {
            Self::move_channel(doc, &ch.id, "");
        }
        if let Some(cats) = map_at(doc, &ROOT, "categories") {
            let _ = doc.delete(&cats, id);
        }
    }

    /// Move a channel into `category` (within its own workspace), appending
    /// it at the end of the target group.
    pub fn move_channel(doc: &mut AutoCommit, id: &str, category: &str) {
        let Some(ch) = Self::list_channels(doc).into_iter().find(|c| c.id == id) else {
            return;
        };
        let order = Self::next_channel_order(doc, &ch.workspace, category);
        if let Some(c) = map_at(doc, &map_at(doc, &ROOT, "channels").unwrap_or(ROOT), id) {
            let _ = doc.put(&c, "category", category);
            let _ = doc.put(&c, "order", i64::from(order));
        }
    }

    /// Move a channel one slot up/down within its category by swapping the
    /// `order` of the adjacent sibling. `up == true` moves it earlier.
    pub fn reorder_channel(doc: &mut AutoCommit, id: &str, up: bool) {
        let Some(ch) = Self::list_channels(doc).into_iter().find(|c| c.id == id) else {
            return;
        };
        let mut siblings: Vec<Channel> = Self::list_channels(doc)
            .into_iter()
            .filter(|c| c.workspace == ch.workspace && c.category == ch.category)
            .collect();
        siblings.sort_by_key(|c| c.order);
        let Some(pos) = siblings.iter().position(|c| c.id == id) else {
            return;
        };
        let swap = if up {
            pos.checked_sub(1)
        } else {
            pos.checked_add(1).filter(|&p| p < siblings.len())
        };
        let Some(swap) = swap else { return };
        let (Some(a), Some(b)) = (siblings.get(pos), siblings.get(swap)) else {
            return;
        };
        let a_order = a.order;
        let b_order = b.order;
        let a_id = a.id.clone();
        let b_id = b.id.clone();
        let chans = map_at(doc, &ROOT, "channels").unwrap_or(ROOT);
        if let Some(a_obj) = map_at(doc, &chans, &a_id) {
            let _ = doc.put(&a_obj, "order", i64::from(b_order));
        }
        if let Some(b_obj) = map_at(doc, &chans, &b_id) {
            let _ = doc.put(&b_obj, "order", i64::from(a_order));
        }
    }

    // -----------------------------------------------------------------------
    // Voice membership
    //
    // `voice` maps a channel id to a map of member states. Membership is
    // shared truth (everyone sees who is in a call); a peer's overlay layout
    // is local. Per-member fields converge as last-writer-wins scalars,
    // which is the right semantics for transient mic/cam state.
    // -----------------------------------------------------------------------

    fn voice_channel_map(doc: &mut AutoCommit, channel: &str) -> ObjId {
        let voice = get_or_map(doc, &ROOT, "voice");
        get_or_map(doc, &voice, channel)
    }

    pub fn voice_join(doc: &mut AutoCommit, channel: &str, user: &str) {
        let ch = Self::voice_channel_map(doc, channel);
        let m = get_or_map(doc, &ch, user);
        // Only seed defaults on first join so a re-join doesn't clobber state.
        if doc.get(&m, "talking").ok().flatten().is_none() {
            let _ = doc.put(&m, "talking", false);
            let _ = doc.put(&m, "muted", false);
            let _ = doc.put(&m, "deafened", false);
            let _ = doc.put(&m, "audio_only", false);
            let _ = doc.put(&m, "sharing", false);
        }
    }

    pub fn voice_leave(doc: &mut AutoCommit, channel: &str, user: &str) {
        if let Some(voice) = map_at(doc, &ROOT, "voice")
            && let Some(ch) = map_at(doc, &voice, channel)
        {
            let _ = doc.delete(&ch, user);
        }
    }

    pub fn voice_set_state(doc: &mut AutoCommit, channel: &str, user: &str, state: VoiceState) {
        let ch = Self::voice_channel_map(doc, channel);
        let m = get_or_map(doc, &ch, user);
        let _ = doc.put(&m, "talking", state.talking);
        let _ = doc.put(&m, "muted", state.muted);
        let _ = doc.put(&m, "deafened", state.deafened);
        let _ = doc.put(&m, "audio_only", state.audio_only);
    }

    pub fn voice_set_field(
        doc: &mut AutoCommit,
        channel: &str,
        user: &str,
        field: &str,
        value: bool,
    ) {
        let ch = Self::voice_channel_map(doc, channel);
        let m = get_or_map(doc, &ch, user);
        let _ = doc.put(&m, field, value);
    }

    /// Publish whether `user` is currently sharing their screen into the call.
    /// This is the shared truth the featured screen tile keys off — every peer
    /// learns a share started (or stopped) over the same collab transport as the
    /// mic/deafen flags. Guarded so a redundant write doesn't churn the doc.
    pub fn voice_set_sharing(doc: &mut AutoCommit, channel: &str, user: &str, sharing: bool) {
        let ch = Self::voice_channel_map(doc, channel);
        let m = get_or_map(doc, &ch, user);
        if read_bool(doc, &m, "sharing").unwrap_or(false) != sharing {
            let _ = doc.put(&m, "sharing", sharing);
        }
    }

    /// Publish `user`'s dialable media ticket into the shared voice membership.
    /// This is the call's signalling write: every peer polling the `voice` map
    /// then learns how to dial this peer's media transport. Guarded so a
    /// re-announce with the same ticket doesn't churn the document.
    pub fn voice_set_ticket(doc: &mut AutoCommit, channel: &str, user: &str, ticket: &str) {
        let ch = Self::voice_channel_map(doc, channel);
        let m = get_or_map(doc, &ch, user);
        if read_str(doc, &m, "ticket").unwrap_or_default() != ticket {
            let _ = doc.put(&m, "ticket", ticket);
        }
    }

    /// Publish which video codecs `user` decodes (hardware and software),
    /// so senders in the call can pick a codec every member takes. Stored as
    /// comma-separated wire labels; guarded so an unchanged announcement
    /// doesn't churn the document.
    pub fn voice_set_video_caps(
        doc: &mut AutoCommit,
        channel: &str,
        user: &str,
        caps: &crate::model::VideoCaps,
    ) {
        let ch = Self::voice_channel_map(doc, channel);
        let m = get_or_map(doc, &ch, user);
        let hw = caps.hardware_decode.join(",");
        let sw = caps.software_decode.join(",");
        if read_str(doc, &m, "video_hw_decode").as_deref() != Some(hw.as_str()) {
            let _ = doc.put(&m, "video_hw_decode", hw);
        }
        if read_str(doc, &m, "video_sw_decode").as_deref() != Some(sw.as_str()) {
            let _ = doc.put(&m, "video_sw_decode", sw);
        }
    }

    // -----------------------------------------------------------------------
    // Readers
    // -----------------------------------------------------------------------

    pub fn list_categories(doc: &AutoCommit) -> Vec<Category> {
        let Some(cats) = map_at(doc, &ROOT, "categories") else {
            return vec![];
        };
        let mut out: Vec<Category> = doc
            .keys(&cats)
            .filter_map(|id| {
                let cat = map_at(doc, &cats, &id)?;
                Some(Category {
                    id,
                    workspace: read_str(doc, &cat, "workspace").unwrap_or_default(),
                    name: read_str(doc, &cat, "name").unwrap_or_default(),
                    order: read_i64_coerced(doc, &cat, "order").unwrap_or(0) as u32,
                    collapsed: read_bool(doc, &cat, "collapsed").unwrap_or(false),
                })
            })
            .collect();
        out.sort_by_key(|c| c.order);
        out
    }

    /// All members of a voice channel, sorted talking-first then by name-key.
    pub fn list_voice_members(doc: &AutoCommit, channel: &str) -> Vec<VoiceMember> {
        let Some(voice) = map_at(doc, &ROOT, "voice") else {
            return vec![];
        };
        let Some(ch) = map_at(doc, &voice, channel) else {
            return vec![];
        };
        let mut out: Vec<VoiceMember> = doc
            .keys(&ch)
            .filter_map(|user| {
                let m = map_at(doc, &ch, &user)?;
                let ticket = read_str(doc, &m, "ticket").unwrap_or_default();
                let labels = |s: String| -> Vec<String> {
                    s.split(',')
                        .filter(|l| !l.is_empty())
                        .map(str::to_string)
                        .collect()
                };
                let video =
                    read_str(doc, &m, "video_sw_decode").map(|sw| crate::model::VideoCaps {
                        hardware_decode: labels(
                            read_str(doc, &m, "video_hw_decode").unwrap_or_default(),
                        ),
                        software_decode: labels(sw),
                    });
                Some(VoiceMember {
                    user,
                    talking: read_bool(doc, &m, "talking").unwrap_or(false),
                    muted: read_bool(doc, &m, "muted").unwrap_or(false),
                    deafened: read_bool(doc, &m, "deafened").unwrap_or(false),
                    audio_only: read_bool(doc, &m, "audio_only").unwrap_or(false),
                    sharing: read_bool(doc, &m, "sharing").unwrap_or(false),
                    ticket: if ticket.is_empty() {
                        None
                    } else {
                        Some(ticket)
                    },
                    video,
                })
            })
            .collect();
        out.sort_by(|a, b| b.talking.cmp(&a.talking).then_with(|| a.user.cmp(&b.user)));
        out
    }

    pub fn voice_count(doc: &AutoCommit, channel: &str) -> usize {
        Self::list_voice_members(doc, channel).len()
    }

    pub fn list_workspaces(doc: &AutoCommit) -> Vec<Workspace> {
        let Some(wss) = map_at(doc, &ROOT, "workspaces") else {
            return vec![];
        };
        let mut out: Vec<Workspace> = doc
            .keys(&wss)
            .filter_map(|id| {
                let w = map_at(doc, &wss, &id)?;
                Some(Workspace {
                    id,
                    name: read_str(doc, &w, "name").unwrap_or_default(),
                    glyph: read_str(doc, &w, "glyph").unwrap_or_default(),
                    color_idx: read_i64_coerced(doc, &w, "color_idx").unwrap_or(0) as usize,
                    order: read_i64_coerced(doc, &w, "order").unwrap_or(0) as u32,
                })
            })
            .collect();
        out.sort_by_key(|w| w.order);
        out
    }

    pub fn list_users(doc: &AutoCommit) -> Vec<User> {
        let Some(us) = map_at(doc, &ROOT, "users") else {
            return vec![];
        };
        doc.keys(&us)
            .filter_map(|id| {
                let u = map_at(doc, &us, &id)?;
                Some(User {
                    id,
                    name: read_str(doc, &u, "name").unwrap_or_default(),
                    initials: read_str(doc, &u, "initials").unwrap_or_default(),
                    color_idx: read_i64_coerced(doc, &u, "color_idx").unwrap_or(0) as usize,
                    status: read_str(doc, &u, "status")
                        .unwrap_or_default()
                        .parse()
                        .unwrap_or(Presence::Online),
                    is_agent: read_bool(doc, &u, "agent").unwrap_or(false),
                })
            })
            .collect()
    }

    pub fn list_channels(doc: &AutoCommit) -> Vec<Channel> {
        let Some(chans) = map_at(doc, &ROOT, "channels") else {
            return vec![];
        };
        let mut out: Vec<Channel> = doc
            .keys(&chans)
            .filter_map(|id| {
                let c = map_at(doc, &chans, &id)?;
                Some(Channel {
                    id,
                    workspace: read_str(doc, &c, "workspace").unwrap_or_default(),
                    category: read_str(doc, &c, "category").unwrap_or_default(),
                    name: read_str(doc, &c, "name").unwrap_or_default(),
                    topic: read_str(doc, &c, "topic").unwrap_or_default(),
                    kind: read_str(doc, &c, "kind")
                        .unwrap_or_default()
                        .parse()
                        .unwrap_or(ChannelKind::Text),
                    order: read_i64_coerced(doc, &c, "order").unwrap_or(0) as u32,
                    // Derived per-peer in `ChatApp::reload` from the read
                    // watermark; not stored in the shared document.
                    unread: 0,
                    archived: read_bool(doc, &c, "archived").unwrap_or(false),
                })
            })
            .collect();
        out.sort_by_key(|c| c.order);
        out
    }

    pub fn list_messages(doc: &AutoCommit, channel: &str) -> Vec<Message> {
        let lists = message_lists(doc, channel);
        let rows: Vec<ObjId> = lists
            .iter()
            .flat_map(|list| {
                (0..doc.length(list)).filter_map(move |i| match doc.get(list, i) {
                    Ok(Some((Value::Object(ObjType::Map), m))) => Some(m),
                    _ => None,
                })
            })
            .collect();
        let mut out = Vec::new();
        for m in rows {
            let parent = read_str(doc, &m, "parent").unwrap_or_default();
            out.push(Message {
                id: read_str(doc, &m, "id").unwrap_or_default(),
                channel: channel.to_string(),
                author: read_str(doc, &m, "author").unwrap_or_default(),
                ts: read_i64_coerced(doc, &m, "ts").unwrap_or(0),
                text: read_text(doc, &m, "text").unwrap_or_default(),
                body: Self::read_message_body(doc, &m),
                edited: read_bool(doc, &m, "edited").unwrap_or(false),
                history: Self::read_history(doc, &m),
                parent: if parent.is_empty() {
                    None
                } else {
                    Some(parent)
                },
                reactions: Self::read_reactions(doc, &m),
                mentions: Self::read_mentions(doc, &m),
                reference: Self::read_reference(doc, &m),
                attachment: Self::read_attachment(doc, &m),
                work: Self::read_work(doc, &m),
                pinned: read_bool(doc, &m, "pinned").unwrap_or(false),
            });
        }
        if lists.len() > 1 {
            // Rows from concurrently created lists interleave by time; the
            // sort is stable, so one list's own order is kept on ties.
            out.sort_by_key(|m| m.ts);
        }
        out
    }

    /// The resolved participant references stored on a message block.
    fn read_mentions(doc: &AutoCommit, msg: &ObjId) -> Vec<crate::mention::Mention> {
        let Some(list) = list_at(doc, msg, "mentions") else {
            return vec![];
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&list, i) else {
                continue;
            };
            let user = read_str(doc, &e, "user").unwrap_or_default();
            if user.is_empty() {
                continue;
            }
            out.push(crate::mention::Mention {
                user,
                start: read_i64_coerced(doc, &e, "start").unwrap_or(0).max(0) as usize,
                len: read_i64_coerced(doc, &e, "len").unwrap_or(0).max(0) as usize,
                is_agent: read_bool(doc, &e, "agent").unwrap_or(false),
            });
        }
        out
    }

    fn read_reactions(doc: &AutoCommit, msg: &ObjId) -> Vec<Reaction> {
        let Some(list) = list_at(doc, msg, "reactions") else {
            return vec![];
        };
        // Group the flat (emoji,user) entries back into per-emoji buckets,
        // remembering first-seen order for a stable layout.
        let mut order: Vec<String> = Vec::new();
        let mut buckets: HashMap<String, Vec<String>> = HashMap::new();
        for i in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&list, i) else {
                continue;
            };
            let emoji = read_str(doc, &e, "emoji").unwrap_or_default();
            let user = read_str(doc, &e, "user").unwrap_or_default();
            if emoji.is_empty() {
                continue;
            }
            let bucket = buckets.entry(emoji.clone()).or_default();
            if !bucket.contains(&user) {
                bucket.push(user);
            }
            if !order.contains(&emoji) {
                order.push(emoji);
            }
        }
        let mut out: Vec<Reaction> = order
            .into_iter()
            .filter_map(|emoji| {
                let users = buckets.remove(&emoji)?;
                if users.is_empty() {
                    None
                } else {
                    Some(Reaction { emoji, users })
                }
            })
            .collect();
        // Stable display order: most-reacted first, then emoji codepoint.
        out.sort_by(|a, b| {
            b.count()
                .cmp(&a.count())
                .then_with(|| a.emoji.cmp(&b.emoji))
        });
        out
    }

    fn read_history(doc: &AutoCommit, msg: &ObjId) -> Vec<EditRecord> {
        let Some(h) = list_at(doc, msg, "history") else {
            return vec![];
        };
        let mut out = Vec::new();
        for i in 0..doc.length(&h) {
            if let Ok(Some((Value::Object(ObjType::Map), rec))) = doc.get(&h, i) {
                out.push(EditRecord {
                    ts: read_i64_coerced(doc, &rec, "ts").unwrap_or(0),
                    text: read_str(doc, &rec, "text").unwrap_or_default(),
                });
            }
        }
        out
    }

    fn read_attachment(doc: &AutoCommit, msg: &ObjId) -> Option<Attachment> {
        let a = map_at(doc, msg, "attachment")?;
        let blob = read_str(doc, &a, "blob").unwrap_or_default();
        match read_str(doc, &a, "kind").unwrap_or_default().as_str() {
            "image" => Some(Attachment::Image {
                name: read_str(doc, &a, "name").unwrap_or_default(),
                blob,
                w: read_i64_coerced(doc, &a, "w").unwrap_or(0) as u32,
                h: read_i64_coerced(doc, &a, "h").unwrap_or(0) as u32,
            }),
            "video" => Some(Attachment::Video {
                name: read_str(doc, &a, "name").unwrap_or_default(),
                blob,
                w: read_i64_coerced(doc, &a, "w").unwrap_or(0) as u32,
                h: read_i64_coerced(doc, &a, "h").unwrap_or(0) as u32,
                size: read_i64_coerced(doc, &a, "size").unwrap_or(0) as u64,
            }),
            "file" => Some(Attachment::File {
                name: read_str(doc, &a, "name").unwrap_or_default(),
                blob,
                size: read_i64_coerced(doc, &a, "size").unwrap_or(0) as u64,
            }),
            _ => None,
        }
    }

    /// The address of what a message is about, or `None` for an ordinary
    /// message — and also for one whose stored address will not parse, which
    /// is deliberately not the same as a nearby address that does. A
    /// reference that cannot be read is no reference at all.
    fn read_reference(doc: &AutoCommit, msg: &ObjId) -> Option<tilo_reference::ResourceRef> {
        let raw = read_str(doc, msg, "reference").unwrap_or_default();
        if raw.is_empty() {
            return None;
        }
        serde_json::from_str(&raw).ok()
    }

    /// The agent record stored on a message, or `None` for an ordinary one,
    /// as the native CRDT map written by [`Self::write_work`].
    fn read_work(doc: &AutoCommit, msg: &ObjId) -> Option<crate::model::AgentWork> {
        let record = map_at(doc, msg, "work")?;
        Self::read_native_work(doc, &record)
    }

    fn read_native_work(doc: &AutoCommit, record: &ObjId) -> Option<crate::model::AgentWork> {
        let transition = serde_json::from_str::<StoredAgentTransition>(
            &read_str(doc, record, "transition").unwrap_or_default(),
        )
        .ok()?;
        let (state, since) = (transition.state, transition.since);
        // Reasoning is one atomic scalar because route and explanation are one
        // authored claim.
        let reasoning = serde_json::from_str::<Option<crate::model::AgentReasoning>>(
            &read_str(doc, record, "reasoning").unwrap_or_default(),
        )
        .ok()
        .flatten();
        let mut trace_with_order = Vec::new();
        if let Some(rows) = map_at(doc, record, "trace") {
            for id in doc.keys(&rows) {
                let Some(row) = map_at(doc, &rows, &id) else {
                    continue;
                };
                if !read_bool(doc, &row, "present").unwrap_or(false) {
                    continue;
                }
                let Ok(outcome) = serde_json::from_str::<crate::model::AgentStepOutcome>(
                    &read_str(doc, &row, "outcome").unwrap_or_default(),
                ) else {
                    continue;
                };
                let sequence = read_i64_coerced(doc, &row, "sequence").unwrap_or(0).max(0) as u64;
                trace_with_order.push((
                    sequence,
                    id.clone(),
                    crate::model::AgentStep {
                        id,
                        sequence,
                        resource: read_str(doc, &row, "resource").unwrap_or_default(),
                        label: read_str(doc, &row, "label").unwrap_or_default(),
                        outcome,
                    },
                ));
            }
        }
        trace_with_order
            .sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        let trace = trace_with_order
            .into_iter()
            .map(|(_, _, step)| step)
            .collect();
        Some(crate::model::AgentWork {
            state,
            since,
            reasoning,
            trace,
        })
    }

    /// Record what an agent is doing with the message `msg` replies to, and
    /// optionally give `msg` the words it did not have when it was posted.
    ///
    /// `say` splices into a body that is still **empty**. That is an
    /// authoring completion, not an edit: nothing is archived into the
    /// message's history and it is never flagged `edited`, because nobody
    /// reworded anything — the agent posted the reply before it had an
    /// answer and is now supplying the answer. A body that already has text
    /// is left alone rather than silently rewritten.
    ///
    /// Returns whether the document changed.
    pub fn set_message_work(
        doc: &mut AutoCommit,
        channel: &str,
        msg: &str,
        work: &crate::model::AgentWork,
        say: Option<&str>,
    ) -> bool {
        let Some(block) = Self::find_message_obj(doc, channel, msg) else {
            return false;
        };
        let mut changed = false;
        if Self::read_work(doc, &block).as_ref() != Some(work) {
            Self::write_work(doc, &block, work);
            changed = true;
        }
        if let Some(text) = say.filter(|t| !t.is_empty())
            && let Ok(Some((Value::Object(ObjType::Text), t))) = doc.get(&block, "text")
            && doc.text(&t).is_ok_and(|s| s.is_empty())
        {
            let _ = doc.splice_text(&t, 0, 0, text);
            changed = true;
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;
    use tilo_automerge::ApplyIncoming;

    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        ChatDoc::seed_if_empty(&mut doc);
        doc
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn messages_round_trip_references_reactions_threads_and_exactly_once_ids() {
        // Phase: a_messages_reference_round_trips_and_an_ordinary_message_carries_none.
        {
            let mut doc = seeded();
            let about = tilo_reference::ResourceRef::whole(
                "files.entry",
                "files.entry:/var/workspace/harbour.png",
                "harbour.png",
            );
            ChatDoc::push_message_with_id(
                &mut doc,
                "c_general",
                "m_about",
                NewMessage {
                    author: model::ME.into(),
                    ts: 1000,
                    text: "look at this".into(),
                    reference: Some(about.clone()),
                    ..Default::default()
                },
            );
            ChatDoc::push_message_with_id(
                &mut doc,
                "c_general",
                "m_plain",
                NewMessage {
                    author: model::ME.into(),
                    ts: 1001,
                    text: "hi".into(),
                    ..Default::default()
                },
            );
            let msgs = ChatDoc::list_messages(&doc, "c_general");
            let with_ref = msgs.iter().find(|m| m.id == "m_about").unwrap();
            assert_eq!(with_ref.reference, Some(about));
            let plain = msgs.iter().find(|m| m.id == "m_plain").unwrap();
            assert_eq!(
                plain.reference, None,
                "an ordinary message must carry no reference"
            );
        }

        // Phase: replayed_message_id_is_exactly_once_and_cannot_break_thread_identity.
        {
            let mut doc = seeded();
            let message = NewMessage {
                author: model::ME.into(),
                ts: NOW,
                text: "idempotent send".into(),
                ..Default::default()
            };
            ChatDoc::push_message_with_id(&mut doc, "c_general", "m_retry", message.clone());
            // A duplicate delivery may carry an equivalent decoded payload, but it
            // must not create a second row or turn `chat.message:m_retry` into an
            // ambiguous resource.
            ChatDoc::push_message_with_id(&mut doc, "c_general", "m_retry", message);
            assert_eq!(
                ChatDoc::list_messages(&doc, "c_general")
                    .iter()
                    .filter(|row| row.id == "m_retry")
                    .count(),
                1
            );

            // A message action cannot manufacture a hidden conversation or a
            // reply-to-reply thread edge when replayed from outside the visible
            // UI. (A parent not yet received remains valid so delivery ordering
            // cannot lose a reply.)
            assert!(!ChatDoc::accepts_message_action(
                &doc,
                "c_missing",
                "m_missing_channel",
                "orphan",
                None,
                None,
            ));
            ChatDoc::push_message_with_id(
                &mut doc,
                "c_general",
                "m_first_reply",
                NewMessage {
                    author: model::ME.into(),
                    ts: NOW,
                    text: "first reply".into(),
                    parent: Some("m_retry".into()),
                    ..Default::default()
                },
            );
            assert!(!ChatDoc::accepts_message_action(
                &doc,
                "c_general",
                "m_nested_reply",
                "nested reply",
                Some("m_first_reply"),
                None,
            ));
            assert!(ChatDoc::list_messages(&doc, "c_missing").is_empty());
            assert!(
                ChatDoc::list_messages(&doc, "c_general")
                    .iter()
                    .all(|row| row.id != "m_nested_reply")
            );
        }

        // Phase: messages_carry_reactions_thread_and_attachment.
        {
            let doc = seeded();
            let msgs = ChatDoc::list_messages(&doc, "c_crit");
            let concept = msgs.iter().find(|m| m.id == "m_concept").unwrap();
            assert!(matches!(concept.attachment, Some(Attachment::Image { .. })));
            assert!(concept.reactions.iter().any(|r| r.emoji == "❤️"));
            // thread replies hang off the concept image
            assert_eq!(model::reply_count(&msgs, "m_concept"), 3);
            // an edited message preserves its prior version
            let sora = msgs.iter().find(|m| m.id == "m_sora").unwrap();
            assert!(sora.edited);
            assert_eq!(sora.history.len(), 1);
        }

        // Phase: toggle_reaction_writes_through_crdt.
        {
            let mut doc = seeded();
            // m_me1 starts with no reactions
            ChatDoc::toggle_reaction(&mut doc, "c_crit", "m_me1", "🔥", model::ME);
            let msgs = ChatDoc::list_messages(&doc, "c_crit");
            let m = msgs.iter().find(|m| m.id == "m_me1").unwrap();
            assert!(
                m.reactions
                    .iter()
                    .any(|r| r.emoji == "🔥" && r.reacted_by(model::ME))
            );
            // toggling again removes it and prunes the emoji
            ChatDoc::toggle_reaction(&mut doc, "c_crit", "m_me1", "🔥", model::ME);
            let msgs = ChatDoc::list_messages(&doc, "c_crit");
            let m = msgs.iter().find(|m| m.id == "m_me1").unwrap();
            assert!(!m.reactions.iter().any(|r| r.emoji == "🔥"));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn seeded_and_created_workspaces_are_minimal_idempotent_and_durable() {
        // Phase: seed_starter_is_minimal_real_and_has_no_demo_content.
        {
            // The live first-run seed: just the local person + an empty workspace.
            let mut doc = AutoCommit::new();
            ChatDoc::seed_starter(&mut doc, model::ME, "Ada Lovelace", "AL");

            let users = ChatDoc::list_users(&doc);
            assert_eq!(users.len(), 1, "only the local user — no fabricated people");
            assert_eq!(users[0].id, model::ME);
            assert_eq!(users[0].name, "Ada Lovelace");

            let ws = ChatDoc::list_workspaces(&doc);
            assert_eq!(ws.len(), 1);
            assert_eq!(ws[0].id, "ws_home");
            assert!(ws.iter().all(|w| w.id != "ws_acme"), "no demo workspaces");

            let chans = ChatDoc::list_channels(&doc);
            assert!(
                chans
                    .iter()
                    .any(|c| c.id == "c_general" && c.kind == ChannelKind::Text)
            );
            assert!(chans.iter().any(|c| c.kind == ChannelKind::Voice));
            assert!(chans.iter().all(|c| c.id != "c_crit"), "no demo channels");
            assert!(
                ChatDoc::list_messages(&doc, "c_general").is_empty(),
                "a real first run has no fabricated conversation"
            );

            // Idempotent: adopting or re-seeding never duplicates.
            ChatDoc::seed_starter(&mut doc, model::ME, "X", "X");
            assert_eq!(ChatDoc::list_users(&doc).len(), 1);
            assert_eq!(ChatDoc::list_workspaces(&doc).len(), 1);
        }

        // Phase: seed_is_idempotent.
        {
            let mut doc = seeded();
            let before = ChatDoc::list_channels(&doc).len();
            ChatDoc::seed_if_empty(&mut doc);
            let after = ChatDoc::list_channels(&doc).len();
            assert_eq!(before, after, "second seed must not duplicate channels");
            assert!(before >= 12);
        }

        // Phase: workspaces_and_channels_round_trip.
        {
            let doc = seeded();
            let ws = ChatDoc::list_workspaces(&doc);
            assert_eq!(ws.first().map(|w| w.name.as_str()), Some("Acme Design"));
            let acme: Vec<_> = ChatDoc::list_channels(&doc)
                .into_iter()
                .filter(|c| c.workspace == "ws_acme")
                .collect();
            assert!(acme.iter().any(|c| c.name == "design-crit"));
            assert!(acme.iter().any(|c| c.kind == ChannelKind::Voice));
        }

        // Phase: channels_carry_category_and_voice_seed.
        {
            let doc = seeded();
            let crit = ChatDoc::list_channels(&doc)
                .into_iter()
                .find(|c| c.id == "c_crit")
                .unwrap();
            assert_eq!(crit.category, "cat_acme_main");
            let cats = ChatDoc::list_categories(&doc);
            assert!(
                cats.iter()
                    .any(|c| c.workspace == "ws_acme" && c.name == "Voice")
            );
            // The huddle is pre-populated with three members; Aria is talking.
            let members = ChatDoc::list_voice_members(&doc, "c_huddle");
            assert_eq!(members.len(), 3);
            assert!(members.iter().any(|m| m.user == "u_aria" && m.talking));
            assert!(members.iter().any(|m| m.user == "u_milo" && m.muted));
            assert!(members.iter().any(|m| m.user == "u_sora" && m.audio_only));
        }

        // Phase: create_workspace_scaffolds_a_usable_workspace.
        {
            let mut doc = seeded();
            let before = ChatDoc::list_workspaces(&doc).len();
            let id = ChatDoc::create_workspace(&mut doc, "Weekend Jam Two");
            let wss = ChatDoc::list_workspaces(&doc);
            assert_eq!(wss.len(), before + 1, "a workspace was created");
            let ws = wss.iter().find(|w| w.id == id).unwrap();
            assert_eq!(ws.name, "Weekend Jam Two");
            assert_eq!(ws.glyph, "WJ", "glyph is a monogram of the name");
            assert!(
                ws.order as usize >= before,
                "appended at the end of the rail"
            );
            // It comes with a usable text channel and a voice channel to call in.
            let chans: Vec<_> = ChatDoc::list_channels(&doc)
                .into_iter()
                .filter(|c| c.workspace == id)
                .collect();
            assert!(
                chans
                    .iter()
                    .any(|c| c.name == "general" && c.kind == ChannelKind::Text)
            );
            assert!(chans.iter().any(|c| c.kind == ChannelKind::Voice));
        }

        // Phase: document_survives_a_save_load_cycle.
        {
            // The persistence contract: send a message, save the raw bytes, load
            // them into a fresh document, and the message is still there.
            let mut doc = seeded();
            ChatDoc::push_message(
                &mut doc,
                "c_general",
                NewMessage {
                    author: "u_me".into(),
                    ts: NOW,
                    text: "persisted!".into(),
                    ..Default::default()
                },
            );
            let bytes = doc.save();
            let reloaded = AutoCommit::load(&bytes).unwrap();
            assert!(
                ChatDoc::is_seeded(&reloaded),
                "reloaded doc keeps the seed guard"
            );
            let msgs = ChatDoc::list_messages(&reloaded, "c_general");
            assert!(
                msgs.iter().any(|m| m.text == "persisted!"),
                "message survived reload"
            );
        }
    }

    #[test]
    fn edits_and_drafts_write_only_the_changes_they_make() {
        // Phase: edit_message_archives_history.
        {
            let mut doc = seeded();
            ChatDoc::edit_message(&mut doc, "c_crit", "m_me1", "Edited body.", &[], NOW);
            let msgs = ChatDoc::list_messages(&doc, "c_crit");
            let m = msgs.iter().find(|m| m.id == "m_me1").unwrap();
            assert_eq!(m.text, "Edited body.");
            assert!(m.edited);
            assert_eq!(m.history.len(), 1, "previous body archived");
        }

        // Phase: editing_a_long_message_splices_only_the_changed_span.
        {
            // Editing a message re-sends its whole body. Deleting and reinserting
            // it tombstones every character on every edit, so a message that is
            // corrected a few times costs many times its own length forever.
            let mut doc = seeded();
            let body = "The deploy finished at 14:02 and the canary is green across \
                        all three regions; rolling the rest of the fleet now."
                .to_string();
            ChatDoc::edit_message(&mut doc, "c_crit", "m_me1", &body, &[], NOW);
            let baseline = doc.save().len();

            // 30 corrections, each touching only the last word.
            for i in 0..30 {
                let edited = format!("{body} v{i}");
                ChatDoc::edit_message(
                    &mut doc,
                    "c_crit",
                    "m_me1",
                    &edited,
                    &[],
                    NOW + i64::from(i),
                );
            }
            let grown = doc.save().len() - baseline;

            let msgs = ChatDoc::list_messages(&doc, "c_crit");
            let m = msgs.iter().find(|m| m.id == "m_me1").unwrap();
            assert_eq!(
                m.text,
                format!("{body} v29"),
                "the last edit must be the body"
            );
            // Each edit legitimately archives the previous body into `history`, so
            // the bound is against the archive cost, not zero. A full rewrite would
            // add the body a second time per edit on top of that.
            assert!(
                grown < 30 * body.len() * 3 / 2,
                "30 one-word edits grew the document by {grown} bytes for a {}-char \
                 body — edit_message is rewriting the whole text",
                body.len()
            );
        }

        // Phase: clearing_an_unset_draft_is_free.
        {
            let mut doc = seeded();
            ChatDoc::ensure_rich_draft(&mut doc, "c_crit");
            let before = doc.save().len();

            for _ in 0..20 {
                ChatDoc::clear_draft(&mut doc, "c_crit");
            }

            assert_eq!(ChatDoc::draft_text(&doc, "c_crit"), "");
            assert_eq!(
                doc.save().len(),
                before,
                "clearing an already-empty draft must not grow the document"
            );
        }

        // Phase: draft_round_trips_and_send_appends.
        {
            let mut doc = seeded();
            let before = ChatDoc::list_messages(&doc, "c_crit").len();
            type_into(&mut doc, "c_crit", 0, "hi there");
            assert_eq!(ChatDoc::draft_text(&doc, "c_crit"), "hi there");
            ChatDoc::push_message(
                &mut doc,
                "c_crit",
                NewMessage {
                    author: model::ME.into(),
                    ts: NOW,
                    text: "hi there".into(),
                    ..Default::default()
                },
            );
            ChatDoc::clear_draft(&mut doc, "c_crit");
            assert_eq!(ChatDoc::draft_text(&doc, "c_crit"), "");
            assert_eq!(ChatDoc::list_messages(&doc, "c_crit").len(), before + 1);
        }
    }

    /// The seed without its shared `rich_drafts`/`messages`/`read`
    /// containers, so the first write on each replica creates them lazily.
    fn lazy_root_seeded() -> AutoCommit {
        let mut doc = seeded();
        for key in [RICH_DRAFTS, "messages", "read"] {
            let _ = doc.delete(&ROOT, key);
        }
        doc.commit();
        doc
    }

    /// Type `text` at character `at` of the draft's first paragraph, as the
    /// composer's edit would, creating the draft first if need be.
    fn type_into(doc: &mut AutoCommit, key: &str, at: usize, text: &str) {
        let root = ChatDoc::ensure_rich_draft(doc, key).unwrap();
        let block = ChatDoc::draft_blocks(doc, key)[0].id.clone();
        tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block,
            at,
            del: 0,
            ins: text.into(),
        }
        .apply_under(doc, &root)
        .unwrap();
    }

    fn msg(text: &str, ts: i64) -> NewMessage {
        NewMessage {
            author: model::ME.into(),
            ts,
            text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn concurrent_first_drafts_on_two_replicas_both_survive() {
        for mut base in [seeded(), lazy_root_seeded()] {
            for _ in 0..8 {
                let mut a = base.fork();
                let mut b = base.fork();
                type_into(&mut a, "c_crit", 0, "left");
                type_into(&mut b, "c_crit", 0, "right");
                a.merge_incoming(&mut b).unwrap();
                let draft = ChatDoc::draft_text(&a, "c_crit");
                assert!(
                    draft.contains("left") && draft.contains("right"),
                    "both replicas' first draft keystrokes survive; got {draft:?}"
                );
                // The next edit joins drafts made at once into one.
                ChatDoc::ensure_rich_draft(&mut a, "c_crit");
                assert_eq!(ChatDoc::rich_draft_candidates(&a, "c_crit").len(), 1);
                let joined = ChatDoc::draft_text(&a, "c_crit");
                assert!(
                    joined.contains("left") && joined.contains("right"),
                    "{joined:?}"
                );
            }
        }
    }

    #[test]
    fn collaborators_typing_into_one_draft_merge_per_character() {
        let mut a = seeded();
        type_into(&mut a, "c_crit", 0, "Hello world");
        let mut c = a.fork();
        type_into(&mut a, "c_crit", 5, ",");
        type_into(&mut c, "c_crit", 11, "!");
        a.merge_incoming(&mut c).unwrap();
        assert_eq!(ChatDoc::draft_text(&a, "c_crit"), "Hello, world!");
    }

    #[test]
    fn a_rich_body_is_stored_beside_its_plain_text_and_edited_as_written() {
        let mut doc = seeded();
        let body = vec![tilo_document::ClipBlock {
            kind: "paragraph".into(),
            text: "hi @Ada".into(),
            marks: vec![tilo_doc::document::MarkSpan {
                start: 3,
                end: 7,
                name: "mention".into(),
                value: tilo_doc::document::MarkValue::Str("u-ada".into()),
            }],
            level: None,
            ordered: None,
            align: None,
            attrs: std::collections::BTreeMap::new(),
        }];
        ChatDoc::push_message_with_id(
            &mut doc,
            "c_crit",
            "m_rich",
            NewMessage {
                body: body.clone(),
                ..msg("hi @Ada", NOW)
            },
        );
        let read = |doc: &AutoCommit| {
            ChatDoc::list_messages(doc, "c_crit")
                .into_iter()
                .find(|m| m.id == "m_rich")
                .unwrap()
        };
        let sent = read(&doc);
        assert_eq!(sent.text, "hi @Ada");
        assert_eq!(sent.blocks().len(), 1);
        assert_eq!(
            sent.blocks()[0].marks,
            [ui::widget::MarkSpan::new(
                3..7,
                ui::widget::Mark::Mention("u-ada".into())
            )]
        );
        let mut bold = body;
        bold[0].marks.push(tilo_doc::document::MarkSpan {
            start: 0,
            end: 2,
            name: "strong".into(),
            value: tilo_doc::document::MarkValue::Bool(true),
        });
        ChatDoc::edit_message(&mut doc, "c_crit", "m_rich", "hi @Ada", &bold, NOW + 5);
        let edited = read(&doc);
        assert!(edited.edited);
        assert!(
            edited.history.is_empty(),
            "the same words: nothing to archive"
        );
        assert_eq!(edited.blocks()[0].marks.len(), 2);
        ChatDoc::edit_message(&mut doc, "c_crit", "m_rich", "plain now", &[], NOW + 9);
        let plain = read(&doc);
        assert_eq!(plain.history.len(), 1);
        assert!(
            plain.body.is_empty(),
            "a plain edit drops the old formatting"
        );
        assert_eq!(plain.blocks()[0].text, "plain now");
    }

    #[test]
    fn concurrent_first_messages_on_two_replicas_both_survive() {
        // A channel created after the seed: its list must come with it.
        let mut origin = seeded();
        ChatDoc::insert_channel(
            &mut origin,
            "c_new",
            "ws_acme",
            "cat_acme_main",
            "new",
            ChannelKind::Text,
        );
        origin.commit();
        let cases = [(origin, "c_new"), (lazy_root_seeded(), "c_crit")];
        for (mut base, channel) in cases {
            for _ in 0..8 {
                let mut a = base.fork();
                let mut b = base.fork();
                ChatDoc::push_message_with_id(&mut a, channel, "m_left", msg("left", NOW));
                ChatDoc::push_message_with_id(&mut b, channel, "m_right", msg("right", NOW + 1));
                a.merge_incoming(&mut b).unwrap();
                let mut texts: Vec<String> = ChatDoc::list_messages(&a, channel)
                    .into_iter()
                    .map(|m| m.text)
                    .collect();
                // Concurrent appends to one shared list order by actor, not
                // time; only membership is the property under test here.
                texts.sort();
                assert_eq!(texts, ["left", "right"], "channel {channel}");
                // Resolution and deletion reach the losing list too.
                ChatDoc::delete_message(&mut a, channel, "m_left");
                ChatDoc::delete_message(&mut a, channel, "m_right");
                assert!(ChatDoc::list_messages(&a, channel).is_empty());
            }
        }
    }

    #[test]
    fn concurrent_first_read_marks_on_two_replicas_both_survive() {
        for _ in 0..8 {
            let mut base = lazy_root_seeded();
            let mut a = base.fork();
            let mut b = base.fork();
            ChatDoc::set_read_watermark(&mut a, "c_crit", "u_a", 3);
            ChatDoc::set_read_watermark(&mut b, "c_crit", "u_b", 5);
            a.merge_incoming(&mut b).unwrap();
            assert_eq!(ChatDoc::read_watermark(&a, "c_crit", "u_a"), 3);
            assert_eq!(ChatDoc::read_watermark(&a, "c_crit", "u_b"), 5);
        }
    }

    /// Regression (frame-performance Chat workload): consecutive sends from
    /// one composer while a collaborator syncs every change must each start
    /// from an empty draft, on a fresh and on a lazily rooted document.
    #[test]
    fn consecutive_sends_with_a_syncing_collaborator_start_from_an_empty_draft() {
        for mut origin in [seeded(), lazy_root_seeded()] {
            let mut a = origin.fork();
            let mut b = origin.fork();
            for n in 0..5 {
                assert_eq!(
                    ChatDoc::draft_text(&a, "c_crit"),
                    "",
                    "send {n} starts empty"
                );
                let text = format!("m{n:04} hello");
                type_into(&mut a, "c_crit", 0, &text);
                b.merge_incoming(&mut a).unwrap();
                a.merge_incoming(&mut b).unwrap();
                let draft = ChatDoc::draft_text(&a, "c_crit");
                assert_eq!(draft, text);
                ChatDoc::push_message(&mut a, "c_crit", msg(&draft, NOW + n));
                ChatDoc::clear_draft(&mut a, "c_crit");
                b.merge_incoming(&mut a).unwrap();
                a.merge_incoming(&mut b).unwrap();
                assert_eq!(
                    ChatDoc::draft_text(&a, "c_crit"),
                    "",
                    "send {n} consumes the draft"
                );
                assert_eq!(
                    ChatDoc::draft_text(&b, "c_crit"),
                    "",
                    "collaborator sees it consumed"
                );
            }
        }
    }

    #[test]
    fn channel_and_category_structure_mutations_round_trip() {
        // Phase: channel_crud_roundtrips.
        {
            let mut doc = seeded();
            let before = ChatDoc::list_channels(&doc).len();
            let id = ChatDoc::create_channel(
                &mut doc,
                "ws_acme",
                "cat_acme_main",
                "design-ops",
                ChannelKind::Text,
            );
            assert_eq!(ChatDoc::list_channels(&doc).len(), before + 1);
            ChatDoc::rename_channel(&mut doc, &id, "ops");
            let c = ChatDoc::list_channels(&doc)
                .into_iter()
                .find(|c| c.id == id)
                .unwrap();
            assert_eq!(c.name, "ops");
            // Move into the voice category, then delete.
            ChatDoc::move_channel(&mut doc, &id, "cat_acme_voice");
            let c = ChatDoc::list_channels(&doc)
                .into_iter()
                .find(|c| c.id == id)
                .unwrap();
            assert_eq!(c.category, "cat_acme_voice");
            ChatDoc::delete_channel(&mut doc, &id);
            assert!(ChatDoc::list_channels(&doc).into_iter().all(|c| c.id != id));
        }

        // Phase: reorder_channel_swaps_adjacent_order.
        {
            let mut doc = seeded();
            // general(0), design-crit(1) in cat_acme_main — move design-crit up.
            let general_before = ChatDoc::list_channels(&doc)
                .into_iter()
                .find(|c| c.id == "c_general")
                .unwrap()
                .order;
            let crit_before = ChatDoc::list_channels(&doc)
                .into_iter()
                .find(|c| c.id == "c_crit")
                .unwrap()
                .order;
            assert!(crit_before > general_before);
            ChatDoc::reorder_channel(&mut doc, "c_crit", true);
            let general_after = ChatDoc::list_channels(&doc)
                .into_iter()
                .find(|c| c.id == "c_general")
                .unwrap()
                .order;
            let crit_after = ChatDoc::list_channels(&doc)
                .into_iter()
                .find(|c| c.id == "c_crit")
                .unwrap()
                .order;
            assert!(
                crit_after < general_after,
                "design-crit now sorts before general"
            );
        }

        // Phase: deleting_category_rehomes_its_channels.
        {
            let mut doc = seeded();
            ChatDoc::delete_category(&mut doc, "cat_acme_main");
            assert!(
                ChatDoc::list_categories(&doc)
                    .into_iter()
                    .all(|c| c.id != "cat_acme_main")
            );
            // Its channels survive, rehomed to the uncategorised group.
            let general = ChatDoc::list_channels(&doc)
                .into_iter()
                .find(|c| c.id == "c_general")
                .unwrap();
            assert_eq!(general.category, "");
        }
    }

    #[test]
    fn voice_membership_and_screen_share_round_trip() {
        // Phase: voice_join_leave_and_state.
        {
            let mut doc = seeded();
            ChatDoc::voice_join(&mut doc, "c_huddle", "u_me");
            assert_eq!(ChatDoc::voice_count(&doc, "c_huddle"), 4);
            ChatDoc::voice_set_state(
                &mut doc,
                "c_huddle",
                "u_me",
                VoiceState {
                    talking: true,
                    muted: true,
                    deafened: true,
                    audio_only: true,
                },
            );
            let me = ChatDoc::list_voice_members(&doc, "c_huddle")
                .into_iter()
                .find(|m| m.user == "u_me")
                .unwrap();
            assert!(me.talking);
            assert!(me.muted);
            assert!(me.deafened);
            assert!(me.audio_only);
            ChatDoc::voice_leave(&mut doc, "c_huddle", "u_me");
            assert_eq!(ChatDoc::voice_count(&doc, "c_huddle"), 3);
        }

        // Phase: voice_screen_share_round_trips_through_the_shared_membership.
        {
            let mut doc = seeded();
            ChatDoc::voice_join(&mut doc, "c_huddle", "u_me");
            let sharing = |doc: &AutoCommit| {
                ChatDoc::list_voice_members(doc, "c_huddle")
                    .into_iter()
                    .find(|m| m.user == "u_me")
                    .is_some_and(|m| m.sharing)
            };
            assert!(!sharing(&doc), "a fresh member is not sharing");
            let video = |doc: &AutoCommit| {
                ChatDoc::list_voice_members(doc, "c_huddle")
                    .into_iter()
                    .find(|m| m.user == "u_me")
                    .and_then(|m| m.video)
            };
            assert_eq!(
                video(&doc),
                None,
                "a member that never announced is a legacy peer"
            );
            let caps = crate::model::VideoCaps {
                hardware_decode: vec!["av1".into(), "vp9".into()],
                software_decode: vec!["h264".into()],
            };
            ChatDoc::voice_set_video_caps(&mut doc, "c_huddle", "u_me", &caps);
            assert_eq!(video(&doc), Some(caps));
            ChatDoc::voice_set_sharing(&mut doc, "c_huddle", "u_me", true);
            assert!(sharing(&doc), "a started share is visible to every peer");
            ChatDoc::voice_set_sharing(&mut doc, "c_huddle", "u_me", false);
            assert!(!sharing(&doc), "a stopped share clears the shared flag");
        }
    }

    #[test]
    fn unread_derives_from_an_idempotent_read_watermark() {
        // Phase: unread_derives_from_the_read_watermark.
        {
            let mut doc = seeded();
            // #design-crit has eight top-level messages (thread replies excluded).
            assert_eq!(ChatDoc::top_level_count(&doc, "c_crit"), 8);
            // Never opened → every top-level message is unread.
            assert_eq!(ChatDoc::unread_count(&doc, "c_crit", model::ME), 8);
            // Reading it clears the badge…
            ChatDoc::mark_read(&mut doc, "c_crit", model::ME);
            assert_eq!(ChatDoc::unread_count(&doc, "c_crit", model::ME), 0);
            // …and a new message makes it unread again.
            ChatDoc::push_message(
                &mut doc,
                "c_crit",
                NewMessage {
                    author: "u_aria".into(),
                    ts: NOW,
                    text: "one more".into(),
                    ..Default::default()
                },
            );
            assert_eq!(ChatDoc::unread_count(&doc, "c_crit", model::ME), 1);
            // A threaded reply is not a top-level message, so it doesn't count.
            ChatDoc::push_message(
                &mut doc,
                "c_crit",
                NewMessage {
                    author: "u_aria".into(),
                    ts: NOW,
                    text: "in the thread".into(),
                    parent: Some("m_concept".into()),
                    ..Default::default()
                },
            );
            assert_eq!(
                ChatDoc::unread_count(&doc, "c_crit", model::ME),
                1,
                "thread replies don't bump unread"
            );
        }

        // Phase: mark_read_is_idempotent_and_does_not_churn.
        {
            // Reading a channel twice with no new messages must not append a new
            // change (the watermark write is guarded on a real value change).
            let mut doc = seeded();
            ChatDoc::mark_read(&mut doc, "c_crit", model::ME);
            let heads = doc.get_heads();
            ChatDoc::mark_read(&mut doc, "c_crit", model::ME);
            assert_eq!(doc.get_heads(), heads, "a no-op mark_read writes nothing");
        }
    }

    #[test]
    fn peers_are_announced_and_their_concurrent_state_converges() {
        // Phase: ensure_user_is_guarded_and_announces_a_peer.
        {
            // Registering the local peer writes once, then no-ops (so it never
            // churns the doc / spams empty collab deltas), and a fresh peer id is
            // announced into the roster so others learn its name + colour.
            let mut doc = seeded();
            let wrote = ChatDoc::ensure_user(&mut doc, "u_bob", "Bob", "BO", 4, Presence::Online);
            assert!(wrote, "a new peer is announced");
            assert!(
                ChatDoc::list_users(&doc)
                    .iter()
                    .any(|u| u.id == "u_bob" && u.name == "Bob")
            );
            let again = ChatDoc::ensure_user(&mut doc, "u_bob", "Bob", "BO", 4, Presence::Online);
            assert!(!again, "an unchanged re-announce writes nothing");
        }

        // Phase: read_watermarks_are_independent_per_peer.
        {
            // The exact multi-participant bug the identity work fixes: two peers
            // must not clobber each other's unread badges. Alice reads a channel;
            // Bob's unread for it is unaffected.
            let mut doc = seeded();
            let total = ChatDoc::top_level_count(&doc, "c_crit") as u32;
            assert!(total > 0);
            ChatDoc::mark_read(&mut doc, "c_crit", "u_alice");
            assert_eq!(
                ChatDoc::unread_count(&doc, "c_crit", "u_alice"),
                0,
                "alice caught up"
            );
            assert_eq!(
                ChatDoc::unread_count(&doc, "c_crit", "u_bob"),
                total,
                "bob's unread is untouched by alice reading"
            );
        }

        // Phase: two_peers_converge_on_a_reaction.
        {
            // Peer A seeds; peer B adopts A's state, both react, then merge.
            let mut a = seeded();
            let mut b = AutoCommit::load(&a.save()).unwrap();
            ChatDoc::toggle_reaction(&mut a, "c_crit", "m_me1", "👍", "u_aria");
            ChatDoc::toggle_reaction(&mut b, "c_crit", "m_me1", "👍", "u_milo");
            let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_loaded).unwrap();
            let msgs = ChatDoc::list_messages(&a, "c_crit");
            let m = msgs.iter().find(|m| m.id == "m_me1").unwrap();
            let thumbs = m.reactions.iter().find(|r| r.emoji == "👍").unwrap();
            assert_eq!(thumbs.count(), 2, "both reactors converge on one emoji");
        }

        // Phase: concurrent_voice_joins_converge_to_the_full_roster.
        {
            // Two peers join the same voice channel before syncing. Because the
            // voice-membership map is pre-created with the channel (shared object),
            // their concurrent joins add distinct user keys to the *same* map and a
            // merge keeps BOTH — no silently-dropped participant.
            let mut a = AutoCommit::new();
            ChatDoc::seed_starter(&mut a, "u_alice", "Alice", "AL");
            let mut b = AutoCommit::load(&a.save()).unwrap();
            // Concurrent, un-synced joins.
            ChatDoc::voice_join(&mut a, "c_lounge", "u_alice");
            ChatDoc::voice_set_ticket(&mut a, "c_lounge", "u_alice", "iroh://alice");
            ChatDoc::voice_join(&mut b, "c_lounge", "u_bob");
            ChatDoc::voice_set_ticket(&mut b, "c_lounge", "u_bob", "iroh://bob");
            // Merge both ways.
            let mut b2 = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b2).unwrap();
            let members = ChatDoc::list_voice_members(&a, "c_lounge");
            assert_eq!(
                members.len(),
                2,
                "both concurrent joiners survive the merge"
            );
            assert!(
                members
                    .iter()
                    .any(|m| m.user == "u_alice" && m.ticket.as_deref() == Some("iroh://alice"))
            );
            assert!(
                members
                    .iter()
                    .any(|m| m.user == "u_bob" && m.ticket.as_deref() == Some("iroh://bob"))
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn attachments_are_portable_and_bounded() {
        // Phase: video_attachment_round_trips_through_the_crdt.
        {
            // A video share references a portable content hash (no bytes in the
            // CRDT) and reads back with its resolution + size intact.
            let mut doc = seeded();
            ChatDoc::push_message(
                &mut doc,
                "c_general",
                NewMessage {
                    author: model::ME.into(),
                    ts: NOW,
                    attachment: Some(Attachment::Video {
                        name: "demo.mp4".into(),
                        blob: "a".repeat(64),
                        w: 1920,
                        h: 1080,
                        size: 4_200_000,
                    }),
                    ..Default::default()
                },
            );
            let msgs = ChatDoc::list_messages(&doc, "c_general");
            match msgs.last().and_then(|m| m.attachment.clone()) {
                Some(Attachment::Video {
                    name,
                    blob,
                    w,
                    h,
                    size,
                }) => {
                    assert_eq!(name, "demo.mp4");
                    assert_eq!(blob, "a".repeat(64));
                    assert_eq!((w, h), (1920, 1080));
                    assert_eq!(size, 4_200_000);
                }
                other => panic!("expected a Video attachment, got {other:?}"),
            }
        }

        // Phase: attachment_blobs_are_portable_hashes_not_paths.
        {
            let bytes = b"shared attachment bytes";
            let content = ContentStore::memory();
            let hash =
                ChatDoc::store_attachment_blob(&content, bytes).expect("small blob accepted");
            assert_eq!(hash.len(), 64);
            assert!(hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert_eq!(content.read(&hash), Some(bytes.to_vec()));
        }

        // Phase: attachment_blob_import_fails_closed_at_the_size_limit.
        {
            let too_large = vec![0; MAX_ATTACHMENT_BYTES as usize + 1];
            assert!(ChatDoc::store_attachment_blob(&ContentStore::memory(), &too_large).is_none());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one labelled phase per original scenario, each with its own fresh fixture"
    )]
    fn agent_work_is_granular_crdt_state_that_merges_without_loss_or_hybrids() {
        // Phase: agent_work_is_native_granular_crdt_state_and_merges_independent_updates.
        {
            use crate::model::{
                AgentReasoning, AgentRoute, AgentStep, AgentStepOutcome, AgentWork, AgentWorkState,
            };

            let mut a = seeded();
            let message = ChatDoc::push_message_with_id(
                &mut a,
                "c_crit",
                "m_native_work",
                NewMessage {
                    author: "agent_claude".into(),
                    ts: NOW,
                    ..Default::default()
                },
            );
            let taken = AgentWork::new(AgentWorkState::Taken, NOW);
            assert!(ChatDoc::set_message_work(
                &mut a, "c_crit", &message, &taken, None
            ));
            let message_obj = ChatDoc::find_message_obj(&a, "c_crit", &message).unwrap();
            assert!(
                map_at(&a, &message_obj, "work").is_some(),
                "new agent records are CRDT maps, never JSON scalars"
            );

            let mut b = AutoCommit::load(&a.save()).unwrap();
            let explained = taken.clone().because(AgentReasoning {
                route: AgentRoute::Floor,
                why: "The operation name matched the request.".into(),
            });
            assert!(ChatDoc::set_message_work(
                &mut a, "c_crit", &message, &explained, None
            ));
            let record = map_at(
                &a,
                &ChatDoc::find_message_obj(&a, "c_crit", &message).unwrap(),
                "work",
            )
            .unwrap();
            assert!(
                map_at(&a, &record, "reasoning").is_none(),
                "route and explanation must not be independently mergeable fields"
            );
            assert_eq!(
                serde_json::from_str::<Option<AgentReasoning>>(
                    &read_str(&a, &record, "reasoning").unwrap_or_default()
                )
                .unwrap(),
                explained.reasoning,
                "reasoning is one explicitly atomic authored record"
            );
            let traced = taken.clone().with_trace(vec![AgentStep::authored(
                "chat.message:m_me1",
                "Pin message",
                AgentStepOutcome::Applied,
                0,
            )]);
            assert!(ChatDoc::set_message_work(
                &mut b, "c_crit", &message, &traced, None
            ));

            let mut b_changes = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_changes).unwrap();
            let merged = ChatDoc::list_messages(&a, "c_crit")
                .into_iter()
                .find(|row| row.id == message)
                .and_then(|row| row.work)
                .unwrap();
            assert_eq!(merged.state, AgentWorkState::Taken);
            assert_eq!(merged.reasoning, explained.reasoning);
            assert_eq!(merged.trace, traced.trace);
        }

        // Phase: concurrent_agent_trace_appends_keep_both_authored_steps.
        {
            use crate::model::{AgentStep, AgentStepOutcome, AgentWork, AgentWorkState};

            let mut a = seeded();
            let message = ChatDoc::push_message_with_id(
                &mut a,
                "c_crit",
                "m_concurrent_trace",
                NewMessage {
                    author: "agent_claude".into(),
                    ts: NOW,
                    ..Default::default()
                },
            );
            let taken = AgentWork::new(AgentWorkState::Taken, NOW);
            ChatDoc::set_message_work(&mut a, "c_crit", &message, &taken, None);
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let left = AgentStep::authored(
                "chat.message:m_me1",
                "Pin message",
                AgentStepOutcome::Asked,
                1,
            );
            let right = AgentStep::authored(
                "chat.channel:c_crit",
                "Mark as read",
                AgentStepOutcome::Applied,
                1,
            );
            let a_work = taken.clone().with_trace(vec![left.clone()]);
            let b_work = taken.with_trace(vec![right.clone()]);
            ChatDoc::set_message_work(&mut a, "c_crit", &message, &a_work, None);
            ChatDoc::set_message_work(&mut b, "c_crit", &message, &b_work, None);

            let mut b_changes = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_changes).unwrap();
            let trace = ChatDoc::list_messages(&a, "c_crit")
                .into_iter()
                .find(|row| row.id == message)
                .and_then(|row| row.work)
                .unwrap()
                .trace;
            assert_eq!(trace.len(), 2, "concurrent appends must not collide");
            assert!(trace.iter().any(|step| step.id == left.id));
            assert!(trace.iter().any(|step| step.id == right.id));
            assert!(
                trace.windows(2).all(|pair| {
                    (pair[0].sequence, &pair[0].id) <= (pair[1].sequence, &pair[1].id)
                }),
                "equal sequence keys use step id as a deterministic tie-break"
            );
        }

        // Phase: stale_agent_trace_update_cannot_tombstone_a_step_learned_later.
        {
            use crate::model::{AgentStep, AgentStepOutcome, AgentWork, AgentWorkState};

            let mut a = seeded();
            let message = ChatDoc::push_message_with_id(
                &mut a,
                "c_crit",
                "m_delayed_trace",
                NewMessage {
                    author: "agent_claude".into(),
                    ts: NOW,
                    ..Default::default()
                },
            );
            let base = AgentWork::new(AgentWorkState::Taken, NOW);
            ChatDoc::set_message_work(&mut a, "c_crit", &message, &base, None);
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let from_a = AgentStep::authored(
                "chat.message:m_me1",
                "Pin message",
                AgentStepOutcome::Applied,
                1,
            );
            let from_b = AgentStep::authored(
                "chat.channel:c_crit",
                "Mark as read",
                AgentStepOutcome::Applied,
                2,
            );
            // A authors its payload before it has seen B's append, but delivery is
            // deliberately delayed until after B's change has merged.
            let stale_a_payload = base.clone().with_trace(vec![from_a.clone()]);
            let b_payload = base.with_trace(vec![from_b.clone()]);
            ChatDoc::set_message_work(&mut b, "c_crit", &message, &b_payload, None);
            let mut b_changes = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_changes).unwrap();
            ChatDoc::set_message_work(&mut a, "c_crit", &message, &stale_a_payload, None);

            let trace = ChatDoc::list_messages(&a, "c_crit")
                .into_iter()
                .find(|row| row.id == message)
                .and_then(|row| row.work)
                .unwrap()
                .trace;
            assert!(trace.iter().any(|step| step.id == from_a.id));
            assert!(
                trace.iter().any(|step| step.id == from_b.id),
                "a stale payload must not infer a collaborator's append is deleted"
            );
        }

        // Phase: concurrent_agent_transitions_are_never_hybridized.
        {
            use crate::model::{AgentWork, AgentWorkState};

            let mut a = seeded();
            let message = ChatDoc::push_message_with_id(
                &mut a,
                "c_crit",
                "m_transition_merge",
                NewMessage {
                    author: "agent_claude".into(),
                    ts: NOW,
                    ..Default::default()
                },
            );
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let left = AgentWork::new(
                AgentWorkState::Doing {
                    did: "Pin message".into(),
                    about: "m_me1".into(),
                },
                NOW + 10,
            );
            let right = AgentWork::new(
                AgentWorkState::Done {
                    did: "Mark as read".into(),
                },
                NOW + 20,
            );
            ChatDoc::set_message_work(&mut a, "c_crit", &message, &left, None);
            ChatDoc::set_message_work(&mut b, "c_crit", &message, &right, None);
            let mut b_changes = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_changes).unwrap();

            let merged = ChatDoc::list_messages(&a, "c_crit")
                .into_iter()
                .find(|row| row.id == message)
                .and_then(|row| row.work)
                .unwrap();
            assert!(
                (merged.state == left.state && merged.since == left.since)
                    || (merged.state == right.state && merged.since == right.since),
                "merge must choose one complete authored transition, not a hybrid: {merged:?}"
            );
        }
    }

    #[test]
    fn a_composer_draft_is_created_by_the_action() {
        use tilo_actions::Action as _;
        let mut doc = ui::automerge::AutoCommit::new();
        crate::actions::ChatAction::EnsureRichDraft {
            key: "channel-1".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert!(ChatDoc::rich_draft_is_settled(&doc, "channel-1"));
        let first = ChatDoc::ensure_rich_draft(&mut doc, "channel-1");
        crate::actions::ChatAction::EnsureRichDraft {
            key: "channel-1".into(),
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            ChatDoc::ensure_rich_draft(&mut doc, "channel-1"),
            first,
            "a second ensure keeps the same draft"
        );
        assert!(
            crate::actions::ChatAction::EnsureRichDraft { key: String::new() }
                .apply(&mut doc)
                .is_err()
        );
    }

    #[test]
    fn a_new_chat_record_starts_from_the_seed_action() {
        use tilo_actions::Action as _;
        let mut doc = automerge::AutoCommit::new();
        crate::actions::ChatAction::SeedStarter {
            id: "person-1".into(),
            name: "Ada".into(),
            initials: "Ad".into(),
            color_idx: 3,
        }
        .apply(&mut doc)
        .unwrap();
        assert!(ChatDoc::is_seeded(&doc));
        assert!(
            ChatDoc::list_channels(&doc)
                .iter()
                .any(|channel| channel.name == "general")
        );
        crate::actions::ChatAction::SeedStarter {
            id: "person-1".into(),
            name: "Ada".into(),
            initials: "Ad".into(),
            color_idx: 3,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(
            ChatDoc::list_channels(&doc)
                .iter()
                .filter(|channel| channel.name == "general")
                .count(),
            1
        );
    }
}
