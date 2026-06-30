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
//! ├── "channels"     (Map<chId,  {workspace,category,name,topic,kind,order,unread}>)
//! ├── "messages"     (Map<chId,  List<Message>>)
//! │      Message = { id, author, ts, text(Text), edited(bool),
//! │                  parent(Str ""), reactions(List<{emoji,user}>),
//! │                  history(List<{ts,text}>), attachment(Map?) }
//! ├── "voice"        (Map<chId,  Map<userId,{talking,muted,deafened,audio_only}>>)
//! └── "drafts"       (Map<chId, Text>)           # per-channel composer body
//! ```
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

// ---------------------------------------------------------------------------
// Seed
// ---------------------------------------------------------------------------

impl ChatDoc {
    pub fn is_seeded(doc: &AutoCommit) -> bool {
        get_bool(doc, &ROOT, "seeded")
    }

    /// Populate the document with a believable demo workspace if it is
    /// empty. Idempotent via the `seeded` guard so a second peer that
    /// adopts shared state never double-seeds. Mirrors the notes app's
    /// seed-or-adopt handshake.
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
        let chans: &[(&str, &str, &str, &str, &str, ChannelKind, u32, u32)] = &[
            ("c_general", "ws_acme", "cat_acme_main", "general", "Company-wide announcements", ChannelKind::Text, 0, 0),
            ("c_crit", "ws_acme", "cat_acme_main", "design-crit", "Share work, get sharp feedback", ChannelKind::Text, 1, 0),
            ("c_frontend", "ws_acme", "cat_acme_main", "frontend", "ui toolkit + app shells", ChannelKind::Text, 2, 3),
            ("c_random", "ws_acme", "cat_acme_main", "random", "Off-topic & watercooler", ChannelKind::Text, 3, 0),
            ("c_standup", "ws_acme", "cat_acme_voice", "Daily Standup", "", ChannelKind::Voice, 0, 0),
            ("c_huddle", "ws_acme", "cat_acme_voice", "Design Huddle", "", ChannelKind::Voice, 1, 0),
            ("c_kernel", "ws_qos", "cat_qos_eng", "kernel", "bootc + immutable base", ChannelKind::Text, 0, 0),
            ("c_compositor", "ws_qos", "cat_qos_eng", "compositor", "2D clusters & sandboxing", ChannelKind::Text, 1, 7),
            ("c_fssync", "ws_qos", "cat_qos_eng", "fs-sync", "iroh projection & hydration", ChannelKind::Text, 2, 0),
            ("c_releases", "ws_qos", "cat_qos_eng", "releases", "ship logs", ChannelKind::Text, 3, 0),
            ("c_warroom", "ws_qos", "cat_qos_voice", "War Room", "", ChannelKind::Voice, 0, 0),
            ("c_ideas", "ws_jam", "cat_jam_main", "ideas", "weekend hacks", ChannelKind::Text, 0, 0),
            ("c_music", "ws_jam", "cat_jam_main", "music", "now playing", ChannelKind::Text, 1, 2),
        ];
        for (id, ws, cat, name, topic, kind, order, unread) in chans {
            Self::put_channel(doc, id, ws, cat, name, topic, *kind, *order, *unread);
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
                        path: "/var/workspace/design/call-overlay-v3.png".into(),
                        w: 1280,
                        h: 720,
                        tint: 1,
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
                "m_sora",
                NewMessage {
                    author: "u_sora".into(),
                    ts: m(41),
                    text: "Reworked the spec to say screenshare is just 'share the workspace' — peers follow you spatially instead of watching a video of your screen.".into(),
                    edited: true,
                    history: vec![
                        EditRecord {
                            ts: m(43),
                            text: "Reworked the spec — no screenshare, peers just follow you around the workspace.".into(),
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
        unread: u32,
    ) {
        let chans = get_or_map(doc, &ROOT, "channels");
        let c = get_or_map(doc, &chans, id);
        let _ = doc.put(&c, "workspace", ws);
        let _ = doc.put(&c, "category", category);
        let _ = doc.put(&c, "name", name);
        let _ = doc.put(&c, "topic", topic);
        let _ = doc.put(&c, "kind", kind.as_str());
        let _ = doc.put(&c, "order", order as i64);
        let _ = doc.put(&c, "unread", unread as i64);
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
                Attachment::Image { name, path, w, h, tint } => {
                    let _ = doc.put(&a, "kind", "image");
                    let _ = doc.put(&a, "name", name.as_str());
                    let _ = doc.put(&a, "path", path.as_str());
                    let _ = doc.put(&a, "w", *w as i64);
                    let _ = doc.put(&a, "h", *h as i64);
                    let _ = doc.put(&a, "tint", *tint as i64);
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
        let cur = doc.text(&text).unwrap_or_default();
        let _ = doc.splice_text(&text, 0, cur.chars().count() as isize, new_text);
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

    pub fn clear_draft(doc: &mut AutoCommit, channel: &str) {
        let obj = Self::draft_obj(doc, channel);
        let cur = doc.text(&obj).unwrap_or_default();
        let _ = doc.splice_text(&obj, 0, cur.chars().count() as isize, "");
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

    /// Create a channel at the end of its category. Returns its fresh id.
    pub fn create_channel(
        doc: &mut AutoCommit,
        ws: &str,
        category: &str,
        name: &str,
        kind: ChannelKind,
    ) -> String {
        let id = format!("c_{}", Uuid::new_v4().simple());
        let order = Self::next_channel_order(doc, ws, category);
        Self::put_channel(doc, &id, ws, category, name, "", kind, order, 0);
        id
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
                Some(VoiceMember {
                    user: user.clone(),
                    talking: get_bool(doc, &m, "talking"),
                    muted: get_bool(doc, &m, "muted"),
                    deafened: get_bool(doc, &m, "deafened"),
                    audio_only: get_bool(doc, &m, "audio_only"),
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
                    unread: get_i64(doc, &c, "unread") as u32,
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
                tint: get_i64(doc, &a, "tint") as usize,
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
