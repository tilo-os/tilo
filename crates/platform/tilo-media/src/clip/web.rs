//! The page recorder: the browser's `WebCodecs` VP8 encoder (`web/webm-clip.mjs`)
//! with its chunks muxed into `WebM` by [`super::webm`].
//!
//! A page has no encoding thread for Rust to drive, and the browser's encoder
//! is asynchronous: [`Recorder::push`] hands a picture over and
//! [`Recorder::finish`] reports the muxed clip to its caller once the encoder
//! has flushed. The camera worker saves and settles the capture from that
//! callback.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::prelude::*;

use super::webm::Muxer;
use super::{ClipResult, Frame, JpegFrame};

#[wasm_bindgen(module = "/web/webm-clip.mjs")]
extern "C" {
    /// Whether this browser can encode video at all.
    #[wasm_bindgen(js_name = videoEncoderAvailable)]
    fn encoder_available() -> bool;

    type JsClip;
    /// An encoder for a `width`×`height` clip, or why one cannot be made.
    #[wasm_bindgen(js_name = createClip, catch)]
    fn create_clip(width: u32, height: u32, fps: u32) -> Result<JsClip, JsValue>;
    /// Hand one RGBA frame to the encoder; an error string when it refused.
    #[wasm_bindgen(method)]
    fn push(this: &JsClip, rgba: &[u8]) -> Option<String>;
    #[wasm_bindgen(method, js_name = onChunk)]
    fn on_chunk(this: &JsClip, handler: &js_sys::Function);
    /// Flush the encoder, then call `done` with an error string (or none) once
    /// every encoded chunk has arrived.
    #[wasm_bindgen(method)]
    fn finish(this: &JsClip, done: &js_sys::Function);
    #[wasm_bindgen(method)]
    fn close(this: &JsClip);
}

/// One clip being captured. The encoder is created on the first frame, when
/// clip's size is known; every later frame must have that size.
pub struct Recorder {
    fps: u32,
    width: u32,
    height: u32,
    clip: Option<Clip>,
    failed: Option<String>,
}

struct Clip {
    encoder: JsClip,
    muxer: Rc<RefCell<Muxer>>,
    // The encoder calls this for every chunk while it lives; holding it keeps
    // the closure alive until the clip closes.
    _on_chunk: Closure<dyn FnMut(f64, bool, Vec<u8>)>,
}

impl Clip {
    fn new(width: u32, height: u32, fps: u32) -> Result<Self, String> {
        let encoder = create_clip(width, height, fps).map_err(|error| error_string(&error))?;
        let muxer = Rc::new(RefCell::new(Muxer::new(width, height, fps)));
        let sink = Rc::clone(&muxer);
        let on_chunk = Closure::<dyn FnMut(f64, bool, Vec<u8>)>::new(
            move |timestamp_us: f64, key: bool, data: Vec<u8>| {
                let timestamp_us = whole_microseconds(timestamp_us);
                sink.borrow_mut().push(timestamp_us, key, data);
            },
        );
        encoder.on_chunk(on_chunk.as_ref().unchecked_ref());
        Ok(Self {
            encoder,
            muxer,
            _on_chunk: on_chunk,
        })
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        self.encoder.close();
    }
}

impl Recorder {
    pub fn new(fps: u32, _jpeg: JpegFrame) -> Self {
        // A browser without WebCodecs cannot encode: say so when the clip is
        // asked for instead of capturing frames that go nowhere.
        let failed = (!encoder_available())
            .then(|| "this browser cannot encode video (WebCodecs is unavailable)".to_string());
        Self {
            fps: fps.max(1),
            width: 0,
            height: 0,
            clip: None,
            failed,
        }
    }

    /// Hand one frame to the encoder. Returns `false` when the frame was not
    /// recorded: nothing usable, a size change mid-clip, or an encoder that
    /// refused (see [`Recorder::finish`], which reports why).
    pub fn push(&mut self, frame: &Frame<'_>) -> bool {
        let (width, height) = (frame.width, frame.height);
        if self.failed.is_some() || width == 0 || height == 0 {
            return false;
        }
        let expected = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4));
        if expected != Some(frame.rgba.len()) {
            self.failed = Some("Camera sent a frame that is not a whole picture".to_string());
            return false;
        }
        if self.clip.is_none() {
            self.width = width;
            self.height = height;
            match Clip::new(width, height, self.fps) {
                Ok(clip) => self.clip = Some(clip),
                Err(error) => {
                    self.failed = Some(error);
                    return false;
                }
            }
        }
        if (width, height) != (self.width, self.height) {
            return false;
        }
        let Some(clip) = &self.clip else {
            return false;
        };
        if let Some(error) = clip.encoder.push(frame.rgba) {
            self.failed = Some(error);
            return false;
        }
        true
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Flush the encoder and hand the muxed clip, with its duration, to
    /// `done` — here or on a later task, when the browser has encoded the
    /// frames it still holds.
    pub fn finish(self, done: Box<dyn FnOnce(ClipResult)>) {
        let Some(clip) = self.clip else {
            done(Err(self
                .failed
                .unwrap_or_else(|| "nothing recorded".to_string())));
            return;
        };
        if let Some(error) = self.failed {
            // The encoder refused mid-clip: the frames it kept do not make
            // the recording the person made, so say why instead of saving a
            // truncated clip.
            done(Err(error));
            return;
        }
        let clip = Rc::new(clip);
        // The callback holds the clip's other reference, so the encoder and
        // its chunk handler live until the flush has delivered every chunk;
        // dropping it there closes the encoder.
        let held = Rc::clone(&clip);
        let settled = Closure::once_into_js(move |error: Option<String>| {
            let result = if let Some(error) = error {
                Err(error)
            } else {
                let muxer = held.muxer.borrow();
                muxer
                    .bytes()
                    .map(|bytes| (bytes, muxer.duration_ms()))
                    .ok_or_else(|| "nothing recorded".to_string())
            };
            done(result);
            drop(held);
        });
        clip.encoder.finish(settled.unchecked_ref());
    }
}

/// The whole microseconds a page encoder's timestamp names, truncating any
/// fraction as the container times frames in whole microseconds, or nothing
/// when it names no time: negative, not finite, or more than a `u64` holds.
fn whole_microseconds(timestamp_us: f64) -> u64 {
    Duration::try_from_secs_f64(timestamp_us / 1_000_000.0)
        .ok()
        .and_then(|named| u64::try_from(named.as_micros()).ok())
        .unwrap_or(0)
}

fn error_string(error: &JsValue) -> String {
    let message = error
        .dyn_ref::<js_sys::Error>()
        .map(|error| String::from(error.message()))
        .filter(|message| !message.is_empty());
    message
        .or_else(|| error.as_string().filter(|message| !message.is_empty()))
        .unwrap_or_else(|| "the browser could not start recording video".to_string())
}
