//! Microphones: an audio data output converting to mono native-endian
//! `f32` at the requested rate.

use std::collections::VecDeque;
use std::ptr::NonNull;
use std::sync::Arc;

use dispatch2::DispatchQueue;
use objc2::runtime::ProtocolObject;
use objc2_av_foundation::AVCaptureAudioDataOutput;
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSDictionary, NSString};
use tilo_lock::MutexRecover as _;

use super::{Capture, SampleDelegate, Slot, number, settle, state};
use crate::{
    AudioSource, CaptureError, CaptureState, DeviceKind, MAX_BUFFERED_SAMPLES, MicrophoneRequest,
};

/// `kAudioFormatLinearPCM` (`'lpcm'`).
const LINEAR_PCM: u32 = 0x6C70_636D;

struct Audio {
    slot: Slot<VecDeque<f32>>,
    rate: u32,
    _capture: Capture,
}

impl AudioSource for Audio {
    fn state(&mut self) -> CaptureState {
        state(&self.slot)
    }

    fn read(&mut self) -> Vec<f32> {
        self.slot
            .lock_recover()
            .latest
            .take()
            .map(Vec::from)
            .unwrap_or_default()
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }
}

pub(crate) fn microphone(
    request: &MicrophoneRequest,
) -> Result<Box<dyn AudioSource>, CaptureError> {
    let kind = DeviceKind::Microphone;
    let rate = request.sample_rate.clamp(8_000, 192_000);
    let (device, input) = super::device_input(kind, request.device.as_deref())?;
    // AVFAudio's settings keys are these very strings.
    let keys = [
        "AVFormatIDKey",
        "AVSampleRateKey",
        "AVNumberOfChannelsKey",
        "AVLinearPCMBitDepthKey",
        "AVLinearPCMIsFloatKey",
        "AVLinearPCMIsNonInterleaved",
        "AVLinearPCMIsBigEndianKey",
    ]
    .map(NSString::from_str);
    let values = [
        number(LINEAR_PCM),
        number(rate),
        number(1),
        number(32),
        number(1),
        number(0),
        number(u32::from(cfg!(target_endian = "big"))),
    ];
    let keys: Vec<&NSString> = keys.iter().map(|key| &**key).collect();
    let settings = NSDictionary::from_retained_objects(&keys, &values);
    let slot: Slot<VecDeque<f32>> = Arc::default();
    let samples = Arc::clone(&slot);
    let delegate = SampleDelegate::new(Box::new(move |sample| {
        let Some(bytes) = bytes(sample) else { return };
        let mut shared = samples.lock_recover();
        let queue = shared.latest.get_or_insert_with(VecDeque::new);
        queue.extend(
            bytes
                .chunks_exact(4)
                .filter_map(|b| b.try_into().ok().map(f32::from_ne_bytes)),
        );
        let excess = queue.len().saturating_sub(MAX_BUFFERED_SAMPLES);
        queue.drain(..excess);
        drop(shared);
        settle(&samples, CaptureState::Live);
    }));
    let queue = DispatchQueue::new("so.tilo.capture.microphone", None);
    // SAFETY: a new output, configured before any session holds it.
    let output = unsafe {
        let output = AVCaptureAudioDataOutput::new();
        output.setAudioSettings(Some(&settings));
        output.setSampleBufferDelegate_queue(
            Some(ProtocolObject::from_ref(&*delegate)),
            Some(&queue),
        );
        output
    };
    let capture = Capture::new(&input, &output, (delegate, queue), &slot, Some(&device))?;
    capture.start_when_allowed(kind, &slot);
    Ok(Box::new(Audio {
        slot,
        rate,
        _capture: capture,
    }))
}

/// The sample data `sample` carries, copied out.
fn bytes(sample: &CMSampleBuffer) -> Option<Vec<u8>> {
    // SAFETY: a sample buffer the output handed its delegate.
    let data = unsafe { sample.data_buffer() }?;
    // SAFETY: a copy of the block buffer's whole length into as many bytes.
    unsafe {
        let mut bytes = vec![0_u8; data.data_length()];
        let destination = NonNull::new(bytes.as_mut_ptr().cast())?;
        (data.copy_data_bytes(0, bytes.len(), destination) == 0).then_some(bytes)
    }
}
