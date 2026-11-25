//! The YouTube origin: keeps a package's types of the `youtube` kinds (`type Video (origin:
//! youtube.video)`) in step with YouTube, from the searches, channels and trending lists its
//! outside effects ask for.
//!
//! The origin is native code the instance runs beside the package: it holds the network, which
//! package code never sees. Each frame the instance polls it; it asks YouTube what an effect
//! asked for, writes the answer into the package's records as one change of the origin (through
//! [`tilolang_runtime::Runtime::write_origin`]), and says what became of the effect.
//!
//! **What is kept.** A search's channels, playlists and videos; a channel's videos; a playlist's
//! videos, with the order its maker put them in; the videos YouTube lists as trending; and where
//! a video's bytes play from. Each is keyed by the service's own id, so asking again never shows
//! the same channel or video twice.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilolang_tilo::host::Host;
use tilolang_tilo::origin::{Origin, OriginCx, OriginType, Polled, origin_types};
use tilolang_tilo::runtime::RuntimeError;
use tilolang_tilo::runtime::origin::OriginWrite;
use tilolang_tilo::runtime::{Delivery, ResourceRef, Value};

use crate::service::{
    Branding, ChannelItem, ChannelPage, CommentsPage, PlaylistItem, PlaylistPage, SearchFilter,
    SearchPage, SegmentPage, Service, ServiceError, TrendingPage, VideoItem, Votes,
};

/// What the origin reads of each youtube kind it keeps in step, from the package's metadata.
struct Types {
    /// The root's collection of each kind.
    channels_field: String,
    videos_field: String,
    playlists_field: String,
    channel: OriginType,
    video: OriginType,
    playlist: OriginType,
    /// The comments, and the root's collection of them: a client that shows none leaves them
    /// out, and the origin writes none.
    comment: Option<OriginType>,
    comments_field: Option<String>,
    /// The `SponsorBlock` segments, and the root's collection of them.
    segment: Option<OriginType>,
    segments_field: Option<String>,
    root: ResourceRef,
    /// The root's own record, whose fields the app reads back: how its fetch stands, and why it
    /// is not in step (`trouble`, `online`), when the app declares them.
    root_ty: OriginType,
}

/// A request handed to the service and not answered yet.
enum Pending {
    Search { query: String, filter: SearchFilter },
    Channel { key: String },
    Playlist { key: String },
    Subscriptions { keys: Vec<String> },
    Trending,
    Stream { key: String, quality: String },
    Comments { key: String },
    Segments { key: String },
    Branding { key: String },
    Votes { key: String },
}

/// What a request came back with.
enum Answered {
    Search(SearchPage),
    Channel(ChannelPage),
    Playlist(PlaylistPage),
    Trending(TrendingPage),
    /// The newest videos of each channel asked for, in the order asked.
    Subscriptions(Vec<ChannelPage>),
    Stream(String),
    Comments(CommentsPage),
    Segments(SegmentPage),
    Branding(Branding),
    Votes(Votes),
}

struct Waiting {
    /// What tells this question's answer from another's: a service over a socket asks on a
    /// thread of its own, and two questions handed over one after the other finish in whichever
    /// order the service gets to them, so an answer is never taken for the oldest question.
    id: u64,
    /// The payload of the effect that asked: what its outcome is filed under.
    key: String,
    pending: Pending,
    answered: Option<Result<Answered, ServiceError>>,
}

/// What the service answered on its own threads, each by the id of the question it answers, as
/// the frame that polled the origin takes them in.
type Answer = (u64, Result<Answered, ServiceError>);
type Answers = Arc<Mutex<VecDeque<Answer>>>;

/// The origin. See the module docs.
pub struct YouTubeOrigin {
    service: Arc<dyn Service>,
    /// Set when the service has an answer, from the thread it answers on.
    woken: Arc<AtomicBool>,
    /// What the service answered, written where a poll takes it in, each by the question it
    /// answers: a service over a socket (a connector, [`crate::remote::RemoteService`]) asks on
    /// a thread of its own, so the frame that hands it an effect never waits for the network,
    /// and the answers finish in an order of their own.
    answered: Answers,
    types: Option<Types>,
    /// Each request the service has not answered yet, oldest first.
    waiting: VecDeque<Waiting>,
    /// What became of each effect the service answered, by the payload that asked it, oldest
    /// first: what an effect handed over and not settled yet reads its outcome from.
    outcomes: BTreeMap<String, VecDeque<Delivery>>,
    /// The id the next question is asked under.
    next_id: u64,
    /// Why YouTube could not be reached the last time it was asked, until it answers.
    unreached: Option<String>,
    /// The instance's frame loop, woken whenever an answer is ready.
    on_answer: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl YouTubeOrigin {
    /// The origin over `service`, which sets `woken` when it has an answer.
    #[must_use]
    pub fn new(service: Arc<dyn Service>, woken: Arc<AtomicBool>) -> Self {
        Self {
            service,
            woken,
            answered: Arc::default(),
            types: None,
            waiting: VecDeque::new(),
            outcomes: BTreeMap::new(),
            next_id: 0,
            unreached: None,
            on_answer: None,
        }
    }

    /// The origin with `wake` called whenever an answer is ready: the instance's frame loop,
    /// woken so the next frame polls it and writes what YouTube said. With `wake` set, each
    /// question is asked on a thread of its own and `wake` runs there, so the frame that
    /// handed the effect over never waits for the network; a service that answers at once
    /// (a test's, a session's scripted one) answers on that thread too.
    #[must_use]
    pub fn waking(
        origin: Rc<RefCell<Self>>,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Rc<RefCell<Self>> {
        origin.borrow_mut().on_answer = Some(wake);
        origin
    }

    /// Why YouTube could not be reached the last time it was asked, until it answers.
    pub(crate) fn unreached(&self) -> Option<&str> {
        self.unreached.as_deref()
    }

    /// Whether a question waits for the service's answer, or an answer to be written.
    pub(crate) fn waits(&self) -> bool {
        !self.waiting.is_empty()
    }

    /// The kinds and the root's collections, read once: the package's own types.
    fn types_of(host: &Host, app: &str) -> Option<Types> {
        let package = host.package(app)?;
        let found = origin_types(&package.meta, "youtube");
        let kind = |k: &str| found.iter().find(|t| t.kind == k).cloned();
        let channel = kind("youtube.channel")?;
        let video = kind("youtube.video")?;
        let playlist = kind("youtube.playlist")?;
        let (_, root_ty) = package.main()?;
        let declared = package.meta.types.iter().find(|t| t.id == root_ty)?;
        let collection_of = |name: &str| {
            declared
                .fields
                .iter()
                .find(|f| f.ty == format!("Collection<{name}>"))
                .map(|f| f.id.clone())
        };
        Some(Types {
            channels_field: collection_of("Channel")?,
            videos_field: collection_of("Video")?,
            playlists_field: collection_of("Playlist")?,
            channel,
            video,
            playlist,
            // Comments and segments are written only where the client shows them.
            comment: kind("youtube.comment"),
            comments_field: collection_of("Comment"),
            segment: kind("youtube.segment"),
            segments_field: collection_of("Segment"),
            // The root as the store made it: its id is the workspace's one resource of its
            // type, not the type's own id.
            root: host.root_of(app).ok()?,
            root_ty: OriginType {
                app: package.meta.app.clone(),
                ty: root_ty,
                kind: String::new(),
                fields: declared
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.id.clone()))
                    .collect(),
            },
        })
    }

    fn text(s: &str) -> Value {
        Value::text(s)
    }

    /// A copy of `pending`, for the thread that asks it.
    fn clone_pending(pending: &Pending) -> Pending {
        match pending {
            Pending::Search { query, filter } => Pending::Search {
                query: query.clone(),
                filter: filter.clone(),
            },
            Pending::Channel { key } => Pending::Channel { key: key.clone() },
            Pending::Playlist { key } => Pending::Playlist { key: key.clone() },
            Pending::Subscriptions { keys } => Pending::Subscriptions { keys: keys.clone() },
            Pending::Trending => Pending::Trending,
            Pending::Stream { key, quality } => Pending::Stream {
                key: key.clone(),
                quality: quality.clone(),
            },
            Pending::Comments { key } => Pending::Comments { key: key.clone() },
            Pending::Segments { key } => Pending::Segments { key: key.clone() },
            Pending::Branding { key } => Pending::Branding { key: key.clone() },
            Pending::Votes { key } => Pending::Votes { key: key.clone() },
        }
    }

    /// Hand `pending` to the service, and wake the instance once it has answered. What
    /// becomes of it is filed under `key`, the payload the effect asked with.
    fn ask(&mut self, pending: Pending, key: String) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        // Over a socket (a connector, the session's own service) the question is asked on a
        // thread of its own: the frame that queued the effect never waits for the network.
        if let Some(on_answer) = self.on_answer.clone() {
            let service = Arc::clone(&self.service);
            let answered = Arc::clone(&self.answered);
            let woken = Arc::clone(&self.woken);
            // The question is cloned for the thread, so the frame keeps its own to wait on.
            let asked_pending = Self::clone_pending(&pending);
            let asked = std::thread::Builder::new()
                .name("youtube-ask".into())
                .spawn(move || {
                    let answer = answer(&*service, &asked_pending);
                    answered
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push_back((id, answer));
                    woken.store(true, Ordering::Release);
                    on_answer();
                });
            if asked.is_ok() {
                self.waiting.push_back(Waiting {
                    id,
                    key,
                    pending,
                    answered: None,
                });
                return;
            }
        }
        let answer = answer(&*self.service, &pending);
        self.waiting.push_back(Waiting {
            id,
            key,
            pending,
            answered: Some(answer),
        });
        self.woken.store(true, Ordering::Release);
    }
}

/// What the service answered to `pending`.
fn answer(service: &dyn Service, pending: &Pending) -> Result<Answered, ServiceError> {
    match pending {
        Pending::Search { query, filter } => service.search(query, filter).map(Answered::Search),
        Pending::Channel { key } => service.channel(key).map(Answered::Channel),
        Pending::Playlist { key } => service.playlist(key).map(Answered::Playlist),
        Pending::Trending => service.trending().map(Answered::Trending),
        Pending::Stream { key, quality } => service.stream(key, quality).map(Answered::Stream),
        Pending::Comments { key } => service.comments(key).map(Answered::Comments),
        Pending::Segments { key } => service.segments(key).map(Answered::Segments),
        Pending::Branding { key } => service.branding(key).map(Answered::Branding),
        Pending::Votes { key } => service.votes(key).map(Answered::Votes),
        Pending::Subscriptions { keys } => {
            let mut pages = Vec::new();
            for key in keys {
                pages.push(service.channel(key)?);
            }
            Ok(Answered::Subscriptions(pages))
        }
    }
}

/// The writes that put `items` of the kind `ty` into the collection `field` of `owner`, keeping
/// the service's fields only: a field the person changes (a like, a watch-later mark) is theirs,
/// and an answer never overwrites it.
fn upsert_writes(
    host: &Host,
    ty: &OriginType,
    owner: &ResourceRef,
    field: &str,
    key_field: &str,
    items: &[(String, Vec<(String, Value)>)],
) -> Vec<OriginWrite> {
    let held: BTreeMap<String, ResourceRef> = items_in(host, owner, field)
        .into_iter()
        .map(|item| (read_text(host, &item, ty, key_field), item))
        .collect();
    items
        .iter()
        .map(|(key, fields)| match held.get(key) {
            Some(at) => OriginWrite::Set {
                target: (*at).clone(),
                fields: fields.clone(),
            },
            None => OriginWrite::Insert {
                owner: owner.clone(),
                field: field.to_owned(),
                ty: ty.ty.clone(),
                fields: fields.clone(),
            },
        })
        .collect()
}

fn items_in(host: &Host, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
    match host.runtime().read(at, field) {
        Ok(Value::List(items)) => items
            .into_iter()
            .filter_map(|v| match v {
                Value::Ref(r) => Some((*r.target).clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn read_text(host: &Host, at: &ResourceRef, ty: &OriginType, field: &str) -> String {
    let Some(id) = ty.field(field) else {
        return String::new();
    };
    match host.runtime().read(at, id) {
        Ok(Value::Text(t)) => t,
        _ => String::new(),
    }
}

/// A `Bool` field of `at`, true when the kind does not declare it: an app that says nothing of
/// whether the service answers is not said to be out of step.
fn read_bool(host: &Host, at: &ResourceRef, ty: &OriginType, field: &str) -> bool {
    let Some(id) = ty.field(field) else {
        return true;
    };
    match host.runtime().read(at, id) {
        Ok(Value::Bool(b)) => b,
        _ => true,
    }
}

fn put(ty: &OriginType, fields: &mut Vec<(String, Value)>, name: &str, value: impl Into<Value>) {
    if let Some(id) = ty.field(name) {
        fields.push((id.to_string(), value.into()));
    }
}

impl Origin for YouTubeOrigin {
    fn name(&self) -> &'static str {
        "youtube"
    }

    fn serves(&self, capability: &str) -> bool {
        matches!(
            capability,
            "youtube.search"
                | "youtube.channel"
                | "youtube.playlist"
                | "youtube.subscriptions"
                | "youtube.trending"
                | "youtube.stream"
                | "youtube.comments"
                | "youtube.segments"
                | "youtube.branding"
                | "youtube.votes"
        )
    }

    fn due(&self) -> bool {
        self.woken.load(Ordering::Acquire)
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
        // The kinds, and the root whose record holds them: the one the origin writes into.
        let Some(types) = self
            .types
            .take()
            .or_else(|| Self::types_of(cx.host, cx.app))
        else {
            return Polled::default();
        };
        let mut polled = Polled::default();
        if self.woken.swap(false, Ordering::AcqRel) {
            polled.wrote = self.take_answers(cx.host, &types);
            // The effect the service carried out is settled: what became of it is in the record.
            polled.settle = true;
        }
        polled.wrote |= self.say(cx.host, &types);
        self.types = Some(types);
        polled
    }

    fn deliver(&mut self, capability: &str, payload: &serde_json::Value) -> Delivery {
        let text = || payload.as_str().unwrap_or_default().to_owned();
        let pending = match capability {
            "youtube.search" => {
                // A search carries the query and what it narrows to: a client that asks
                // without filters hands the query alone.
                let (query, filter) = search_ask(payload);
                Pending::Search { query, filter }
            }
            "youtube.channel" => Pending::Channel { key: text() },
            "youtube.playlist" => Pending::Playlist { key: text() },
            "youtube.trending" => Pending::Trending,
            "youtube.stream" => {
                let (key, quality) = stream_ask(payload);
                Pending::Stream { key, quality }
            }
            "youtube.comments" => Pending::Comments { key: text() },
            "youtube.segments" => Pending::Segments { key: text() },
            "youtube.branding" => Pending::Branding { key: text() },
            "youtube.votes" => Pending::Votes { key: text() },
            "youtube.subscriptions" => Pending::Subscriptions {
                // The channels the person followed when they asked for the feed: their keys, in
                // the order the feed lists them.
                keys: payload
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|key| key.as_str().map(str::to_owned))
                    .collect(),
            },
            _ => return Delivery::Rejected(format!("YouTube serves no `{capability}`")),
        };
        self.ask(pending, payload.to_string());
        // The answer is in: wake the instance's frame, which polls and writes it.
        if let Some(wake) = &self.on_answer {
            wake();
        }
        Delivery::Pending
    }

    fn outcome(&mut self, capability: &str, payload: &serde_json::Value) -> Option<Delivery> {
        if !self.serves(capability) {
            return None;
        }
        let key = payload.to_string();
        let outcome = self.outcomes.get_mut(&key)?.pop_front()?;
        if self.outcomes.get(&key).is_some_and(VecDeque::is_empty) {
            self.outcomes.remove(&key);
        }
        Some(outcome)
    }
}

/// A search effect's payload: the query, and what it narrows to. A client that asks without
/// filters hands the query alone, and one that asks with them hands the query, the kinds it
/// shows, how long they run and how recent they are, in that order.
fn search_ask(payload: &serde_json::Value) -> (String, SearchFilter) {
    if let Some(query) = payload.as_str() {
        return (query.to_owned(), SearchFilter::default());
    }
    let parts = payload.as_array().cloned().unwrap_or_default();
    let part = |at: usize| {
        parts
            .get(at)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let first = part(0);
    if first.is_empty() {
        // An object names each part: what a client that thinks in records hands.
        let field = |name: &str| {
            payload
                .get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        return (
            field("query"),
            SearchFilter {
                kind: field("kind"),
                length: field("length"),
                published: field("published"),
            },
        );
    }
    (
        first,
        SearchFilter {
            kind: part(1),
            length: part(2),
            published: part(3),
        },
    )
}

/// A stream effect's payload: the video, and the tallest picture asked for. A client that names
/// none asks for the best that fits.
fn stream_ask(payload: &serde_json::Value) -> (String, String) {
    if let Some(key) = payload.as_str() {
        return (key.to_owned(), String::new());
    }
    if let Some(parts) = payload.as_array() {
        let part = |at: usize| {
            parts
                .get(at)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        return (part(0), part(1));
    }
    (
        payload
            .get("video")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        payload
            .get("quality")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    )
}

impl YouTubeOrigin {
    /// Write what the service answered since the last poll, and say whether anything changed.
    fn take_answers(&mut self, host: &Host, t: &Types) -> bool {
        let mut wrote = false;
        // What the service answered on its own thread, each by the question it answers.
        let took: Vec<Answer> = self
            .answered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect();
        for (id, answer) in took {
            // The question this answers, wherever it stands: two questions handed over one
            // after the other finish in whichever order the service gets to them, so an answer
            // is never taken for the oldest question still waiting.
            if let Some(waiting) = self.waiting.iter_mut().find(|waiting| waiting.id == id) {
                waiting.answered = Some(answer);
            }
        }
        while let Some(mut waiting) = self.waiting.pop_front() {
            let Some(answered) = waiting.answered.take() else {
                self.waiting.push_front(waiting);
                break;
            };
            let outcome = match Self::write_step(host, t, &waiting.pending, &answered) {
                Ok(written) => {
                    wrote |= written;
                    match &answered {
                        Ok(_) => Delivery::Delivered,
                        Err(error) => Delivery::Rejected(error.0.clone()),
                    }
                }
                Err(error) => Delivery::Rejected(format!(
                    "what YouTube said could not be written: {error}"
                )),
            };
            self.outcomes
                .entry(waiting.key)
                .or_default()
                .push_back(outcome);
            // The community marks are extras over YouTube's own answer: their services being
            // down says nothing of whether YouTube can be reached.
            if core(&waiting.pending) {
                self.reached(&answered);
            }
        }
        wrote
    }

    /// One answer, written as one change of the origin: whether anything was written, or why
    /// it could not be written. What cannot be written is said once in the log and not kept.
    fn write_step(
        host: &Host,
        t: &Types,
        pending: &Pending,
        answered: &Result<Answered, ServiceError>,
    ) -> Result<bool, String> {
        Self::write_answer(host, t, pending, answered).map_err(|error| {
            tracing::warn!(%error, "what YouTube said could not be written");
            error.to_string()
        })
    }

    /// Whether YouTube answered: an answer that failed says why, until one comes back.
    fn reached(&mut self, answered: &Result<Answered, ServiceError>) {
        self.unreached = match answered {
            Ok(_) => None,
            Err(error) => Some(error.0.clone()),
        };
    }

    /// The app says whether YouTube answers, and why it is not in step: one not reached says
    /// why. Written when it differs from what the record holds.
    fn say(&self, host: &Host, t: &Types) -> bool {
        let trouble = self.unreached.as_deref().unwrap_or("");
        let online = self.unreached.is_none();
        if read_text(host, &t.root, &t.root_ty, "trouble") == trouble
            && read_bool(host, &t.root, &t.root_ty, "online") == online
        {
            return false;
        }
        let mut fields = Vec::new();
        put(&t.root_ty, &mut fields, "trouble", Self::text(trouble));
        put(&t.root_ty, &mut fields, "online", Value::Bool(online));
        if fields.is_empty() {
            return false;
        }
        let wrote = host.runtime().apply_origin(
            "youtube",
            vec![OriginWrite::Set {
                target: t.root.clone(),
                fields,
            }],
            None,
        );
        // What the app says of itself is said again by the next poll: it says so here.
        if let Err(error) = &wrote {
            tracing::warn!(%error, "what the app says of itself was not written");
        }
        wrote.is_ok()
    }

    /// Write what the service answered to `pending` as one change of the origin: the channels,
    /// playlists and videos it brought, or what it said went wrong, and the state its fetch is
    /// left in. Whether it wrote; a change the workspace does not take leaves nothing behind.
    ///
    /// # Errors
    ///
    /// A store that cannot take the change; then none of what YouTube said is written.
    fn write_answer(
        host: &Host,
        t: &Types,
        pending: &Pending,
        answered: &Result<Answered, ServiceError>,
    ) -> Result<bool, RuntimeError> {
        let mut writes = Vec::new();
        match answered {
            Err(error) => Self::state_writes(t, pending, "failed", &error.0, &mut writes),
            Ok(Answered::Search(page)) => {
                writes.extend(search_writes(host, t, page));
                Self::state_writes(t, pending, "done", "", &mut writes);
            }
            Ok(Answered::Channel(page)) => writes.extend(channel_writes(host, t, page)),
            Ok(Answered::Playlist(page)) => writes.extend(playlist_writes(host, t, page)),
            Ok(Answered::Trending(page)) => writes.extend(trending_writes(host, t, page)),
            Ok(Answered::Subscriptions(pages)) => {
                for page in pages {
                    writes.extend(channel_writes(host, t, page));
                }
                Self::state_writes(t, pending, "done", "", &mut writes);
            }
            Ok(Answered::Stream(url)) => {
                writes.extend(stream_writes(host, t, stream_key(pending), url));
            }
            Ok(Answered::Comments(page)) => writes.extend(comments_writes(host, t, page)),
            Ok(Answered::Segments(page)) => writes.extend(segments_writes(host, t, page)),
            Ok(Answered::Branding(branding)) => writes.extend(branding_writes(host, t, branding)),
            Ok(Answered::Votes(votes)) => writes.extend(votes_writes(host, t, votes)),
        }
        if writes.is_empty() {
            return Ok(false);
        }
        host.runtime()
            .write_origin("youtube", None, |change| change.write(writes))?;
        Ok(true)
    }

    /// What the app reads back of its own fetch: the state it stands in and why it failed, for a
    /// fetch that has such fields, added to `writes`. A channel's page, a trending list and a
    /// video's stream write their own fields and have no state to settle.
    fn state_writes(
        t: &Types,
        pending: &Pending,
        state: &str,
        trouble: &str,
        writes: &mut Vec<OriginWrite>,
    ) {
        let (what, why) = match pending {
            Pending::Search { .. } => ("search_state", "search_trouble"),
            Pending::Subscriptions { .. } => ("feed_state", "feed_trouble"),
            Pending::Channel { .. }
            | Pending::Playlist { .. }
            | Pending::Trending
            | Pending::Stream { .. }
            | Pending::Comments { .. }
            | Pending::Segments { .. }
            | Pending::Branding { .. }
            | Pending::Votes { .. } => return,
        };
        let mut fields = Vec::new();
        put(&t.root_ty, &mut fields, what, Self::text(state));
        put(&t.root_ty, &mut fields, why, Self::text(trouble));
        if !fields.is_empty() {
            writes.push(OriginWrite::Set {
                target: t.root.clone(),
                fields,
            });
        }
    }
}

fn search_writes(host: &Host, t: &Types, page: &SearchPage) -> Vec<OriginWrite> {
    let query = page.query.as_str();
    let channels: Vec<(String, Vec<(String, Value)>)> = page
        .channels
        .iter()
        .map(|c| {
            (
                c.key.clone(),
                channel_fields(&t.channel, c, Some(query), None),
            )
        })
        .collect();
    let mut writes = upsert_writes(
        host,
        &t.channel,
        &t.root,
        &t.channels_field,
        "key",
        &channels,
    );
    let videos: Vec<(String, Vec<(String, Value)>)> = page
        .videos
        .iter()
        .map(|v| (v.key.clone(), video_fields(&t.video, v, query)))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.video,
        &t.root,
        &t.videos_field,
        "key",
        &videos,
    ));
    let playlists: Vec<(String, Vec<(String, Value)>)> = page
        .playlists
        .iter()
        .map(|p| (p.key.clone(), playlist_fields(&t.playlist, p, Some(query))))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.playlist,
        &t.root,
        &t.playlists_field,
        "key",
        &playlists,
    ));
    writes
}

/// The fields of a channel the service named. A search row carries no banner and names the search
/// that found it; a channel's page carries its banner and names no search, so neither takes away
/// what the other said.
fn channel_fields(
    ty: &OriginType,
    c: &ChannelItem,
    found_for: Option<&str>,
    banner: Option<&str>,
) -> Vec<(String, Value)> {
    let mut pairs = vec![
        ("key", YouTubeOrigin::text(&c.key)),
        ("name", YouTubeOrigin::text(&c.name)),
        ("handle", YouTubeOrigin::text(&c.handle)),
        ("subscribers", Value::Int(c.subscribers)),
        ("avatar", YouTubeOrigin::text(&c.avatar)),
        ("description", YouTubeOrigin::text(&c.description)),
        ("verified", Value::Bool(c.verified)),
    ];
    if let Some(banner) = banner {
        pairs.push(("banner", YouTubeOrigin::text(banner)));
    }
    if let Some(found_for) = found_for {
        pairs.push(("found_for", YouTubeOrigin::text(found_for)));
    }
    declared(ty, pairs)
}

/// The fields of a playlist the service named, and the search that found it, when one did.
fn playlist_fields(
    ty: &OriginType,
    p: &PlaylistItem,
    found_for: Option<&str>,
) -> Vec<(String, Value)> {
    let mut pairs = vec![
        ("key", YouTubeOrigin::text(&p.key)),
        ("title", YouTubeOrigin::text(&p.title)),
        ("channel_key", YouTubeOrigin::text(&p.channel_key)),
        ("channel_name", YouTubeOrigin::text(&p.channel_name)),
        ("video_count", Value::Int(p.video_count)),
        ("thumbnail", YouTubeOrigin::text(&p.thumbnail)),
    ];
    if let Some(found_for) = found_for {
        pairs.push(("found_for", YouTubeOrigin::text(found_for)));
    }
    declared(ty, pairs)
}

fn channel_writes(host: &Host, t: &Types, page: &ChannelPage) -> Vec<OriginWrite> {
    let fields = channel_fields(&t.channel, &page.channel, None, Some(&page.channel.banner));
    let mut writes = upsert_writes(
        host,
        &t.channel,
        &t.root,
        &t.channels_field,
        "key",
        &[(page.channel.key.clone(), fields)],
    );
    let videos: Vec<(String, Vec<(String, Value)>)> = page
        .videos
        .iter()
        .map(|v| (v.key.clone(), video_fields(&t.video, v, "")))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.video,
        &t.root,
        &t.videos_field,
        "key",
        &videos,
    ));
    writes
}

/// A playlist's page: the playlist, the keys of the videos it holds in its maker's order, and the
/// videos themselves, which a page shows by following those keys.
fn playlist_writes(host: &Host, t: &Types, page: &PlaylistPage) -> Vec<OriginWrite> {
    let mut fields = playlist_fields(&t.playlist, &page.playlist, None);
    put(
        &t.playlist,
        &mut fields,
        "video_keys",
        Value::List(
            page.videos
                .iter()
                .map(|v| Value::text(v.key.as_str()))
                .collect(),
        ),
    );
    let mut writes = upsert_writes(
        host,
        &t.playlist,
        &t.root,
        &t.playlists_field,
        "key",
        &[(page.playlist.key.clone(), fields)],
    );
    let videos: Vec<(String, Vec<(String, Value)>)> = page
        .videos
        .iter()
        .map(|v| (v.key.clone(), video_fields(&t.video, v, "")))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.video,
        &t.root,
        &t.videos_field,
        "key",
        &videos,
    ));
    writes
}

fn trending_writes(host: &Host, t: &Types, page: &TrendingPage) -> Vec<OriginWrite> {
    let trending: Vec<(String, Vec<(String, Value)>)> = page
        .videos
        .iter()
        .map(|v| {
            let mut fields = video_fields(&t.video, v, "");
            put(&t.video, &mut fields, "trending", Value::Bool(true));
            (v.key.clone(), fields)
        })
        .collect();
    upsert_writes(host, &t.video, &t.root, &t.videos_field, "key", &trending)
}

fn stream_writes(host: &Host, t: &Types, key: &str, url: &str) -> Vec<OriginWrite> {
    let Some(at) = items_in(host, &t.root, &t.videos_field)
        .into_iter()
        .find(|item| read_text(host, item, &t.video, "key") == key)
    else {
        // A video the app does not hold was not asked for its stream: there is nothing to write.
        return Vec::new();
    };
    let mut fields = Vec::new();
    put(&t.video, &mut fields, "stream", YouTubeOrigin::text(url));
    vec![OriginWrite::Set { target: at, fields }]
}

fn stream_key(pending: &Pending) -> &str {
    match pending {
        Pending::Stream { key, .. } => key,
        _ => "",
    }
}

/// Whether `pending` asks YouTube itself: a failure there says YouTube cannot be reached. The
/// community services beside it (SponsorBlock, DeArrow, Return YouTube Dislike) and a video's
/// comments are extras, whose being unreachable says nothing of YouTube's own answer.
fn core(pending: &Pending) -> bool {
    matches!(
        pending,
        Pending::Search { .. }
            | Pending::Channel { .. }
            | Pending::Playlist { .. }
            | Pending::Subscriptions { .. }
            | Pending::Trending
            | Pending::Stream { .. }
    )
}

/// A video's comments: each written where the client keeps them, keyed by the comment's own id,
/// and the ones the video no longer has taken out. A client that shows no comments keeps none.
fn comments_writes(host: &Host, t: &Types, page: &CommentsPage) -> Vec<OriginWrite> {
    let (Some(ty), Some(field)) = (&t.comment, &t.comments_field) else {
        return Vec::new();
    };
    let items: Vec<(String, Vec<(String, Value)>)> = page
        .comments
        .iter()
        .map(|c| (c.key.clone(), comment_fields(ty, &page.video, c)))
        .collect();
    let kept: BTreeSet<&str> = page.comments.iter().map(|c| c.key.as_str()).collect();
    let mut writes = gone_writes(host, t, ty, field, &page.video, |key| kept.contains(key));
    writes.extend(upsert_writes(host, ty, &t.root, field, "key", &items));
    writes
}

/// The fields of a comment the service named, as the package's kind declares them.
fn comment_fields(
    ty: &OriginType,
    video: &str,
    c: &crate::service::CommentItem,
) -> Vec<(String, Value)> {
    declared(
        ty,
        vec![
            ("key", YouTubeOrigin::text(&c.key)),
            ("video_key", YouTubeOrigin::text(video)),
            ("author", YouTubeOrigin::text(&c.author)),
            ("avatar", YouTubeOrigin::text(&c.avatar)),
            ("text", YouTubeOrigin::text(&c.text)),
            ("likes", Value::Int(c.likes)),
            ("published", YouTubeOrigin::text(&c.published)),
            ("pinned", Value::Bool(c.pinned)),
            ("hearted", Value::Bool(c.hearted)),
            ("replies", Value::Int(c.replies)),
        ],
    )
}

/// The items of the collection `field` of `owner` whose `video_key` is `video` and whose `key`
/// `kept` refuses taken out: a comment or a segment the service no longer lists.
fn gone_writes(
    host: &Host,
    t: &Types,
    ty: &OriginType,
    field: &str,
    video: &str,
    kept: impl Fn(&str) -> bool,
) -> Vec<OriginWrite> {
    items_in(host, &t.root, field)
        .into_iter()
        .filter(|item| {
            read_text(host, item, ty, "video_key") == video
                && !kept(&read_text(host, item, ty, "key"))
        })
        .map(|item| OriginWrite::Remove {
            owner: t.root.clone(),
            field: field.to_owned(),
            item,
        })
        .collect()
}

/// The `SponsorBlock` segments of a video: the ones it plays replaced by what the service says
/// now, each keyed by where it stands, so asking again rewrites them rather than piling up.
fn segments_writes(host: &Host, t: &Types, page: &SegmentPage) -> Vec<OriginWrite> {
    let (Some(ty), Some(field)) = (&t.segment, &t.segments_field) else {
        return Vec::new();
    };
    // Every segment of the answer replaces the ones the client held for the video.
    let items: Vec<(String, Vec<(String, Value)>)> = page
        .segments
        .iter()
        .map(|s| {
            let key = format!("{}:{}:{}", s.video_key, s.category, s.start);
            (
                key,
                declared(
                    ty,
                    vec![
                        (
                            "key",
                            YouTubeOrigin::text(&format!(
                                "{}:{}:{}",
                                s.video_key, s.category, s.start,
                            )),
                        ),
                        ("video_key", YouTubeOrigin::text(&s.video_key)),
                        ("category", YouTubeOrigin::text(&s.category)),
                        ("start", Value::Int(whole_seconds(s.start))),
                        ("end", Value::Int(whole_seconds(s.end))),
                        ("votes", Value::Int(s.votes)),
                    ],
                ),
            )
        })
        .collect();
    let kept: BTreeSet<&str> = items.iter().map(|(key, _)| key.as_str()).collect();
    let mut writes = gone_writes(host, t, ty, field, &page.video, |key| kept.contains(key));
    writes.extend(upsert_writes(host, ty, &t.root, field, "key", &items));
    writes
}

/// A segment's time in whole seconds, as the client's records keep it: SponsorBlock answers
/// fractional seconds, and a skip a fraction of a second off is not seen.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a segment's time in whole seconds, which a skip reads"
)]
fn whole_seconds(at: f64) -> i64 {
    at.round() as i64
}

/// `DeArrow`'s community title and picture of a video, written onto the video the client holds.
fn branding_writes(host: &Host, t: &Types, branding: &Branding) -> Vec<OriginWrite> {
    let Some(at) = items_in(host, &t.root, &t.videos_field)
        .into_iter()
        .find(|item| read_text(host, item, &t.video, "key") == branding.video)
    else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    put(
        &t.video,
        &mut fields,
        "dearrow_title",
        YouTubeOrigin::text(&branding.title),
    );
    put(
        &t.video,
        &mut fields,
        "dearrow_thumbnail",
        YouTubeOrigin::text(&branding.thumbnail),
    );
    if fields.is_empty() {
        return Vec::new();
    }
    vec![OriginWrite::Set { target: at, fields }]
}

/// Return YouTube Dislike's estimate of how many people disliked a video, written onto it.
fn votes_writes(host: &Host, t: &Types, votes: &Votes) -> Vec<OriginWrite> {
    let Some(at) = items_in(host, &t.root, &t.videos_field)
        .into_iter()
        .find(|item| read_text(host, item, &t.video, "key") == votes.video)
    else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    put(
        &t.video,
        &mut fields,
        "dislikes",
        Value::Int(votes.dislikes),
    );
    if fields.is_empty() {
        return Vec::new();
    }
    vec![OriginWrite::Set { target: at, fields }]
}

/// The fields an answer writes into a video of the kind, the search that found it included.
fn video_fields(ty: &OriginType, v: &VideoItem, found_for: &str) -> Vec<(String, Value)> {
    declared(
        ty,
        vec![
            ("key", YouTubeOrigin::text(&v.key)),
            ("title", YouTubeOrigin::text(&v.title)),
            ("channel_key", YouTubeOrigin::text(&v.channel_key)),
            ("channel_name", YouTubeOrigin::text(&v.channel_name)),
            ("channel_avatar", YouTubeOrigin::text(&v.channel_avatar)),
            ("views", Value::Int(v.views)),
            ("published", YouTubeOrigin::text(&v.published)),
            ("duration", Value::Int(v.duration)),
            ("description", YouTubeOrigin::text(&v.description)),
            ("likes", Value::Int(v.likes)),
            ("thumbnail", YouTubeOrigin::text(&v.thumbnail)),
            ("live", Value::Bool(v.live)),
            ("found_for", YouTubeOrigin::text(found_for)),
        ],
    )
}

/// `pairs`, with the ids of the fields `ty` declares: a kind that does not declare one leaves it
/// out, as it leaves out the fields of a kind an app names another way.
fn declared(ty: &OriginType, pairs: Vec<(&str, Value)>) -> Vec<(String, Value)> {
    pairs
        .into_iter()
        .filter_map(|(name, value)| Some((ty.field(name)?.to_owned(), value)))
        .collect()
}
