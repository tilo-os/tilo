//! Device-free compatibility utilities for existing codec diagnostics.
//! Native calls use the persistent per-publisher `VoiceAudio` pipelines.
use super::{
    AudioError, Decoder, Duration, FRAME_SAMPLES, Instant, MAX_PACKET_BYTES, PlayoutFrame,
    PublicationId, RunningPipeline, encoder, gst, gst_app, raw_caps, validate_frame,
};
use gst::prelude::*;

#[cfg(test)]
const RATE: i32 = 48_000;
#[cfg(test)]
const CHANNELS: i32 = 1;
/// One encode covers at most 5 s of 48 kHz mono: long enough to observe the
/// encoder settle into DTX.
const MAX_ENCODE_SAMPLES: usize = 5 * 48_000;
const MAX_ENCODE_BYTES: u64 = 2 * 5 * 48_000 + 2_000;
/// 250 frames plus the encoder's trailing flush.
const MAX_ENCODE_PACKETS: usize = 264;

pub fn test_tone(rate: i32, channels: i32, ms: u64) -> Vec<i16> {
    if rate <= 0 || channels != 1 || ms > 1000 {
        return Vec::new();
    }
    let count =
        usize::try_from(u64::try_from(rate).unwrap_or(0).saturating_mul(ms) / 1000).unwrap_or(0);
    (0..count)
        .map(|index| {
            let index = f64::from(u32::try_from(index).unwrap_or(u32::MAX));
            #[expect(
                clippy::cast_possible_truncation,
                reason = "a sine scaled by 12000 always fits i16"
            )]
            let sample =
                ((index / f64::from(rate) * 440.0 * std::f64::consts::TAU).sin() * 12_000.0) as i16;
            sample
        })
        .collect()
}

pub struct OpusEncoder;
impl OpusEncoder {
    pub fn new() -> Option<Self> {
        gst::init().ok()?;
        encoder().ok()?;
        Some(Self)
    }
    pub fn encode(&mut self, pcm: &[i16]) -> Vec<Vec<u8>> {
        Self::try_encode(pcm).unwrap_or_default()
    }
    fn try_encode(pcm: &[i16]) -> Result<Vec<Vec<u8>>, AudioError> {
        if pcm.is_empty() || pcm.len() > MAX_ENCODE_SAMPLES {
            return Err(AudioError::Capacity);
        }
        let pipeline = RunningPipeline(gst::Pipeline::new());
        let source = gst_app::AppSrc::builder().format(gst::Format::Time).build();
        source.set_caps(Some(&raw_caps()));
        source.set_property("max-bytes", MAX_ENCODE_BYTES);
        let encode = encoder()?;
        let sink = gst_app::AppSink::builder()
            .sync(false)
            .max_buffers(64)
            .build();
        pipeline
            .0
            .add_many([source.upcast_ref(), &encode, sink.upcast_ref()])
            .map_err(|_| AudioError::Codec)?;
        gst::Element::link_many([source.upcast_ref(), &encode, sink.upcast_ref()])
            .map_err(|_| AudioError::Codec)?;
        pipeline
            .0
            .set_state(gst::State::Playing)
            .map_err(|_| AudioError::Codec)?;
        for (index, chunk) in pcm.chunks(FRAME_SAMPLES).enumerate() {
            let mut samples = chunk.to_vec();
            samples.resize(FRAME_SAMPLES, 0);
            let mut buffer = gst::Buffer::from_slice(
                samples
                    .into_iter()
                    .flat_map(i16::to_le_bytes)
                    .collect::<Vec<_>>(),
            );
            let writable = buffer.get_mut().ok_or(AudioError::Codec)?;
            writable.set_pts(gst::ClockTime::from_mseconds(
                u64::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_mul(20))
                    .ok_or(AudioError::Capacity)?,
            ));
            writable.set_duration(gst::ClockTime::from_mseconds(20));
            source.push_buffer(buffer).map_err(|_| AudioError::Codec)?;
        }
        source.end_of_stream().map_err(|_| AudioError::Codec)?;
        let bus = pipeline.0.bus().ok_or(AudioError::Codec)?;
        let mut packets = Vec::new();
        // Drain until the encoder has flushed everything: end of stream or a
        // pipeline error ends the loop, never how long one packet takes.
        loop {
            let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(100)) else {
                if sink.is_eos() {
                    break;
                }
                if bus.pop_filtered(&[gst::MessageType::Error]).is_some() {
                    return Err(AudioError::Codec);
                }
                continue;
            };
            if packets.len() == MAX_ENCODE_PACKETS {
                return Err(AudioError::Capacity);
            }
            let buffer = sample.buffer().ok_or(AudioError::Codec)?;
            if buffer.size() == 0 || buffer.size() > MAX_PACKET_BYTES {
                return Err(AudioError::InvalidFrame);
            }
            packets.push(
                buffer
                    .map_readable()
                    .map_err(|_| AudioError::Codec)?
                    .as_slice()
                    .to_vec(),
            );
        }
        if packets.is_empty() {
            return Err(AudioError::Codec);
        }
        Ok(packets)
    }
}

pub struct OpusDecoder {
    decoder: Decoder,
}
impl OpusDecoder {
    pub fn new() -> Option<Self> {
        gst::init().ok()?;
        Some(Self {
            decoder: Decoder::new().ok()?,
        })
    }
    pub fn decode(&mut self, packet: &[u8]) -> Vec<i16> {
        self.try_decode(packet).unwrap_or_default()
    }
    fn try_decode(&mut self, packet: &[u8]) -> Result<Vec<i16>, AudioError> {
        validate_frame(960, packet)?;
        let now = Instant::now();
        let deadline = now
            .checked_add(Duration::from_millis(300))
            .ok_or(AudioError::Codec)?;
        self.decoder.push(
            PlayoutFrame {
                publisher: PublicationId {
                    namespace: "codec-diagnostic".into(),
                    carrier_generation: 0,
                    incarnation: 1,
                },
                sequence: 0,
                timestamp_48k: 0,
                expires_at: deadline,
                payload: Some(packet.to_vec()),
            },
            now,
        )?;
        loop {
            self.decoder.pull(Instant::now())?;
            if let Some(frame) = self.decoder.ready.pop_front() {
                return Ok(frame.samples);
            }
            if Instant::now() >= deadline {
                return Err(AudioError::Codec);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_real_codec_roundtrip_is_device_free_and_bounded() {
        let mut encoder = OpusEncoder::new().expect("required Opus encoder");
        let mut decoder = OpusDecoder::new().expect("required Opus decoder");
        let pcm = test_tone(RATE, CHANNELS, 60);
        assert_eq!(pcm.len(), 2880);
        assert!(pcm.iter().any(|sample| sample.abs() > 1000));
        assert!(test_tone(RATE, CHANNELS, 1001).is_empty());
        let packets = encoder.encode(&pcm);
        assert!(!packets.is_empty());
        assert!(
            packets
                .iter()
                .all(|packet| !packet.is_empty() && packet.len() <= MAX_PACKET_BYTES)
        );
        assert!(
            packets
                .iter()
                .map(|packet| decoder.decode(packet).len())
                .sum::<usize>()
                >= 1920
        );
        assert!(encoder.encode(&vec![0; MAX_ENCODE_SAMPLES + 1]).is_empty());
        assert!(decoder.decode(&vec![0; MAX_PACKET_BYTES + 1]).is_empty());
    }
}
