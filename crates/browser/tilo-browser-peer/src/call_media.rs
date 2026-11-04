//! A page in a call, over the bridge its host carries: the turns the page and
//! the bridge take with each other.
//!
//! The page seals what its microphone and camera make and opens what the call's
//! devices send ([`tilo_call_lane::Page`]), and this hands those frames to the
//! device bridging it and takes back what the call sent ([`LaneCall`]). One
//! turn is one `POST` of a sealed [`Up`](tilo_call_lane::page::Up) and its
//! [`Down`](tilo_call_lane::page::Down) answer, on the session's own host: no
//! key of the call leaves the page, because what travels is already sealed and
//! names the bridge's device as its sender.

use std::sync::Arc;

use tilo_call_lane::page::{self, Down, Picture, Sealed, Up};
use tilo_call_lane::wire::{layer_of_wire, source_of_wire};
use tilo_call_lane::{Heard, Page, Roster, Source};
use tilo_relay_protocol::web::API_PREFIX;

use crate::{HttpClient, LinkError};

/// The route a page's turns travel on, below the host origin.
#[must_use]
pub fn route() -> String {
    format!("{API_PREFIX}{}", page::PATH)
}

/// A page's call, as far as its host's bridge is concerned: the page's own
/// sealing and opening, and the turns in between.
pub struct LaneCall {
    page: Page,
    client: Arc<dyn HttpClient>,
    /// The session the host knows this device by: the page is a member, and
    /// its turns carry the token a member's do.
    session: Option<String>,
}

impl std::fmt::Debug for LaneCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LaneCall")
            .field("call", &self.page.call())
            .field("bridge", &self.page.bridge())
            .finish_non_exhaustive()
    }
}

impl LaneCall {
    /// Take part in `page`'s call through `client`, on the session `session`
    /// the host issued this device.
    #[must_use]
    pub fn new(page: Page, client: Arc<dyn HttpClient>, session: Option<String>) -> Self {
        Self {
            page,
            client,
            session,
        }
    }

    /// The page's own side of the call: what it seals, and what it opens.
    #[must_use]
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// The page's own side of the call, to seal with a new epoch's keys or to
    /// seal under them.
    pub fn page_mut(&mut self) -> &mut Page {
        &mut self.page
    }

    /// Take one turn: hand the bridge what the page sealed, and take back what
    /// the call's devices sent since the last turn.
    ///
    /// # Errors
    ///
    /// [`LinkError::Unavailable`] when the host was not reached or carries no
    /// call lane, and [`LinkError::Refused`] when the bridge would not carry
    /// the turn (it carries another call, or the call is over).
    pub async fn turn(&self, up: Up) -> Result<Down, LinkError> {
        let body =
            postcard::to_stdvec(&up).map_err(|error| LinkError::Malformed(error.to_string()))?;
        let (status, bytes) = self
            .client
            .post(&route(), self.session.as_deref(), body)
            .await
            .map_err(LinkError::Unavailable)?;
        match status {
            200 => postcard::from_bytes(&bytes)
                .map_err(|error| LinkError::Malformed(error.to_string())),
            401 | 403 => Err(LinkError::Refused(
                String::from_utf8_lossy(&bytes).into_owned(),
            )),
            404 => Err(LinkError::Unavailable(
                "the host carries no call lane".into(),
            )),
            status => Err(LinkError::Unavailable(format!("status {status}"))),
        }
    }

    /// What a turn's answer carries for the page to play and draw: the frames
    /// the call's devices sent, opened with the call's keys, and what the
    /// devices ask of the page ([`opened`]).
    #[must_use]
    pub fn opened(&mut self, down: &Down) -> Opened {
        opened(&mut self.page, down)
    }

    /// The page's own side of the call, once the turn is over.
    #[must_use]
    pub fn into_page(self) -> Page {
        self.page
    }
}

/// What `down` carries for `page` to play and draw: the frames the call's
/// devices sent, opened with the call's keys, and what the devices ask of the
/// page.
///
/// A frame that does not open (a device the roster does not name, or one
/// sealed under an epoch this page has no key for) is dropped, as a device
/// drops a datagram it cannot open.
///
/// Who may send to the page is the devices the bridge's answer names, and the
/// bridge itself, whose key the page's own frames carry. That is not what
/// keeps a frame honest: it must still authenticate under a key derived for
/// its sender, which the host neither holds nor derives.
#[must_use]
pub fn opened(page: &mut Page, down: &Down) -> Opened {
    let bridge = page.bridge();
    page.set_roster(Roster::new(down.devices.iter().copied().chain([bridge])));
    let mut opened = Opened {
        devices: down.devices.clone(),
        shows: down.shows.clone(),
        keyframes: down.keyframes.clone(),
        ..Opened::default()
    };
    for carried in &down.audio {
        if let Ok(heard) = page.heard(carried.from, &carried.datagram) {
            opened
                .audio
                .extend(heard.into_iter().map(|frame| (carried.from, frame)));
        }
    }
    for carried in &down.pictures {
        let Some(source) = source_of_wire(carried.source) else {
            continue;
        };
        if let Ok(object) = page.opened(carried.from, source, &carried.sealed) {
            let keyframe = tilo_call_lane::picture::VideoObject::decode(&object)
                .is_ok_and(|picture| picture.keyframe);
            opened.pictures.push(OpenedPicture {
                from: carried.from,
                source,
                keyframe,
                object,
            });
        }
    }
    opened
}

/// What a turn brought the page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Opened {
    /// The devices in the call, as the bridge has a lane to them.
    pub devices: Vec<[u8; 32]>,
    /// What the call's devices said, oldest first, each with the device it
    /// came from: play it.
    pub audio: Vec<([u8; 32], Heard)>,
    /// The pictures the page is sent, in the order they came: decode each in
    /// the source's own sequence.
    pub pictures: Vec<OpenedPicture>,
    /// What each device shows of the page: the layers to encode for.
    pub shows: Vec<page::Showing>,
    /// Sources a device asks the page to start over at a keyframe.
    pub keyframes: Vec<page::Asked>,
}

/// One picture the page is sent, opened: a tilo-video/1 object of `source`,
/// which `from` sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedPicture {
    pub from: [u8; 32],
    pub source: Source,
    /// Whether a decoder may start here.
    pub keyframe: bool,
    pub object: Vec<u8>,
}

/// What a page hands its bridge on one turn, as the page's own app says it
/// (`web/voice.mjs`): the frames its microphone and encoders made, what it
/// shows of the devices in the call, and which of them it asks to start over
/// at a keyframe. Ids are hex and every payload is base64, so the page holds
/// no binary in its JSON.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LaneTurn {
    /// The call the page is in, as hex.
    pub call: String,
    /// The app and record whose channel seals the call: the keys it is sealed
    /// and opened under are that channel's, from the group the page joined.
    pub app: String,
    pub record: String,
    /// The page's microphone frames, oldest first, in the order it made them.
    pub audio: Vec<MicFrame>,
    /// The encoded pictures of the page's camera or screen, as its encoder
    /// numbers them.
    pub pictures: Vec<PictureFrame>,
    /// What the page shows of each device in the call.
    pub shows: Vec<Showing>,
    /// Sources the page asks a device to start over at a keyframe.
    pub keyframes: Vec<Asked>,
}

/// One Opus frame of the page's microphone.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MicFrame {
    /// The first sample's place in the page's 48 kHz timeline.
    pub timestamp_48k: u64,
    #[serde(with = "base64_bytes")]
    pub opus: Vec<u8>,
}

/// One encoded picture of the page's, before the call seals it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PictureFrame {
    /// The camera or the screen, as [`tilo_call_lane::wire::source_wire`] codes it.
    pub source: u8,
    /// The layer, as [`tilo_call_lane::wire::layer_wire`] codes it.
    pub layer: u8,
    /// This picture's place in its source's stream.
    pub sequence: u64,
    pub keyframe: bool,
    pub timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    /// The encoded access unit.
    #[serde(with = "base64_bytes")]
    pub payload: Vec<u8>,
}

/// What a device of the call shows of the page, as the page's app says it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Showing {
    /// The device, as hex.
    pub device: String,
    /// The layers of its camera the page shows.
    pub camera: Vec<u8>,
    pub screen: Vec<u8>,
}

/// Asking a device to start a source over at a keyframe.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Asked {
    /// The device, as hex.
    pub device: String,
    /// The source, as [`tilo_call_lane::wire::source_wire`] codes it.
    pub source: u8,
}

/// What a turn brought the page, for its app to play and draw (`web/voice.mjs`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LaneAnswer {
    /// The devices the bridge has a lane to, as hex.
    pub devices: Vec<String>,
    /// The frames the call's devices sent, opened, each with the device it
    /// came from: play them.
    pub audio: Vec<HeardFrame>,
    /// The pictures the call's devices sent, opened.
    pub pictures: Vec<SeenFrame>,
    /// What each device shows of the page: the layers to encode for.
    pub shows: Vec<Showing>,
    /// What each device asks the page to start over at a keyframe.
    pub keyframes: Vec<Asked>,
}

/// One frame of the page's microphone, opened, with the device that sent it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeardFrame {
    /// The device, as hex.
    pub from: String,
    /// The frame's place in the sender's stream: a datagram carries the
    /// frames just before its newest one too, so the page plays only a
    /// sequence it has not played.
    pub sequence: u64,
    /// The first sample's place in the sender's 48 kHz timeline.
    pub timestamp_48k: u64,
    #[serde(with = "base64_bytes")]
    pub opus: Vec<u8>,
}

/// One picture of the page's, opened, with the device that sent it: a
/// tilo-video/1 object in `object`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeenFrame {
    /// The device, as hex.
    pub from: String,
    /// The camera or the screen, as [`tilo_call_lane::wire::source_wire`] codes it.
    pub source: u8,
    /// Whether a decoder may start here.
    pub keyframe: bool,
    #[serde(with = "base64_bytes")]
    pub object: Vec<u8>,
}

impl LaneTurn {
    /// The channel whose content keys seal `workspace`'s call.
    #[must_use]
    pub fn channel(&self, workspace: &str) -> collab_types::ChannelId {
        collab_types::document_channel_id(&self.app, workspace, &self.record)
    }

    /// The turn `page` hands its bridge: its microphone's and pictures' frames
    /// sealed as the bridge's device, and what it shows and asks of the call's
    /// devices. A frame the call cannot carry (a picture source or layer no
    /// code names, a microphone frame the seal refuses) is dropped here, as
    /// the page drops what it cannot send.
    pub fn up(&self, page: &mut Page) -> Up {
        let mut up = page.up();
        for frame in &self.audio {
            if let Ok(Some(sealed)) = page.audio(frame.timestamp_48k, &frame.opus) {
                up.audio.push(sealed);
            }
        }
        for picture in &self.pictures {
            let (Some(source), Some(layer)) =
                (source_of_wire(picture.source), layer_of_wire(picture.layer))
            else {
                continue;
            };
            let sealed = Picture {
                source,
                layer,
                sequence: picture.sequence,
                keyframe: picture.keyframe,
                timestamp_us: picture.timestamp_us,
                width: picture.width,
                height: picture.height,
                payload: picture.payload.clone(),
            };
            if let Ok(sealed) = page.picture(sealed) {
                up.pictures.push(Sealed {
                    source: picture.source,
                    layer: picture.layer,
                    keyframe: picture.keyframe,
                    sealed,
                });
            }
        }
        up.shows = self.shows.iter().filter_map(Showing::to_page).collect();
        up.keyframes = self.keyframes.iter().filter_map(Asked::to_page).collect();
        up
    }
}

impl Showing {
    fn to_page(&self) -> Option<page::Showing> {
        Some(page::Showing {
            device: device_of(&self.device)?,
            camera: self.camera.clone(),
            screen: self.screen.clone(),
        })
    }
}

impl Asked {
    fn to_page(&self) -> Option<page::Asked> {
        Some(page::Asked {
            device: device_of(&self.device)?,
            source: self.source,
        })
    }
}

fn device_of(hex: &str) -> Option<[u8; 32]> {
    hex::decode(hex).ok()?.try_into().ok()
}

impl LaneAnswer {
    /// `opened` as the page's app reads it.
    #[must_use]
    pub fn of(opened: Opened) -> Self {
        Self {
            devices: opened.devices.iter().map(hex::encode).collect(),
            audio: opened
                .audio
                .into_iter()
                .map(|(from, heard)| HeardFrame {
                    from: hex::encode(from),
                    sequence: heard.sequence,
                    timestamp_48k: heard.timestamp_48k,
                    opus: heard.opus,
                })
                .collect(),
            pictures: opened
                .pictures
                .into_iter()
                .map(|picture| SeenFrame {
                    from: hex::encode(picture.from),
                    source: tilo_call_lane::wire::source_wire(picture.source),
                    keyframe: picture.keyframe,
                    object: picture.object,
                })
                .collect(),
            shows: opened
                .shows
                .into_iter()
                .map(|showing| Showing {
                    device: hex::encode(showing.device),
                    camera: showing.camera,
                    screen: showing.screen,
                })
                .collect(),
            keyframes: opened
                .keyframes
                .into_iter()
                .map(|asked| Asked {
                    device: hex::encode(asked.device),
                    source: asked.source,
                })
                .collect(),
        }
    }
}

/// Bytes in a page's JSON as base64, which is what its app carries pictures
/// and its audio in.
mod base64_bytes {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(text.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}
