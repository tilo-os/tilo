//! What a person's desktop sends to, and hears from, the live session its device's daemon
//! holds.
//!
//! The daemon owns the device's endpoint, so it is the device's one place in each workspace's
//! live session ([`crate::live_session::LiveSession`]). The desktop still captures, encodes,
//! plays out and draws; it just no longer seals. Its capture goes over the link it opens on the
//! daemon's socket ([`crate::link`]), unsealed, and the daemon seals it under the workspace's
//! keys and publishes it into the live session; what the session hears, the daemon opens and
//! hands back over the same link. Every object is a `moq-lite` object on a track, so nothing
//! here adds a wire format beside the carrier the link already speaks.
//!
//! What the desktop needs to know to capture cheaply — which pages, windows and picture layers
//! anybody asks for, and how much the session hears — the daemon publishes as objects on the
//! same link. A track the desktop starts sending after it was asked for starts with a keyframe,
//! which is what a group starts with anyway.

use std::collections::BTreeMap;
use std::sync::Arc;

use moq_net::Timestamp;
use serde::{Deserialize, Serialize};

use crate::Task;
use crate::carrier;
use crate::live::Track;
use crate::live_session::{Heard, LiveSession, Next, next};
use crate::page::PageObject;
use crate::video::{Layer, Source, VideoObject};
use crate::voice::VoiceObject;

/// The broadcast the desktop publishes its capture in.
pub const CAPTURE: &str = "capture";
/// The broadcast the daemon publishes what the live session hears and what the desktop needs to
/// know in.
pub const HELD: &str = "held";

/// The desktop's voice: one voice object per object.
pub const AUDIO: &str = "audio";
/// The desktop's presence words, each one encoded presence frame.
pub const PRESENCE: &str = "presence";
/// The desktop's activity words, each one encoded as presence carries it.
pub const ACTIVITY: &str = "activity";
/// What the desktop says in a shared window: one encoded [`crate::share::Said`] per object.
pub const SHARE: &str = "share";
/// The devices and tracks the desktop shows, as JSON [`Show`].
pub const SHOW: &str = "show";
/// Whether the desktop hears the live session's presence, as one byte.
pub const HEARING: &str = "hearing";
/// Every picture the desktop sends, in every layer: one [`PictureWire`] each.
pub const PICTURE: &str = "picture";
/// Every shared window's picture: one [`WindowWire`] each.
pub const WINDOW: &str = "window";
/// Every shared window's page: one [`PageWire`] each.
pub const PAGE: &str = "page";

/// What the live session heard, one object each.
pub const HEARD: &str = "heard";
/// What anybody asks the desktop to capture, as JSON [`Demand`].
pub const DEMAND: &str = "demand";
/// How far the live session came, as JSON [`State`].
pub const STATE: &str = "state";

/// How long the daemon keeps what it hands the desktop: a link that lasted a moment too long
/// finds the session as it is, not its whole history.
pub const HELD_RETAIN: std::time::Duration = std::time::Duration::from_secs(2);
/// How long the desktop's capture is kept for the daemon.
pub const CAPTURE_RETAIN: std::time::Duration = std::time::Duration::from_secs(2);
/// Bytes the daemon's link origins cache.
const HELD_BYTES: u64 = 8 * 1024 * 1024;
/// Bytes the desktop's capture origin caches.
const CAPTURE_BYTES: u64 = 32 * 1024 * 1024;
/// How long a reader waits for a track the desktop has not opened yet.
const OPENING_WAIT: std::time::Duration = std::time::Duration::from_millis(20);

/// The tag one picture's source is named by on the link.
#[must_use]
pub const fn source_tag(source: Source) -> u8 {
    match source {
        Source::Camera => 0,
        Source::Screen => 1,
    }
}

/// The source a picture's tag names.
#[must_use]
pub const fn source_of(tag: u8) -> Option<Source> {
    match tag {
        0 => Some(Source::Camera),
        1 => Some(Source::Screen),
        _ => None,
    }
}

/// The tag one layer is named by on the link.
#[must_use]
pub const fn layer_tag(layer: Layer) -> u8 {
    match layer {
        Layer::Low => 0,
        Layer::Mid => 1,
        Layer::High => 2,
    }
}

/// The layer a tag names.
#[must_use]
pub const fn layer_of(tag: u8) -> Option<Layer> {
    match tag {
        0 => Some(Layer::Low),
        1 => Some(Layer::Mid),
        2 => Some(Layer::High),
        _ => None,
    }
}

/// One picture the desktop sends: which source and layer it is of, then the encoded object.
#[must_use]
pub fn encode_picture(source: Source, object: &VideoObject) -> Option<Vec<u8>> {
    let mut bytes = vec![source_tag(source), layer_tag(object.layer)];
    bytes.extend_from_slice(&object.encode().ok()?);
    Some(bytes)
}

/// One shared window's picture: the tab, then the encoded object.
#[must_use]
pub fn encode_window(tab: u64, object: &VideoObject) -> Option<Vec<u8>> {
    let mut bytes = tab.to_le_bytes().to_vec();
    bytes.extend_from_slice(&object.encode().ok()?);
    Some(bytes)
}

/// One shared window's page: the tab, then the encoded object.
#[must_use]
pub fn encode_page(tab: u64, object: &PageObject) -> Option<Vec<u8>> {
    let mut bytes = tab.to_le_bytes().to_vec();
    bytes.extend_from_slice(&object.encode().ok()?);
    Some(bytes)
}

/// One object the live session heard, as the daemon hands it back: who said it, which of their
/// tracks, its place there, and its opened body.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeardWire {
    pub sender: [u8; 32],
    pub track: String,
    pub sequence: u32,
    pub body: Vec<u8>,
}

impl HeardWire {
    /// `heard` as the link carries it.
    #[must_use]
    pub fn of(heard: &Heard) -> Self {
        Self {
            sender: heard.sender,
            track: crate::live::track(heard.track),
            sequence: heard.sequence,
            body: heard.body.clone(),
        }
    }

    /// The object it carries, when its track is one this build knows.
    #[must_use]
    pub fn into_heard(self) -> Option<Heard> {
        let track = crate::live::parse_track(&self.track)?;
        Some(Heard {
            sender: self.sender,
            track,
            sequence: self.sequence,
            body: self.body,
        })
    }
}

/// What anybody asks the desktop to capture now, and where the live session stands.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Demand {
    pub revision: u64,
    pub voice: bool,
    /// Source and layer of every picture anybody shows.
    pub pictures: Vec<(u8, u8)>,
    pub pages: Vec<u64>,
    pub windows: Vec<u64>,
}

/// How far the live session came: what a call view and a shared window show of it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub ended: bool,
    pub unheard: usize,
    pub heard: u64,
    pub refused: BTreeMap<String, usize>,
    /// Each device heard, with the presence objects opened from it and how often it left.
    pub presence_from: BTreeMap<String, (u64, u64)>,
}

/// The devices and tracks the desktop shows, as the live session is told to read them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Show {
    pub device: [u8; 32],
    pub tracks: Vec<String>,
}

/// The daemon's side of one desktop's link: the live session `live` carries what the desktop
/// publishes and hands it what the session hears, until either ends.
///
/// # Errors
///
/// The link did not speak the live session's protocol, or its setup failed.
pub async fn serve<S>(transport: S, live: Arc<LiveSession>) -> Result<(), ()>
where
    S: carrier::Carrier,
    S::SendStream: Send,
    S::RecvStream: Send,
{
    let (capture, capture_task) =
        carrier::OriginTask::produce(carrier::origin_config(CAPTURE_BYTES, CAPTURE_RETAIN));
    let (held, held_task) =
        carrier::OriginTask::produce(carrier::origin_config(HELD_BYTES, HELD_RETAIN));
    // What the desktop reads is this link's own view of what the daemon holds, which answers
    // each track the session asks for: the origin itself only offers. Its broadcast and tracks
    // exist before the session starts, so the session is told of them as it arrives.
    let view = crate::mirror::Mirror::start(held.consume(), HELD_BYTES, HELD_RETAIN, None, None);
    let Some(handed) = held_tracks(&held) else {
        return Err(());
    };
    let server = moq_net::Server::new()
        .with_subscriber(capture.clone())
        .with_publisher(view.consume());
    let accepted = carrier::accept(
        transport,
        move |_grant| async move { Some((server, ())) },
        || {},
    )
    .await;
    let Ok((accepted, ())) = accepted else {
        return Err(());
    };
    let mut tasks = vec![
        Task::spawn(take_capture(capture.consume(), Arc::clone(&live))),
        Task::spawn(hand_heard(Arc::clone(&live), handed.heard)),
        Task::spawn(say_demands(Arc::clone(&live), handed.demand)),
        Task::spawn(say_state(Arc::clone(&live), handed.state)),
    ];
    let carrier::Accepted {
        session,
        driver,
        pace,
        admission: _admission,
        transport: _transport,
    } = accepted;
    let mut driver = std::pin::pin!(pace.drive(driver));
    tokio::select! {
        _ = session.closed() => {}
        _ = &mut driver => {}
    }
    tasks.clear();
    drop((capture_task, held_task, view));
    Ok(())
}

/// Read what the desktop publishes and do it in the live session, until the link ends.
async fn take_capture(from: moq_net::origin::Consumer, live: Arc<LiveSession>) {
    let Ok(broadcast) = from.routed_broadcast(CAPTURE).await else {
        return;
    };
    let names = [
        AUDIO, PRESENCE, ACTIVITY, SHARE, SHOW, HEARING, PICTURE, WINDOW, PAGE,
    ];
    let mut readers = Vec::new();
    for name in names {
        readers.push(Task::spawn(take_one(
            broadcast.clone(),
            name.to_owned(),
            Arc::clone(&live),
        )));
    }
    let _ = broadcast.closed().await;
    readers.clear();
}

/// Read one track the desktop publishes and do each object in the live session.
async fn take_one(broadcast: moq_net::broadcast::Consumer, name: String, live: Arc<LiveSession>) {
    let Some(producer) = opened(&broadcast, &name).await else {
        return;
    };
    // Control words say the state the desktop is in now: what it said before this reader
    // started still stands, so they are read from the oldest group kept. Only a control word is
    // ever read that way; capture and media start at the newest.
    let control = matches!(name.as_str(), SHOW | HEARING);
    let start = if control {
        Some(moq_net::track::Position::group(0))
    } else {
        producer.latest().map(moq_net::track::Position::group)
    };
    let Ok(mut subscriber) = producer
        .subscribe(
            moq_net::track::Subscription::default()
                .with_max_age(CAPTURE_RETAIN)
                .with_start(start),
        )
        .await
    else {
        return;
    };
    loop {
        match next(&mut subscriber).await {
            Next::Datagram(datagram) => {
                if name == AUDIO {
                    send_audio(&live, &datagram.payload);
                }
            }
            Next::Group(mut group) => {
                let sequence = u32::try_from(group.sequence).unwrap_or(u32::MAX);
                while let Ok(Some(frame)) = group.read_frame().await {
                    do_object(&live, &name, sequence, &frame.payload);
                }
            }
            Next::End => return,
        }
    }
}

/// Wait until the desktop opens `name`, or the broadcast goes away.
async fn opened(
    broadcast: &moq_net::broadcast::Consumer,
    name: &str,
) -> Option<moq_net::track::Consumer> {
    let mut gone = std::pin::pin!(broadcast.closed());
    loop {
        if let Ok(producer) = broadcast.track(name) {
            return Some(producer);
        }
        tokio::select! {
            _ = &mut gone => return None,
            () = tokio::time::sleep(OPENING_WAIT) => {}
        }
    }
}

/// Do one object the desktop published in the live session.
fn do_object(live: &LiveSession, name: &str, sequence: u32, body: &[u8]) {
    match name {
        AUDIO => send_audio(live, body),
        PRESENCE => {
            let _ = live.publish_burst(Track::Presence, &[(sequence, body)]);
        }
        ACTIVITY => {
            let _ = live.publish_burst(Track::Activity, &[(sequence, body)]);
        }
        SHARE => {
            if let Some(said) = crate::share::Said::decode(body) {
                let _ = live.say(&[said]);
            }
        }
        SHOW => {
            if let Ok(show) = serde_json::from_slice::<Show>(body) {
                let tracks: Vec<Track> = show
                    .tracks
                    .iter()
                    .filter_map(|track| crate::live::parse_track(track))
                    .collect();
                live.show(show.device, &tracks);
            }
        }
        HEARING => {
            live.hear_presence(body.first().is_some_and(|byte| *byte != 0));
        }
        PICTURE => {
            let Some((tag, rest)) = body.split_first() else {
                return;
            };
            let Some((layer, object)) = rest.split_first() else {
                return;
            };
            let (Some(source), Some(_)) = (source_of(*tag), layer_of(*layer)) else {
                return;
            };
            if let Ok(object) = VideoObject::decode(object) {
                let _ = live.send_picture(source, &object);
            }
        }
        WINDOW => {
            let Some((tab, object)) = split_tab(body) else {
                return;
            };
            if let Ok(object) = VideoObject::decode(object) {
                let _ = live.send_window(tab, &object);
            }
        }
        PAGE => {
            let Some((tab, object)) = split_tab(body) else {
                return;
            };
            if let Ok(object) = PageObject::decode(object) {
                let _ = live.send_page(tab, &object);
            }
        }
        _ => {}
    }
}

fn split_tab(body: &[u8]) -> Option<(u64, &[u8])> {
    let (tab, object) = body.split_first_chunk::<8>()?;
    Some((u64::from_le_bytes(*tab), object))
}

/// Publish the desktop's voice as this device's.
fn send_audio(live: &LiveSession, body: &[u8]) {
    let Ok(object) = VoiceObject::decode(body) else {
        return;
    };
    let _ = live.publish(
        Track::Audio,
        u32::try_from(object.sequence).unwrap_or(u32::MAX),
        body,
    );
}

/// Hand the desktop what the live session hears, one object each, until the session ends.
async fn hand_heard(live: Arc<LiveSession>, track: moq_net::track::Producer) {
    let mut sequence = 0_u64;
    loop {
        if live.ended() {
            break;
        }
        let mut wrote = false;
        while let Some(heard) = live.try_recv() {
            let Ok(body) = postcard::to_allocvec(&HeardWire::of(&heard)) else {
                continue;
            };
            if !write_one(&track, &mut sequence, &body) {
                return;
            }
            wrote = true;
        }
        if !wrote {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
    }
}

/// Tell the desktop what anybody asks the live session for, until it ends.
async fn say_demands(live: Arc<LiveSession>, track: moq_net::track::Producer) {
    let mut said = u64::MAX;
    let mut sequence = 0_u64;
    loop {
        if live.ended() {
            return;
        }
        let revision = live.demand_revision();
        if revision != said {
            said = revision;
            let demand = Demand {
                revision,
                voice: live.voice_wanted(),
                pictures: live
                    .wanted_pictures()
                    .into_iter()
                    .map(|(source, layer)| (source_tag(source), layer_tag(layer)))
                    .collect(),
                pages: live.wanted_pages(),
                windows: live.wanted_windows(),
            };
            let Ok(body) = serde_json::to_vec(&demand) else {
                return;
            };
            if !write_one(&track, &mut sequence, &body) {
                return;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// Say how far the live session came, until it ends.
async fn say_state(live: Arc<LiveSession>, track: moq_net::track::Producer) {
    let mut sequence = 0_u64;
    loop {
        let ended = live.ended();
        let state = State {
            ended,
            unheard: live.unheard(),
            heard: live.heard_total(),
            refused: live.refusals(),
            presence_from: live
                .presence_from()
                .into_iter()
                .map(|(device, counts)| (hex::encode(device), counts))
                .collect(),
        };
        let Ok(body) = serde_json::to_vec(&state) else {
            return;
        };
        if !write_one(&track, &mut sequence, &body) || ended {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// What the desktop reads, opened and announced before the session starts.
struct Handed {
    heard: moq_net::track::Producer,
    demand: moq_net::track::Producer,
    state: moq_net::track::Producer,
}

/// Open the broadcast the desktop reads and its tracks, and announce them.
fn held_tracks(held: &moq_net::origin::Producer) -> Option<Handed> {
    let broadcast = held.create_broadcast(HELD).ok()?;
    let track = |name: &str| {
        broadcast
            .create_track(
                name,
                moq_net::track::Info::default().with_max_age(HELD_RETAIN),
            )
            .ok()
    };
    let handed = Handed {
        heard: track(HEARD)?,
        demand: track(DEMAND)?,
        state: track(STATE)?,
    };
    broadcast.announce(moq_net::origin::Route::default()).ok()?;
    Some(handed)
}

/// Write `body` as one group of `track`.
fn write_one(track: &moq_net::track::Producer, sequence: &mut u64, body: &[u8]) -> bool {
    let Ok(mut group) = track.append_group() else {
        return false;
    };
    let at = Timestamp::from_scale(*sequence, 1_000_000).unwrap_or(Timestamp::ZERO);
    *sequence = sequence.wrapping_add(1);
    group.write_frame(at, body).is_ok() && group.finish().is_ok()
}

mod client;
pub use client::Call;

#[cfg(test)]
mod plumbing_tests;
