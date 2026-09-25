//! Device-free compatibility utilities for existing codec diagnostics.
//! Native calls use the persistent per-publisher `VoiceAudio` pipelines.
use super::*;

#[cfg(test)]
const RATE: i32 = 48_000;
#[cfg(test)]
const CHANNELS: i32 = 1;

pub fn test_tone(rate: i32, channels: i32, ms: u64) -> Vec<i16> {
    if rate <= 0 || channels != 1 || ms > 1000 {
        return Vec::new();
    }
    let count = usize::try_from(u64::try_from(rate).unwrap_or(0) * ms / 1000).unwrap_or(0);
    (0..count)
        .map(|index| {
            ((index as f64 / f64::from(rate) * 440.0 * std::f64::consts::TAU).sin() * 12_000.0)
                as i16
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
        self.try_encode(pcm).unwrap_or_default()
    }
    fn try_encode(&mut self, pcm: &[i16]) -> Result<Vec<Vec<u8>>, AudioError> {
        if pcm.is_empty() || pcm.len() > 48_000 {
            return Err(AudioError::Capacity);
        }
        let pipeline = RunningPipeline(gst::Pipeline::new());
        let source = gst_app::AppSrc::builder().format(gst::Format::Time).build();
        source.set_caps(Some(&raw_caps()));
        source.set_property("max-bytes", 98_000u64);
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
                u64::try_from(index).map_err(|_| AudioError::Capacity)? * 20,
            ));
            writable.set_duration(gst::ClockTime::from_mseconds(20));
            source.push_buffer(buffer).map_err(|_| AudioError::Codec)?;
        }
        source.end_of_stream().map_err(|_| AudioError::Codec)?;
        let mut packets = Vec::new();
        for _ in 0..64 {
            let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(300)) else {
                break;
            };
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
        self.decoder.push(
            PlayoutFrame {
                publisher: PublicationId {
                    namespace: "codec-diagnostic".into(),
                    carrier_generation: 0,
                },
                sequence: 0,
                timestamp_48k: 0,
                expires_at: now + Duration::from_millis(300),
                payload: Some(packet.to_vec()),
            },
            now,
        )?;
        let deadline = now + Duration::from_millis(300);
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
        assert!(encoder.encode(&vec![0; 48_001]).is_empty());
        assert!(decoder.decode(&vec![0; MAX_PACKET_BYTES + 1]).is_empty());
    }
}
