//! The page backend: `getUserMedia` and `getDisplayMedia`.

#![allow(
    clippy::unnecessary_wraps,
    reason = "the open functions share the native backends' fallible signatures"
)]

use wasm_bindgen::prelude::*;

use crate::{
    AudioSource, CameraRequest, CaptureError, CaptureState, Device, DeviceKind, Frame,
    MicrophoneRequest, ScreenRequest, VideoSource, Wake, valid_rgba_frame,
};

#[wasm_bindgen(module = "/web/tilo-capture.mjs")]
extern "C" {
    #[wasm_bindgen(js_name = devices)]
    fn js_devices(kind: &str) -> String;

    type TiloCaptureVideo;
    #[wasm_bindgen(constructor)]
    fn new(
        screen: bool,
        device_id: &str,
        width: u32,
        height: u32,
        cursor: bool,
    ) -> TiloCaptureVideo;
    #[wasm_bindgen(method, js_name = state)]
    fn status(this: &TiloCaptureVideo) -> String;
    #[wasm_bindgen(method, js_name = errorKind)]
    fn error_kind(this: &TiloCaptureVideo) -> String;
    #[wasm_bindgen(method, js_name = errorMessage)]
    fn error_message(this: &TiloCaptureVideo) -> String;
    #[wasm_bindgen(method)]
    fn frame(this: &TiloCaptureVideo) -> Option<Vec<u8>>;
    #[wasm_bindgen(method, js_name = frameWidth)]
    fn frame_width(this: &TiloCaptureVideo) -> u32;
    #[wasm_bindgen(method, js_name = frameHeight)]
    fn frame_height(this: &TiloCaptureVideo) -> u32;
    #[wasm_bindgen(method)]
    fn close(this: &TiloCaptureVideo);
    #[wasm_bindgen(method, js_name = onChange)]
    fn on_change(this: &TiloCaptureVideo, callback: &Closure<dyn Fn()>);

    type TiloCaptureAudio;
    #[wasm_bindgen(constructor)]
    fn new(device_id: &str, sample_rate: u32) -> TiloCaptureAudio;
    #[wasm_bindgen(method, js_name = state)]
    fn status(this: &TiloCaptureAudio) -> String;
    #[wasm_bindgen(method, js_name = errorKind)]
    fn error_kind(this: &TiloCaptureAudio) -> String;
    #[wasm_bindgen(method, js_name = errorMessage)]
    fn error_message(this: &TiloCaptureAudio) -> String;
    #[wasm_bindgen(method)]
    fn read(this: &TiloCaptureAudio) -> Vec<f32>;
    #[wasm_bindgen(method, js_name = sampleRate)]
    fn sample_rate(this: &TiloCaptureAudio) -> f64;
    #[wasm_bindgen(method)]
    fn close(this: &TiloCaptureAudio);
}

fn state_of(status: &str, kind: &str, message: String) -> CaptureState {
    match status {
        "live" => CaptureState::Live,
        "ended" | "closed" => CaptureState::Ended,
        "failed" => CaptureState::Failed(match kind {
            "permission" => CaptureError::PermissionDenied,
            "notfound" => CaptureError::NotFound,
            "cancelled" => CaptureError::Cancelled,
            _ => CaptureError::Failed(message),
        }),
        _ => CaptureState::Starting,
    }
}

pub(crate) fn devices(kind: DeviceKind) -> Vec<Device> {
    let name = match kind {
        DeviceKind::Camera => "camera",
        DeviceKind::Microphone => "microphone",
    };
    js_devices(name)
        .lines()
        .filter_map(|row| {
            let (id, label) = row.split_once('\t')?;
            Some(Device {
                id: id.to_owned(),
                label: label.to_owned(),
                kind,
            })
        })
        .collect()
}

/// A page capture, and the callback its status changes call while it runs.
struct Video(TiloCaptureVideo, Option<Closure<dyn Fn()>>);

impl VideoSource for Video {
    fn state(&mut self) -> CaptureState {
        state_of(
            &self.0.status(),
            &self.0.error_kind(),
            self.0.error_message(),
        )
    }

    fn take_latest(&mut self) -> Option<Frame> {
        let rgba = self.0.frame()?;
        let (width, height) = (self.0.frame_width(), self.0.frame_height());
        valid_rgba_frame(width, height, rgba.len()).then_some(Frame {
            width,
            height,
            rgba,
        })
    }

    fn notify(&mut self, wake: Wake) -> bool {
        let callback = Closure::<dyn Fn()>::new(move || wake());
        self.0.on_change(&callback);
        self.1 = Some(callback);
        true
    }
}

impl Drop for Video {
    fn drop(&mut self) {
        self.0.close();
    }
}

pub(crate) fn open_camera(request: &CameraRequest) -> Result<Box<dyn VideoSource>, CaptureError> {
    Ok(Box::new(Video(
        TiloCaptureVideo::new(
            false,
            request.device.as_deref().unwrap_or(""),
            request.width,
            request.height,
            false,
        ),
        None,
    )))
}

pub(crate) fn open_screen(request: ScreenRequest) -> Result<Box<dyn VideoSource>, CaptureError> {
    Ok(Box::new(Video(
        TiloCaptureVideo::new(true, "", 0, 0, request.cursor),
        None,
    )))
}

struct Audio(TiloCaptureAudio);

impl AudioSource for Audio {
    fn state(&mut self) -> CaptureState {
        state_of(
            &self.0.status(),
            &self.0.error_kind(),
            self.0.error_message(),
        )
    }

    fn read(&mut self) -> Vec<f32> {
        self.0.read()
    }

    fn sample_rate(&self) -> u32 {
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "an audio context's rate is a small positive integer"
        )]
        let rate = self.0.sample_rate().max(0.0) as u32;
        rate
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.0.close();
    }
}

pub(crate) fn open_microphone(
    request: &MicrophoneRequest,
) -> Result<Box<dyn AudioSource>, CaptureError> {
    Ok(Box::new(Audio(TiloCaptureAudio::new(
        request.device.as_deref().unwrap_or(""),
        request.sample_rate,
    ))))
}
