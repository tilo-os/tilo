//! The music origin: keeps a package's types of the `music` kinds (`type Track (origin:
//! music.track)`) in step with the catalogue, from the searches, pages, streams, lyrics and
//! pinned songs its outside effects ask for.
//!
//! The origin is native code the instance runs beside the package: it holds the network, which
//! package code never sees. Each frame the instance polls it; it asks the catalogue what an
//! effect asked for, writes the answer into the package's records as one change of the origin
//! (through [`tilolang_runtime::Runtime::write_origin`]), and says what became of the effect.
//!
//! **What is kept.** A search's songs, albums, artists and playlists; the home page's
//! suggestions; an artist's page; an album's and a catalogue playlist's songs; where a song's
//! bytes play from; a song's words; and where a kept song's bytes are. Each is keyed by the
//! catalogue's own id, so asking again never shows the same song twice.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tilolang_tilo::host::Host;
use tilolang_tilo::origin::{Origin, OriginCx, OriginType, Polled, origin_types};
use tilolang_tilo::runtime::RuntimeError;
use tilolang_tilo::runtime::origin::OriginWrite;
use tilolang_tilo::runtime::{Delivery, ResourceRef, Value};

use crate::service::{
    AlbumItem, AlbumPage, ArtistItem, ArtistPage, HomePage, HomeSection, Item, Lyrics,
    PlaylistItem, SearchPage, Service, ServiceError, SpotifyDevice, SpotifyOp, SpotifyReply,
    TrackItem,
};

/// What the origin reads of each music kind it keeps in step, from the package's metadata.
struct Types {
    tracks_field: String,
    albums_field: String,
    artists_field: String,
    playlists_field: String,
    suggestions_field: String,
    sections_field: String,
    devices_field: String,
    track: OriginType,
    album: OriginType,
    artist: OriginType,
    playlist: OriginType,
    suggestion: OriginType,
    section: OriginType,
    device: OriginType,
    /// The kind of what plays next on Spotify, when the package declares it.
    queue: Option<OriginType>,
    /// The field of the library that holds the queue's songs, when the package declares it.
    queue_field: Option<String>,
    root: ResourceRef,
    root_ty: OriginType,
}

/// A request handed to the service and not answered yet.
enum Pending {
    Search {
        query: String,
    },
    Home,
    Artist {
        key: String,
    },
    Album {
        key: String,
    },
    Playlist {
        key: String,
    },
    Stream {
        key: String,
    },
    Lyrics {
        title: String,
        artist: String,
    },
    Pin {
        url: String,
        name: String,
    },
    Spotify {
        op: SpotifyOp,
        /// The record the effect is about, when the app named one: a playlist of the person's
        /// own that a write makes or changes.
        at: Option<String>,
    },
}

/// What a request came back with.
enum Answered {
    Search(SearchPage),
    Home(HomePage),
    Artist(ArtistPage),
    Album(AlbumPage),
    Playlist(AlbumPage),
    Stream(String),
    Lyrics { title: String, lyrics: Lyrics },
    Pinned { url: String, address: String },
    Spotify(SpotifyOp, Option<String>, Box<SpotifyReply>),
}

struct Waiting {
    /// What tells this question's answer from another's: a service over a socket asks on a
    /// thread of its own, and two questions handed over one after the other finish in whichever
    /// order the service gets to them, so an answer is never taken for the oldest question.
    id: u64,
    pending: Pending,
    answered: Option<Result<Answered, ServiceError>>,
}

/// What the service answered on its own threads, each by the id of the question it answers, as
/// the frame that polled the origin takes them in.
type Answer = (u64, Result<Answered, ServiceError>);
type Answers = Arc<Mutex<VecDeque<Answer>>>;

/// The origin. See the module docs.
pub struct MusicOrigin {
    service: Arc<dyn Service>,
    /// Set when the service has an answer, from the thread it answers on.
    woken: Arc<AtomicBool>,
    /// What the service answered, written where a poll takes it in, each by the question it
    /// answers: a service over a socket asks on a thread of its own, so the frame that hands it
    /// an effect never waits for the network, and the answers finish in an order of their own.
    answered: Answers,
    types: Option<Types>,
    /// Each request the service has not answered yet, oldest first.
    waiting: VecDeque<Waiting>,
    /// The id the next question is asked under.
    next_id: u64,
    /// Why the catalogue could not be reached the last time it was asked, until it answers.
    unreached: Option<String>,
    /// The instance's frame loop, woken whenever an answer is ready.
    on_answer: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl MusicOrigin {
    /// The origin over `service`, which sets `woken` when it has an answer.
    #[must_use]
    pub fn new(service: Arc<dyn Service>, woken: Arc<AtomicBool>) -> Self {
        Self {
            service,
            woken,
            answered: Arc::default(),
            types: None,
            waiting: VecDeque::new(),
            next_id: 0,
            unreached: None,
            on_answer: None,
        }
    }

    /// The origin with `wake` called whenever an answer is ready: the instance's frame loop,
    /// woken so the next frame polls it and writes what the catalogue said. With `wake` set,
    /// each question is asked on a thread of its own and `wake` runs there, so the frame that
    /// handed the effect over never waits for the network.
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
        let found = origin_types(&package.meta, "music");
        let kind = |k: &str| found.iter().find(|t| t.kind == k).cloned();
        let track = kind("music.track")?;
        let album = kind("music.album")?;
        let artist = kind("music.artist")?;
        let playlist = kind("music.playlist")?;
        let suggestion = kind("music.suggestion")?;
        let section = kind("music.section")?;
        let device = kind("music.device")?;
        let queue = kind("music.spotify_queue");
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
            tracks_field: collection_of("Track")?,
            albums_field: collection_of("Album")?,
            artists_field: collection_of("Artist")?,
            playlists_field: collection_of("Playlist")?,
            suggestions_field: collection_of("Suggestion")?,
            sections_field: collection_of("Section")?,
            devices_field: collection_of("SpotifyDevice")?,
            queue_field: if queue.is_some() {
                collection_of("SpotifyQueueItem")
            } else {
                None
            },
            track,
            album,
            artist,
            playlist,
            suggestion,
            section,
            device,
            queue,
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
            Pending::Home => Pending::Home,
            Pending::Artist { key } => Pending::Artist { key: key.clone() },
            Pending::Album { key } => Pending::Album { key: key.clone() },
            Pending::Playlist { key } => Pending::Playlist { key: key.clone() },
            Pending::Stream { key } => Pending::Stream { key: key.clone() },
            Pending::Lyrics { title, artist } => Pending::Lyrics {
                title: title.clone(),
                artist: artist.clone(),
            },
            Pending::Pin { url, name } => Pending::Pin {
                url: url.clone(),
                name: name.clone(),
            },
            Pending::Spotify { op, at } => Pending::Spotify {
                op: op.clone(),
                at: at.clone(),
            },
        }
    }

    /// Hand `pending` to the service, and wake the instance once it has answered.
    fn ask(&mut self, pending: Pending) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        if let Some(on_answer) = self.on_answer.clone() {
            let service = Arc::clone(&self.service);
            let answered = Arc::clone(&self.answered);
            let woken = Arc::clone(&self.woken);
            let asked_pending = Self::clone_pending(&pending);
            let asked = std::thread::Builder::new()
                .name("music-ask".into())
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
                    pending,
                    answered: None,
                });
                return;
            }
        }
        let answer = answer(&*self.service, &pending);
        self.waiting.push_back(Waiting {
            id,
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
        Pending::Home => service.home().map(Answered::Home),
        Pending::Artist { key } => service.artist(key).map(Answered::Artist),
        Pending::Album { key } => service.album(key).map(Answered::Album),
        Pending::Playlist { key } => service.playlist(key).map(Answered::Playlist),
        Pending::Stream { key } => service.stream(key).map(Answered::Stream),
        Pending::Lyrics { title, artist } => {
            service
                .lyrics(title, artist)
                .map(|lyrics| Answered::Lyrics {
                    title: title.clone(),
                    lyrics,
                })
        }
        Pending::Pin { url, name } => service.pin(url, name).map(|address| Answered::Pinned {
            url: url.clone(),
            address,
        }),
        Pending::Spotify { op, at } => service
            .spotify(op)
            .map(|reply| Answered::Spotify(op.clone(), at.clone(), Box::new(reply))),
    }
}

/// The writes that put `items` of the kind `ty` into the collection `field` of `owner`, keeping
/// the service's fields only: a field the person changes (a like, a keep) is theirs, and an
/// answer never overwrites it.
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

/// The fields of the record `key` names in `field`, when it is there already: what a write
/// reads back changes, never adds a record the app has not shown.
fn marked(
    host: &Host,
    t: &Types,
    ty: &OriginType,
    field: &str,
    key: &str,
    pairs: &[(&str, Value)],
) -> Vec<OriginWrite> {
    items_in(host, &t.root, field)
        .into_iter()
        .find(|at| read_text(host, at, ty, "key") == key)
        .map(|at| OriginWrite::Set {
            target: at,
            fields: declared(ty, pairs.to_vec()),
        })
        .into_iter()
        .collect()
}

/// The fields `pairs` of the record `at`, named as the record's type declares them: a type that
/// names none of them takes no write.
fn toward(host: &Host, at: &ResourceRef, pairs: Vec<(&str, Value)>) -> Vec<OriginWrite> {
    let fields: Vec<(String, Value)> = host
        .packages()
        .iter()
        .flat_map(|package| package.meta.types.iter())
        .find(|ty| ty.id == at.ty)
        .map(|ty| {
            pairs
                .into_iter()
                .filter_map(|(name, value)| {
                    ty.fields
                        .iter()
                        .find(|field| field.name == name)
                        .map(|field| (field.id.clone(), value))
                })
                .collect()
        })
        .unwrap_or_default();
    if fields.is_empty() {
        Vec::new()
    } else {
        vec![OriginWrite::Set {
            target: at.clone(),
            fields,
        }]
    }
}

/// What plays next on Spotify, written as the library's snapshot of it: every song in its
/// place, the ones that left taken out, so a second refresh never shows two queues.
fn queue_writes(host: &Host, t: &Types, songs: &[TrackItem]) -> Vec<OriginWrite> {
    let (Some(ty), Some(field)) = (&t.queue, &t.queue_field) else {
        return Vec::new();
    };
    let mut rows: BTreeMap<String, Vec<(String, Value)>> = BTreeMap::new();
    for song in songs {
        rows.insert(song.key.clone(), queue_entry_fields(ty, song));
    }
    let held = items_in(host, &t.root, field);
    let kept: Vec<String> = held
        .iter()
        .map(|at| read_text(host, at, ty, "key"))
        .collect();
    let mut writes = Vec::new();
    for at in &held {
        let key = read_text(host, at, ty, "key");
        if let Some(fields) = rows.get(&key) {
            writes.push(OriginWrite::Set {
                target: at.clone(),
                fields: fields.clone(),
            });
        } else {
            writes.push(OriginWrite::Remove {
                owner: t.root.clone(),
                field: field.clone(),
                item: at.clone(),
            });
        }
    }
    for (key, fields) in rows {
        if !kept.contains(&key) {
            writes.push(OriginWrite::Insert {
                owner: t.root.clone(),
                field: field.clone(),
                ty: ty.ty.clone(),
                fields,
            });
        }
    }
    writes
}

fn queue_entry_fields(ty: &OriginType, song: &TrackItem) -> Vec<(String, Value)> {
    declared(
        ty,
        vec![
            ("key", MusicOrigin::text(&song.key)),
            ("title", MusicOrigin::text(&song.title)),
            ("artist", MusicOrigin::text(&song.artist)),
            ("album", MusicOrigin::text(&song.album)),
            ("cover", MusicOrigin::text(&song.cover)),
            ("seconds", Value::Int(song.seconds)),
        ],
    )
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

/// `pairs`, with the ids of the fields `ty` declares: a kind that does not declare one leaves it
/// out, as it leaves out the fields of a kind an app names another way.
fn declared(ty: &OriginType, pairs: Vec<(&str, Value)>) -> Vec<(String, Value)> {
    pairs
        .into_iter()
        .filter_map(|(name, value)| Some((ty.field(name)?.to_owned(), value)))
        .collect()
}

impl Origin for MusicOrigin {
    fn name(&self) -> &'static str {
        "music"
    }

    fn serves(&self, capability: &str) -> bool {
        matches!(
            capability,
            "music.search"
                | "music.home"
                | "music.artist"
                | "music.album"
                | "music.playlist"
                | "music.stream"
                | "music.lyrics"
                | "music.pin"
                | "music.spotify"
        )
    }

    fn due(&self) -> bool {
        self.woken.load(Ordering::Acquire)
    }

    fn poll(&mut self, cx: &OriginCx<'_>) -> Polled {
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
        let text = |at: usize| {
            payload
                .as_array()
                .and_then(|items| items.get(at))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let first = payload.as_str().map_or_else(|| text(0), str::to_owned);
        // The id the app names in the payload is what a write is about: a playlist of the
        // person's own, which the answer fills in with what Spotify made of it.
        let named = |at: usize| {
            payload
                .as_array()?
                .get(at)?
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        };
        let pending = match capability {
            "music.search" => Pending::Search { query: first },
            "music.home" => Pending::Home,
            "music.artist" => Pending::Artist { key: first },
            "music.album" => Pending::Album { key: first },
            "music.playlist" => Pending::Playlist { key: first },
            "music.stream" => Pending::Stream { key: first },
            "music.lyrics" => Pending::Lyrics {
                title: first,
                artist: text(1),
            },
            "music.pin" => Pending::Pin {
                url: first,
                name: text(1),
            },
            "music.spotify" => {
                let args: Vec<String> = (1..=3).map(text).collect();
                let Some(op) = spotify_ask(&first, &args) else {
                    return Delivery::Rejected(format!(
                        "the music origin has no Spotify operation `{first}`"
                    ));
                };
                Pending::Spotify { op, at: named(2) }
            }
            _ => return Delivery::Rejected(format!("the music origin serves no `{capability}`")),
        };
        self.ask(pending);
        if let Some(wake) = &self.on_answer {
            wake();
        }
        Delivery::Pending
    }
}

impl MusicOrigin {
    /// Write what the service answered since the last poll, and say whether anything changed.
    fn take_answers(&mut self, host: &Host, t: &Types) -> bool {
        let mut wrote = false;
        let took: Vec<Answer> = self
            .answered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect();
        for (id, answer) in took {
            // The question this answers, wherever it stands: two questions handed over one
            // after the other finish in whichever order the service gets to them.
            if let Some(waiting) = self.waiting.iter_mut().find(|waiting| waiting.id == id) {
                waiting.answered = Some(answer);
            }
        }
        while let Some(mut waiting) = self.waiting.pop_front() {
            let Some(answered) = waiting.answered.take() else {
                self.waiting.push_front(waiting);
                break;
            };
            wrote |= Self::write_step(host, t, &waiting.pending, &answered);
            self.reached(&answered);
        }
        wrote
    }

    /// One answer, written as one change of the origin: whether anything was written. What
    /// cannot be written is said once in the log and not kept.
    fn write_step(
        host: &Host,
        t: &Types,
        pending: &Pending,
        answered: &Result<Answered, ServiceError>,
    ) -> bool {
        match Self::write_answer(host, t, pending, answered) {
            Ok(wrote) => wrote,
            Err(error) => {
                tracing::warn!(%error, "what the catalogue said could not be written");
                false
            }
        }
    }

    /// Whether the catalogue answered: an answer that failed says why, until one comes back.
    fn reached(&mut self, answered: &Result<Answered, ServiceError>) {
        self.unreached = match answered {
            Ok(_) => None,
            Err(error) => Some(error.0.clone()),
        };
    }

    /// The app says whether the catalogue answers, and why it is not in step.
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
            "music",
            vec![OriginWrite::Set {
                target: t.root.clone(),
                fields,
            }],
            None,
        );
        if let Err(error) = &wrote {
            tracing::warn!(%error, "what the app says of itself was not written");
        }
        wrote.is_ok()
    }

    /// Write what the service answered to `pending` as one change of the origin.
    ///
    /// # Errors
    ///
    /// A store that cannot take the change; then none of what the catalogue said is written.
    fn write_answer(
        host: &Host,
        t: &Types,
        pending: &Pending,
        answered: &Result<Answered, ServiceError>,
    ) -> Result<bool, RuntimeError> {
        let mut writes = Vec::new();
        match answered {
            Err(error) => {
                Self::state_writes(t, pending, "failed", &error.0, &mut writes);
                if matches!(pending, Pending::Spotify { .. }) {
                    Self::spotify_trouble(t, &error.0, &mut writes);
                }
            }
            Ok(Answered::Search(page)) => {
                writes.extend(search_writes(host, t, page));
                Self::state_writes(t, pending, "done", "", &mut writes);
            }
            Ok(Answered::Home(page)) => {
                writes.extend(home_writes(host, t, page));
                Self::state_writes(t, pending, "done", "", &mut writes);
            }
            Ok(Answered::Artist(page)) => writes.extend(artist_writes(host, t, page)),
            Ok(Answered::Album(page)) => {
                writes.extend(release_writes(host, t, &t.album, &t.albums_field, page));
            }
            Ok(Answered::Playlist(page)) => {
                writes.extend(release_writes(
                    host,
                    t,
                    &t.playlist,
                    &t.playlists_field,
                    page,
                ));
            }
            Ok(Answered::Stream(url)) => {
                writes.extend(stream_writes(host, t, stream_key(pending), url));
            }
            Ok(Answered::Lyrics { title, lyrics }) => {
                writes.extend(lyrics_writes(host, t, title, lyrics));
            }
            Ok(Answered::Pinned { url, address }) => {
                writes.extend(pin_writes(host, t, url, address));
            }
            Ok(Answered::Spotify(op, at, reply)) => {
                writes.extend(spotify_writes(host, t, op, at.as_ref(), reply));
            }
        }
        if writes.is_empty() {
            return Ok(false);
        }
        host.runtime()
            .write_origin("music", None, |change| change.write(writes))?;
        Ok(true)
    }

    /// Why Spotify could not do what it was asked: the app shows it in its Spotify section.
    fn spotify_trouble(t: &Types, why: &str, writes: &mut Vec<OriginWrite>) {
        let mut fields = Vec::new();
        put(
            &t.root_ty,
            &mut fields,
            "spotify_state",
            Self::text("trouble"),
        );
        put(&t.root_ty, &mut fields, "spotify_trouble", Self::text(why));
        if !fields.is_empty() {
            writes.push(OriginWrite::Set {
                target: t.root.clone(),
                fields,
            });
        }
    }

    /// What the app reads back of its own fetch: the state it stands in and why it failed.
    fn state_writes(
        t: &Types,
        pending: &Pending,
        state: &str,
        trouble: &str,
        writes: &mut Vec<OriginWrite>,
    ) {
        let (what, why) = match pending {
            Pending::Search { .. } => ("search_state", "search_trouble"),
            Pending::Home => ("home_state", "home_trouble"),
            Pending::Spotify { op, .. } => match op.name() {
                "search" => ("search_state", "search_trouble"),
                "home" => ("home_state", "home_trouble"),
                _ => return,
            },
            _ => return,
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

/// The Spotify operation an effect names, with its arguments: the payload's texts after the
/// operation's name. A switch is said as `true`/`false` or `1`/`0`.
fn spotify_ask(name: &str, args: &[String]) -> Option<SpotifyOp> {
    let arg = |at: usize| args.get(at).cloned().unwrap_or_default();
    let flag = |at: usize| matches!(args.get(at).map(String::as_str), Some("true" | "1"));
    Some(match name {
        "connect" => SpotifyOp::Connect,
        "finish" => SpotifyOp::Finish,
        "disconnect" => SpotifyOp::Disconnect,
        "status" => SpotifyOp::Status,
        "devices" => SpotifyOp::Devices,
        "transfer" => SpotifyOp::Transfer { device: arg(0) },
        "play" => SpotifyOp::Play {
            uri: arg(0),
            device: String::new(),
        },
        "pause" => SpotifyOp::Pause { device: arg(0) },
        "next" => SpotifyOp::Next { device: arg(0) },
        "previous" => SpotifyOp::Previous { device: arg(0) },
        "now" => SpotifyOp::Now,
        "search" => SpotifyOp::Search { query: arg(0) },
        "home" => SpotifyOp::Home,
        "album" => SpotifyOp::Album { key: arg(0) },
        "artist" => SpotifyOp::Artist { key: arg(0) },
        "playlist" => SpotifyOp::Playlist { key: arg(0) },
        "like" => SpotifyOp::Like {
            key: arg(0),
            liked: flag(1),
        },
        "save_album" => SpotifyOp::SaveAlbum {
            key: arg(0),
            keep: flag(1),
        },
        "follow" => SpotifyOp::Follow {
            key: arg(0),
            follow: flag(1),
        },
        "create_playlist" => SpotifyOp::CreatePlaylist { name: arg(0) },
        "edit_playlist" => SpotifyOp::EditPlaylist {
            key: arg(0),
            name: arg(1),
            description: arg(2),
        },
        "playlist_add" => SpotifyOp::AddToPlaylist {
            key: arg(0),
            uri: arg(1),
        },
        "playlist_remove" => SpotifyOp::RemoveFromPlaylist {
            key: arg(0),
            uri: arg(1),
        },
        "queue" => SpotifyOp::Queue,
        "add_to_queue" => SpotifyOp::AddToQueue { uri: arg(0) },
        "saved_page" => SpotifyOp::SavedPage {
            kind: arg(0),
            offset: arg(1).parse().unwrap_or(0),
        },
        _ => return None,
    })
}

/// What Spotify answered, written where the app reads it: its catalogue pages as any
/// catalogue's, and the person's sign-in, devices and what plays into the library's own fields.
#[allow(
    clippy::too_many_lines,
    reason = "one write for each part of an answer, in the order the library reads them"
)]
fn spotify_writes(
    host: &Host,
    t: &Types,
    op: &SpotifyOp,
    at: Option<&String>,
    reply: &SpotifyReply,
) -> Vec<OriginWrite> {
    let mut writes = Vec::new();
    // A sign-in Spotify will not renew is the person's to give again: that, and nothing else
    // the answer might have held.
    if reply.expired.yes() {
        let mut fields = Vec::new();
        put(
            &t.root_ty,
            &mut fields,
            "spotify_state",
            MusicOrigin::text("expired"),
        );
        put(
            &t.root_ty,
            &mut fields,
            "spotify_trouble",
            MusicOrigin::text("Spotify ended the sign-in"),
        );
        if !fields.is_empty() {
            writes.push(OriginWrite::Set {
                target: t.root.clone(),
                fields,
            });
        }
        return writes;
    }
    if let Some(page) = &reply.search {
        writes.extend(search_writes(host, t, page));
    }
    if let Some(page) = &reply.home {
        writes.extend(home_writes(host, t, page));
    }
    // What the person keeps in Spotify, page by page: each page says where the page after it
    // starts, so the app can ask for more without counting for itself. A home page's own write
    // already put its sections and rows there.
    for page in &reply.pages {
        if reply.home.is_none() {
            writes.extend(suggestions(
                host,
                t,
                &page.section.title,
                &page.section.items,
            ));
        }
        let mut fields = Vec::new();
        put(
            &t.root_ty,
            &mut fields,
            &format!("spotify_next_{}", page.kind),
            Value::Int(page.next),
        );
        if !fields.is_empty() {
            writes.push(OriginWrite::Set {
                target: t.root.clone(),
                fields,
            });
        }
    }
    match op {
        SpotifyOp::Album { .. } => {
            if let Some(page) = &reply.album {
                writes.extend(release_writes(host, t, &t.album, &t.albums_field, page));
            }
        }
        SpotifyOp::Artist { .. } => {
            if let Some(page) = &reply.artist {
                writes.extend(artist_writes(host, t, page));
            }
        }
        SpotifyOp::Playlist { .. } => {
            if let Some(page) = &reply.playlist {
                writes.extend(release_writes(
                    host,
                    t,
                    &t.playlist,
                    &t.playlists_field,
                    page,
                ));
            }
        }
        SpotifyOp::Like { key, .. } => writes.extend(marked(
            host,
            t,
            &t.track,
            &t.tracks_field,
            key,
            &[("liked", Value::Bool(reply.liked.yes()))],
        )),
        SpotifyOp::SaveAlbum { key, .. } => writes.extend(marked(
            host,
            t,
            &t.album,
            &t.albums_field,
            key,
            &[("in_library", Value::Bool(reply.saved.yes()))],
        )),
        SpotifyOp::Follow { key, .. } => writes.extend(marked(
            host,
            t,
            &t.artist,
            &t.artists_field,
            key,
            &[("followed", Value::Bool(reply.followed.yes()))],
        )),
        // A playlist of the person's own, as Spotify has it: the app holds the key its writes
        // go to, and what the provider says it is called now.
        SpotifyOp::CreatePlaylist { .. } | SpotifyOp::EditPlaylist { .. } => {
            if let (Some(at), Some(made)) = (at, &reply.made)
                && let Some(probe) = ResourceRef::parse_id(at, String::new())
                && let Some(at) = host.runtime().resolve_ref(&probe)
            {
                writes.extend(toward(
                    host,
                    &at,
                    vec![
                        ("spotify", MusicOrigin::text(&made.key)),
                        ("name", MusicOrigin::text(&made.title)),
                        ("description", MusicOrigin::text(&made.description)),
                    ],
                ));
            }
        }
        SpotifyOp::Queue | SpotifyOp::AddToQueue { .. } => {
            writes.extend(queue_writes(host, t, &reply.queue));
        }
        _ => {}
    }
    let mut fields = Vec::new();
    let state = if !reply.url.is_empty() {
        "connecting"
    } else if matches!(op, SpotifyOp::Disconnect) {
        "off"
    } else if !reply.account.is_empty() {
        "connected"
    } else {
        ""
    };
    if !state.is_empty() {
        put(
            &t.root_ty,
            &mut fields,
            "spotify_state",
            MusicOrigin::text(state),
        );
    }
    if !reply.url.is_empty() {
        put(
            &t.root_ty,
            &mut fields,
            "connect_url",
            MusicOrigin::text(&reply.url),
        );
    }
    if !reply.account.is_empty() {
        put(
            &t.root_ty,
            &mut fields,
            "spotify_account",
            MusicOrigin::text(&reply.account),
        );
        put(
            &t.root_ty,
            &mut fields,
            "spotify_product",
            MusicOrigin::text(&reply.product),
        );
        put(
            &t.root_ty,
            &mut fields,
            "connect_url",
            MusicOrigin::text(""),
        );
    }
    if matches!(op, SpotifyOp::Disconnect) {
        put(
            &t.root_ty,
            &mut fields,
            "spotify_account",
            MusicOrigin::text(""),
        );
        put(
            &t.root_ty,
            &mut fields,
            "spotify_product",
            MusicOrigin::text(""),
        );
        put(
            &t.root_ty,
            &mut fields,
            "connect_url",
            MusicOrigin::text(""),
        );
        put(
            &t.root_ty,
            &mut fields,
            "spotify_now",
            MusicOrigin::text(""),
        );
        put(
            &t.root_ty,
            &mut fields,
            "spotify_active",
            MusicOrigin::text(""),
        );
    }
    if !reply.now.is_empty() || !reply.active.is_empty() || reply.playing.yes() {
        put(
            &t.root_ty,
            &mut fields,
            "spotify_now",
            MusicOrigin::text(&reply.now),
        );
        put(
            &t.root_ty,
            &mut fields,
            "spotify_playing",
            Value::Bool(reply.playing.yes()),
        );
    }
    if !reply.active.is_empty() {
        put(
            &t.root_ty,
            &mut fields,
            "spotify_active",
            MusicOrigin::text(&reply.active),
        );
    }
    put(
        &t.root_ty,
        &mut fields,
        "spotify_trouble",
        MusicOrigin::text(""),
    );
    if !fields.is_empty() {
        writes.push(OriginWrite::Set {
            target: t.root.clone(),
            fields,
        });
    }
    if !reply.devices.is_empty() {
        let items: Vec<(String, Vec<(String, Value)>)> = reply
            .devices
            .iter()
            .map(|device| (device.key.clone(), device_fields(&t.device, device)))
            .collect();
        writes.extend(upsert_writes(
            host,
            &t.device,
            &t.root,
            &t.devices_field,
            "key",
            &items,
        ));
    }
    writes
}

fn device_fields(ty: &OriginType, device: &SpotifyDevice) -> Vec<(String, Value)> {
    let pairs = vec![
        ("key", MusicOrigin::text(&device.key)),
        ("name", MusicOrigin::text(&device.name)),
        ("kind", MusicOrigin::text(&device.kind)),
        ("active", Value::Bool(device.active)),
        ("volume", Value::Int(device.volume)),
    ];
    declared(ty, pairs)
}

fn track_fields(ty: &OriginType, tr: &TrackItem, found_for: &str) -> Vec<(String, Value)> {
    let mut pairs = vec![
        ("key", MusicOrigin::text(&tr.key)),
        ("title", MusicOrigin::text(&tr.title)),
        ("artist", MusicOrigin::text(&tr.artist)),
        ("artist_key", MusicOrigin::text(&tr.artist_key)),
        ("album", MusicOrigin::text(&tr.album)),
        ("album_key", MusicOrigin::text(&tr.album_key)),
        ("seconds", Value::Int(tr.seconds)),
        ("plays", Value::Int(tr.plays)),
        ("cover", MusicOrigin::text(&tr.cover)),
    ];
    // A Spotify song plays on Spotify's own device, not from bytes this device fetches: its
    // address is the URI that plays it, which the app hands back to Spotify.
    if tr.key.starts_with("spotify:") {
        pairs.push(("stream", MusicOrigin::text(&tr.key)));
    }
    if !found_for.is_empty() {
        pairs.push(("found_for", MusicOrigin::text(found_for)));
    }
    declared(ty, pairs)
}

fn album_fields(ty: &OriginType, a: &AlbumItem, found_for: &str) -> Vec<(String, Value)> {
    let mut pairs = vec![
        ("key", MusicOrigin::text(&a.key)),
        ("title", MusicOrigin::text(&a.title)),
        ("artist", MusicOrigin::text(&a.artist)),
        ("artist_key", MusicOrigin::text(&a.artist_key)),
        ("year", Value::Int(a.year)),
        ("release_kind", MusicOrigin::text(&a.kind)),
        ("cover", MusicOrigin::text(&a.cover)),
    ];
    if !found_for.is_empty() {
        pairs.push(("found_for", MusicOrigin::text(found_for)));
    }
    declared(ty, pairs)
}

fn artist_fields(ty: &OriginType, a: &ArtistItem, found_for: &str) -> Vec<(String, Value)> {
    let mut pairs = vec![
        ("key", MusicOrigin::text(&a.key)),
        ("name", MusicOrigin::text(&a.name)),
        ("subscribers", Value::Int(a.subscribers)),
        ("avatar", MusicOrigin::text(&a.avatar)),
        (
            "genres",
            Value::List(
                a.genres
                    .iter()
                    .map(|genre| MusicOrigin::text(genre))
                    .collect(),
            ),
        ),
    ];
    if !found_for.is_empty() {
        pairs.push(("found_for", MusicOrigin::text(found_for)));
    }
    declared(ty, pairs)
}

fn playlist_fields(ty: &OriginType, p: &PlaylistItem, found_for: &str) -> Vec<(String, Value)> {
    let mut pairs = vec![
        ("key", MusicOrigin::text(&p.key)),
        ("title", MusicOrigin::text(&p.title)),
        ("subtitle", MusicOrigin::text(&p.subtitle)),
        ("track_count", Value::Int(p.track_count)),
        ("cover", MusicOrigin::text(&p.cover)),
        ("description", MusicOrigin::text(&p.description)),
    ];
    if !found_for.is_empty() {
        pairs.push(("found_for", MusicOrigin::text(found_for)));
    }
    declared(ty, pairs)
}

fn suggestions(host: &Host, t: &Types, section: &str, items: &[Item]) -> Vec<OriginWrite> {
    let rows: Vec<(String, Vec<(String, Value)>)> = items
        .iter()
        .map(|item| {
            let (title, subtitle, cover) = match item {
                Item::Track(t) => (t.title.clone(), t.artist.clone(), t.cover.clone()),
                Item::Album(a) => (a.title.clone(), a.artist.clone(), a.cover.clone()),
                Item::Artist(a) => (a.name.clone(), String::new(), a.avatar.clone()),
                Item::Playlist(p) => (p.title.clone(), p.subtitle.clone(), p.cover.clone()),
            };
            let key = format!("{section}/{}", item.key());
            (
                key,
                declared(
                    &t.suggestion,
                    vec![
                        (
                            "key",
                            MusicOrigin::text(&format!("{section}/{}", item.key())),
                        ),
                        ("section", MusicOrigin::text(section)),
                        ("kind", MusicOrigin::text(item.kind())),
                        ("target", MusicOrigin::text(item.key())),
                        ("title", MusicOrigin::text(&title)),
                        ("subtitle", MusicOrigin::text(&subtitle)),
                        ("cover", MusicOrigin::text(&cover)),
                    ],
                ),
            )
        })
        .collect();
    upsert_writes(
        host,
        &t.suggestion,
        &t.root,
        &t.suggestions_field,
        "key",
        &rows,
    )
}

fn search_writes(host: &Host, t: &Types, page: &SearchPage) -> Vec<OriginWrite> {
    let query = page.query.as_str();
    let mut writes = Vec::new();
    let tracks: Vec<(String, Vec<(String, Value)>)> = page
        .tracks
        .iter()
        .map(|tr| (tr.key.clone(), track_fields(&t.track, tr, query)))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.track,
        &t.root,
        &t.tracks_field,
        "key",
        &tracks,
    ));
    let albums: Vec<(String, Vec<(String, Value)>)> = page
        .albums
        .iter()
        .map(|a| (a.key.clone(), album_fields(&t.album, a, query)))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.album,
        &t.root,
        &t.albums_field,
        "key",
        &albums,
    ));
    let artists: Vec<(String, Vec<(String, Value)>)> = page
        .artists
        .iter()
        .map(|a| (a.key.clone(), artist_fields(&t.artist, a, query)))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.artist,
        &t.root,
        &t.artists_field,
        "key",
        &artists,
    ));
    let playlists: Vec<(String, Vec<(String, Value)>)> = page
        .playlists
        .iter()
        .map(|p| (p.key.clone(), playlist_fields(&t.playlist, p, query)))
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

fn home_writes(host: &Host, t: &Types, page: &HomePage) -> Vec<OriginWrite> {
    let mut writes = Vec::new();
    let sections: Vec<(String, Vec<(String, Value)>)> = page
        .sections
        .iter()
        .enumerate()
        .map(|(at, HomeSection { title, .. })| {
            let key = format!("{at}/{title}");
            (
                key.clone(),
                declared(
                    &t.section,
                    vec![
                        ("key", MusicOrigin::text(&key)),
                        ("title", MusicOrigin::text(title)),
                        ("order", Value::Int(i64::try_from(at).unwrap_or(i64::MAX))),
                    ],
                ),
            )
        })
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.section,
        &t.root,
        &t.sections_field,
        "key",
        &sections,
    ));
    for HomeSection { title, items } in &page.sections {
        writes.extend(suggestions(host, t, title, items));
    }
    writes
}

fn artist_writes(host: &Host, t: &Types, page: &ArtistPage) -> Vec<OriginWrite> {
    let mut writes = upsert_writes(
        host,
        &t.artist,
        &t.root,
        &t.artists_field,
        "key",
        &[(
            page.artist.key.clone(),
            artist_fields(&t.artist, &page.artist, ""),
        )],
    );
    let tracks: Vec<(String, Vec<(String, Value)>)> = page
        .tracks
        .iter()
        .map(|tr| (tr.key.clone(), track_fields(&t.track, tr, "")))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.track,
        &t.root,
        &t.tracks_field,
        "key",
        &tracks,
    ));
    let albums: Vec<(String, Vec<(String, Value)>)> = page
        .albums
        .iter()
        .map(|a| (a.key.clone(), album_fields(&t.album, a, "")))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.album,
        &t.root,
        &t.albums_field,
        "key",
        &albums,
    ));
    let similar: Vec<(String, Vec<(String, Value)>)> = page
        .similar
        .iter()
        .map(|a| (a.key.clone(), artist_fields(&t.artist, a, "")))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.artist,
        &t.root,
        &t.artists_field,
        "key",
        &similar,
    ));
    writes
}

fn release_writes(
    host: &Host,
    t: &Types,
    ty: &OriginType,
    field: &str,
    page: &AlbumPage,
) -> Vec<OriginWrite> {
    let mut writes = upsert_writes(
        host,
        ty,
        &t.root,
        field,
        "key",
        &[(page.album.key.clone(), album_fields(ty, &page.album, ""))],
    );
    let tracks: Vec<(String, Vec<(String, Value)>)> = page
        .tracks
        .iter()
        .map(|tr| (tr.key.clone(), track_fields(&t.track, tr, "")))
        .collect();
    writes.extend(upsert_writes(
        host,
        &t.track,
        &t.root,
        &t.tracks_field,
        "key",
        &tracks,
    ));
    writes
}

fn stream_writes(host: &Host, t: &Types, key: &str, url: &str) -> Vec<OriginWrite> {
    let Some(at) = items_in(host, &t.root, &t.tracks_field)
        .into_iter()
        .find(|item| read_text(host, item, &t.track, "key") == key)
    else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    put(&t.track, &mut fields, "stream", MusicOrigin::text(url));
    vec![OriginWrite::Set { target: at, fields }]
}

fn stream_key(pending: &Pending) -> &str {
    match pending {
        Pending::Stream { key } => key,
        _ => "",
    }
}

fn lyrics_writes(host: &Host, t: &Types, title: &str, lyrics: &Lyrics) -> Vec<OriginWrite> {
    let Some(at) = items_in(host, &t.root, &t.tracks_field)
        .into_iter()
        .find(|item| read_text(host, item, &t.track, "title") == title)
    else {
        return Vec::new();
    };
    let body = lyrics
        .lines
        .iter()
        .map(|line| {
            if lyrics.synced {
                format!("[{}]{}\n", clock(line.at_ms), line.text)
            } else {
                format!("{}\n", line.text)
            }
        })
        .collect::<String>();
    let mut fields = Vec::new();
    put(&t.track, &mut fields, "lyrics", MusicOrigin::text(&body));
    put(
        &t.track,
        &mut fields,
        "lyrics_state",
        MusicOrigin::text(if lyrics.synced { "synced" } else { "plain" }),
    );
    vec![OriginWrite::Set { target: at, fields }]
}

fn pin_writes(host: &Host, t: &Types, url: &str, address: &str) -> Vec<OriginWrite> {
    let Some(at) = items_in(host, &t.root, &t.tracks_field)
        .into_iter()
        .find(|item| read_text(host, item, &t.track, "stream") == url)
    else {
        return Vec::new();
    };
    let mut fields = Vec::new();
    put(&t.track, &mut fields, "offline", MusicOrigin::text(address));
    vec![OriginWrite::Set { target: at, fields }]
}

/// `at` as a song's words write a time: `m:ss`, for an LRC line's tag.
fn clock(at_ms: i64) -> String {
    let seconds = at_ms.max(0) / 1000;
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}
