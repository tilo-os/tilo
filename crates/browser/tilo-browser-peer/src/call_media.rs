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

use tilo_call_lane::page::{self, Down, Up};
use tilo_call_lane::wire::source_of_wire;
use tilo_call_lane::{Heard, Page, Source};
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
    /// devices ask of the page.
    ///
    /// A frame that does not open (a device the roster does not name, or one
    /// sealed under an epoch this page has no key for) is dropped, as a device
    /// drops a datagram it cannot open.
    #[must_use]
    pub fn opened(&self, down: &Down) -> Opened {
        let mut opened = Opened {
            devices: down.devices.clone(),
            shows: down.shows.clone(),
            keyframes: down
                .keyframes
                .iter()
                .filter_map(page::Asked::source)
                .collect(),
            ..Opened::default()
        };
        for carried in &down.audio {
            if let Ok(heard) = self.page.heard(carried.from, &carried.datagram) {
                opened
                    .audio
                    .extend(heard.into_iter().map(|frame| (carried.from, frame)));
            }
        }
        for carried in &down.pictures {
            let Some(source) = source_of_wire(carried.source) else {
                continue;
            };
            if let Ok(object) = self.page.opened(carried.from, source, &carried.sealed) {
                opened.pictures.push(OpenedPicture {
                    from: carried.from,
                    source,
                    object,
                });
            }
        }
        opened
    }
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
    pub keyframes: Vec<Source>,
}

/// One picture the page is sent, opened: a tilo-video/1 object of `source`,
/// which `from` sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedPicture {
    pub from: [u8; 32],
    pub source: Source,
    pub object: Vec<u8>,
}
