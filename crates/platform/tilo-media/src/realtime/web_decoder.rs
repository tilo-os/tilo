//! [`RealtimeDecoder`] in a page: the browser's `WebCodecs` decoder, hardware
//! where it has one (`web/tilo-media.mjs`, `TiloVideoDecoder`).
//!
//! `WebCodecs` decodes on its own schedule: [`RealtimeDecoder::decode`] hands
//! one access unit over and returns the newest picture decoded since the
//! last call, if any; [`RealtimeDecoder::take_late_picture`] collects one
//! that arrived without a newer unit. Pictures come back as RGBA, the same
//! contract as the native decoder's [`super::DecodeOutput::Rgba`].

use std::sync::Arc;
use std::time::Duration;

use wasm_bindgen::prelude::*;

use super::{DecodedPicture, DecoderConfig, PictureWake, RealtimeError, StreamCodec, StreamColor};
use crate::frame::{Frame, FramePayload};
use crate::hw::realtime::CodecElement;

#[wasm_bindgen(module = "/web/tilo-media.mjs")]
extern "C" {
    type TiloVideoDecoder;

    #[wasm_bindgen(constructor, catch)]
    fn new(codec: &str) -> Result<TiloVideoDecoder, JsValue>;
    #[wasm_bindgen(method)]
    fn decode(this: &TiloVideoDecoder, bytes: &[u8], keyframe: bool) -> bool;
    #[wasm_bindgen(method)]
    fn pending(this: &TiloVideoDecoder) -> bool;
    #[wasm_bindgen(method, js_name = hasFailed)]
    fn has_failed(this: &TiloVideoDecoder) -> bool;
    #[wasm_bindgen(method)]
    fn take(this: &TiloVideoDecoder) -> Option<Vec<u8>>;
    #[wasm_bindgen(method, js_name = takenWidthPx)]
    fn taken_width(this: &TiloVideoDecoder) -> u32;
    #[wasm_bindgen(method, js_name = takenHeightPx)]
    fn taken_height(this: &TiloVideoDecoder) -> u32;
    #[wasm_bindgen(method)]
    fn close(this: &TiloVideoDecoder);
}

/// A real-time decoder in a page; see the [module docs](self).
pub struct RealtimeDecoder {
    inner: TiloVideoDecoder,
    codec: StreamCodec,
    /// The stream has given a keyframe: units before one decode to nothing.
    started: bool,
}

impl std::fmt::Debug for RealtimeDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeDecoder")
            .field("started", &self.started)
            .finish_non_exhaustive()
    }
}

impl Drop for RealtimeDecoder {
    fn drop(&mut self) {
        self.inner.close();
    }
}

impl RealtimeDecoder {
    /// A `WebCodecs` decoder for `config.codec`.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::Unavailable`] when the browser has no `WebCodecs` or
    /// cannot decode the codec.
    pub fn new(config: DecoderConfig) -> Result<Self, RealtimeError> {
        let codec = match config.codec {
            StreamCodec::H264 => "h264",
            StreamCodec::Vp9 => "vp9",
            StreamCodec::Av1 => "av1",
            StreamCodec::HevcMain10 => "",
        };
        let inner = TiloVideoDecoder::new(codec).map_err(|_| RealtimeError::Unavailable {
            codec: config.codec,
            tried: vec!["WebCodecs".into()],
        })?;
        Ok(Self {
            inner,
            codec: config.codec,
            started: false,
        })
    }

    /// The same as [`Self::new`]: a page's decoder never waits.
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn new_without_waiting(config: DecoderConfig) -> Result<Self, RealtimeError> {
        Self::new(config)
    }

    pub fn startup_deadline(&self) -> Option<std::time::Instant> {
        None
    }

    pub fn element(&self) -> Option<&'static CodecElement> {
        None
    }

    pub fn set_display_size(&mut self, _size: Option<(u32, u32)>) {}

    pub fn set_stream_color(&mut self, _color: Option<StreamColor>) {}

    pub fn set_picture_wait(&mut self, _wait: Duration) {}

    pub fn set_picture_wake(&mut self, _wake: Option<PictureWake>) {}

    /// Hand `unit` to the browser's decoder; the newest picture decoded
    /// since the last call, if any.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::Pipeline`] once the browser's decoder failed.
    pub fn decode(
        &mut self,
        unit: &[u8],
        keyframe_hint: Option<bool>,
    ) -> Result<Option<DecodedPicture>, RealtimeError> {
        let keyframe = keyframe_hint.unwrap_or(false) || super::is_keyframe(self.codec, unit);
        if !self.started && !keyframe {
            return Ok(None);
        }
        self.started = true;
        if !self.inner.decode(unit, keyframe) || self.inner.has_failed() {
            return Err(RealtimeError::Pipeline(
                "the browser's decoder failed".into(),
            ));
        }
        self.take_late_picture()
    }

    pub fn picture_pending(&self) -> bool {
        self.inner.pending()
    }

    /// A picture the browser decoded since the last call, if any.
    ///
    /// # Errors
    ///
    /// [`RealtimeError::Pipeline`] once the browser's decoder failed.
    pub fn take_late_picture(&mut self) -> Result<Option<DecodedPicture>, RealtimeError> {
        if self.inner.has_failed() {
            return Err(RealtimeError::Pipeline(
                "the browser's decoder failed".into(),
            ));
        }
        let Some(rgba) = self.inner.take() else {
            return Ok(None);
        };
        Ok(Some(DecodedPicture {
            frame: Frame {
                width: self.inner.taken_width(),
                height: self.inner.taken_height(),
                pts_ms: 0,
                payload: FramePayload::Rgba(Arc::new(rgba)),
            },
            color: StreamColor::BT709,
        }))
    }
}
