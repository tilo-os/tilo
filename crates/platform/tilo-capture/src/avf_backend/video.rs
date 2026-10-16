//! Cameras and the screen: a video data output delivering BGRA pictures,
//! turned into the tightly packed RGBA [`Frame`]s the API hands over.

use std::sync::Arc;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_av_foundation::AVCaptureVideoDataOutput;
use objc2_core_foundation::CFString;
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress, kCVPixelBufferHeightKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey, kCVPixelFormatType_32BGRA,
    kCVReturnSuccess,
};
use objc2_foundation::{NSDictionary, NSString};
use tilo_lock::MutexRecover as _;

use super::{Capture, Feed, SampleDelegate, Slot, number, settle, state};
#[cfg(avf_camera)]
use crate::CameraRequest;
#[cfg(avf_screen)]
use crate::ScreenRequest;
use crate::{CaptureError, CaptureState, Frame, VideoSource, valid_rgba_frame};

struct Video {
    slot: Slot<Frame>,
    _capture: Capture,
}

impl VideoSource for Video {
    fn state(&mut self) -> CaptureState {
        state(&self.slot)
    }

    fn take_latest(&mut self) -> Option<Frame> {
        self.slot.lock_recover().latest.take()
    }
}

#[cfg(avf_camera)]
pub(crate) fn camera(request: &CameraRequest) -> Result<Box<dyn VideoSource>, CaptureError> {
    let kind = crate::DeviceKind::Camera;
    let (device, input) = super::device_input(kind, request.device.as_deref())?;
    let (output, feed, slot) = output(Some((request.width, request.height)), "camera");
    let capture = Capture::new(&input, &output, feed, &slot, Some(&device))?;
    capture.start_when_allowed(kind, &slot);
    Ok(Box::new(Video {
        slot,
        _capture: capture,
    }))
}

/// The main display. macOS asks for the Screen Recording permission itself;
/// until it is given the pictures hold only the desktop background.
#[cfg(avf_screen)]
pub(crate) fn screen(request: ScreenRequest) -> Result<Box<dyn VideoSource>, CaptureError> {
    use objc2::AllocAnyThread as _;
    use objc2_av_foundation::AVCaptureScreenInput;
    // SAFETY: an input for the display CoreGraphics names as the main one.
    let input = unsafe {
        let input = AVCaptureScreenInput::initWithDisplayID(
            AVCaptureScreenInput::alloc(),
            objc2_core_graphics::CGMainDisplayID(),
        )
        .ok_or(CaptureError::NotFound)?;
        input.setCapturesCursor(request.cursor);
        input
    };
    let (output, feed, slot) = output(None, "screen");
    let capture = Capture::new(&input, &output, feed, &slot, None)?;
    capture.start_now(&slot);
    Ok(Box::new(Video {
        slot,
        _capture: capture,
    }))
}

/// A Core Foundation string key as the `NSString` it is toll-free bridged to.
fn key(cf: &CFString) -> &NSString {
    // SAFETY: CFString and NSString are toll-free bridged.
    unsafe { &*std::ptr::from_ref(cf).cast::<NSString>() }
}

/// A data output delivering BGRA pictures, scaled to `size` where given,
/// into a latest-frame slot, on its own queue named after `what`.
fn output(
    size: Option<(u32, u32)>,
    what: &str,
) -> (Retained<AVCaptureVideoDataOutput>, Feed, Slot<Frame>) {
    // SAFETY: immutable framework constants.
    let (format, width, height) = unsafe {
        (
            key(kCVPixelBufferPixelFormatTypeKey),
            key(kCVPixelBufferWidthKey),
            key(kCVPixelBufferHeightKey),
        )
    };
    let mut keys = vec![format];
    let mut values = vec![number(kCVPixelFormatType_32BGRA)];
    if let Some((w, h)) = size {
        keys.extend([width, height]);
        values.extend([number(w), number(h)]);
    }
    let settings = NSDictionary::from_retained_objects(&keys, &values);
    let slot: Slot<Frame> = Arc::default();
    let frames = Arc::clone(&slot);
    let delegate = SampleDelegate::new(Box::new(move |sample| {
        if let Some(frame) = frame(sample) {
            frames.lock_recover().latest = Some(frame);
            settle(&frames, CaptureState::Live);
        }
    }));
    let queue = DispatchQueue::new(&format!("so.tilo.capture.{what}"), None);
    // SAFETY: a new output, configured before any session holds it.
    let output = unsafe {
        let output = AVCaptureVideoDataOutput::new();
        output.setVideoSettings(Some(&settings));
        output.setAlwaysDiscardsLateVideoFrames(true);
        output.setSampleBufferDelegate_queue(
            Some(ProtocolObject::from_ref(&*delegate)),
            Some(&queue),
        );
        output
    };
    (output, (delegate, queue), slot)
}

/// The BGRA picture in `sample`, as a frame.
fn frame(sample: &CMSampleBuffer) -> Option<Frame> {
    // SAFETY: a sample buffer the output handed its delegate.
    let pixels = unsafe { sample.image_buffer() }?;
    if CVPixelBufferGetPixelFormatType(&pixels) != kCVPixelFormatType_32BGRA {
        return None;
    }
    let read_only = CVPixelBufferLockFlags::ReadOnly;
    // SAFETY: locked for reading until the unlock below.
    if unsafe { CVPixelBufferLockBaseAddress(&pixels, read_only) } != kCVReturnSuccess {
        return None;
    }
    let (width, height) = (
        CVPixelBufferGetWidth(&pixels),
        CVPixelBufferGetHeight(&pixels),
    );
    let stride = CVPixelBufferGetBytesPerRow(&pixels);
    let base = CVPixelBufferGetBaseAddress(&pixels)
        .cast::<u8>()
        .cast_const();
    let rgba = stride
        .checked_mul(height)
        .filter(|_| !base.is_null())
        .and_then(|len| {
            // SAFETY: the locked buffer holds `height` rows of `stride` bytes.
            rgba_from_bgra(
                unsafe { std::slice::from_raw_parts(base, len) },
                width,
                stride,
            )
        });
    // SAFETY: the lock taken above.
    unsafe { CVPixelBufferUnlockBaseAddress(&pixels, read_only) };
    let (width, height, rgba) = (
        u32::try_from(width).ok()?,
        u32::try_from(height).ok()?,
        rgba?,
    );
    valid_rgba_frame(width, height, rgba.len()).then_some(Frame {
        width,
        height,
        rgba,
    })
}

/// Rows of `stride` bytes, each starting with `width` BGRA pixels, as
/// tightly packed RGBA; `None` when a row is shorter than its pixels.
fn rgba_from_bgra(bytes: &[u8], width: usize, stride: usize) -> Option<Vec<u8>> {
    let row = width.checked_mul(4)?;
    if stride < row || stride == 0 {
        return None;
    }
    let mut rgba = Vec::with_capacity(row.checked_mul(bytes.len().checked_div(stride)?)?);
    for line in bytes.chunks_exact(stride) {
        for pixel in line.get(..row)?.chunks_exact(4) {
            if let [b, g, r, a] = *pixel {
                rgba.extend_from_slice(&[r, g, b, a]);
            }
        }
    }
    Some(rgba)
}

#[cfg(test)]
mod tests {
    use super::rgba_from_bgra;

    #[test]
    fn bgra_rows_become_packed_rgba_without_their_padding() {
        let bgra = [
            1, 2, 3, 4, 5, 6, 7, 8, 0xEE, 0xEE, // row 0, two pixels and padding
            9, 10, 11, 12, 13, 14, 15, 16, 0xEE, 0xEE,
        ];
        assert_eq!(
            rgba_from_bgra(&bgra, 2, 10).unwrap(),
            [3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12, 15, 14, 13, 16]
        );
        assert_eq!(rgba_from_bgra(&bgra, 3, 10), None);
        assert_eq!(rgba_from_bgra(&bgra, 0, 0), None);
    }
}
