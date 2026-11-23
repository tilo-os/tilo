//! The page side of [`crate::edit`]: `web/tilo-media-edit.mjs`.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/web/tilo-media-edit.mjs")]
extern "C" {
    pub(crate) type TiloFrameSeeker;

    #[wasm_bindgen(constructor, catch)]
    pub(crate) fn new() -> Result<TiloFrameSeeker, JsValue>;
    #[wasm_bindgen(method)]
    pub(crate) fn open(this: &TiloFrameSeeker, url: &str);
    #[wasm_bindgen(method, js_name = openImage)]
    pub(crate) fn open_image(this: &TiloFrameSeeker, url: &str);
    #[wasm_bindgen(method)]
    pub(crate) fn seek(this: &TiloFrameSeeker, seconds: f64, fps: f64);
    #[wasm_bindgen(method)]
    pub(crate) fn follow(this: &TiloFrameSeeker, seconds: f64, fps: f64, rate: f64);
    #[wasm_bindgen(method, js_name = shownFrame)]
    pub(crate) fn shown_frame(this: &TiloFrameSeeker) -> f64;
    #[wasm_bindgen(method)]
    pub(crate) fn width(this: &TiloFrameSeeker) -> u32;
    #[wasm_bindgen(method)]
    pub(crate) fn height(this: &TiloFrameSeeker) -> u32;
    #[wasm_bindgen(method)]
    pub(crate) fn picture(this: &TiloFrameSeeker, max_width: u32, max_height: u32) -> Vec<u8>;
    #[wasm_bindgen(method, js_name = pictureWidth)]
    pub(crate) fn picture_width(this: &TiloFrameSeeker) -> u32;
    #[wasm_bindgen(method, js_name = pictureHeight)]
    pub(crate) fn picture_height(this: &TiloFrameSeeker) -> u32;
    #[wasm_bindgen(method, js_name = takeError)]
    pub(crate) fn take_error(this: &TiloFrameSeeker) -> Option<String>;
    #[wasm_bindgen(method)]
    pub(crate) fn close(this: &TiloFrameSeeker);

    pub(crate) type TiloClipEncoder;

    #[wasm_bindgen(constructor, catch)]
    pub(crate) fn new(
        width: u32,
        height: u32,
        fps: f64,
        bitrate: f64,
        sound: bool,
    ) -> Result<TiloClipEncoder, JsValue>;
    #[wasm_bindgen(method)]
    pub(crate) fn state(this: &TiloClipEncoder) -> String;
    #[wasm_bindgen(method, js_name = takeReason)]
    pub(crate) fn take_reason(this: &TiloClipEncoder) -> String;
    #[wasm_bindgen(method)]
    pub(crate) fn codec(this: &TiloClipEncoder) -> String;
    #[wasm_bindgen(method)]
    pub(crate) fn backlog(this: &TiloClipEncoder) -> u32;
    #[wasm_bindgen(method)]
    pub(crate) fn encode(this: &TiloClipEncoder, rgba: &[u8], index: f64, key: bool) -> bool;
    #[wasm_bindgen(method, js_name = encodeSound)]
    pub(crate) fn encode_sound(this: &TiloClipEncoder, samples: &[f32], start: f64) -> bool;
    #[wasm_bindgen(method, js_name = hasSound)]
    pub(crate) fn has_sound(this: &TiloClipEncoder) -> bool;
    #[wasm_bindgen(method)]
    pub(crate) fn finish(this: &TiloClipEncoder);
    #[wasm_bindgen(method, js_name = takeBytes)]
    pub(crate) fn take_bytes(this: &TiloClipEncoder) -> Vec<u8>;
    #[wasm_bindgen(method)]
    pub(crate) fn close(this: &TiloClipEncoder);

    #[wasm_bindgen(js_name = videoEncoderAvailable)]
    pub(crate) fn video_encoder_available() -> bool;

    pub(crate) type TiloPcmDecoder;

    #[wasm_bindgen(constructor, catch)]
    pub(crate) fn new(url: &str) -> Result<TiloPcmDecoder, JsValue>;
    #[wasm_bindgen(method)]
    pub(crate) fn state(this: &TiloPcmDecoder) -> String;
    #[wasm_bindgen(method)]
    pub(crate) fn samples(this: &TiloPcmDecoder, from: f64, count: f64) -> Vec<f32>;
    #[wasm_bindgen(method)]
    pub(crate) fn close(this: &TiloPcmDecoder);

    pub(crate) type TiloPcmPlayer;

    #[wasm_bindgen(constructor, catch)]
    pub(crate) fn new() -> Result<TiloPcmPlayer, JsValue>;
    #[wasm_bindgen(method)]
    pub(crate) fn start(this: &TiloPcmPlayer, at: f64) -> bool;
    #[wasm_bindgen(method)]
    pub(crate) fn position(this: &TiloPcmPlayer) -> f64;
    #[wasm_bindgen(method)]
    pub(crate) fn push(this: &TiloPcmPlayer, samples: &[f32], at: f64);
    #[wasm_bindgen(method)]
    pub(crate) fn stop(this: &TiloPcmPlayer);
    #[wasm_bindgen(method)]
    pub(crate) fn close(this: &TiloPcmPlayer);
}
