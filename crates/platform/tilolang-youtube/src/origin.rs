//! The YouTube origin: keeps a package's types of the `youtube` kinds (`type Video (origin:
//! youtube.video)`) in step with YouTube, from the searches, channels and trending lists its
//! outside effects ask for.
//!
//! The origin is native code the instance runs beside the package: it holds the network, which
//! package code never sees. Each frame the instance polls it; it asks YouTube what an effect
//! asked for, writes the answer into the package's records as one change of the origin (through
//! [`tilolang_runtime::Runtime::write_origin`]), and says what became of the effect. An answer
//! the workspace cannot keep is kept here and taken in again by the next poll, before anything
//! newer, the app saying why it stands unwritten.
//!
//! **What is kept.** A search's channels, playlists and videos; a channel's videos; the videos
//! YouTube lists as trending; and where a video's bytes play from. Each is keyed by the
//! service's own id, so asking again never shows the same channel or video twice.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilolang_tilo::host::Host;
use tilolang_tilo::origin::{Origin, OriginCx, OriginType, Polled, origin_types};
use tilolang_tilo::runtime::RuntimeError;
use tilolang_tilo::runtime::origin::{OriginWrite, Unwritten};
use tilolang_tilo::runtime::{Delivery, ResourceRef, Value};

use crate::service::{
    ChannelItem, ChannelPage, PlaylistItem, SearchPage, Service, ServiceError, TrendingPage,
    VideoItem,
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
    root: ResourceRef,
    /// The root's own record, whose fields the app reads back: how its fetch stands, and why it
    /// is not in step (`trouble`, `online`), when the app declares them.
    root_ty: OriginType,
}

/// A request handed to the service and not answered yet.
enum Pending {
    Search { query: String },
    Channel { key: String },
    Subscriptions { keys: Vec<String> },
    Trending,
    Stream { key: String },
}

/// What a request came back with.
enum Answered {
    Search(SearchPage),
    Channel(ChannelPage),
    Trending(TrendingPage),
    /// The newest videos of each channel asked for, in the order asked.
    Subscriptions(Vec<ChannelPage>),
    Stream(String),
}

struct Waiting {
    pending: Pending,
    answered: Option<Result<Answered, ServiceError>>,
}

/// An answer of the service the app's records could not keep, kept to be written again by the
/// next poll: what YouTube said is not lost with the poll that could not keep it.
struct Unanswered {
    pending: Pending,
    answered: Result<Answered, ServiceError>,
    /// Why the app could not keep it, which the app says while it waits.
    trouble: String,
}

/// The origin. See the module docs.
pub struct YouTubeOrigin {
    service: Arc<dyn Service>,
    /// Set when the service has an answer, from the thread it answers on.
    woken: Arc<AtomicBool>,
    /// What the service answered, written where a poll takes it in: a service over a socket
    /// (a connector, [`crate::remote::RemoteService`]) asks on a thread of its own, so the
    /// frame that hands it an effect never waits for the network.
    answered: Arc<Mutex<VecDeque<Result<Answered, ServiceError>>>>,
    types: Option<Types>,
    /// Each request the service has not answered yet, oldest first.
    waiting: VecDeque<Waiting>,
    /// The answers the app's records could not keep, oldest first.
    unwritten: Unwritten<Unanswered>,
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
            unwritten: Unwritten::new(),
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
            Pending::Search { query } => Pending::Search {
                query: query.clone(),
            },
            Pending::Channel { key } => Pending::Channel { key: key.clone() },
            Pending::Subscriptions { keys } => Pending::Subscriptions { keys: keys.clone() },
            Pending::Trending => Pending::Trending,
            Pending::Stream { key } => Pending::Stream { key: key.clone() },
        }
    }

    /// Hand `pending` to the service, and wake the instance once it has answered.
    fn ask(&mut self, pending: Pending) {
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
                        .push_back(answer);
                    woken.store(true, Ordering::Release);
                    on_answer();
                });
            if asked.is_ok() {
                self.waiting.push_back(Waiting {
                    pending,
                    answered: None,
                });
                return;
            }
        }
        let answer = answer(&*self.service, &pending);
        self.waiting.push_back(Waiting {
            pending,
            answered: Some(answer),
        });
        self.woken.store(true, Ordering::Release);
    }
}

/// What the service answered to `pending`.
fn answer(service: &dyn Service, pending: &Pending) -> Result<Answered, ServiceError> {
    match pending {
        Pending::Search { query } => service.search(query).map(Answered::Search),
        Pending::Channel { key } => service.channel(key).map(Answered::Channel),
        Pending::Trending => service.trending().map(Answered::Trending),
        Pending::Stream { key } => service.stream(key).map(Answered::Stream),
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
                | "youtube.subscriptions"
                | "youtube.trending"
                | "youtube.stream"
        )
    }

    fn due(&self) -> bool {
        self.woken.load(Ordering::Acquire) || !self.unwritten.is_empty()
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
        // What the app could not keep last time, before anything newer: an answer waits until it
        // is written, never behind one that came after it.
        let mut polled = Polled {
            wrote: self.take_again(cx.host, &types),
            settle: false,
        };
        if self.woken.swap(false, Ordering::AcqRel) {
            polled.wrote |= self.take_answers(cx.host, &types);
            // The effect the service carried out is settled: what became of it is in the record,
            // or waits to be written by the next poll.
            polled.settle = true;
        }
        polled.wrote |= self.say(cx.host, &types);
        self.types = Some(types);
        polled
    }

    fn deliver(&mut self, capability: &str, payload: &serde_json::Value) -> Delivery {
        let pending = match capability {
            "youtube.search" => Pending::Search {
                query: payload.as_str().unwrap_or_default().to_owned(),
            },
            "youtube.channel" => Pending::Channel {
                key: payload.as_str().unwrap_or_default().to_owned(),
            },
            "youtube.trending" => Pending::Trending,
            "youtube.stream" => Pending::Stream {
                key: payload.as_str().unwrap_or_default().to_owned(),
            },
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
        self.ask(pending);
        // The answer is in: wake the instance's frame, which polls and writes it.
        if let Some(wake) = &self.on_answer {
            wake();
        }
        Delivery::Pending
    }
}

impl YouTubeOrigin {
    /// Write what the service answered since the last poll, and say whether anything changed.
    fn take_answers(&mut self, host: &Host, t: &Types) -> bool {
        let mut wrote = false;
        // What the service answered on its own thread, oldest first.
        let took: Vec<Result<Answered, ServiceError>> = self
            .answered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect();
        for answer in took {
            // The oldest question still waiting on the service takes this answer, in the order
            // it was asked; one already answered is not asked again.
            if let Some(waiting) = self
                .waiting
                .iter_mut()
                .find(|waiting| waiting.answered.is_none())
            {
                waiting.answered = Some(answer);
            }
        }
        while let Some(mut waiting) = self.waiting.pop_front() {
            let Some(answered) = waiting.answered.take() else {
                self.waiting.push_front(waiting);
                break;
            };
            let mut answer = Unanswered {
                pending: waiting.pending,
                answered,
                trouble: String::new(),
            };
            let written = Self::write_step(host, t, &mut answer);
            self.reached(&answer.answered);
            if written {
                wrote = true;
            } else {
                self.unwritten.keep(answer);
            }
        }
        wrote
    }

    /// The answers the app could not keep, taken again: what is written no longer waits, and what
    /// is not stays for the next poll, the app saying why.
    fn take_again(&mut self, host: &Host, t: &Types) -> bool {
        let mut wrote = false;
        let mut unwritten = std::mem::take(&mut self.unwritten);
        unwritten.take_again(|answer| {
            let written = Self::write_step(host, t, answer);
            wrote |= written;
            written
        });
        self.unwritten = unwritten;
        wrote
    }

    /// One answer, written as one change of the origin: `answer` says why it still waits when the
    /// workspace does not take it.
    fn write_step(host: &Host, t: &Types, answer: &mut Unanswered) -> bool {
        match Self::write_answer(host, t, &answer.pending, &answer.answered) {
            Ok(wrote) => wrote,
            Err(error) => {
                tracing::warn!(%error, "what YouTube said could not be written");
                answer.trouble = format!("What YouTube said could not be saved: {error}");
                false
            }
        }
    }

    /// Whether YouTube answered: an answer that failed says why, until one comes back.
    fn reached(&mut self, answered: &Result<Answered, ServiceError>) {
        self.unreached = match answered {
            Ok(_) => None,
            Err(error) => Some(error.0.clone()),
        };
    }

    /// The app says whether YouTube answers, and why it is not in step: one not reached is said
    /// before an answer the workspace could not keep, which the next poll writes again. Written
    /// when it differs from what the record holds.
    fn say(&self, host: &Host, t: &Types) -> bool {
        let trouble = match (&self.unreached, self.unwritten.steps().last()) {
            (Some(unreached), _) => unreached.as_str(),
            (None, Some(waiting)) => waiting.trouble.as_str(),
            (None, None) => "",
        };
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
    /// A store that cannot take the change; then none of what YouTube said is kept.
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
            Pending::Channel { .. } | Pending::Trending | Pending::Stream { .. } => return,
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
        Pending::Stream { key } => key,
        _ => "",
    }
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
