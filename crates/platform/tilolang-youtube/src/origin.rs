//! The YouTube origin: keeps a package's types of the `youtube` kinds (`type Video (origin:
//! youtube.video)`) in step with YouTube, from the searches, channels and trending lists its
//! outside effects ask for.
//!
//! The origin is native code the instance runs beside the package: it holds the network, which
//! package code never sees. Each frame the instance polls it; it asks YouTube what an effect
//! asked for, writes the answer into the package's records as a change of the origin (through
//! [`tilolang_runtime::Runtime::apply_origin`]), and says what became of the effect.
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
use tilolang_tilo::runtime::origin::OriginWrite;
use tilolang_tilo::runtime::store::FieldKey;
use tilolang_tilo::runtime::{Delivery, ResourceRef, Value};

use crate::service::{ChannelPage, SearchPage, Service, ServiceError, TrendingPage, VideoItem};

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
    /// The root's fields an answer settles, when the package declares them by name: how its
    /// search and its feed stand, and why the last one failed. An app that names them another
    /// way writes them itself.
    status: StatusFields,
}

/// The root's fields that say how the last fetch stands, by their ids.
#[derive(Default)]
struct StatusFields {
    search_state: Option<String>,
    search_trouble: Option<String>,
    feed_state: Option<String>,
    feed_trouble: Option<String>,
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
    /// What the store changed since the last poll: whether a field a request wrote, or where a
    /// search's fields, moved.
    changed: Rc<RefCell<Vec<FieldKey>>>,
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
            changed: Rc::default(),
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
        let root_meta = package.meta.types.iter().find(|t| t.id == root_ty)?;
        let collection_of = |name: &str| {
            root_meta
                .fields
                .iter()
                .find(|f| f.ty == format!("Collection<{name}>"))
                .map(|f| f.id.clone())
        };
        let field_of = |name: &str| {
            root_meta
                .fields
                .iter()
                .find(|f| f.name == name && f.ty == "Text")
                .map(|f| f.id.clone())
        };
        Some(Types {
            channels_field: collection_of("Channel")?,
            videos_field: collection_of("Video")?,
            playlists_field: collection_of("Playlist")?,
            channel,
            video,
            playlist,
            status: StatusFields {
                search_state: field_of("search_state"),
                search_trouble: field_of("search_trouble"),
                feed_state: field_of("feed_state"),
                feed_trouble: field_of("feed_trouble"),
            },
            // The root as the store made it: its id is the workspace's one resource of its
            // type, not the type's own id.
            root: host.root_of(app).ok()?,
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

/// Write `items` of the kind `ty` into the collection `field` of the root, keeping the service's
/// fields only: a field the person changes (a like, a watch-later mark) is theirs, and an answer
/// never overwrites it.
fn upsert_items(
    host: &Host,
    ty: &OriginType,
    owner: &ResourceRef,
    field: &str,
    key_field: &str,
    items: &[(String, Vec<(String, Value)>)],
) {
    let held: BTreeMap<String, ResourceRef> = items_in(host, owner, field)
        .into_iter()
        .map(|item| (read_text(host, &item, ty, key_field), item))
        .collect();
    let mut writes = Vec::new();
    for (key, fields) in items {
        match held.get(key) {
            Some(at) => writes.push(OriginWrite::Set {
                target: (*at).clone(),
                fields: fields.clone(),
            }),
            None => writes.push(OriginWrite::Insert {
                owner: owner.clone(),
                field: field.to_owned(),
                ty: ty.ty.clone(),
                fields: fields.clone(),
            }),
        }
    }
    if !writes.is_empty() {
        match host.runtime().apply_origin("youtube", writes, None) {
            Ok(made) => {
                tracing::debug!(
                    items = made.len(),
                    "what the service answered is in the record"
                );
            }
            // An answer nobody can write is not shown, and nothing asks again: it says so.
            Err(error) => {
                tracing::warn!(%error, "what the service answered is not written");
            }
        }
    }
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
        self.woken.load(Ordering::Acquire)
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
        let mut polled = Polled::default();
        if self.woken.swap(false, Ordering::AcqRel) {
            polled.wrote = self.take_answers(cx.host, cx.app);
            // The effect the service carried out is settled: what became of it is in the
            // record, and the outbox takes it as answered rather than asking again.
            polled.settle = true;
        }
        let _ = self.changed.take();
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
    fn take_answers(&mut self, host: &Host, app: &str) -> bool {
        let Some(types) = self.types.take().or_else(|| {
            // The first poll, with the package whose types hold the YouTube kinds: the one the
            // origin writes into.
            Self::types_of(host, app)
        }) else {
            return false;
        };
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
        while let Some(waiting) = self.waiting.pop_front() {
            let Some(answered) = waiting.answered else {
                self.waiting.push_front(waiting);
                break;
            };
            // What the app reads back of its own fetch: an answer settles the state it set
            // while asking, and a failure says why, so a search never reads "searching" for
            // ever.
            let (what, trouble) = match &waiting.pending {
                Pending::Search { .. } => (
                    types.status.search_state.as_deref(),
                    types.status.search_trouble.as_deref(),
                ),
                Pending::Subscriptions { .. } => (
                    types.status.feed_state.as_deref(),
                    types.status.feed_trouble.as_deref(),
                ),
                // A channel's page and a video's stream write their own fields, and the
                // trending list is what it is: none of them has a state to settle.
                Pending::Channel { .. } | Pending::Trending | Pending::Stream { .. } => {
                    (None, None)
                }
            };
            match answered {
                Err(error) => {
                    let mut fields = Vec::new();
                    if let Some(id) = what {
                        fields.push((id.to_owned(), Value::text("failed")));
                    }
                    if let Some(id) = trouble {
                        fields.push((id.to_owned(), Value::text(&error.0)));
                    }
                    wrote |= Self::write_fields(host, &types, fields);
                }
                Ok(Answered::Search(page)) => {
                    wrote |= Self::write_search(host, &types, &page);
                    wrote |= Self::settle(host, &types, what, trouble);
                }
                Ok(Answered::Channel(page)) => {
                    wrote |= Self::write_channel(host, &types, &page);
                }
                Ok(Answered::Trending(page)) => {
                    wrote |= Self::write_trending(host, &types, &page);
                }
                Ok(Answered::Subscriptions(pages)) => {
                    for page in pages {
                        wrote |= Self::write_channel(host, &types, &page);
                    }
                    wrote |= Self::settle(host, &types, what, trouble);
                }
                Ok(Answered::Stream(url)) => {
                    wrote |= Self::write_stream(host, &types, stream_key(&waiting.pending), &url);
                }
            }
        }
        self.types = Some(types);
        wrote
    }

    /// The root's state says its fetch went through, and its trouble field is cleared.
    fn settle(host: &Host, t: &Types, what: Option<&str>, trouble: Option<&str>) -> bool {
        let mut fields = Vec::new();
        if let Some(id) = what {
            fields.push((id.to_owned(), Value::text("done")));
        }
        if let Some(id) = trouble {
            fields.push((id.to_owned(), Value::text("")));
        }
        Self::write_fields(host, t, fields)
    }

    /// Set `fields` on the root, as one change of the origin.
    fn write_fields(host: &Host, t: &Types, fields: Vec<(String, Value)>) -> bool {
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
        // An answer nobody can write leaves the app saying it still searches: it says why.
        if let Err(error) = &wrote {
            tracing::warn!(%error, "the state of a fetch is not written");
        }
        wrote.is_ok()
    }

    fn write_search(host: &Host, t: &Types, page: &SearchPage) -> bool {
        let query = page.query.clone();
        let channel_items: Vec<(String, Vec<(String, Value)>)> = page
            .channels
            .iter()
            .map(|c| {
                let mut fields = Vec::new();
                put(&t.channel, &mut fields, "key", Self::text(&c.key));
                put(&t.channel, &mut fields, "name", Self::text(&c.name));
                put(&t.channel, &mut fields, "handle", Self::text(&c.handle));
                put(
                    &t.channel,
                    &mut fields,
                    "subscribers",
                    Value::Int(c.subscribers),
                );
                put(&t.channel, &mut fields, "avatar", Self::text(&c.avatar));
                put(
                    &t.channel,
                    &mut fields,
                    "description",
                    Self::text(&c.description),
                );
                put(&t.channel, &mut fields, "verified", Value::Bool(c.verified));
                put(&t.channel, &mut fields, "found_for", Self::text(&query));
                (c.key.clone(), fields)
            })
            .collect();
        upsert_items(
            host,
            &t.channel,
            &t.root,
            &t.channels_field,
            "key",
            &channel_items,
        );
        let video_items: Vec<(String, Vec<(String, Value)>)> = page
            .videos
            .iter()
            .map(|v| (v.key.clone(), video_fields(&t.video, v, &query)))
            .collect();
        upsert_items(
            host,
            &t.video,
            &t.root,
            &t.videos_field,
            "key",
            &video_items,
        );
        let playlist_items: Vec<(String, Vec<(String, Value)>)> = page
            .playlists
            .iter()
            .map(|p| {
                let mut fields = Vec::new();
                put(&t.playlist, &mut fields, "key", Self::text(&p.key));
                put(&t.playlist, &mut fields, "title", Self::text(&p.title));
                put(
                    &t.playlist,
                    &mut fields,
                    "channel_key",
                    Self::text(&p.channel_key),
                );
                put(
                    &t.playlist,
                    &mut fields,
                    "channel_name",
                    Self::text(&p.channel_name),
                );
                put(
                    &t.playlist,
                    &mut fields,
                    "video_count",
                    Value::Int(p.video_count),
                );
                put(
                    &t.playlist,
                    &mut fields,
                    "thumbnail",
                    Self::text(&p.thumbnail),
                );
                put(&t.playlist, &mut fields, "found_for", Self::text(&query));
                (p.key.clone(), fields)
            })
            .collect();
        upsert_items(
            host,
            &t.playlist,
            &t.root,
            &t.playlists_field,
            "key",
            &playlist_items,
        );
        true
    }

    fn write_channel(host: &Host, t: &Types, page: &ChannelPage) -> bool {
        let mut fields = Vec::new();
        put(
            &t.channel,
            &mut fields,
            "key",
            Self::text(&page.channel.key),
        );
        put(
            &t.channel,
            &mut fields,
            "name",
            Self::text(&page.channel.name),
        );
        put(
            &t.channel,
            &mut fields,
            "handle",
            Self::text(&page.channel.handle),
        );
        put(
            &t.channel,
            &mut fields,
            "subscribers",
            Value::Int(page.channel.subscribers),
        );
        put(
            &t.channel,
            &mut fields,
            "avatar",
            Self::text(&page.channel.avatar),
        );
        put(
            &t.channel,
            &mut fields,
            "banner",
            Self::text(&page.channel.banner),
        );
        put(
            &t.channel,
            &mut fields,
            "description",
            Self::text(&page.channel.description),
        );
        put(
            &t.channel,
            &mut fields,
            "verified",
            Value::Bool(page.channel.verified),
        );
        upsert_items(
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
        upsert_items(host, &t.video, &t.root, &t.videos_field, "key", &videos);
        true
    }

    fn write_trending(host: &Host, t: &Types, page: &TrendingPage) -> bool {
        let written: Vec<(String, Vec<(String, Value)>)> = page
            .videos
            .iter()
            .map(|v| {
                let mut fields = video_fields(&t.video, v, "");
                put(&t.video, &mut fields, "trending", Value::Bool(true));
                (v.key.clone(), fields)
            })
            .collect();
        upsert_items(host, &t.video, &t.root, &t.videos_field, "key", &written);
        true
    }

    fn write_stream(host: &Host, t: &Types, key: &str, url: &str) -> bool {
        let Some(at) = items_in(host, &t.root, &t.videos_field)
            .into_iter()
            .find(|item| read_text(host, item, &t.video, "key") == key)
        else {
            return false;
        };
        let mut fields = Vec::new();
        put(&t.video, &mut fields, "stream", Self::text(url));
        let wrote = host.runtime().apply_origin(
            "youtube",
            vec![OriginWrite::Set { target: at, fields }],
            None,
        );
        // A stream nobody can write is not shown, as an answer nobody can write is: the video
        // keeps what it had, and it says so.
        if let Err(error) = &wrote {
            tracing::warn!(%error, "the stream of a video is not written");
        }
        wrote.is_ok()
    }
}

fn stream_key(pending: &Pending) -> &str {
    match pending {
        Pending::Stream { key } => key,
        _ => "",
    }
}

/// The fields an answer writes into a video of the kind, the search that found it included.
fn video_fields(ty: &OriginType, v: &VideoItem, found_for: &str) -> Vec<(String, Value)> {
    let mut fields = Vec::new();
    put(ty, &mut fields, "key", YouTubeOrigin::text(&v.key));
    put(ty, &mut fields, "title", YouTubeOrigin::text(&v.title));
    put(
        ty,
        &mut fields,
        "channel_key",
        YouTubeOrigin::text(&v.channel_key),
    );
    put(
        ty,
        &mut fields,
        "channel_name",
        YouTubeOrigin::text(&v.channel_name),
    );
    put(
        ty,
        &mut fields,
        "channel_avatar",
        YouTubeOrigin::text(&v.channel_avatar),
    );
    put(ty, &mut fields, "views", Value::Int(v.views));
    put(
        ty,
        &mut fields,
        "published",
        YouTubeOrigin::text(&v.published),
    );
    put(ty, &mut fields, "duration", Value::Int(v.duration));
    put(
        ty,
        &mut fields,
        "description",
        YouTubeOrigin::text(&v.description),
    );
    put(ty, &mut fields, "likes", Value::Int(v.likes));
    put(
        ty,
        &mut fields,
        "thumbnail",
        YouTubeOrigin::text(&v.thumbnail),
    );
    put(ty, &mut fields, "live", Value::Bool(v.live));
    put(ty, &mut fields, "found_for", YouTubeOrigin::text(found_for));
    fields
}
