//! The desktop's side of the link into the device's live session: what it publishes unsealed
//! and what it hears opened, as its capture, playout and drawing need them.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use moq_net::Timestamp;
use tilo_lock::MutexRecover as _;

use super::{
    ACTIVITY, AUDIO, CAPTURE, DEMAND, HEARD, HEARING, HELD, HELD_RETAIN, PAGE, PICTURE, PRESENCE,
    SHARE, SHOW, STATE, WINDOW, encode_page, encode_picture, encode_window,
};
use crate::Task;
use crate::carrier;
use crate::link::{End, Opening, Session, Toward, write_route};
use crate::live::Track;
use crate::live_session::{Heard, LiveError};
use crate::page::PageObject;
use crate::video::{Layer, Source, VideoObject};

/// How long the desktop keeps its capture on the link for the daemon.
const CAPTURE_RETAIN: Duration = std::time::Duration::from_secs(2);
/// Bytes the desktop's capture origin caches.
const CAPTURE_BYTES: u64 = 32 * 1024 * 1024;
/// Bytes the daemon's hand-back origin caches.
const HELD_BYTES: u64 = 8 * 1024 * 1024;
/// How long one waits for a track the daemon has not opened yet.
const OPENING_WAIT: Duration = Duration::from_millis(20);
/// The most objects heard and not yet taken the desktop keeps.
const HEARD_KEPT: usize = 2048;
/// How many times the desktop opens a link before it gives up on this device.
const ATTEMPTS: u32 = 5;

/// One live session as the person's desktop drives it through its device's daemon: the
/// surface its capture, playout and drawing use, carried over the link.
pub struct Call {
    shared: Arc<Shared>,
    device: [u8; 32],
    /// The capture the desktop publishes, by track name, as the daemon reads it.
    tracks: Mutex<BTreeMap<String, moq_net::track::Producer>>,
    /// Moves every timestamp written, so an object is never written at one before it.
    clock: AtomicU64,
    _connected: carrier::Connected,
    _origins: (carrier::OriginTask, carrier::OriginTask),
    _readers: Vec<Task>,
}

/// What the link's readers keep for the capture side: everything the daemon says, and what the
/// capture loops have not taken yet.
struct Shared {
    heard: Mutex<VecDeque<Heard>>,
    heard_total: AtomicU64,
    unheard: AtomicU64,
    refused: Mutex<BTreeMap<String, usize>>,
    presence_from: Mutex<BTreeMap<[u8; 32], (u64, u64)>>,
    demand: Mutex<super::Demand>,
    revision: AtomicU64,
    ended: AtomicBool,
    wake: OnceLock<Arc<dyn Fn() + Send + Sync>>,
    ended_cb: OnceLock<Arc<dyn Fn() + Send + Sync>>,
    unheard_cb: OnceLock<Arc<dyn Fn() + Send + Sync>>,
    /// What anybody wants now and the last state that was shown: what is newly wanted is what
    /// a capture loop starts a keyframe for.
    seen_pictures: Mutex<BTreeSet<(u8, u8)>>,
    seen_pages: Mutex<BTreeSet<u64>>,
    seen_windows: Mutex<BTreeSet<u64>>,
    new_pictures: Mutex<BTreeSet<(u8, u8)>>,
    new_pages: Mutex<BTreeSet<u64>>,
    new_windows: Mutex<BTreeSet<u64>>,
}

impl Call {
    /// Open one link into `workspace`'s live session as this device, on the daemon's socket,
    /// and run it until it ends.
    ///
    /// # Errors
    ///
    /// The link could not be opened or admitted, or the daemon holds no live session for the
    /// workspace yet.
    pub async fn open(
        open: impl Fn() -> Opening + Send + Sync + 'static,
        workspace: String,
        device: [u8; 32],
    ) -> Result<Arc<Self>, String> {
        let (capture, capture_task) =
            carrier::OriginTask::produce(carrier::origin_config(CAPTURE_BYTES, CAPTURE_RETAIN));
        let (held, held_task) =
            carrier::OriginTask::produce(carrier::origin_config(HELD_BYTES, HELD_RETAIN));
        let shared = Arc::new(Shared {
            heard: Mutex::default(),
            heard_total: AtomicU64::new(0),
            unheard: AtomicU64::new(0),
            refused: Mutex::default(),
            presence_from: Mutex::default(),
            demand: Mutex::default(),
            revision: AtomicU64::new(0),
            ended: AtomicBool::new(false),
            wake: OnceLock::new(),
            ended_cb: OnceLock::new(),
            unheard_cb: OnceLock::new(),
            seen_pictures: Mutex::default(),
            seen_pages: Mutex::default(),
            seen_windows: Mutex::default(),
            new_pictures: Mutex::default(),
            new_pages: Mutex::default(),
            new_windows: Mutex::default(),
        });
        // The tracks the daemon reads are opened before the first object, so a reader that
        // starts with the link finds them.
        let broadcast = capture
            .create_broadcast(CAPTURE)
            .map_err(|error| error.to_string())?;
        let mut tracks = BTreeMap::new();
        for name in [
            AUDIO, PRESENCE, ACTIVITY, SHARE, SHOW, HEARING, PICTURE, WINDOW, PAGE,
        ] {
            let track = broadcast
                .create_track(
                    name,
                    moq_net::track::Info::default().with_max_age(CAPTURE_RETAIN),
                )
                .map_err(|error| error.to_string())?;
            tracks.insert(name.to_owned(), track);
        }
        // The daemon learns the capture and its tracks from their announcement.
        broadcast
            .announce(moq_net::origin::Route::default())
            .map_err(|error| error.to_string())?;
        let connected = Self::connect(&open, &workspace, capture.consume(), held.clone()).await?;
        let readers = vec![Task::spawn(read_held(held, Arc::clone(&shared)))];
        Ok(Arc::new(Self {
            shared,
            device,
            tracks: Mutex::new(tracks),
            clock: AtomicU64::new(0),
            _connected: connected,
            _origins: (capture_task, held_task),
            _readers: readers,
        }))
    }

    /// One link's carrier, admitted as this device and started, opening the daemon's route
    /// first.
    async fn connect(
        open: &(dyn Fn() -> Opening + Send + Sync),
        workspace: &str,
        capture: moq_net::origin::Consumer,
        held: moq_net::origin::Producer,
    ) -> Result<carrier::Connected, String> {
        let mut last = String::new();
        for _ in 0..ATTEMPTS {
            let opened = open();
            let result = async {
                let (reader, mut writer) = opened.await.map_err(|error| error.to_string())?;
                write_route(
                    &mut writer,
                    &Toward::Call {
                        workspace: workspace.to_owned(),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
                let transport = Session::new(reader, writer, End::Dialler);
                carrier::start(
                    transport,
                    &[0_u8],
                    Some(capture.clone()),
                    Some(held.clone()),
                )
                .await
                .map_err(|error| format!("{error:?}"))
            }
            .await;
            match result {
                Ok(connected) => return Ok(connected),
                Err(error) => last = error,
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        Err(last)
    }

    /// Who this device is.
    #[must_use]
    pub fn device(&self) -> [u8; 32] {
        self.device
    }

    /// Whether the live session ended under this link.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.shared.ended.load(Ordering::Acquire)
    }

    /// Have `wake` woken wherever the desktop next reads this session.
    pub fn set_wake(&self, wake: Arc<dyn Fn() + Send + Sync>) {
        let _ = self.shared.wake.set(wake);
    }

    /// Have `ended` run when the live session ends.
    pub fn set_ended(&self, ended: Arc<dyn Fn() + Send + Sync>) {
        let _ = self.shared.ended_cb.set(ended);
    }

    /// Have `moved` run wherever what the desktop has not heard moves.
    pub fn set_unheard_changed(&self, moved: Arc<dyn Fn() + Send + Sync>) {
        let _ = self.shared.unheard_cb.set(moved);
    }

    /// Whether this desktop reads the live session's presence at all.
    pub fn hear_presence(&self, hear: bool) {
        let _ = self.put(HEARING, &[u8::from(hear)]);
    }

    /// Show `tracks` of `device`: what this desktop's drawing reads.
    pub fn show(&self, device: [u8; 32], tracks: &[Track]) {
        let show = super::Show {
            device,
            tracks: tracks
                .iter()
                .map(|track| crate::live::track(*track))
                .collect(),
        };
        if let Ok(body) = serde_json::to_vec(&show) {
            let _ = self.put(SHOW, &body);
        }
    }

    /// What the live session asks for now, as it last said.
    #[must_use]
    pub fn demand_revision(&self) -> u64 {
        self.shared.revision.load(Ordering::Acquire)
    }

    /// The pictures anybody shows: each source's layers.
    #[must_use]
    pub fn wanted_pictures(&self) -> Vec<(Source, Layer)> {
        self.shared
            .demand
            .lock_recover()
            .pictures
            .iter()
            .filter_map(|(source, layer)| {
                Some((super::source_of(*source)?, super::layer_of(*layer)?))
            })
            .collect()
    }

    /// Whether `source` has a layer nobody asked for since the last take.
    #[must_use]
    pub fn wants_picture_keyframe(&self, source: Source) -> bool {
        let tag = super::source_tag(source);
        self.shared
            .new_pictures
            .lock_recover()
            .iter()
            .any(|(wanted, _)| *wanted == tag)
    }

    /// Take the layers of `source` that became wanted since the last take: each starts with a
    /// keyframe.
    #[must_use]
    pub fn take_picture_keyframes(&self, source: Source) -> Vec<Layer> {
        let tag = super::source_tag(source);
        let mut new = self.shared.new_pictures.lock_recover();
        let layers: Vec<Layer> = new
            .iter()
            .filter(|(wanted, _)| *wanted == tag)
            .filter_map(|(_, layer)| super::layer_of(*layer))
            .collect();
        new.retain(|(wanted, _)| *wanted != tag);
        layers
    }

    /// The shared windows anybody shows.
    #[must_use]
    pub fn wanted_windows(&self) -> Vec<u64> {
        self.shared.demand.lock_recover().windows.clone()
    }

    /// Take the windows that became wanted since the last take.
    #[must_use]
    pub fn take_window_keyframes(&self) -> Vec<u64> {
        std::mem::take(&mut *self.shared.new_windows.lock_recover())
            .into_iter()
            .collect()
    }

    /// The pages anybody shows.
    #[must_use]
    pub fn wanted_pages(&self) -> Vec<u64> {
        self.shared.demand.lock_recover().pages.clone()
    }

    /// Take the pages that became wanted since the last take: each starts with a whole frame.
    #[must_use]
    pub fn take_page_keyframes(&self) -> Vec<u64> {
        std::mem::take(&mut *self.shared.new_pages.lock_recover())
            .into_iter()
            .collect()
    }

    /// Whether anyone in the call hears this device.
    #[must_use]
    pub fn voice_wanted(&self) -> bool {
        self.shared.demand.lock_recover().voice
    }

    /// Send one picture of `source`, sealed by the daemon.
    ///
    /// # Errors
    ///
    /// The object could not be encoded, or the link is gone.
    pub fn send_picture(&self, source: Source, object: &VideoObject) -> Result<(), LiveError> {
        let body = encode_picture(source, object).ok_or(LiveError::Invalid)?;
        self.put(PICTURE, &body)
    }

    /// Send one picture of the shared window `tab`, sealed by the daemon.
    ///
    /// # Errors
    ///
    /// As [`Self::send_picture`].
    pub fn send_window(&self, tab: u64, object: &VideoObject) -> Result<(), LiveError> {
        let body = encode_window(tab, object).ok_or(LiveError::Invalid)?;
        self.put(WINDOW, &body)
    }

    /// Send one object of the shared window `tab`'s page, sealed by the daemon.
    ///
    /// # Errors
    ///
    /// As [`Self::send_picture`].
    pub fn send_page(&self, tab: u64, object: &PageObject) -> Result<(), LiveError> {
        let body = encode_page(tab, object).ok_or(LiveError::Invalid)?;
        self.put(PAGE, &body)
    }

    /// What this desktop says in a shared window, sealed by the daemon.
    ///
    /// # Errors
    ///
    /// One of `said` could not be encoded, or the link is gone.
    pub fn say(&self, said: &[crate::share::Said]) -> Result<(), LiveError> {
        for one in said {
            let body = one.encode().map_err(|_| LiveError::Invalid)?;
            self.put(SHARE, &body)?;
        }
        Ok(())
    }

    /// Publish `body` as object `sequence` of `track`: voice and presence travel this way, and
    /// everything else has its own call.
    ///
    /// # Errors
    ///
    /// [`LiveError::Invalid`] for a track only [`Self::send_picture`],
    /// [`Self::send_window`], [`Self::send_page`] or [`Self::say`] carries, and
    /// [`LiveError::Ended`] once the session ended.
    pub fn publish(&self, track: Track, _sequence: u32, body: &[u8]) -> Result<(), LiveError> {
        match track {
            Track::Audio => self.put_datagram(AUDIO, body),
            Track::Presence => self.put(PRESENCE, body),
            Track::Activity => self.put(ACTIVITY, body),
            _ => Err(LiveError::Invalid),
        }
    }

    /// The next object the live session heard, opened, or `None`.
    #[must_use]
    pub fn try_recv(&self) -> Option<Heard> {
        self.shared.heard.lock_recover().pop_front()
    }

    /// How many objects the live session opened for this desktop.
    #[must_use]
    pub fn heard_total(&self) -> u64 {
        self.shared.heard_total.load(Ordering::Relaxed)
    }

    /// Devices the live session reads but has no room to read, as it last said.
    #[must_use]
    pub fn unheard(&self) -> usize {
        self.shared.unheard.load(Ordering::Relaxed) as usize
    }

    /// Why the live session could not open what it read, by reason and count.
    #[must_use]
    pub fn refusals(&self) -> BTreeMap<String, usize> {
        self.shared.refused.lock_recover().clone()
    }

    /// Per device, the presence objects opened from it and how often it was heard leaving.
    #[must_use]
    pub fn presence_from(&self) -> BTreeMap<[u8; 32], (u64, u64)> {
        self.shared.presence_from.lock_recover().clone()
    }

    /// Write `body` as one group of `name`.
    fn put(&self, name: &str, body: &[u8]) -> Result<(), LiveError> {
        if self.ended() {
            return Err(LiveError::Ended);
        }
        let track = {
            let tracks = self.tracks.lock_recover();
            tracks.get(name).cloned().ok_or(LiveError::Unavailable)?
        };
        let at = self.now();
        let mut group = track.append_group().map_err(|_| LiveError::Unavailable)?;
        group
            .write_frame(at, body)
            .map_err(|_| LiveError::Unavailable)?;
        group.finish().map_err(|_| LiveError::Unavailable)
    }

    /// Write `body` as one datagram of `name`.
    fn put_datagram(&self, name: &str, body: &[u8]) -> Result<(), LiveError> {
        if self.ended() {
            return Err(LiveError::Ended);
        }
        let mut tracks = self.tracks.lock_recover();
        let track = tracks.get_mut(name).ok_or(LiveError::Unavailable)?;
        let at = self.now();
        track
            .append_datagram(at, body)
            .map(|_| ())
            .map_err(|_| LiveError::Unavailable)
    }

    /// Now on this desktop's link clock.
    fn now(&self) -> Timestamp {
        let at = self.clock.fetch_add(1, Ordering::Relaxed);
        Timestamp::from_scale(at, 1_000_000).unwrap_or(Timestamp::ZERO)
    }
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Call")
            .field("device", &hex::encode(self.device))
            .field("ended", &self.ended())
            .finish_non_exhaustive()
    }
}

/// Read what the daemon publishes on `held` until the link ends.
async fn read_held(held: moq_net::origin::Producer, shared: Arc<Shared>) {
    let Some(held_names) = crate::live::exact(&[HELD]) else {
        return;
    };
    let Ok(consumer) = held.consume().scope("", &held_names) else {
        return;
    };
    let Ok(broadcast) = consumer.routed_broadcast(HELD).await else {
        shared.finish();
        return;
    };
    let mut readers = Vec::new();
    for name in [HEARD, DEMAND, STATE] {
        readers.push(Task::spawn(read_one(
            broadcast.clone(),
            name.to_owned(),
            Arc::clone(&shared),
        )));
    }
    let _ = broadcast.closed().await;
    readers.clear();
    shared.finish();
}

/// Read one track the daemon publishes.
async fn read_one(broadcast: moq_net::broadcast::Consumer, name: String, shared: Arc<Shared>) {
    let mut gone = std::pin::pin!(broadcast.closed());
    let producer = loop {
        if let Ok(producer) = broadcast.track(name.as_str()) {
            break producer;
        }
        tokio::select! {
            _ = &mut gone => return,
            () = tokio::time::sleep(OPENING_WAIT) => {}
        }
    };
    // Everything still kept: what the daemon said before this link started reading is the
    // state the session is in.
    let Ok(mut subscriber) = producer
        .subscribe(
            moq_net::track::Subscription::default()
                .with_max_age(crate::link_call::HELD_RETAIN)
                .with_start(moq_net::track::Position::group(0)),
        )
        .await
    else {
        return;
    };
    loop {
        match crate::live_session::next(&mut subscriber).await {
            crate::live_session::Next::Datagram(_) => {}
            crate::live_session::Next::Group(mut group) => {
                while let Ok(Some(frame)) = group.read_frame().await {
                    match name.as_str() {
                        HEARD => shared.heard_object(&frame.payload),
                        DEMAND => shared.demand_object(&frame.payload),
                        STATE => shared.state_object(&frame.payload),
                        _ => {}
                    }
                }
            }
            crate::live_session::Next::End => return,
        }
    }
}

impl Shared {
    fn finish(&self) {
        if !self.ended.swap(true, Ordering::AcqRel) {
            if let Some(ended) = self.ended_cb.get() {
                ended();
            }
        }
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }

    fn heard_object(&self, body: &[u8]) {
        let Ok(wire) = postcard::from_bytes::<super::HeardWire>(body) else {
            return;
        };
        let Some(heard) = wire.into_heard() else {
            return;
        };
        let mut heard_queue = self.heard.lock_recover();
        if heard_queue.len() >= HEARD_KEPT {
            heard_queue.pop_front();
        }
        heard_queue.push_back(heard);
        drop(heard_queue);
        self.heard_total.fetch_add(1, Ordering::Relaxed);
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }

    fn demand_object(&self, body: &[u8]) {
        let Ok(demand) = serde_json::from_slice::<super::Demand>(body) else {
            return;
        };
        {
            let pictures: BTreeSet<(u8, u8)> = demand.pictures.iter().copied().collect();
            let pages: BTreeSet<u64> = demand.pages.iter().copied().collect();
            let windows: BTreeSet<u64> = demand.windows.iter().copied().collect();
            let mut seen = self.seen_pictures.lock_recover();
            let mut new = self.new_pictures.lock_recover();
            for picture in pictures.difference(&seen) {
                new.insert(*picture);
            }
            new.retain(|picture| pictures.contains(picture));
            *seen = pictures;
            drop(new);
            drop(seen);
            let mut seen = self.seen_pages.lock_recover();
            let mut new = self.new_pages.lock_recover();
            for page in pages.difference(&seen) {
                new.insert(*page);
            }
            new.retain(|page| pages.contains(page));
            *seen = pages;
            drop(new);
            drop(seen);
            let mut seen = self.seen_windows.lock_recover();
            let mut new = self.new_windows.lock_recover();
            for window in windows.difference(&seen) {
                new.insert(*window);
            }
            new.retain(|window| windows.contains(window));
            *seen = windows;
        }
        self.revision.store(demand.revision, Ordering::Release);
        *self.demand.lock_recover() = demand;
        if let Some(wake) = self.wake.get() {
            wake();
        }
    }

    fn state_object(&self, body: &[u8]) {
        let Ok(state) = serde_json::from_slice::<super::State>(body) else {
            return;
        };
        let unheard_moved =
            self.unheard.swap(state.unheard as u64, Ordering::AcqRel) != state.unheard as u64;
        self.heard_total.store(state.heard, Ordering::Relaxed);
        *self.refused.lock_recover() = state.refused;
        *self.presence_from.lock_recover() = state
            .presence_from
            .iter()
            .filter_map(|(device, counts)| {
                Some((hex::decode(device).ok()?.try_into().ok()?, *counts))
            })
            .collect();
        if unheard_moved && let Some(moved) = self.unheard_cb.get() {
            moved();
        }
        if state.ended {
            self.finish();
        } else if let Some(wake) = self.wake.get() {
            wake();
        }
    }
}
