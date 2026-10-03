//! The page backend: the browser's own media element decodes and plays.
//!
//! Pictures come back as RGBA ([`VideoFrame`]), exactly what the synthesized
//! and `GStreamer` backends hand the toolkit, so the `VideoPlayer` widget and
//! every app paint them unchanged. The element also drives the page's Media
//! Session: media keys and the system's now-playing controls act on it
//! directly, and the engine's next poll reports the result.

use std::sync::Arc;

use wasm_bindgen::prelude::*;

use crate::event::{MediaEvent, VideoFrame};
use crate::frame::{Frame, FramePayload};
use crate::source::Source;

#[wasm_bindgen(module = "/web/tilo-media.mjs")]
extern "C" {
    type TiloMediaElement;

    #[wasm_bindgen(constructor, catch)]
    fn new() -> Result<TiloMediaElement, JsValue>;
    #[wasm_bindgen(method)]
    fn open(this: &TiloMediaElement, url: &str);
    #[wasm_bindgen(method)]
    fn play(this: &TiloMediaElement);
    #[wasm_bindgen(method)]
    fn pause(this: &TiloMediaElement);
    #[wasm_bindgen(method)]
    fn seek(this: &TiloMediaElement, ms: f64);
    #[wasm_bindgen(method, js_name = setVolume)]
    fn set_volume(this: &TiloMediaElement, volume: f64);
    #[wasm_bindgen(method, js_name = setMuted)]
    fn set_muted(this: &TiloMediaElement, muted: bool);
    #[wasm_bindgen(method, js_name = setRate)]
    fn set_rate(this: &TiloMediaElement, rate: f64);
    #[wasm_bindgen(method, js_name = positionMs)]
    fn position_ms(this: &TiloMediaElement) -> f64;
    #[wasm_bindgen(method, js_name = durationMs)]
    fn duration_ms(this: &TiloMediaElement) -> f64;
    #[wasm_bindgen(method, js_name = isPlaying)]
    fn is_playing(this: &TiloMediaElement) -> bool;
    #[wasm_bindgen(method, js_name = isEnded)]
    fn is_ended(this: &TiloMediaElement) -> bool;
    #[wasm_bindgen(method, js_name = bufferedPercent)]
    fn buffered_percent(this: &TiloMediaElement) -> f64;
    #[wasm_bindgen(method, js_name = takeError)]
    fn take_error(this: &TiloMediaElement) -> Option<String>;
    #[wasm_bindgen(method)]
    fn frame(this: &TiloMediaElement) -> Option<Vec<u8>>;
    #[wasm_bindgen(method, js_name = frameWidth)]
    fn frame_width(this: &TiloMediaElement) -> u32;
    #[wasm_bindgen(method, js_name = frameHeight)]
    fn frame_height(this: &TiloMediaElement) -> u32;
    #[wasm_bindgen(method, js_name = setMetadata)]
    fn set_metadata(this: &TiloMediaElement, title: &str, artist: &str, album: &str, artwork: &str);
    #[wasm_bindgen(method, js_name = publishState)]
    fn publish_state(this: &TiloMediaElement);
    #[wasm_bindgen(method, js_name = takeActions)]
    fn take_actions(this: &TiloMediaElement) -> String;
    #[wasm_bindgen(method)]
    fn close(this: &TiloMediaElement);

    #[wasm_bindgen(js_name = objectUrl)]
    fn object_url(bytes: &[u8], mime: &str) -> String;
    #[wasm_bindgen(js_name = mediaElementsAvailable)]
    fn media_elements_available() -> bool;
}

/// A page-local `blob:` URL over `bytes`, playable by a media element.
pub(crate) fn blob_url(bytes: &[u8], mime: &str) -> String {
    object_url(bytes, mime)
}

fn to_ms(value: f64) -> Option<u64> {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked finite and non-negative; the element reports whole milliseconds"
    )]
    (value.is_finite() && value >= 0.0).then(|| value as u64)
}

pub(crate) struct WebState {
    element: TiloMediaElement,
    opened: bool,
    announced_duration: Option<u64>,
    last_position: Option<u64>,
    last_buffering: Option<u8>,
    ended_reported: bool,
    /// A seek was issued and no picture has been read since.
    seek_pending: bool,
    latest: Option<VideoFrame>,
    latest_native: Option<Frame>,
    actions: Vec<crate::NowPlayingAction>,
}

impl WebState {
    pub(crate) fn new() -> Option<Self> {
        if !media_elements_available() {
            return None;
        }
        let element = TiloMediaElement::new().ok()?;
        Some(Self {
            element,
            opened: false,
            announced_duration: None,
            last_position: None,
            last_buffering: None,
            ended_reported: false,
            seek_pending: false,
            latest: None,
            latest_native: None,
            actions: Vec::new(),
        })
    }

    pub(crate) fn open(&mut self, source: &Source, volume: f32) {
        self.opened = true;
        self.announced_duration = None;
        self.last_position = None;
        self.last_buffering = None;
        self.ended_reported = false;
        self.latest = None;
        self.latest_native = None;
        self.element.set_volume(f64::from(volume));
        match source {
            Source::Remote { url } => self.element.open(url),
            // The page has no file system: a path is only meaningful when a
            // host mapped it to a URL the page can fetch (a same-origin path).
            other => {
                let path = other.abs_path().unwrap_or_default();
                self.element.open(&path.to_string_lossy());
            }
        }
    }

    pub(crate) fn set_playing(&mut self, playing: bool) {
        if playing {
            self.ended_reported = false;
            self.element.play();
        } else {
            self.element.pause();
        }
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.element.is_playing()
    }

    pub(crate) fn seek(&mut self, ms: u64) {
        #[allow(
            clippy::cast_precision_loss,
            reason = "milliseconds; f64 is exact to 2^53"
        )]
        self.element.seek(ms as f64);
        self.seek_pending = true;
        self.ended_reported = false;
    }

    pub(crate) fn set_volume(&mut self, volume: f32) {
        self.element.set_volume(f64::from(volume));
    }

    pub(crate) fn set_muted(&mut self, muted: bool) {
        self.element.set_muted(muted);
    }

    pub(crate) fn set_rate(&mut self, rate: f32) {
        self.element.set_rate(f64::from(rate));
    }

    pub(crate) fn position_ms(&self) -> u64 {
        to_ms(self.element.position_ms()).unwrap_or(0)
    }

    pub(crate) fn duration_ms(&self) -> Option<u64> {
        to_ms(self.element.duration_ms())
    }

    pub(crate) fn buffering_percent(&self) -> u8 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=100 first"
        )]
        let percent = self.element.buffered_percent().clamp(0.0, 100.0) as u8;
        percent
    }

    pub(crate) fn latest_frame(&self) -> Option<&VideoFrame> {
        self.latest.as_ref()
    }

    pub(crate) fn latest(&self) -> Option<&Frame> {
        self.latest_native.as_ref()
    }

    pub(crate) fn frame_is_current(&self) -> bool {
        self.latest.is_some() && !self.seek_pending
    }

    pub(crate) fn set_now_playing(&self, now: &crate::NowPlaying) {
        self.element.set_metadata(
            &now.title,
            &now.artist,
            &now.album,
            now.artwork_url.as_deref().unwrap_or(""),
        );
    }

    pub(crate) fn take_now_playing_actions(&mut self) -> Vec<crate::NowPlayingAction> {
        std::mem::take(&mut self.actions)
    }

    pub(crate) fn poll(&mut self) -> Vec<MediaEvent> {
        let mut events = Vec::new();
        if !self.opened {
            return events;
        }
        for action in self.element.take_actions().lines() {
            if let Some(action) = crate::NowPlayingAction::from_page(action) {
                self.actions.push(action);
            }
        }
        if let Some(error) = self.element.take_error() {
            events.push(MediaEvent::Error(error));
        }
        if let Some(duration) = self.duration_ms()
            && self.announced_duration != Some(duration)
        {
            self.announced_duration = Some(duration);
            events.push(MediaEvent::DurationKnown { ms: duration });
        }
        let buffering = self.buffering_percent();
        if self.last_buffering != Some(buffering) {
            self.last_buffering = Some(buffering);
            events.push(MediaEvent::Buffering { percent: buffering });
        }
        let position = self.position_ms();
        if self.last_position != Some(position) {
            self.last_position = Some(position);
            events.push(MediaEvent::Position { ms: position });
        }
        if let Some(rgba) = self.element.frame() {
            let (width, height) = (self.element.frame_width(), self.element.frame_height());
            let expected = (width as usize)
                .saturating_mul(height as usize)
                .saturating_mul(4);
            if rgba.len() == expected {
                let frame = VideoFrame::new(width, height, rgba, position);
                self.latest_native = Some(Frame {
                    width,
                    height,
                    pts_ms: position,
                    payload: FramePayload::Rgba(Arc::clone(&frame.rgba)),
                });
                self.latest = Some(frame);
                self.seek_pending = false;
            }
        }
        if self.element.is_ended() && !self.ended_reported {
            self.ended_reported = true;
            events.push(MediaEvent::Ended);
        }
        self.element.publish_state();
        events
    }
}

impl Drop for WebState {
    fn drop(&mut self) {
        self.element.close();
    }
}
