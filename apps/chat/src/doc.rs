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
//! │      Message = { id, author, ts, text(Text), edited(bool),
//! │                  parent(Str ""), reactions(List<{emoji,user}>),
//! │                  history(List<{ts,text}>), attachment(Map?) }
//! ├── "voice"        (Map<chId,  Map<userId,{talking,muted,deafened,audio_only}>>)
//! ├── "read"         (Map<userId, Map<chId, i64>>) # each peer's read watermark
//! └── "drafts"       (Map<chId, Text>)           # per-channel composer body
//! ```
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

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use uuid::Uuid;

use crate::model::{
    Attachment, Category, Channel, ChannelKind, EditRecord, Message, Presence, Reaction, User,
    VoiceMember, Workspace,
};

/// Deterministic "now" for the seeded fixture so the offline screenshot is
/// stable (epoch → 2026-06-30 12:40 UTC). Runtime sends use wall-clock.
pub const NOW: i64 = 1_782_477_600;

/// A message to append, assembled by the seed or the live composer.
#[derive(Clone, Debug, Default)]
pub struct NewMessage {
    pub author: String,
    pub ts: i64,
    pub text: String,
    pub parent: Option<String>,
    pub reactions: Vec<(String, Vec<String>)>,
    pub attachment: Option<Attachment>,
    pub history: Vec<EditRecord>,
    pub edited: bool,
}

pub struct ChatDoc;

// ---------------------------------------------------------------------------
// Low-level scalar/object accessors
// ---------------------------------------------------------------------------

fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> String {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
            ScalarValue::Str(s) => s.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    }
}

fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> i64 {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => v.to_i64().unwrap_or(0),
        _ => 0,
    }
}

fn get_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> bool {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(v), _))) => matches!(v.as_ref(), ScalarValue::Boolean(true)),
        _ => false,
    }
}

fn get_text(doc: &AutoCommit, obj: &ObjId, key: &str) -> String {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Text), t))) => doc.text(&t).unwrap_or_default(),
        _ => String::new(),
    }
}

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

fn get_or_map(doc: &mut AutoCommit, obj: &ObjId, key: &str) -> ObjId {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::Map), o))) => o,
        _ => doc.put_object(obj, key, ObjType::Map).expect("put map"),
    }
}

fn get_or_list(doc: &mut AutoCommit, obj: &ObjId, key: &str) -> ObjId {
    match doc.get(obj, key) {
        Ok(Some((Value::Object(ObjType::List), o))) => o,
        _ => doc.put_object(obj, key, ObjType::List).expect("put list"),
    }
}

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
    if letters.is_empty() { "WS".to_string() } else { letters }
}

// ---------------------------------------------------------------------------
// Seed
// ---------------------------------------------------------------------------

impl ChatDoc {
    pub fn is_seeded(doc: &AutoCommit) -> bool {
        get_bool(doc, &ROOT, "seeded")
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

        Self::put_user(doc, me_id, me_name, me_initials, 0, Presence::Online);
        Self::put_workspace(doc, "ws_home", "My Workspace", "MW", 0, 0);
        Self::put_category(doc, "cat_home_text", "ws_home", "Channels", 0, false);
        Self::put_category(doc, "cat_home_voice", "ws_home", "Voice", 1, false);
        Self::put_channel(doc, "c_general", "ws_home", "cat_home_text", "general", "", ChannelKind::Text, 0);
        Self::put_channel(doc, "c_lounge", "ws_home", "cat_home_voice", "Lounge", "", ChannelKind::Voice, 0);
    }

    /// Populate the document with a believable demo workspace if it is
    /// empty. **Demo/screenshot only** — the live app seeds via
    /// [`Self::seed_starter`] instead. Idempotent via the `seeded` guard.
    pub fn seed_if_empty(doc: &mut AutoCommit) {
        if Self::is_seeded(doc) {
            return;
        }
        let _ = doc.put(&ROOT, "seeded", true);

        // --- users -------------------------------------------------------
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

        // --- workspaces --------------------------------------------------
        for (id, name, glyph, color, order) in [
            ("ws_acme", "Acme Design", "AD", 0usize, 0u32),
            ("ws_qos", "qOS Core", "QC", 1, 1),
            ("ws_jam", "Weekend Jam", "WJ", 3, 2),
        ] {
            Self::put_workspace(doc, id, name, glyph, color, order);
        }

        // --- categories --------------------------------------------------
        // Channels live under a named category; "" is the implicit
        // uncategorised group rendered first.
        for (id, ws, name, order) in [
            ("cat_acme_main", "ws_acme", "Channels", 0u32),
            ("cat_acme_voice", "ws_acme", "Voice", 1),
            ("cat_qos_eng", "ws_qos", "Engineering", 0),
            ("cat_qos_voice", "ws_qos", "Voice", 1),
            ("cat_jam_main", "ws_jam", "Hangout", 0),
        ] {
            Self::put_category(doc, id, ws, name, order, false);
        }

        // --- channels ----------------------------------------------------
        // Unread badges are not seeded — they derive from genuinely unseen
        // messages (the `read` watermark). #general carries a welcome message
        // the demo peer hasn't opened, so it shows a real "1" out of the box.
        let chans: &[(&str, &str, &str, &str, &str, ChannelKind, u32)] = &[
            ("c_general", "ws_acme", "cat_acme_main", "general", "Company-wide announcements", ChannelKind::Text, 0),
            ("c_crit", "ws_acme", "cat_acme_main", "design-crit", "Share work, get sharp feedback", ChannelKind::Text, 1),
            ("c_frontend", "ws_acme", "cat_acme_main", "frontend", "ui toolkit + app shells", ChannelKind::Text, 2),
            ("c_random", "ws_acme", "cat_acme_main", "random", "Off-topic & watercooler", ChannelKind::Text, 3),
            ("c_standup", "ws_acme", "cat_acme_voice", "Daily Standup", "", ChannelKind::Voice, 0),
            ("c_huddle", "ws_acme", "cat_acme_voice", "Design Huddle", "", ChannelKind::Voice, 1),
            ("c_kernel", "ws_qos", "cat_qos_eng", "kernel", "bootc + immutable base", ChannelKind::Text, 0),
            ("c_compositor", "ws_qos", "cat_qos_eng", "compositor", "2D clusters & sandboxing", ChannelKind::Text, 1),
            ("c_fssync", "ws_qos", "cat_qos_eng", "fs-sync", "iroh projection & hydration", ChannelKind::Text, 2),
            ("c_releases", "ws_qos", "cat_qos_eng", "releases", "ship logs", ChannelKind::Text, 3),
            ("c_warroom", "ws_qos", "cat_qos_voice", "War Room", "", ChannelKind::Voice, 0),
            ("c_ideas", "ws_jam", "cat_jam_main", "ideas", "weekend hacks", ChannelKind::Text, 0),
            ("c_music", "ws_jam", "cat_jam_main", "music", "now playing", ChannelKind::Text, 1),
        ];
        for (id, ws, cat, name, topic, kind, order) in chans {
            Self::put_channel(doc, id, ws, cat, name, topic, *kind, *order);
        }

        // --- voice membership: a lively Design Huddle ---------------------
        // Mirrors the call overlay's demo roster so the sidebar presence and
        // the overlay agree out of the box. Aria is the active speaker.
        for (user, talking, muted, audio_only) in [
            ("u_aria", true, false, false),
            ("u_milo", false, true, false),
            ("u_sora", false, false, true),
        ] {
            Self::voice_join(doc, "c_huddle", user);
            Self::voice_set_state(doc, "c_huddle", user, talking, muted, false, audio_only);
        }

        // --- a real conversation in #design-crit -------------------------
        let m = |off_min: i64| NOW - off_min * 60;
        let parent_id = "m_concept";
        let convo: Vec<(&str, NewMessage)> = vec![
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
                        path: crate::images::DEMO_CALL_GRID.into(),
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
            (
                "m_devon",
                NewMessage {
                    author: "u_devon".into(),
                    ts: m(70),
                    text: "Dropping the transport notes here so they don't get lost.".into(),
                    attachment: Some(Attachment::File {
                        name: "moq-over-iroh.md".into(),
                        path: "/var/workspace/notes/moq-over-iroh.md".into(),
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
                        path: "/var/workspace/design/tile-promotion.webm".into(),
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
        ];
        for (id, nm) in convo {
            Self::push_message_with_id(doc, "c_crit", id, nm);
        }

        // --- a thread hanging off the concept image ----------------------
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

        // a little life in #general too
        Self::push_message(
            doc,
            "c_general",
            NewMessage {
                author: "u_iris".into(),
                ts: m(220),
                text: "Welcome to Acme Design on qOS 👋 Channels relate to the workspace you're standing in — switch workspaces from the rail on the left.".into(),
                reactions: vec![("👋".into(), vec!["u_me".into(), "u_aria".into(), "u_milo".into()])],
                ..Default::default()
            },
        );
    }

    // -----------------------------------------------------------------------
    // Writers
    // -----------------------------------------------------------------------

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
        let _ = doc.put(&w, "order", order as i64);
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
        let _ = doc.put(&cat, "order", order as i64);
        let _ = doc.put(&cat, "collapsed", collapsed);
    }

    #[allow(clippy::too_many_arguments)]
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
        let _ = doc.put(&c, "order", order as i64);
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
    }

    fn messages_list_mut(doc: &mut AutoCommit, channel: &str) -> ObjId {
        let root_map = get_or_map(doc, &ROOT, "messages");
        get_or_list(doc, &root_map, channel)
    }

    /// Append a message with a caller-chosen id (used by the seed so the
    /// thread parent / reaction targets are stable string keys).
    pub fn push_message_with_id(
        doc: &mut AutoCommit,
        channel: &str,
        id: &str,
        nm: NewMessage,
    ) -> String {
        let list = Self::messages_list_mut(doc, channel);
        let idx = doc.length(&list);
        let block = doc.insert_object(&list, idx, ObjType::Map).expect("insert msg");
        let _ = doc.put(&block, "id", id);
        let _ = doc.put(&block, "author", nm.author.as_str());
        let _ = doc.put(&block, "ts", nm.ts);
        let _ = doc.put(&block, "parent", nm.parent.clone().unwrap_or_default());
        let _ = doc.put(&block, "edited", nm.edited);

        let text = doc.put_object(&block, "text", ObjType::Text).expect("text");
        if !nm.text.is_empty() {
            let _ = doc.splice_text(&text, 0, 0, &nm.text);
        }

        // reactions — an append-only List of {emoji,user} entries. Modelling
        // each reaction as its own list element (rather than a Map keyed by
        // emoji holding a per-emoji list) is what makes two peers reacting
        // with the *same new emoji* concurrently converge: independent
        // appends to one List both survive a merge, whereas two competing
        // objects under the same map key would not.
        let rx = doc.put_object(&block, "reactions", ObjType::List).expect("rx");
        let mut ri = 0usize;
        for (emoji, users) in &nm.reactions {
            for u in users {
                let e = doc.insert_object(&rx, ri, ObjType::Map).expect("rxentry");
                let _ = doc.put(&e, "emoji", emoji.as_str());
                let _ = doc.put(&e, "user", u.as_str());
                ri += 1;
            }
        }

        // history
        let hist = doc.put_object(&block, "history", ObjType::List).expect("hist");
        for (i, rec) in nm.history.iter().enumerate() {
            let h = doc.insert_object(&hist, i, ObjType::Map).expect("hrec");
            let _ = doc.put(&h, "ts", rec.ts);
            let _ = doc.put(&h, "text", rec.text.as_str());
        }

        // attachment
        if let Some(att) = &nm.attachment {
            let a = doc.put_object(&block, "attachment", ObjType::Map).expect("att");
            match att {
                Attachment::Image { name, path, w, h } => {
                    let _ = doc.put(&a, "kind", "image");
                    let _ = doc.put(&a, "name", name.as_str());
                    let _ = doc.put(&a, "path", path.as_str());
                    let _ = doc.put(&a, "w", *w as i64);
                    let _ = doc.put(&a, "h", *h as i64);
                }
                Attachment::Video { name, path, w, h, size } => {
                    let _ = doc.put(&a, "kind", "video");
                    let _ = doc.put(&a, "name", name.as_str());
                    let _ = doc.put(&a, "path", path.as_str());
                    let _ = doc.put(&a, "w", *w as i64);
                    let _ = doc.put(&a, "h", *h as i64);
                    let _ = doc.put(&a, "size", *size as i64);
                }
                Attachment::File { name, path, size } => {
                    let _ = doc.put(&a, "kind", "file");
                    let _ = doc.put(&a, "name", name.as_str());
                    let _ = doc.put(&a, "path", path.as_str());
                    let _ = doc.put(&a, "size", *size as i64);
                }
            }
        }
        id.to_string()
    }

    /// Append a message with a fresh UUID id.
    pub fn push_message(doc: &mut AutoCommit, channel: &str, nm: NewMessage) -> String {
        let id = format!("m_{}", Uuid::new_v4().simple());
        Self::push_message_with_id(doc, channel, &id, nm)
    }

    fn find_message_obj(doc: &AutoCommit, channel: &str, msg_id: &str) -> Option<ObjId> {
        let root_map = map_at(doc, &ROOT, "messages")?;
        let list = list_at(doc, &root_map, channel)?;
        for i in 0..doc.length(&list) {
            if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) {
                if get_str(doc, &m, "id") == msg_id {
                    return Some(m);
                }
            }
        }
        None
    }

    /// Toggle `user`'s `emoji` reaction on a message. Creates the reactions
    /// map / emoji list as needed and prunes an emoji once its last reactor
    /// leaves — converges with the pure [`crate::model::toggle_reaction`].
    pub fn toggle_reaction(
        doc: &mut AutoCommit,
        channel: &str,
        msg_id: &str,
        emoji: &str,
        user: &str,
    ) {
        let Some(block) = Self::find_message_obj(doc, channel, msg_id) else { return };
        let list = get_or_list(doc, &block, "reactions");
        // Find an existing (emoji,user) entry.
        let mut found: Option<usize> = None;
        for i in 0..doc.length(&list) {
            if let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&list, i) {
                if get_str(doc, &e, "emoji") == emoji && get_str(doc, &e, "user") == user {
                    found = Some(i);
                    break;
                }
            }
        }
        match found {
            Some(i) => {
                let _ = doc.delete(&list, i);
            }
            None => {
                let n = doc.length(&list);
                let e = doc.insert_object(&list, n, ObjType::Map).expect("rxentry");
                let _ = doc.put(&e, "emoji", emoji);
                let _ = doc.put(&e, "user", user);
            }
        }
    }

    /// The index of an existing `(emoji, user)` reaction entry on `list`, or
    /// `None`. Shared by the toggle / add / remove writers so they agree on what
    /// "already reacted" means.
    fn reaction_index(doc: &AutoCommit, list: &ObjId, emoji: &str, user: &str) -> Option<usize> {
        for i in 0..doc.length(list) {
            if let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(list, i) {
                if get_str(doc, &e, "emoji") == emoji && get_str(doc, &e, "user") == user {
                    return Some(i);
                }
            }
        }
        None
    }

    /// Add `user`'s `emoji` reaction to a message. Idempotent: a second add of
    /// the same `(emoji, user)` is a no-op, so replaying an add never
    /// double-counts. The counterpart of [`Self::remove_reaction`]; the on-screen
    /// toggle picks whichever the current state calls for at authoring time.
    pub fn add_reaction(doc: &mut AutoCommit, channel: &str, msg_id: &str, emoji: &str, user: &str) {
        let Some(block) = Self::find_message_obj(doc, channel, msg_id) else { return };
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
    pub fn remove_reaction(doc: &mut AutoCommit, channel: &str, msg_id: &str, emoji: &str, user: &str) {
        let Some(block) = Self::find_message_obj(doc, channel, msg_id) else { return };
        let Some(list) = list_at(doc, &block, "reactions") else { return };
        if let Some(i) = Self::reaction_index(doc, &list, emoji, user) {
            let _ = doc.delete(&list, i);
        }
    }

    /// Delete a message row from a channel. Idempotent — an unknown id is a
    /// no-op. Threaded replies (if any) are left in place; they simply lose their
    /// parent card, which is recoverable.
    pub fn delete_message(doc: &mut AutoCommit, channel: &str, msg_id: &str) {
        let Some(root_map) = map_at(doc, &ROOT, "messages") else { return };
        let Some(list) = list_at(doc, &root_map, channel) else { return };
        for i in 0..doc.length(&list) {
            if let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) {
                if get_str(doc, &m, "id") == msg_id {
                    let _ = doc.delete(&list, i);
                    return;
                }
            }
        }
    }

    /// Set (or clear) a message's pinned flag. Shared truth: every peer sees the
    /// same pins. Guarded so a redundant write doesn't churn the document.
    pub fn set_message_pinned(doc: &mut AutoCommit, channel: &str, msg_id: &str, pinned: bool) {
        let Some(block) = Self::find_message_obj(doc, channel, msg_id) else { return };
        if get_bool(doc, &block, "pinned") != pinned {
            let _ = doc.put(&block, "pinned", pinned);
        }
    }

    /// Replace a message's body, archiving the previous text into its edit
    /// history and flagging it edited.
    pub fn edit_message(
        doc: &mut AutoCommit,
        channel: &str,
        msg_id: &str,
        new_text: &str,
        ts: i64,
    ) {
        let Some(block) = Self::find_message_obj(doc, channel, msg_id) else { return };
        let old = get_text(doc, &block, "text");
        if old == new_text {
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

    /// The per-channel draft `Text` object, created on demand. The composer
    /// splices directly into this so a draft is collaborative state too.
    pub fn draft_obj(doc: &mut AutoCommit, channel: &str) -> ObjId {
        let drafts = get_or_map(doc, &ROOT, "drafts");
        get_or_text(doc, &drafts, channel)
    }

    pub fn draft_text(doc: &AutoCommit, channel: &str) -> String {
        match map_at(doc, &ROOT, "drafts") {
            Some(drafts) => get_text(doc, &drafts, channel),
            None => String::new(),
        }
    }

    /// Replace the draft body with `text`, diff-spliced so only the changed
    /// span is written (never a whole-text rewrite per keystroke — see
    /// `ui/HIG.md` §6). The composer calls this on every edit so the draft
    /// stays collaborative + persistent.
    pub fn set_draft(doc: &mut AutoCommit, channel: &str, text: &str) {
        let obj = Self::draft_obj(doc, channel);
        ui::doc::splice_text_diffed(doc, &obj, text);
    }

    pub fn clear_draft(doc: &mut AutoCommit, channel: &str) {
        let obj = Self::draft_obj(doc, channel);
        ui::doc::splice_text_diffed(doc, &obj, "");
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
        Self::list_messages(doc, channel).iter().filter(|m| m.is_top_level()).count()
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
        if get_str(doc, &u, "name") != name {
            let _ = doc.put(&u, "name", name);
            wrote = true;
        }
        if get_str(doc, &u, "initials") != initials {
            let _ = doc.put(&u, "initials", initials);
            wrote = true;
        }
        if get_i64(doc, &u, "color_idx") != color as i64 {
            let _ = doc.put(&u, "color_idx", color as i64);
            wrote = true;
        }
        if get_str(doc, &u, "status") != status.as_str() {
            let _ = doc.put(&u, "status", status.as_str());
            wrote = true;
        }
        wrote
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
        match map_at(doc, &ROOT, "read").and_then(|r| map_at(doc, &r, user)) {
            Some(m) => get_i64(doc, &m, channel),
            None => 0,
        }
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
        if get_i64(doc, &m, channel) != count {
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
            .map(|m| m + 1)
            .unwrap_or(0)
    }

    /// Next free `order` for a channel within `(ws, category)`.
    fn next_channel_order(doc: &AutoCommit, ws: &str, category: &str) -> u32 {
        Self::list_channels(doc)
            .into_iter()
            .filter(|c| c.workspace == ws && c.category == category)
            .map(|c| c.order)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0)
    }

    /// Create a category at the end of `ws`. Returns its fresh id.
    pub fn create_category(doc: &mut AutoCommit, ws: &str, name: &str) -> String {
        let id = format!("cat_{}", Uuid::new_v4().simple());
        let order = Self::next_category_order(doc, ws);
        Self::put_category(doc, &id, ws, name, order, false);
        id
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
            .map(|m| m + 1)
            .unwrap_or(0);
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
        if let Some(c) = map_at(doc, &map_at(doc, &ROOT, "channels").unwrap_or(ROOT), id) {
            if get_bool(doc, &c, "archived") != archived {
                let _ = doc.put(&c, "archived", archived);
            }
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
            .map(|c| c.collapsed)
            .unwrap_or(false);
        Self::set_category_collapsed(doc, id, !collapsed);
    }

    /// Delete a channel and its messages, voice membership, and draft.
    pub fn delete_channel(doc: &mut AutoCommit, id: &str) {
        if let Some(chans) = map_at(doc, &ROOT, "channels") {
            let _ = doc.delete(&chans, id);
        }
        if let Some(msgs) = map_at(doc, &ROOT, "messages") {
            let _ = doc.delete(&msgs, id);
        }
        if let Some(voice) = map_at(doc, &ROOT, "voice") {
            let _ = doc.delete(&voice, id);
        }
        if let Some(drafts) = map_at(doc, &ROOT, "drafts") {
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
        let Some(ch) = Self::list_channels(doc).into_iter().find(|c| c.id == id) else { return };
        let order = Self::next_channel_order(doc, &ch.workspace, category);
        if let Some(c) = map_at(doc, &map_at(doc, &ROOT, "channels").unwrap_or(ROOT), id) {
            let _ = doc.put(&c, "category", category);
            let _ = doc.put(&c, "order", order as i64);
        }
    }

    /// Move a channel one slot up/down within its category by swapping the
    /// `order` of the adjacent sibling. `up == true` moves it earlier.
    pub fn reorder_channel(doc: &mut AutoCommit, id: &str, up: bool) {
        let Some(ch) = Self::list_channels(doc).into_iter().find(|c| c.id == id) else { return };
        let mut siblings: Vec<Channel> = Self::list_channels(doc)
            .into_iter()
            .filter(|c| c.workspace == ch.workspace && c.category == ch.category)
            .collect();
        siblings.sort_by_key(|c| c.order);
        let Some(pos) = siblings.iter().position(|c| c.id == id) else { return };
        let swap = if up { pos.checked_sub(1) } else { pos.checked_add(1).filter(|&p| p < siblings.len()) };
        let Some(swap) = swap else { return };
        let a_order = siblings[pos].order;
        let b_order = siblings[swap].order;
        let chans = map_at(doc, &ROOT, "channels").unwrap_or(ROOT);
        if let Some(a) = map_at(doc, &chans, &siblings[pos].id) {
            let _ = doc.put(&a, "order", b_order as i64);
        }
        if let Some(b) = map_at(doc, &chans, &siblings[swap].id) {
            let _ = doc.put(&b, "order", a_order as i64);
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
        if let Some(voice) = map_at(doc, &ROOT, "voice") {
            if let Some(ch) = map_at(doc, &voice, channel) {
                let _ = doc.delete(&ch, user);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn voice_set_state(
        doc: &mut AutoCommit,
        channel: &str,
        user: &str,
        talking: bool,
        muted: bool,
        deafened: bool,
        audio_only: bool,
    ) {
        let ch = Self::voice_channel_map(doc, channel);
        let m = get_or_map(doc, &ch, user);
        let _ = doc.put(&m, "talking", talking);
        let _ = doc.put(&m, "muted", muted);
        let _ = doc.put(&m, "deafened", deafened);
        let _ = doc.put(&m, "audio_only", audio_only);
    }

    pub fn voice_set_field(doc: &mut AutoCommit, channel: &str, user: &str, field: &str, value: bool) {
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
        if get_bool(doc, &m, "sharing") != sharing {
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
        if get_str(doc, &m, "ticket") != ticket {
            let _ = doc.put(&m, "ticket", ticket);
        }
    }

    // -----------------------------------------------------------------------
    // Readers
    // -----------------------------------------------------------------------

    pub fn list_categories(doc: &AutoCommit) -> Vec<Category> {
        let Some(cats) = map_at(doc, &ROOT, "categories") else { return vec![] };
        let mut out: Vec<Category> = doc
            .keys(&cats)
            .filter_map(|id| {
                let cat = map_at(doc, &cats, &id)?;
                Some(Category {
                    id: id.clone(),
                    workspace: get_str(doc, &cat, "workspace"),
                    name: get_str(doc, &cat, "name"),
                    order: get_i64(doc, &cat, "order") as u32,
                    collapsed: get_bool(doc, &cat, "collapsed"),
                })
            })
            .collect();
        out.sort_by_key(|c| c.order);
        out
    }

    /// All members of a voice channel, sorted talking-first then by name-key.
    pub fn list_voice_members(doc: &AutoCommit, channel: &str) -> Vec<VoiceMember> {
        let Some(voice) = map_at(doc, &ROOT, "voice") else { return vec![] };
        let Some(ch) = map_at(doc, &voice, channel) else { return vec![] };
        let mut out: Vec<VoiceMember> = doc
            .keys(&ch)
            .filter_map(|user| {
                let m = map_at(doc, &ch, &user)?;
                let ticket = get_str(doc, &m, "ticket");
                Some(VoiceMember {
                    user: user.clone(),
                    talking: get_bool(doc, &m, "talking"),
                    muted: get_bool(doc, &m, "muted"),
                    deafened: get_bool(doc, &m, "deafened"),
                    audio_only: get_bool(doc, &m, "audio_only"),
                    sharing: get_bool(doc, &m, "sharing"),
                    ticket: if ticket.is_empty() { None } else { Some(ticket) },
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
        let Some(wss) = map_at(doc, &ROOT, "workspaces") else { return vec![] };
        let mut out: Vec<Workspace> = doc
            .keys(&wss)
            .filter_map(|id| {
                let w = map_at(doc, &wss, &id)?;
                Some(Workspace {
                    id: id.clone(),
                    name: get_str(doc, &w, "name"),
                    glyph: get_str(doc, &w, "glyph"),
                    color_idx: get_i64(doc, &w, "color_idx") as usize,
                    order: get_i64(doc, &w, "order") as u32,
                })
            })
            .collect();
        out.sort_by_key(|w| w.order);
        out
    }

    pub fn list_users(doc: &AutoCommit) -> Vec<User> {
        let Some(us) = map_at(doc, &ROOT, "users") else { return vec![] };
        doc.keys(&us)
            .filter_map(|id| {
                let u = map_at(doc, &us, &id)?;
                Some(User {
                    id: id.clone(),
                    name: get_str(doc, &u, "name"),
                    initials: get_str(doc, &u, "initials"),
                    color_idx: get_i64(doc, &u, "color_idx") as usize,
                    status: Presence::from_str(&get_str(doc, &u, "status")),
                })
            })
            .collect()
    }

    pub fn list_channels(doc: &AutoCommit) -> Vec<Channel> {
        let Some(chans) = map_at(doc, &ROOT, "channels") else { return vec![] };
        let mut out: Vec<Channel> = doc
            .keys(&chans)
            .filter_map(|id| {
                let c = map_at(doc, &chans, &id)?;
                Some(Channel {
                    id: id.clone(),
                    workspace: get_str(doc, &c, "workspace"),
                    category: get_str(doc, &c, "category"),
                    name: get_str(doc, &c, "name"),
                    topic: get_str(doc, &c, "topic"),
                    kind: ChannelKind::from_str(&get_str(doc, &c, "kind")),
                    order: get_i64(doc, &c, "order") as u32,
                    // Derived per-peer in `ChatApp::reload` from the read
                    // watermark; not stored in the shared document.
                    unread: 0,
                    archived: get_bool(doc, &c, "archived"),
                })
            })
            .collect();
        out.sort_by_key(|c| c.order);
        out
    }

    pub fn list_messages(doc: &AutoCommit, channel: &str) -> Vec<Message> {
        let Some(root_map) = map_at(doc, &ROOT, "messages") else { return vec![] };
        let Some(list) = list_at(doc, &root_map, channel) else { return vec![] };
        let mut out = Vec::new();
        for i in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), m))) = doc.get(&list, i) else { continue };
            let parent = get_str(doc, &m, "parent");
            out.push(Message {
                id: get_str(doc, &m, "id"),
                channel: channel.to_string(),
                author: get_str(doc, &m, "author"),
                ts: get_i64(doc, &m, "ts"),
                text: get_text(doc, &m, "text"),
                edited: get_bool(doc, &m, "edited"),
                history: Self::read_history(doc, &m),
                parent: if parent.is_empty() { None } else { Some(parent) },
                reactions: Self::read_reactions(doc, &m),
                attachment: Self::read_attachment(doc, &m),
                pinned: get_bool(doc, &m, "pinned"),
            });
        }
        out
    }

    fn read_reactions(doc: &AutoCommit, msg: &ObjId) -> Vec<Reaction> {
        let Some(list) = list_at(doc, msg, "reactions") else { return vec![] };
        // Group the flat (emoji,user) entries back into per-emoji buckets,
        // remembering first-seen order for a stable layout.
        let mut order: Vec<String> = Vec::new();
        let mut buckets: HashMap<String, Vec<String>> = HashMap::new();
        for i in 0..doc.length(&list) {
            let Ok(Some((Value::Object(ObjType::Map), e))) = doc.get(&list, i) else { continue };
            let emoji = get_str(doc, &e, "emoji");
            let user = get_str(doc, &e, "user");
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
        out.sort_by(|a, b| b.count().cmp(&a.count()).then_with(|| a.emoji.cmp(&b.emoji)));
        out
    }

    fn read_history(doc: &AutoCommit, msg: &ObjId) -> Vec<EditRecord> {
        let Some(h) = list_at(doc, msg, "history") else { return vec![] };
        let mut out = Vec::new();
        for i in 0..doc.length(&h) {
            if let Ok(Some((Value::Object(ObjType::Map), rec))) = doc.get(&h, i) {
                out.push(EditRecord {
                    ts: get_i64(doc, &rec, "ts"),
                    text: get_str(doc, &rec, "text"),
                });
            }
        }
        out
    }

    fn read_attachment(doc: &AutoCommit, msg: &ObjId) -> Option<Attachment> {
        let a = map_at(doc, msg, "attachment")?;
        match get_str(doc, &a, "kind").as_str() {
            "image" => Some(Attachment::Image {
                name: get_str(doc, &a, "name"),
                path: get_str(doc, &a, "path"),
                w: get_i64(doc, &a, "w") as u32,
                h: get_i64(doc, &a, "h") as u32,
            }),
            "video" => Some(Attachment::Video {
                name: get_str(doc, &a, "name"),
                path: get_str(doc, &a, "path"),
                w: get_i64(doc, &a, "w") as u32,
                h: get_i64(doc, &a, "h") as u32,
                size: get_i64(doc, &a, "size") as u64,
            }),
            "file" => Some(Attachment::File {
                name: get_str(doc, &a, "name"),
                path: get_str(doc, &a, "path"),
                size: get_i64(doc, &a, "size") as u64,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model;

    fn seeded() -> AutoCommit {
        let mut doc = AutoCommit::new();
        ChatDoc::seed_if_empty(&mut doc);
        doc
    }

    #[test]
    fn seed_starter_is_minimal_real_and_has_no_demo_content() {
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
        assert!(chans.iter().any(|c| c.id == "c_general" && c.kind == ChannelKind::Text));
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

    #[test]
    fn seed_is_idempotent() {
        let mut doc = seeded();
        let before = ChatDoc::list_channels(&doc).len();
        ChatDoc::seed_if_empty(&mut doc);
        let after = ChatDoc::list_channels(&doc).len();
        assert_eq!(before, after, "second seed must not duplicate channels");
        assert!(before >= 12);
    }

    #[test]
    fn workspaces_and_channels_round_trip() {
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

    #[test]
    fn messages_carry_reactions_thread_and_attachment() {
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

    #[test]
    fn toggle_reaction_writes_through_crdt() {
        let mut doc = seeded();
        // m_me1 starts with no reactions
        ChatDoc::toggle_reaction(&mut doc, "c_crit", "m_me1", "🔥", model::ME);
        let msgs = ChatDoc::list_messages(&doc, "c_crit");
        let m = msgs.iter().find(|m| m.id == "m_me1").unwrap();
        assert!(m.reactions.iter().any(|r| r.emoji == "🔥" && r.reacted_by(model::ME)));
        // toggling again removes it and prunes the emoji
        ChatDoc::toggle_reaction(&mut doc, "c_crit", "m_me1", "🔥", model::ME);
        let msgs = ChatDoc::list_messages(&doc, "c_crit");
        let m = msgs.iter().find(|m| m.id == "m_me1").unwrap();
        assert!(!m.reactions.iter().any(|r| r.emoji == "🔥"));
    }

    #[test]
    fn edit_message_archives_history() {
        let mut doc = seeded();
        ChatDoc::edit_message(&mut doc, "c_crit", "m_me1", "Edited body.", NOW);
        let msgs = ChatDoc::list_messages(&doc, "c_crit");
        let m = msgs.iter().find(|m| m.id == "m_me1").unwrap();
        assert_eq!(m.text, "Edited body.");
        assert!(m.edited);
        assert_eq!(m.history.len(), 1, "previous body archived");
    }

    #[test]
    fn editing_a_long_message_splices_only_the_changed_span() {
        // Editing a message re-sends its whole body. Deleting and reinserting
        // it tombstones every character on every edit, so a message that is
        // corrected a few times costs many times its own length forever.
        let mut doc = seeded();
        let body = "The deploy finished at 14:02 and the canary is green across \
                    all three regions; rolling the rest of the fleet now."
            .to_string();
        ChatDoc::edit_message(&mut doc, "c_crit", "m_me1", &body, NOW);
        let baseline = doc.save().len();

        // 30 corrections, each touching only the last word.
        for i in 0..30 {
            let edited = format!("{body} v{i}");
            ChatDoc::edit_message(&mut doc, "c_crit", "m_me1", &edited, NOW + i as i64);
        }
        let grown = doc.save().len() - baseline;

        let msgs = ChatDoc::list_messages(&doc, "c_crit");
        let m = msgs.iter().find(|m| m.id == "m_me1").unwrap();
        assert_eq!(m.text, format!("{body} v29"), "the last edit must be the body");
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

    #[test]
    fn clearing_an_unset_draft_is_free() {
        let mut doc = seeded();
        ChatDoc::draft_obj(&mut doc, "c_crit");
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

    #[test]
    fn draft_round_trips_and_send_appends() {
        let mut doc = seeded();
        let before = ChatDoc::list_messages(&doc, "c_crit").len();
        let draft = ChatDoc::draft_obj(&mut doc, "c_crit");
        let _ = doc.splice_text(&draft, 0, 0, "hi there");
        assert_eq!(ChatDoc::draft_text(&doc, "c_crit"), "hi there");
        ChatDoc::push_message(
            &mut doc,
            "c_crit",
            NewMessage { author: model::ME.into(), ts: NOW, text: "hi there".into(), ..Default::default() },
        );
        ChatDoc::clear_draft(&mut doc, "c_crit");
        assert_eq!(ChatDoc::draft_text(&doc, "c_crit"), "");
        assert_eq!(ChatDoc::list_messages(&doc, "c_crit").len(), before + 1);
    }

    #[test]
    fn channels_carry_category_and_voice_seed() {
        let doc = seeded();
        let crit = ChatDoc::list_channels(&doc).into_iter().find(|c| c.id == "c_crit").unwrap();
        assert_eq!(crit.category, "cat_acme_main");
        let cats = ChatDoc::list_categories(&doc);
        assert!(cats.iter().any(|c| c.workspace == "ws_acme" && c.name == "Voice"));
        // The huddle is pre-populated with three members; Aria is talking.
        let members = ChatDoc::list_voice_members(&doc, "c_huddle");
        assert_eq!(members.len(), 3);
        assert!(members.iter().any(|m| m.user == "u_aria" && m.talking));
        assert!(members.iter().any(|m| m.user == "u_milo" && m.muted));
        assert!(members.iter().any(|m| m.user == "u_sora" && m.audio_only));
    }

    #[test]
    fn channel_crud_roundtrips() {
        let mut doc = seeded();
        let before = ChatDoc::list_channels(&doc).len();
        let id = ChatDoc::create_channel(&mut doc, "ws_acme", "cat_acme_main", "design-ops", ChannelKind::Text);
        assert_eq!(ChatDoc::list_channels(&doc).len(), before + 1);
        ChatDoc::rename_channel(&mut doc, &id, "ops");
        let c = ChatDoc::list_channels(&doc).into_iter().find(|c| c.id == id).unwrap();
        assert_eq!(c.name, "ops");
        // Move into the voice category, then delete.
        ChatDoc::move_channel(&mut doc, &id, "cat_acme_voice");
        let c = ChatDoc::list_channels(&doc).into_iter().find(|c| c.id == id).unwrap();
        assert_eq!(c.category, "cat_acme_voice");
        ChatDoc::delete_channel(&mut doc, &id);
        assert!(ChatDoc::list_channels(&doc).into_iter().all(|c| c.id != id));
    }

    #[test]
    fn reorder_channel_swaps_adjacent_order() {
        let mut doc = seeded();
        // general(0), design-crit(1) in cat_acme_main — move design-crit up.
        let general_before = ChatDoc::list_channels(&doc).into_iter().find(|c| c.id == "c_general").unwrap().order;
        let crit_before = ChatDoc::list_channels(&doc).into_iter().find(|c| c.id == "c_crit").unwrap().order;
        assert!(crit_before > general_before);
        ChatDoc::reorder_channel(&mut doc, "c_crit", true);
        let general_after = ChatDoc::list_channels(&doc).into_iter().find(|c| c.id == "c_general").unwrap().order;
        let crit_after = ChatDoc::list_channels(&doc).into_iter().find(|c| c.id == "c_crit").unwrap().order;
        assert!(crit_after < general_after, "design-crit now sorts before general");
    }

    #[test]
    fn deleting_category_rehomes_its_channels() {
        let mut doc = seeded();
        ChatDoc::delete_category(&mut doc, "cat_acme_main");
        assert!(ChatDoc::list_categories(&doc).into_iter().all(|c| c.id != "cat_acme_main"));
        // Its channels survive, rehomed to the uncategorised group.
        let general = ChatDoc::list_channels(&doc).into_iter().find(|c| c.id == "c_general").unwrap();
        assert_eq!(general.category, "");
    }

    #[test]
    fn voice_join_leave_and_state() {
        let mut doc = seeded();
        ChatDoc::voice_join(&mut doc, "c_huddle", "u_me");
        assert_eq!(ChatDoc::voice_count(&doc, "c_huddle"), 4);
        ChatDoc::voice_set_field(&mut doc, "c_huddle", "u_me", "muted", true);
        let me = ChatDoc::list_voice_members(&doc, "c_huddle").into_iter().find(|m| m.user == "u_me").unwrap();
        assert!(me.muted);
        ChatDoc::voice_leave(&mut doc, "c_huddle", "u_me");
        assert_eq!(ChatDoc::voice_count(&doc, "c_huddle"), 3);
    }

    #[test]
    fn voice_screen_share_round_trips_through_the_shared_membership() {
        let mut doc = seeded();
        ChatDoc::voice_join(&mut doc, "c_huddle", "u_me");
        let sharing = |doc: &AutoCommit| {
            ChatDoc::list_voice_members(doc, "c_huddle")
                .into_iter()
                .find(|m| m.user == "u_me")
                .map(|m| m.sharing)
                .unwrap_or(false)
        };
        assert!(!sharing(&doc), "a fresh member is not sharing");
        ChatDoc::voice_set_sharing(&mut doc, "c_huddle", "u_me", true);
        assert!(sharing(&doc), "a started share is visible to every peer");
        ChatDoc::voice_set_sharing(&mut doc, "c_huddle", "u_me", false);
        assert!(!sharing(&doc), "a stopped share clears the shared flag");
    }

    #[test]
    fn document_survives_a_save_load_cycle() {
        // The persistence contract: send a message, save the raw bytes, load
        // them into a fresh document, and the message is still there.
        let mut doc = seeded();
        ChatDoc::push_message(
            &mut doc,
            "c_general",
            NewMessage { author: "u_me".into(), ts: NOW, text: "persisted!".into(), ..Default::default() },
        );
        let bytes = doc.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        assert!(ChatDoc::is_seeded(&reloaded), "reloaded doc keeps the seed guard");
        let msgs = ChatDoc::list_messages(&reloaded, "c_general");
        assert!(msgs.iter().any(|m| m.text == "persisted!"), "message survived reload");
    }

    #[test]
    fn create_workspace_scaffolds_a_usable_workspace() {
        let mut doc = seeded();
        let before = ChatDoc::list_workspaces(&doc).len();
        let id = ChatDoc::create_workspace(&mut doc, "Weekend Jam Two");
        let wss = ChatDoc::list_workspaces(&doc);
        assert_eq!(wss.len(), before + 1, "a workspace was created");
        let ws = wss.iter().find(|w| w.id == id).unwrap();
        assert_eq!(ws.name, "Weekend Jam Two");
        assert_eq!(ws.glyph, "WJ", "glyph is a monogram of the name");
        assert!(ws.order as usize >= before, "appended at the end of the rail");
        // It comes with a usable text channel and a voice channel to call in.
        let chans: Vec<_> =
            ChatDoc::list_channels(&doc).into_iter().filter(|c| c.workspace == id).collect();
        assert!(chans.iter().any(|c| c.name == "general" && c.kind == ChannelKind::Text));
        assert!(chans.iter().any(|c| c.kind == ChannelKind::Voice));
    }

    #[test]
    fn unread_derives_from_the_read_watermark() {
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
            NewMessage { author: "u_aria".into(), ts: NOW, text: "one more".into(), ..Default::default() },
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
        assert_eq!(ChatDoc::unread_count(&doc, "c_crit", model::ME), 1, "thread replies don't bump unread");
    }

    #[test]
    fn mark_read_is_idempotent_and_does_not_churn() {
        // Reading a channel twice with no new messages must not append a new
        // change (the watermark write is guarded on a real value change).
        let mut doc = seeded();
        ChatDoc::mark_read(&mut doc, "c_crit", model::ME);
        let heads = doc.get_heads();
        ChatDoc::mark_read(&mut doc, "c_crit", model::ME);
        assert_eq!(doc.get_heads(), heads, "a no-op mark_read writes nothing");
    }

    #[test]
    fn concurrent_voice_joins_converge_to_the_full_roster() {
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
        a.merge(&mut b2).unwrap();
        let members = ChatDoc::list_voice_members(&a, "c_lounge");
        assert_eq!(members.len(), 2, "both concurrent joiners survive the merge");
        assert!(members.iter().any(|m| m.user == "u_alice" && m.ticket.as_deref() == Some("iroh://alice")));
        assert!(members.iter().any(|m| m.user == "u_bob" && m.ticket.as_deref() == Some("iroh://bob")));
    }

    #[test]
    fn video_attachment_round_trips_through_the_crdt() {
        // A video share references the FUSE path (no bytes in the CRDT) and
        // reads back as a Video with its resolution + size intact.
        let mut doc = seeded();
        ChatDoc::push_message(
            &mut doc,
            "c_general",
            NewMessage {
                author: model::ME.into(),
                ts: NOW,
                attachment: Some(Attachment::Video {
                    name: "demo.mp4".into(),
                    path: "/var/workspace/clips/demo.mp4".into(),
                    w: 1920,
                    h: 1080,
                    size: 4_200_000,
                }),
                ..Default::default()
            },
        );
        let msgs = ChatDoc::list_messages(&doc, "c_general");
        match msgs.last().and_then(|m| m.attachment.clone()) {
            Some(Attachment::Video { name, path, w, h, size }) => {
                assert_eq!(name, "demo.mp4");
                assert_eq!(path, "/var/workspace/clips/demo.mp4");
                assert_eq!((w, h), (1920, 1080));
                assert_eq!(size, 4_200_000);
            }
            other => panic!("expected a Video attachment, got {other:?}"),
        }
    }

    #[test]
    fn ensure_user_is_guarded_and_announces_a_peer() {
        // Registering the local peer writes once, then no-ops (so it never
        // churns the doc / spams empty collab deltas), and a fresh peer id is
        // announced into the roster so others learn its name + colour.
        let mut doc = seeded();
        let wrote = ChatDoc::ensure_user(&mut doc, "u_bob", "Bob", "BO", 4, Presence::Online);
        assert!(wrote, "a new peer is announced");
        assert!(ChatDoc::list_users(&doc).iter().any(|u| u.id == "u_bob" && u.name == "Bob"));
        let again = ChatDoc::ensure_user(&mut doc, "u_bob", "Bob", "BO", 4, Presence::Online);
        assert!(!again, "an unchanged re-announce writes nothing");
    }

    #[test]
    fn read_watermarks_are_independent_per_peer() {
        // The exact multi-participant bug the identity work fixes: two peers
        // must not clobber each other's unread badges. Alice reads a channel;
        // Bob's unread for it is unaffected.
        let mut doc = seeded();
        let total = ChatDoc::top_level_count(&doc, "c_crit") as u32;
        assert!(total > 0);
        ChatDoc::mark_read(&mut doc, "c_crit", "u_alice");
        assert_eq!(ChatDoc::unread_count(&doc, "c_crit", "u_alice"), 0, "alice caught up");
        assert_eq!(
            ChatDoc::unread_count(&doc, "c_crit", "u_bob"),
            total,
            "bob's unread is untouched by alice reading"
        );
    }

    #[test]
    fn two_peers_converge_on_a_reaction() {
        // Peer A seeds; peer B adopts A's state, both react, then merge.
        let mut a = seeded();
        let mut b = AutoCommit::load(&a.save()).unwrap();
        ChatDoc::toggle_reaction(&mut a, "c_crit", "m_me1", "👍", "u_aria");
        ChatDoc::toggle_reaction(&mut b, "c_crit", "m_me1", "👍", "u_milo");
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        let msgs = ChatDoc::list_messages(&a, "c_crit");
        let m = msgs.iter().find(|m| m.id == "m_me1").unwrap();
        let thumbs = m.reactions.iter().find(|r| r.emoji == "👍").unwrap();
        assert_eq!(thumbs.count(), 2, "both reactors converge on one emoji");
    }
}
